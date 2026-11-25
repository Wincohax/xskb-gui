//! Pure HID packet framing, checksum, reply parsing, and HID key labels for the
//! XsEngine keyboard protocol (see `docs/protocol.md`).

pub const VID: u16 = 0x1CA2;
pub const PID: u16 = 0x1901;
pub const VENDOR_USAGE_PAGE: u16 = 0xFFA0;
pub const REPORT_SIZE: usize = 64;
pub const MAGIC: u8 = 0x5C;

pub const CMD_SUB: u8 = 0x00;
pub const CMD_INFO: u8 = 0x01;
pub const CMD_CONFIG: u8 = 0x18;
pub const CMD_RGB: u8 = 0x2A;
pub const CMD_MATRIX: u8 = 0x2B;

pub const HOST_MODE: u8 = 0x04;
pub const HOST_EFFECT_KEYBOARD: u8 = 0x15;

pub const MATRIX_ROWS: usize = 6;
pub const MATRIX_COLS: usize = 21;
pub const LEDS_PER_PACKET: usize = 14;
pub const CONFIG_LEN: usize = 43;

/// Known 6x21 LED matrix of the Spark/AULA WIN 68 HE MAX, used only as a
/// visual fallback when no keyboard is currently connected.
pub const FALLBACK_WIN68_MATRIX: [[u8; MATRIX_COLS]; MATRIX_ROWS] = [
    [0; MATRIX_COLS],
    [
        41, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 45, 46, 42, 73, 0, 0, 0, 0, 0, 0,
    ],
    [
        43, 20, 26, 8, 21, 23, 28, 24, 12, 18, 19, 47, 48, 49, 76, 0, 0, 0, 0, 0, 0,
    ],
    [
        57, 4, 22, 7, 9, 10, 11, 13, 14, 15, 51, 52, 0, 40, 75, 0, 0, 0, 0, 0, 0,
    ],
    [
        225, 0, 29, 27, 6, 25, 5, 17, 16, 54, 55, 56, 229, 82, 78, 0, 0, 0, 0, 0, 0,
    ],
    [
        224, 227, 226, 0, 0, 0, 44, 0, 0, 230, 1, 228, 80, 81, 79, 0, 0, 0, 0, 0, 0,
    ],
];

/// Compute the XsEngine packet checksum:
/// `chk = (0x5C + cmd + len + 0x35 + payload[len - 1]) & 0xFF`.
pub fn checksum(cmd: u8, payload: &[u8]) -> u8 {
    assert!(
        !payload.is_empty() && payload.len() <= 0xFC,
        "payload length must be 1..=0xFC, got {}",
        payload.len()
    );
    let len = payload.len() as u8;
    let last = *payload.last().unwrap();
    MAGIC
        .wrapping_add(cmd)
        .wrapping_add(len)
        .wrapping_add(0x35)
        .wrapping_add(last)
}

/// Build a framed packet: `[0x5C, len, cmd, chk, payload...]`.
pub fn build_packet(cmd: u8, payload: &[u8]) -> Vec<u8> {
    assert!(
        !payload.is_empty() && payload.len() <= 0xFC,
        "payload length must be 1..=0xFC, got {}",
        payload.len()
    );
    let chk = checksum(cmd, payload);
    let mut pkt = Vec::with_capacity(4 + payload.len());
    pkt.push(MAGIC);
    pkt.push(payload.len() as u8);
    pkt.push(cmd);
    pkt.push(chk);
    pkt.extend_from_slice(payload);
    pkt
}

/// Split a packet into zero-padded 64-byte HID output reports (no report ID).
pub fn chunk_reports(pkt: &[u8]) -> Vec<[u8; REPORT_SIZE]> {
    if pkt.is_empty() {
        return Vec::new();
    }
    pkt.chunks(REPORT_SIZE)
        .map(|slice| {
            let mut report = [0u8; REPORT_SIZE];
            report[..slice.len()].copy_from_slice(slice);
            report
        })
        .collect()
}

/// `0x01` device info request (`5c 06 01 97 01 02 03 04 ff ff`).
pub fn info_request() -> Vec<u8> {
    build_packet(CMD_INFO, &[0x01, 0x02, 0x03, 0x04, 0xFF, 0xFF])
}

/// `0x00 / 0x26` product name request (`5c 03 00 93 26 ff ff`).
pub fn product_name_request() -> Vec<u8> {
    build_packet(CMD_SUB, &[0x26, 0xFF, 0xFF])
}

/// `0x00 / 0x01` firmware version request (`5c 03 00 93 01 ff ff`).
pub fn firmware_request() -> Vec<u8> {
    build_packet(CMD_SUB, &[0x01, 0xFF, 0xFF])
}

/// `0x2B` LED matrix row pair request (`5c 03 2b c0 00 00 01` for `row_a = 0`).
pub fn matrix_request(row_a: u8) -> Vec<u8> {
    build_packet(CMD_MATRIX, &[0x00, row_a, row_a + 1])
}

/// `0x18` lighting config read request (43 zero bytes).
pub fn config_read_request() -> Vec<u8> {
    build_packet(CMD_CONFIG, &[0u8; CONFIG_LEN])
}

/// `0x18` lighting config write request (forces `payload[0] = 1`).
pub fn config_write_request(cfg: &[u8; CONFIG_LEN]) -> Vec<u8> {
    let mut payload = *cfg;
    payload[0] = 0x01;
    build_packet(CMD_CONFIG, &payload)
}

/// Check whether a 43-byte lighting config is already in host-driven mode.
pub fn is_host_mode(cfg: &[u8; CONFIG_LEN], effect: u8) -> bool {
    cfg[38] == HOST_MODE && cfg[39] == effect
}

/// Derive the host-driven lighting config from a previously read 43-byte config.
pub fn make_host_mode_config(old: &[u8; CONFIG_LEN], effect: u8) -> [u8; CONFIG_LEN] {
    let mut cfg = *old;
    cfg[0] = 0x01;
    cfg[37] = 0x01 | (old[37] & 0x12);
    cfg[38] = HOST_MODE;
    cfg[39] = effect;
    cfg[41] = 0x00;
    cfg
}

/// Pack `(led_index, [r, g, b])` tuples into `0x2A` packets (14 LEDs per 57-byte payload).
pub fn build_rgb_packets(colors: &[(u8, [u8; 3])]) -> Vec<Vec<u8>> {
    let mut packets = Vec::with_capacity(colors.len().div_ceil(LEDS_PER_PACKET));
    for chunk in colors.chunks(LEDS_PER_PACKET) {
        let mut body = [0u8; 1 + 4 * LEDS_PER_PACKET];
        body[0] = 0x01;
        for (i, &(idx, [r, g, b])) in chunk.iter().enumerate() {
            let base = 1 + 4 * i;
            body[base] = idx;
            body[base + 1] = r;
            body[base + 2] = g;
            body[base + 3] = b;
        }
        packets.push(build_packet(CMD_RGB, &body));
    }
    packets
}

/// Check whether a 64-byte HID reply matches the expected request command (`cmd | 0x80`).
pub fn is_matching_reply(reply: &[u8], request_cmd: u8) -> bool {
    reply.len() >= 4 && reply[0] == MAGIC && reply[2] == (request_cmd | 0x80)
}

#[derive(Debug, Clone, Default)]
pub struct DeviceInfoParsed {
    pub mode: String,
    pub app_string: String,
}

/// Parse the `0x81` reply from `0x01` device info.
pub fn parse_device_info(reply: &[u8]) -> Option<DeviceInfoParsed> {
    if !is_matching_reply(reply, CMD_INFO) || reply.len() < 8 {
        return None;
    }
    let payload = &reply[4..];
    let mode = if payload[0] == 0 {
        "App".to_string()
    } else {
        format!("Boot ({})", payload[0])
    };

    // Extract printable ASCII tokens (e.g. "App V1.1.3") from the reply payload.
    let mut runs: Vec<String> = Vec::new();
    let mut cur = String::new();
    for &b in payload {
        if (0x20..=0x7E).contains(&b) {
            cur.push(b as char);
        } else {
            if cur.len() >= 4 {
                runs.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= 4 {
        runs.push(cur);
    }

    let app_string = runs
        .iter()
        .find(|s| s.starts_with("App ") || s.starts_with("Boot "))
        .cloned()
        .or_else(|| runs.first().cloned())
        .unwrap_or_default();

    Some(DeviceInfoParsed { mode, app_string })
}

/// Parse the `0x80` reply from `0x00 / 0x26` (product name, e.g. `"WIN 68 HE MAX"`).
pub fn parse_product_name(reply: &[u8]) -> Option<String> {
    if !is_matching_reply(reply, CMD_SUB) || reply.len() < 7 || reply[5] != 0x26 {
        return None;
    }
    let end = (6 + 31).min(reply.len());
    let slice = &reply[6..end];
    let nul = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    let s = String::from_utf8_lossy(&slice[..nul]).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Parse the `0x80` reply from `0x00 / 0x01` (firmware version nibbles).
pub fn parse_firmware_version(reply: &[u8]) -> Option<String> {
    if !is_matching_reply(reply, CMD_SUB) || reply.len() < 8 || reply[5] != 0x01 {
        return None;
    }
    let b6 = reply[6];
    let b7 = reply[7];
    Some(format!("{}.{}.{}", b6 >> 4, b6 & 0x0F, b7 & 0x0F))
}

/// Parse one `0xAB` reply into two 21-column rows of LED indices.
pub fn parse_matrix_pair(reply: &[u8]) -> Option<([u8; MATRIX_COLS], [u8; MATRIX_COLS])> {
    if !is_matching_reply(reply, CMD_MATRIX) || reply.len() < 4 + 45 {
        return None;
    }
    let p = &reply[4..];
    let mut row_a = [0u8; MATRIX_COLS];
    let mut row_b = [0u8; MATRIX_COLS];
    row_a.copy_from_slice(&p[2..2 + MATRIX_COLS]);
    row_b.copy_from_slice(&p[24..24 + MATRIX_COLS]);
    Some((row_a, row_b))
}

/// Parse the 43-byte lighting config from a `0x98` reply.
pub fn parse_config_reply(reply: &[u8]) -> Option<[u8; CONFIG_LEN]> {
    if !is_matching_reply(reply, CMD_CONFIG) || reply.len() < 4 + CONFIG_LEN {
        return None;
    }
    let mut cfg = [0u8; CONFIG_LEN];
    cfg.copy_from_slice(&reply[4..4 + CONFIG_LEN]);
    Some(cfg)
}

/// Return rows that contain at least one key (LED index != 0).
pub fn active_rows(matrix: &[[u8; MATRIX_COLS]; MATRIX_ROWS]) -> Vec<usize> {
    (0..MATRIX_ROWS)
        .filter(|&r| matrix[r].iter().any(|&c| c != 0))
        .collect()
}

/// Return columns that contain at least one key (LED index != 0).
pub fn active_cols(matrix: &[[u8; MATRIX_COLS]; MATRIX_ROWS]) -> Vec<usize> {
    (0..MATRIX_COLS)
        .filter(|&c| (0..MATRIX_ROWS).any(|r| matrix[r][c] != 0))
        .collect()
}

/// Return sorted unique non-zero LED indices in the matrix.
pub fn unique_leds(matrix: &[[u8; MATRIX_COLS]; MATRIX_ROWS]) -> Vec<u8> {
    let mut leds: Vec<u8> = matrix
        .iter()
        .flat_map(|row| row.iter().copied())
        .filter(|&c| c != 0)
        .collect();
    leds.sort_unstable();
    leds.dedup();
    leds
}

/// Map a USB HID Keyboard/Keypad usage code (plus vendor `0x01` = Fn) to a short keycap label.
pub fn hid_key_label(usage: u8) -> &'static str {
    match usage {
        0x01 => "Fn",
        0x04 => "A",
        0x05 => "B",
        0x06 => "C",
        0x07 => "D",
        0x08 => "E",
        0x09 => "F",
        0x0A => "G",
        0x0B => "H",
        0x0C => "I",
        0x0D => "J",
        0x0E => "K",
        0x0F => "L",
        0x10 => "M",
        0x11 => "N",
        0x12 => "O",
        0x13 => "P",
        0x14 => "Q",
        0x15 => "R",
        0x16 => "S",
        0x17 => "T",
        0x18 => "U",
        0x19 => "V",
        0x1A => "W",
        0x1B => "X",
        0x1C => "Y",
        0x1D => "Z",
        0x1E => "1",
        0x1F => "2",
        0x20 => "3",
        0x21 => "4",
        0x22 => "5",
        0x23 => "6",
        0x24 => "7",
        0x25 => "8",
        0x26 => "9",
        0x27 => "0",
        0x28 => "Enter",
        0x29 => "Esc",
        0x2A => "Bksp",
        0x2B => "Tab",
        0x2C => "Space",
        0x2D => "-",
        0x2E => "=",
        0x2F => "[",
        0x30 => "]",
        0x31 => "\\",
        0x32 => "#",
        0x33 => ";",
        0x34 => "'",
        0x35 => "`",
        0x36 => ",",
        0x37 => ".",
        0x38 => "/",
        0x39 => "Caps",
        0x3A => "F1",
        0x3B => "F2",
        0x3C => "F3",
        0x3D => "F4",
        0x3E => "F5",
        0x3F => "F6",
        0x40 => "F7",
        0x41 => "F8",
        0x42 => "F9",
        0x43 => "F10",
        0x44 => "F11",
        0x45 => "F12",
        0x46 => "PrtSc",
        0x47 => "ScrLk",
        0x48 => "Pause",
        0x49 => "Ins",
        0x4A => "Home",
        0x4B => "PgUp",
        0x4C => "Del",
        0x4D => "End",
        0x4E => "PgDn",
        0x4F => "→",
        0x50 => "←",
        0x51 => "↓",
        0x52 => "↑",
        0xE0 => "Ctrl",
        0xE1 => "Shift",
        0xE2 => "Alt",
        0xE3 => "Super",
        0xE4 => "RCtrl",
        0xE5 => "RShft",
        0xE6 => "RAlt",
        0xE7 => "RSup",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_0x01_info_request_matches_expected_bytes() {
        let pkt = info_request();
        assert_eq!(
            pkt,
            vec![0x5c, 0x06, 0x01, 0x97, 0x01, 0x02, 0x03, 0x04, 0xff, 0xff]
        );
    }

    #[test]
    fn packet_0x00_26_product_name_matches_expected_bytes() {
        let pkt = product_name_request();
        assert_eq!(pkt, vec![0x5c, 0x03, 0x00, 0x93, 0x26, 0xff, 0xff]);
    }

    #[test]
    fn packet_0x2b_rows_0_1_matches_expected_bytes() {
        let pkt = matrix_request(0);
        assert_eq!(pkt, vec![0x5c, 0x03, 0x2b, 0xc0, 0x00, 0x00, 0x01]);
    }

    #[test]
    fn host_mode_config_preserves_flags_bits_and_sets_mode_effect() {
        let mut orig = [0u8; CONFIG_LEN];
        orig[37] = 0xC1; // bit0=1, bit1=0, bit4=0, upper bits cleared by mask
        orig[38] = 0x04;
        orig[39] = 0x03;
        orig[40] = 0x01;
        orig[41] = 0x02;
        orig[42] = 0x00;

        assert!(!is_host_mode(&orig, HOST_EFFECT_KEYBOARD));

        let host = make_host_mode_config(&orig, HOST_EFFECT_KEYBOARD);
        assert_eq!(host[0], 0x01);
        assert_eq!(host[37], 0x01);
        assert_eq!(host[38], 0x04);
        assert_eq!(host[39], 0x15);
        assert_eq!(host[40], 0x01);
        assert_eq!(host[41], 0x00);
        assert!(is_host_mode(&host, HOST_EFFECT_KEYBOARD));

        // Verify bits 1 and 4 (0x12) are preserved when set.
        orig[37] = 0x12;
        let host2 = make_host_mode_config(&orig, HOST_EFFECT_KEYBOARD);
        assert_eq!(host2[37], 0x13);
    }

    #[test]
    fn rgb_packets_chunk_into_14_leds_and_pad_to_57_bytes() {
        let leds: Vec<(u8, [u8; 3])> = (1u8..=16).map(|i| (i, [0xFF, 0x80, 0x10])).collect();
        let pkts = build_rgb_packets(&leds);
        assert_eq!(pkts.len(), 2);
        assert_eq!(pkts[0].len(), 4 + 57);
        assert_eq!(pkts[1].len(), 4 + 57);
        assert_eq!(pkts[0][0], MAGIC);
        assert_eq!(pkts[0][1], 57);
        assert_eq!(pkts[0][2], CMD_RGB);
        assert_eq!(pkts[0][4], 0x01);
        // First LED entry in packet 0
        assert_eq!(&pkts[0][5..9], &[1, 0xFF, 0x80, 0x10]);
        // Second packet has 2 LEDs, remaining 12 slots zero-padded
        assert_eq!(&pkts[1][5..13], &[15, 0xFF, 0x80, 0x10, 16, 0xFF, 0x80, 0x10]);
        assert!(pkts[1][13..].iter().all(|&b| b == 0));
    }

    #[test]
    fn parse_real_hardware_replies() {
        let info_reply: [u8; 64] = [
            0x5c, 0x3c, 0x81, 0x4d, 0x00, 0x01, 0x19, 0x02, 0x06, 0xc0, 0x01, 0x00, 0x10, 0x38,
            0x31, 0x35, 0x34, 0x31, 0x34, 0x35, 0x32, 0x30, 0x39, 0x30, 0x39, 0x31, 0x34, 0x38,
            0x36, 0x10, 0x41, 0x70, 0x70, 0x20, 0x56, 0x31, 0x2e, 0x31, 0x2e, 0x33, 0x00, 0x00,
            0x31, 0x36, 0x3a, 0x30, 0x10, 0x4e, 0x6f, 0x76, 0x20, 0x31, 0x34, 0x20, 0x35, 0x30,
            0x32, 0x35, 0x38, 0x04, 0x01, 0x03, 0x4b, 0xff,
        ];
        let parsed = parse_device_info(&info_reply).unwrap();
        assert_eq!(parsed.mode, "App");
        assert_eq!(parsed.app_string, "App V1.1.3");

        let mut fw_reply = [0u8; 64];
        fw_reply[..8].copy_from_slice(&[0x5c, 0x04, 0x80, 0x00, 0x00, 0x01, 0x09, 0x01]);
        assert_eq!(parse_firmware_version(&fw_reply).as_deref(), Some("0.9.1"));

        let mut name_reply = [0u8; 64];
        name_reply[..6].copy_from_slice(&[0x5c, 0x20, 0x80, 0x00, 0x00, 0x26]);
        let name = b"WIN 68 HE MAX";
        name_reply[6..6 + name.len()].copy_from_slice(name);
        assert_eq!(
            parse_product_name(&name_reply).as_deref(),
            Some("WIN 68 HE MAX")
        );
    }

    #[test]
    fn fallback_matrix_has_68_unique_leds_5_rows_15_cols() {
        assert_eq!(active_rows(&FALLBACK_WIN68_MATRIX).len(), 5);
        assert_eq!(active_cols(&FALLBACK_WIN68_MATRIX).len(), 15);
        assert_eq!(unique_leds(&FALLBACK_WIN68_MATRIX).len(), 68);
    }
}

//! Linux hidraw discovery, raw 64-byte HID report I/O, startup probe, and
//! lighting config backup/restore (shared with `tools/xskb.py`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use crate::protocol::{
    CMD_CONFIG, CMD_INFO, CMD_MATRIX, CMD_SUB, CONFIG_LEN, HOST_EFFECT_KEYBOARD, MATRIX_COLS,
    MATRIX_ROWS, PID, REPORT_SIZE, VENDOR_USAGE_PAGE, VID, build_packet, build_rgb_packets,
    chunk_reports, config_read_request, config_write_request, firmware_request, info_request,
    is_host_mode, is_matching_reply, make_host_mode_config, matrix_request, parse_config_reply,
    parse_device_info, parse_firmware_version, parse_matrix_pair, parse_product_name,
    product_name_request,
};

#[derive(Debug, Clone)]
pub struct DeviceProbe {
    pub hidraw_path: PathBuf,
    pub product_name: String,
    pub firmware_version: String,
    pub app_info: String,
    pub mode_name: String,
    pub matrix: [[u8; MATRIX_COLS]; MATRIX_ROWS],
    pub config: [u8; CONFIG_LEN],
}

/// Path to the stock lighting backup shared with `tools/xskb.py`.
pub fn backup_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("xskb")
        .join("lighting-backup.bin"))
}

/// Discover the vendor hidraw device (`/dev/hidrawN`) matching VID `1ca2`, PID `1901`,
/// and HID Usage Page `0xFFA0` (`06 A0 FF` in the report descriptor).
pub fn find_hidraw() -> Result<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir("/sys/class/hidraw")
        .context("failed to read /sys/class/hidraw")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();

    let needle_id = format!("{VID:08X}:{PID:08X}");
    let usage_page_bytes = [
        0x06u8,
        (VENDOR_USAGE_PAGE & 0xFF) as u8,
        (VENDOR_USAGE_PAGE >> 8) as u8,
    ];

    for sys_dev in entries {
        let uevent_path = sys_dev.join("device/uevent");
        let Ok(uevent) = fs::read_to_string(&uevent_path) else {
            continue;
        };
        if !uevent.to_ascii_uppercase().contains(&needle_id) {
            continue;
        }
        let desc_path = sys_dev.join("device/report_descriptor");
        let Ok(desc) = fs::read(&desc_path) else {
            continue;
        };
        if desc.windows(3).any(|w| w == usage_page_bytes) {
            if let Some(name) = sys_dev.file_name() {
                return Ok(PathBuf::from("/dev").join(name));
            }
        }
    }

    bail!("vendor hidraw interface (VID {VID:04x}:PID {PID:04x}, usage page 0x{VENDOR_USAGE_PAGE:04X}) not found")
}

/// Other processes that have `path` open, as "name (pid)" list, from /proc/*/fd.
pub fn other_holders(path: &Path) -> String {
    let me = std::process::id().to_string();
    let mut found = Vec::new();
    if let Ok(procs) = fs::read_dir("/proc") {
        for p in procs.flatten() {
            let pid = p.file_name().to_string_lossy().to_string();
            if pid == me || !pid.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let Ok(fds) = fs::read_dir(p.path().join("fd")) else { continue };
            if fds.flatten().any(|fd| fs::read_link(fd.path()).is_ok_and(|l| l == path)) {
                let comm = fs::read_to_string(p.path().join("comm")).unwrap_or_default();
                found.push(format!("{} ({pid})", comm.trim()));
            }
        }
    }
    if found.is_empty() { "none found".into() } else { found.join(", ") }
}

pub struct HidDevice {
    path: PathBuf,
    file: File,
}

impl HidDevice {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        // Only one xskb writer at a time: a second one (e.g. a forgotten `xskb.py music`)
        // would overwrite every frame we send. Advisory lock shared with tools/xskb.py.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!(
                "keyboard is busy: another program is driving {} ({})",
                path.display(),
                other_holders(&path)
            );
        }
        let others = other_holders(&path);
        if others != "none found" {
            eprintln!("warning: {} is also open in: {others}", path.display());
        }
        let mut dev = Self { path, file };
        dev.drain();
        Ok(dev)
    }

    fn poll_fd(&self, events: i16, timeout_ms: i32) -> io::Result<bool> {
        let mut pfd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(err);
        }
        Ok(rc > 0 && (pfd.revents & events) != 0)
    }

    /// Drain any stale unread input reports without blocking.
    pub fn drain(&mut self) {
        let mut scratch = [0u8; 2048];
        while matches!(self.poll_fd(libc::POLLIN, 0), Ok(true)) {
            match self.file.read(&mut scratch) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    /// Write a framed packet in 64-byte zero-padded raw reports (no report ID).
    pub fn write_packet(&mut self, pkt: &[u8]) -> Result<()> {
        for report in chunk_reports(pkt) {
            let mut written = 0;
            let deadline = Instant::now() + Duration::from_millis(250);
            while written < REPORT_SIZE {
                match self.file.write(&report[written..]) {
                    Ok(0) => bail!("hidraw write returned 0 bytes"),
                    Ok(n) => written += n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        let rem = deadline.saturating_duration_since(Instant::now());
                        if rem.is_zero() {
                            bail!("hidraw write timed out");
                        }
                        let _ = self.poll_fd(libc::POLLOUT, rem.as_millis().min(50) as i32);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        return Err(e).with_context(|| {
                            format!("hidraw write failed on {}", self.path.display())
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Drain stale input, send `pkt` (with command `cmd`), and wait up to `timeout` for a matching reply.
    pub fn transact_raw(
        &mut self,
        cmd: u8,
        pkt: &[u8],
        timeout: Duration,
    ) -> Result<[u8; REPORT_SIZE]> {
        self.drain();
        self.write_packet(pkt)?;
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; REPORT_SIZE];
        while Instant::now() < deadline {
            let rem = deadline.saturating_duration_since(Instant::now());
            let wait_ms = rem.as_millis().clamp(1, i32::MAX as u128) as i32;
            if self.poll_fd(libc::POLLIN, wait_ms)? {
                match self.file.read(&mut buf) {
                    Ok(n) if n >= 4 && is_matching_reply(&buf[..n], cmd) => {
                        return Ok(buf);
                    }
                    Ok(_) => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e).context("hidraw read failed"),
                }
            }
        }
        bail!("timed out waiting for reply to command 0x{cmd:02X}")
    }

    /// Read the 6x21 LED matrix (three `0x2B` transactions).
    pub fn read_matrix(&mut self) -> Result<[[u8; MATRIX_COLS]; MATRIX_ROWS]> {
        let mut matrix = [[0u8; MATRIX_COLS]; MATRIX_ROWS];
        for row_a in [0u8, 2, 4] {
            let req = matrix_request(row_a);
            let reply = self.transact_raw(CMD_MATRIX, &req, Duration::from_millis(500))?;
            let (a, b) = parse_matrix_pair(&reply)
                .ok_or_else(|| anyhow!("invalid 0x2B reply for rows {},{}", row_a, row_a + 1))?;
            matrix[row_a as usize] = a;
            matrix[(row_a + 1) as usize] = b;
        }
        Ok(matrix)
    }

    /// Read the 43-byte lighting config (`0x18` with 43 zero bytes).
    pub fn read_config(&mut self) -> Result<[u8; CONFIG_LEN]> {
        let req = config_read_request();
        let reply = self.transact_raw(CMD_CONFIG, &req, Duration::from_millis(2000))?;
        parse_config_reply(&reply).ok_or_else(|| anyhow!("invalid 0x18 config reply"))
    }

    /// Write a 43-byte lighting config (`0x18` with `payload[0] = 1`).
    pub fn write_config(&mut self, cfg: &[u8; CONFIG_LEN]) -> Result<()> {
        let req = config_write_request(cfg);
        let _ = self.transact_raw(CMD_CONFIG, &req, Duration::from_millis(1500))?;
        Ok(())
    }

    /// Read-only startup probe (`0x01`, `0x00/26`, `0x00/01`, `0x2B` x3, `0x18` read).
    pub fn startup_probe(&mut self) -> Result<DeviceProbe> {
        let info_reply = self.transact_raw(CMD_INFO, &info_request(), Duration::from_millis(500))?;
        let info = parse_device_info(&info_reply)
            .ok_or_else(|| anyhow!("invalid 0x01 device info reply"))?;

        let product_name = self
            .transact_raw(CMD_SUB, &product_name_request(), Duration::from_millis(500))
            .ok()
            .and_then(|r| parse_product_name(&r))
            .unwrap_or_else(|| "Unknown".to_string());

        let firmware_version = self
            .transact_raw(CMD_SUB, &firmware_request(), Duration::from_millis(500))
            .ok()
            .and_then(|r| parse_firmware_version(&r))
            .unwrap_or_else(|| "Unknown".to_string());

        let matrix = self.read_matrix()?;
        let config = self.read_config()?;

        Ok(DeviceProbe {
            hidraw_path: self.path.clone(),
            product_name,
            firmware_version,
            app_info: info.app_string,
            mode_name: info.mode,
            matrix,
            config,
        })
    }

    /// Ensure `~/.config/xskb/lighting-backup.bin` exists without ever overwriting it.
    pub fn ensure_backup(&mut self) -> Result<PathBuf> {
        let path = backup_path()?;
        if !path.exists() {
            let cfg = self.read_config()?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::write(&path, cfg)
                .with_context(|| format!("failed to write backup {}", path.display()))?;
        }
        Ok(path)
    }

    /// Switch the keyboard into host-driven RGB mode (`p8=0x04, p9=0x15`), backing up first.
    /// Skips the write and 500 ms wait if the device is already in host-driven mode.
    pub fn enter_host_mode(&mut self) -> Result<bool> {
        self.ensure_backup()?;
        let cfg = self.read_config()?;
        if is_host_mode(&cfg, HOST_EFFECT_KEYBOARD) {
            return Ok(false);
        }
        let host_cfg = make_host_mode_config(&cfg, HOST_EFFECT_KEYBOARD);
        self.write_config(&host_cfg)?;
        // Firmware drops frames while applying the new mode.
        thread::sleep(Duration::from_millis(500));
        Ok(true)
    }

    /// Restore the stock lighting config saved in `~/.config/xskb/lighting-backup.bin`.
    pub fn restore_stock(&mut self) -> Result<()> {
        let path = backup_path()?;
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read lighting backup at {}", path.display()))?;
        if bytes.len() < CONFIG_LEN {
            bail!(
                "lighting backup at {} is too short ({} bytes, expected {CONFIG_LEN})",
                path.display(),
                bytes.len()
            );
        }
        let mut cfg = [0u8; CONFIG_LEN];
        cfg.copy_from_slice(&bytes[..CONFIG_LEN]);
        self.write_config(&cfg)?;
        Ok(())
    }

    /// Send one full per-key RGB frame (`0x2A` packets, write-only).
    pub fn send_frame(&mut self, colors: &[(u8, [u8; 3])], gap: Duration) -> Result<()> {
        let pkts = build_rgb_packets(colors);
        for pkt in pkts {
            self.write_packet(&pkt)?;
            if !gap.is_zero() {
                thread::sleep(gap);
            }
        }
        Ok(())
    }

    /// Send a raw command/payload packet (convenience helper).
    #[allow(dead_code)]
    pub fn transact(&mut self, cmd: u8, payload: &[u8], timeout: Duration) -> Result<[u8; REPORT_SIZE]> {
        let pkt = build_packet(cmd, payload);
        self.transact_raw(cmd, &pkt, timeout)
    }
}

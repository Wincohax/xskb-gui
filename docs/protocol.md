# XsEngine keyboard HID protocol (recovered)

Source: Ghidra 12.2 decompilation of `XsEngineSetup/xseg.dll`
(sha256 `80ffb84f…0c3b`) via REA `decompile-all` (Evidence `ev_4be897ea…cc21`),
cross-checked against raw x86-64 disassembly for the packet header/checksum.
**Static analysis only. Nothing here has been checked against real hardware yet.**
Function addresses are given so each claim can be re-checked.

Legend: **[obs]** = read directly from code, **[inf]** = inference, **[?]** = unknown.

## 1. Transport (`HIDCom`)

- Open: `hid_open_path(path)` then `hid_set_nonblocking(dev, 1)`. `0x18008b940` [obs]
- Device list / matching comes from the cloud list (`vid`, `pid`, `usage_page`, `usage`, `protocol`);
  pick the hidraw interface with the matching usage page. [obs strings; exact values **[?]**]
- **Write** `0x18008ba80` [obs]:
  - Total packet size = `pkt[1] + 4`.
  - The packet is sent in 64-byte slices. Each `hid_write` is **65 bytes**: `0x00` (report ID) + 64 bytes of packet.
    The last slice is zero-padded (the buffer is static).
- **Transact** `0x18008b9e0` [obs]: drain input (`hid_read_timeout(dev, buf, 0x800, 0)`), write the packet,
  then `hid_read_timeout(dev, reply, 65, timeout_ms)`.
  - The reply uses the same packet layout, starting at `reply[0]`. hidapi strips the report ID on Linux hidraw.
    Confirm this on hardware.
- Before init, the device thread drains stale input once. `0x18008b8c0` [obs]

## 2. Packet format

```
off  size  field
0    1     magic   = 0x5C
1    1     len     = payload length N (max 0xFC)
2    1     cmd
3    1     chk
4    N     payload
```

Checksum, exactly as the binary computes it (`0x18008bb90` asm) [obs]:

```
chk = (0x5C + cmd + N + 0x35 + payload[N-1]) & 0xFF      # payload[N-1] only added when 1 <= N <= 0xFC
```

This adds only the **last** payload byte, not a sum over all bytes. That looks odd, but it is the same
in every builder (`0x18008bb40`, `…bb90`, `…bc20`, `…c020`, `…c5d0`). Verify it against a USB capture.

The reply's `cmd` byte is `request_cmd | 0x80` [obs]. Every reader checks it:
0x00→0x80, 0x01→0x81, 0x18→0x98, 0x2B→0xAB.

## 3. Commands

| cmd | payload (request) | reply check / content | where |
|---|---|---|---|
| `0x01` | `01 02 03 04 FF FF` | `0x81`. `u32` at payload+1. Mode byte → `0` = "App mode" (`App模式`), else "Boot mode" (`Boot模式`). A 16-byte string field and a 10-byte string field (name / model **[inf]**). | `c5d0` / `ebf0` |
| `0x00` | `26 FF FF` | `0x80` and `payload[1]==0x26`. Up to 31 bytes of serial number, logged as `SN:%s`. | `bb40` / `e880` |
| `0x00` | `01 FF FF` | `0x80` and `payload[1]==0x01`. Firmware version from `payload[2..3]` nibbles (`%d.%d.%d`). | `bb40` / `eb00` |
| `0x2B` | `00 <rowA> <rowB>` | `0xAB`. Reply payload `[2..22]` = 21 cells of row A, `[24..44]` = 21 cells of row B. Each cell is an LED index. Sent for rows (0,1), (2,3), (4,5): a **6×21 LED matrix**. | `bb90` / `e0c0` |
| `0x18` | see §4 | `0x98` when reading the lighting config | `c020` / `e510`, `ee10` |
| `0x2A` | see §5 | none (write-only) | `bc20` / `f1a0` |

## 4. `0x18` lighting config (43-byte payload) [obs layout, field meanings inf]

```
[0]      rw        0 = read, 1 = write
[1..4]   00 00 00 00
[5..32]  7 colour slots × 4 bytes: c0 c1 c2 FF
[33..36] 00 00 00 00
[37]     flags     bit0 = (rw==1 → 1), bit1, bit4 (preserved from the read)
[38]     p8        light mode
[39]     p9        effect / sub-mode
[40]     p10       preserved
[41]     p11       written as 0
[42]     p12       preserved
```

- Init reads the config with `rw=0`, all zero, timeout 2000 ms. It keeps the 7 colours and bytes 37..42. `e510`
- To put the keyboard under host control, it writes `rw=1` with flags bit0 set and **`p8 = 0x04`**.
  `p9` is **`0x15` (21)** for `Keyboard` and **`0x0B` (11)** for `ZLXQ_Keyboard`. Other fields are written back unchanged.
  Timeout 1500 ms. (`ee10` / `b1ff0`; reached via vtable slot 0 after init [inf].)
- Colour byte order **[?]**: the reader packs `payload[5],[6],[7]` as `b5<<16|b6<<8|b7`. The writer emits the
  low byte first. Either the device uses BGR on write, or the app swaps the order on each round-trip. Check on hardware.

## 5. `0x2A` per-key RGB frame [obs]

```
payload[0]        = 0x01
payload[1 + 4*i]  = LED index   (from the 6×21 matrix)
payload[2 + 4*i]  = R
payload[3 + 4*i]  = G
payload[4 + 4*i]  = B           for i in 0..13  (14 LEDs per packet)
```

- `N = 57`, so the packet is 61 bytes and fits in one 65-byte HID write.
- The frame sender walks the 6×21 matrix and skips empty cells. It sends full batches of 14, then one partial
  batch padded with zeros. No reply is read. If a write fails, the engine logs `set rgb fail,kill self`.
  (`d0c0` / `d8e0` → `f1a0`)
- Which slot holds R, G and B is inferred from the order the four arrays are filled (idx, +4, +8, +0xC of a
  16-byte `{idx,r,g,b}` record). [inf]

## 6. Init sequence (`0x18008d220`) [obs]

1. Open the hidraw path and set it non-blocking.
2. Allocate a 6×21 matrix. Drain pending input.
3. `0x01` device info. Abort on failure. Log App/Boot mode.
4. `0x00/0x26` serial number.
5. `0x00/0x01` firmware version.
6. `0x2B` ×3: read the LED matrix.
7. `0x18` read the lighting config.
8. Subclass hook: `0x18` write to switch to host-driven mode (`p8=4`, `p9=21|11`).
9. Stream `0x2A` frames.

## Hardware check: Spark/AULA WIN 68 HE MAX (`1ca2:1901`), 2026-11-25
- Vendor interface is `hidraw2` (interface 2): usage page `0xFFA0`, 64-byte input and output reports, **no report ID**.
  Write the 64 raw bytes directly to hidraw.
- Every read-only command got a valid reply. This **confirms the header and checksum formula** and the `cmd|0x80` reply.
  - `0x01` → `App V1.1.3`, build date `Nov 14 …`, a 16-character numeric string.
  - `0x00/26` → `"WIN 68 HE MAX"`. This is the product name, not a serial number.
  - `0x00/01` → `09 01`.
  - `0x2B` → the matrix only uses 15 of the 21 columns, and row 0 is empty.
    **LED indices are HID keyboard usage codes** (Esc=0x29, A=0x04, LCtrl=0xE0 …). The `0x01` cell is the Fn key.
  - `0x18` read → colour slots `0000ff 00ff00 007f7f ff0000 7f007f 7f7f00 7f7f7f`, then
    `[37]=0xC1 [38]=0x04 [39]=0x03 [40]=0x01 [41]=0x02 [42]=0x00`. The stock config is backed up to `~/.config/xskb/lighting-backup.bin`.
- `0x2A` frames alone, with no mode switch, did not light the keys as expected.
- With the mode switch (`0x18` write: `[38]=0x04`, `[39]=0x15`), the whole board went **red for `ff0000`**.
  This confirms the `(idx, R, G, B)` slot order and that WIN68 uses the `Keyboard` variant (`0x15`).
  The first frames after the switch were dropped (board black or partly lit). The tool now waits 500 ms after the switch
  and paces packets 2 ms apart.
- `restore` (writing back the saved `0x18` config) brings back the stock lighting. Confirmed.

## Open questions
- Exact VID/PID/usage page for your keyboard. Get them from `lsusb` and the report descriptor, or from the cloud list.
- Does the checksum formula hold on real traffic?
- Colour byte order in `0x18`. R/G/B slot order in `0x2A`.
- How to restore the keyboard's own lighting on exit. Probably a `0x18` write with the saved `p8`/`p9`. Not yet traced.

# xskb-gui

Linux desktop GUI for controlling the RGB lighting of XsEngine USB keyboards
(tested against the Spark/AULA WIN 68 HE MAX, USB `1ca2:1901`, vendor HID usage
page `0xFFA0`), built in Rust with standalone GPUI.

The reverse-engineered HID protocol it speaks is documented in
[docs/protocol.md](docs/protocol.md).

## GPUI source

- **Crate / Repository:** [`gpui-ce/gpui-ce`](https://github.com/gpui-ce/gpui-ce)
  via crates.io releases `gpui-ce = "0.3.0"` and `gpui_ce_platform = "0.3.0"`
  (published 2026-10-20 from `gpui-ce/gpui-ce`).
- **Why this repo:** Zed Industries' official standalone repository
  [`zed-industries/gpui`](https://github.com/zed-industries/gpui) currently
  contains only a placeholder `README.md` (`"See you in a few weeks."`, commit
  `8d31300b294f`, 2026-11-23) with no Rust code, and the `gpui` crate on
  crates.io (`0.2.2`, 2025-10-22) still points at the `zed-industries/zed`
  monorepo. `gpui-ce/gpui-ce` (GPUI Community Edition, 1.6k+ stars, actively
  maintained standalone workspace) is the credible maintained standalone GPUI
  repository available today.

## Features

- **Device status:** auto-discovers `/dev/hidrawN` via
  `/sys/class/hidraw/*/device/{uevent,report_descriptor}` (VID `1ca2`, PID
  `1901`, usage page `0xFFA0`), shows connection state, product name, firmware
  version, app info string, and hidraw path.
- **Live keyboard preview:** renders the 6x21 LED matrix returned by `0x2B` with
  HID usage keycap labels and live per-key RGB colours.
- **Solid colour mode:** colour presets, per-channel R/G/B controls, and a
  brightness control.
- **Music reactive mode:** captures PipeWire output via `pw-record` and runs a
  1024-point FFT (`rustfft`) into 15 log-spaced bands (40 Hz – 16 kHz) matching
  the `xskb.py` Python prototype's auto-gain, attack/decay, and noise floor. Supports
  **Rainbow** and **Meter** styles, configurable FPS (default 40), and a
  sensitivity control.
- **Safe stock lighting restore:** shares `~/.config/xskb/lighting-backup.bin`
  with the `xskb.py` Python prototype (never overwrites an existing backup), provides an
  **Off / Restore Stock** button, and restores stock lighting automatically on
  window close, `Ctrl+Q`, `SIGINT`, and `SIGTERM`.
- **Non-blocking architecture:** all HID I/O and PipeWire audio processing run
  on a dedicated background thread (`xskb-worker`).

## Building and running

### Linux dependencies

On Debian/Ubuntu:
```sh
sudo apt-get install -y build-essential pkg-config \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libfontconfig-dev
```

On Bazzite / immutable Fedora (no `rpm-ostree` or `sudo` layering needed):
- Install Rust in your home directory via [rustup](https://rustup.rs).
- Install the missing dev library via Homebrew (`brew install libxkbcommon`;
  `fontconfig`, `libxcb`, and `libx11` are also available from Homebrew) **or**
  build inside a Fedora `distrobox` / `toolbox` with `libxkbcommon-devel`,
  `libxkbcommon-x11-devel`, `wayland-devel`, `libxcb-devel`, and
  `fontconfig-devel`.

Build with Homebrew libraries on Bazzite:
```sh
cd gui
PKG_CONFIG_PATH="/home/linuxbrew/.linuxbrew/lib/pkgconfig:/home/linuxbrew/.linuxbrew/share/pkgconfig" \
LIBRARY_PATH="/home/linuxbrew/.linuxbrew/lib" \
cargo build --release
```

Run the GUI:
```sh
./target/release/xskb-gui
```

Run a read-only hardware probe (does not change keyboard lighting):
```sh
./target/release/xskb-gui --probe
```

Run unit tests (packet/checksum builder + FFT band mapping):
```sh
PKG_CONFIG_PATH="/home/linuxbrew/.linuxbrew/lib/pkgconfig:/home/linuxbrew/.linuxbrew/share/pkgconfig" \
LIBRARY_PATH="/home/linuxbrew/.linuxbrew/lib" \
cargo test
```

<p align="center">
  <img src="assets/icon.svg" width="96" height="96" alt="XSKB icon">
</p>

<h1 align="center">XSKB Keyboard Lighting</h1>

<p align="center">
  Make your keyboard dance to your music on Linux.<br>
  RGB lighting control for the <b>Spark / AULA WIN 68 HE MAX</b> and other XsEngine keyboards.
</p>

---

## What you can do

- **Music mode** — the keys light up in time with whatever is playing on your computer: songs, videos,
  games. Choose a **Rainbow** spectrum or a **Meter** style, and adjust the sensitivity to taste.
- **Solid colours** — one click on a preset (Orange, Cyan, Purple, Warm white…) or mix your own, and set
  the brightness.
- **Live preview** — see what's on your keyboard right inside the app.
- **Keep it running in the background** — close the window and XSKB stays in your system tray, so the
  music lighting keeps going. Right-click the tray icon to switch music mode on or off, or to quit.
- **Your stock lighting is safe** — the first time it runs, XSKB saves a copy of your keyboard's original
  lighting. Quitting the app (or pressing **Off / Restore Stock**) puts it back.

## Getting started

### 1. Let your user talk to the keyboard (one time)

Linux only lets programs control USB devices that have been allowed. Paste this into a terminal once, then
unplug and replug the keyboard:

```bash
echo 'SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1ca2", ATTRS{idProduct}=="1901", TAG+="uaccess"' \
  | sudo tee /etc/udev/rules.d/70-xskb.rules
sudo udevadm control --reload && sudo udevadm trigger
```

(The same rule is in [`udev/70-xskb.rules`](udev/70-xskb.rules) if you'd rather copy the file.)

### 2. Run the app

Download `XSKB-x86_64.AppImage` from the [Releases page](../../releases/latest), make it executable
and open it:

```bash
chmod +x XSKB-x86_64.AppImage
./XSKB-x86_64.AppImage
```

You can also double-click it in your file manager. Want it to start hidden in the tray when you log in?
Add the AppImage to your desktop's autostart list with the `--background` option.

Music mode listens to your computer's sound through PipeWire, which almost every current Linux desktop
already uses. Nothing is recorded or saved.

## Good to know

- **One program at a time.** If something else is already controlling the lighting, XSKB will tell you
  which program it is so you can close it.
- **No tray?** On desktops without a system tray (for example GNOME without the AppIndicator extension),
  closing the window quits the app and restores your stock lighting.
- Your original lighting backup is kept at `~/.config/xskb/lighting-backup.bin`. XSKB never overwrites it.

## Troubleshooting

| Problem | Try this |
|---|---|
| The app says **Disconnected** | Make sure you did step 1, replug the keyboard, then press **Reconnect**. |
| "keyboard is busy: another program is driving…" | Close the program named in the message, then press **Reconnect**. |
| Music mode doesn't react | Check that sound is actually playing and that PipeWire is running. Try raising the sensitivity. |
| "failed to spawn pw-record" | Install your distribution's PipeWire tools package (it provides `pw-record`). |
| The tray icon doesn't show up | Your desktop may not have a system tray. On GNOME, install the *AppIndicator and KStatusNotifierItem Support* extension. |

To check that the keyboard is detected without changing its lighting, run the app with `--probe`.

## Building from source

You need [Rust](https://rustup.rs) and a few development libraries for the window toolkit
([GPUI-CE](https://github.com/gpui-ce/gpui-ce)).

**Debian / Ubuntu**

```bash
sudo apt-get install -y build-essential pkg-config \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libfontconfig-dev
```

**Fedora**

```bash
sudo dnf install gcc pkgconf-pkg-config libxkbcommon-devel libxkbcommon-x11-devel \
  wayland-devel libxcb-devel fontconfig-devel
```

On immutable systems such as Bazzite or Silverblue, run that inside a Fedora
[distrobox](https://distrobox.it) and build there; the finished program runs on the host.

Then:

```bash
cargo build --release
./target/release/xskb-gui
```

`cargo test` runs the unit tests (packet building and audio band mapping) without a keyboard.

### Building the AppImage

To package the app as a single-file AppImage that runs on most Linux distributions, run the build script where the build dependencies above are installed (inside the distrobox on Bazzite):

```bash
packaging/build-appimage.sh      # → target/appimage/XSKB-x86_64.AppImage
```

The first run downloads linuxdeploy and appimagetool into `~/.cache/appimage-tools`. The AppImage uses the PipeWire tools already on your system for the music mode, so it doesn't bundle them.

## About

This is an independent project, not affiliated with Spark, AULA or XsEngine. The keyboard's lighting
protocol is documented in [`docs/protocol.md`](docs/protocol.md). Licensed under the GPL-3.0.

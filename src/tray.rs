//! System tray integration via StatusNotifierItem (`ksni`) with embedded
//! ARGB32 icon pixmaps so the icon renders even outside an installed theme
//! (e.g. AppImage or local build).

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result, bail};
use ksni::blocking::TrayMethods;

use crate::audio::MusicStyle;
use crate::worker::{ActiveMode, WorkerCmd};

pub const APP_ID: &str = "io.github.wincohax.Xskb";
pub const APP_TITLE: &str = "XSKB Keyboard Lighting";

const TRAY_PNG_22: &[u8] = include_bytes!("../assets/tray-22.png");
const TRAY_PNG_32: &[u8] = include_bytes!("../packaging/icons/32x32/io.github.wincohax.Xskb.png");
const TRAY_PNG_48: &[u8] = include_bytes!("../packaging/icons/48x48/io.github.wincohax.Xskb.png");
const TRAY_PNG_64: &[u8] = include_bytes!("../packaging/icons/64x64/io.github.wincohax.Xskb.png");

static EMBEDDED_ICONS: LazyLock<Vec<ksni::Icon>> = LazyLock::new(|| {
    [TRAY_PNG_22, TRAY_PNG_32, TRAY_PNG_48, TRAY_PNG_64]
        .into_iter()
        .map(|bytes| {
            decode_png_to_argb_icon(bytes).expect("embedded tray PNG must decode to ARGB32")
        })
        .collect()
});

/// Decode a PNG image into a `ksni::Icon` (ARGB32 big-endian / network byte order).
pub fn decode_png_to_argb_icon(png_bytes: &[u8]) -> Result<ksni::Icon> {
    let mut decoder = png::Decoder::new(Cursor::new(png_bytes));
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );

    let mut reader = decoder.read_info().context("failed to read PNG header")?;
    let buf_size = reader
        .output_buffer_size()
        .context("PNG output buffer size overflow")?;
    let mut raw = vec![0u8; buf_size];
    let info = reader
        .next_frame(&mut raw)
        .context("failed to decode PNG frame")?;
    let bytes = &raw[..info.buffer_size()];

    if info.width == 0 || info.height == 0 {
        bail!("PNG icon has zero dimension ({}x{})", info.width, info.height);
    }

    let pixel_count = (info.width as usize)
        .checked_mul(info.height as usize)
        .context("PNG pixel count overflow")?;

    let mut argb = Vec::with_capacity(pixel_count * 4);
    match info.color_type {
        png::ColorType::Rgba => {
            if bytes.len() != pixel_count * 4 {
                bail!(
                    "unexpected RGBA buffer length: got {}, expected {}",
                    bytes.len(),
                    pixel_count * 4
                );
            }
            for &[r, g, b, a] in bytes.as_chunks::<4>().0 {
                argb.extend_from_slice(&[a, r, g, b]);
            }
        }
        png::ColorType::Rgb => {
            if bytes.len() != pixel_count * 3 {
                bail!(
                    "unexpected RGB buffer length: got {}, expected {}",
                    bytes.len(),
                    pixel_count * 3
                );
            }
            for &[r, g, b] in bytes.as_chunks::<3>().0 {
                argb.extend_from_slice(&[0xFF, r, g, b]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            if bytes.len() != pixel_count * 2 {
                bail!(
                    "unexpected GrayscaleAlpha buffer length: got {}, expected {}",
                    bytes.len(),
                    pixel_count * 2
                );
            }
            for &[y, a] in bytes.as_chunks::<2>().0 {
                argb.extend_from_slice(&[a, y, y, y]);
            }
        }
        png::ColorType::Grayscale => {
            if bytes.len() != pixel_count {
                bail!(
                    "unexpected Grayscale buffer length: got {}, expected {}",
                    bytes.len(),
                    pixel_count
                );
            }
            for &y in bytes {
                argb.extend_from_slice(&[0xFF, y, y, y]);
            }
        }
        other => bail!("unsupported decoded PNG color type: {other:?}"),
    }

    Ok(ksni::Icon {
        width: i32::try_from(info.width).context("PNG width exceeds i32")?,
        height: i32::try_from(info.height).context("PNG height exceeds i32")?,
        data: argb,
    })
}

pub fn embedded_tray_icons() -> &'static [ksni::Icon] {
    &EMBEDDED_ICONS
}

/// Music settings needed when starting music mode from the tray menu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MusicConfig {
    pub style: MusicStyle,
    pub fps: u32,
    pub sensitivity: f32,
}

/// Events emitted by the tray thread (or single-instance IPC) to the GPUI main thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEvent {
    ShowWindow { activation_token: Option<String> },
    ToggleMusicMode,
    RestoreStock,
    Quit,
    WatcherOffline,
    WatcherOnline,
}

/// Pure command mapping result for a [`TrayEvent`].
#[derive(Debug, Clone, PartialEq)]
pub enum TrayAction {
    ShowWindow { activation_token: Option<String> },
    Worker(WorkerCmd),
    Quit,
    TrayOffline,
    TrayOnline,
}

/// Map a tray event + current keyboard mode and music settings into the concrete action
/// to take on the main thread / worker.
pub fn map_tray_event(
    event: TrayEvent,
    current_mode: ActiveMode,
    music: MusicConfig,
) -> TrayAction {
    match event {
        TrayEvent::ShowWindow { activation_token } => TrayAction::ShowWindow { activation_token },
        TrayEvent::ToggleMusicMode => {
            if current_mode == ActiveMode::Music {
                TrayAction::Worker(WorkerCmd::RestoreStock)
            } else {
                TrayAction::Worker(WorkerCmd::StartMusic {
                    style: music.style,
                    fps: music.fps,
                    sensitivity: music.sensitivity,
                })
            }
        }
        TrayEvent::RestoreStock => TrayAction::Worker(WorkerCmd::RestoreStock),
        TrayEvent::Quit => TrayAction::Quit,
        TrayEvent::WatcherOffline => TrayAction::TrayOffline,
        TrayEvent::WatcherOnline => TrayAction::TrayOnline,
    }
}

struct XskbTray {
    tx: Sender<TrayEvent>,
    online: Arc<AtomicBool>,
    active_mode: ActiveMode,
    connected: bool,
}

impl ksni::Tray for XskbTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn title(&self) -> String {
        APP_TITLE.into()
    }

    fn icon_name(&self) -> String {
        // Leave empty so StatusNotifierHost always uses our embedded ARGB32 pixmaps.
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        embedded_tray_icons().to_vec()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let conn = if self.connected {
            "Connected"
        } else {
            "Disconnected"
        };
        ksni::ToolTip {
            title: APP_TITLE.into(),
            description: format!("{} ({conn})", self.active_mode.label()),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(TrayEvent::ShowWindow {
            activation_token: ksni::current_xdg_activation_token(),
        });
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};

        vec![
            StandardItem {
                label: "Show window".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayEvent::ShowWindow {
                        activation_token: ksni::current_xdg_activation_token(),
                    });
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Music mode".into(),
                checked: self.active_mode == ActiveMode::Music,
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayEvent::ToggleMusicMode);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Off / Restore stock lighting".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayEvent::RestoreStock);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.tx.send(TrayEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.online.store(false, Ordering::SeqCst);
        let _ = self.tx.send(TrayEvent::WatcherOffline);
        // Keep listening so if the desktop shell restarts, the tray comes back online.
        true
    }

    fn watcher_online(&self) {
        self.online.store(true, Ordering::SeqCst);
        let _ = self.tx.send(TrayEvent::WatcherOnline);
    }
}

/// Handle to the running StatusNotifierItem service.
pub struct TrayController {
    handle: ksni::blocking::Handle<XskbTray>,
    online: Arc<AtomicBool>,
}

impl TrayController {
    pub fn spawn(
        tx: Sender<TrayEvent>,
        initial_mode: ActiveMode,
        initial_connected: bool,
    ) -> std::result::Result<Self, ksni::Error> {
        // Ensure embedded icons decode cleanly before registering on D-Bus.
        let _ = embedded_tray_icons();

        let online = Arc::new(AtomicBool::new(true));
        let tray = XskbTray {
            tx,
            online: Arc::clone(&online),
            active_mode: initial_mode,
            connected: initial_connected,
        };
        let handle = tray.spawn()?;
        Ok(Self { handle, online })
    }

    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::SeqCst) && !self.handle.is_closed()
    }

    /// Push updated keyboard mode / connection status to the tray menu & tooltip.
    /// Must be called off the UI thread so D-Bus I/O never blocks rendering.
    pub fn sync_state(&self, active_mode: ActiveMode, connected: bool) {
        let _ = self.handle.update(|tray| {
            tray.active_mode = active_mode;
            tray.connected = connected;
        });
    }

    /// Request tray service shutdown without blocking.
    pub fn shutdown(&self) {
        self.online.store(false, Ordering::SeqCst);
        let _ = self.handle.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("valid PNG header");
            writer.write_image_data(rgba).expect("write PNG data");
        }
        out
    }

    #[test]
    fn decode_png_converts_rgba_to_big_endian_argb() {
        let rgba = [0x11, 0x22, 0x33, 0x44, 0xAA, 0xBB, 0xCC, 0xFF];
        let png_bytes = encode_rgba_png(2, 1, &rgba);

        let icon = decode_png_to_argb_icon(&png_bytes).expect("decode synthetic PNG");
        assert_eq!(icon.width, 2);
        assert_eq!(icon.height, 1);
        assert_eq!(
            icon.data,
            vec![0x44, 0x11, 0x22, 0x33, 0xFF, 0xAA, 0xBB, 0xCC]
        );
    }

    #[test]
    fn decode_png_handles_rgb_without_alpha() {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("valid PNG header");
            writer
                .write_image_data(&[0x7C, 0x3A, 0xED])
                .expect("write RGB PNG");
        }

        let icon = decode_png_to_argb_icon(&out).expect("decode RGB PNG");
        assert_eq!(icon.width, 1);
        assert_eq!(icon.height, 1);
        assert_eq!(icon.data, vec![0xFF, 0x7C, 0x3A, 0xED]);
    }

    #[test]
    fn decode_png_rejects_invalid_bytes() {
        assert!(decode_png_to_argb_icon(b"not a png").is_err());
    }

    #[test]
    fn embedded_icons_decode_to_valid_argb32_pixmaps() {
        let icons = embedded_tray_icons();
        let expected_sizes = [22, 32, 48, 64];
        assert_eq!(icons.len(), expected_sizes.len());

        for (icon, expected) in icons.iter().zip(expected_sizes) {
            assert_eq!(icon.width, expected);
            assert_eq!(icon.height, expected);
            assert_eq!(icon.data.len(), (expected as usize) * (expected as usize) * 4);
            // Verify there are visible opaque/translucent pixels with purple/magenta tones.
            let has_visible_accent = icon
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .any(|&[a, r, _g, b]| a > 200 && r > 100 && b > 150);
            assert!(
                has_visible_accent,
                "expected visible purple/magenta pixels in {expected}x{expected} tray icon"
            );
        }
    }

    #[test]
    fn tray_command_mapping_toggles_music_and_maps_actions() {
        let cfg = MusicConfig {
            style: MusicStyle::Meter,
            fps: 50,
            sensitivity: 1.5,
        };

        assert_eq!(
            map_tray_event(TrayEvent::ToggleMusicMode, ActiveMode::Stock, cfg),
            TrayAction::Worker(WorkerCmd::StartMusic {
                style: MusicStyle::Meter,
                fps: 50,
                sensitivity: 1.5,
            })
        );
        assert_eq!(
            map_tray_event(TrayEvent::ToggleMusicMode, ActiveMode::Solid, cfg),
            TrayAction::Worker(WorkerCmd::StartMusic {
                style: MusicStyle::Meter,
                fps: 50,
                sensitivity: 1.5,
            })
        );
        assert_eq!(
            map_tray_event(TrayEvent::ToggleMusicMode, ActiveMode::Music, cfg),
            TrayAction::Worker(WorkerCmd::RestoreStock)
        );
        assert_eq!(
            map_tray_event(TrayEvent::RestoreStock, ActiveMode::Solid, cfg),
            TrayAction::Worker(WorkerCmd::RestoreStock)
        );
        assert_eq!(
            map_tray_event(
                TrayEvent::ShowWindow {
                    activation_token: Some("tok".into())
                },
                ActiveMode::Stock,
                cfg
            ),
            TrayAction::ShowWindow {
                activation_token: Some("tok".into())
            }
        );
        assert_eq!(
            map_tray_event(TrayEvent::Quit, ActiveMode::Music, cfg),
            TrayAction::Quit
        );
        assert_eq!(
            map_tray_event(TrayEvent::WatcherOffline, ActiveMode::Music, cfg),
            TrayAction::TrayOffline
        );
        assert_eq!(
            map_tray_event(TrayEvent::WatcherOnline, ActiveMode::Music, cfg),
            TrayAction::TrayOnline
        );
    }
}

mod audio;
mod hid;
mod protocol;
mod worker;

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Bounds, Context, IntoElement, KeyBinding, Render, SharedString, TitlebarOptions, Window,
    WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_platform::application;

use crate::audio::MusicStyle;
use crate::protocol::{active_cols, active_rows, hid_key_label};
use crate::worker::{ActiveMode, SharedState, WorkerCmd, WorkerHandle};

actions!(xskb, [Quit]);

struct ColorPreset {
    name: &'static str,
    rgb: [u8; 3],
}

const PRESETS: &[ColorPreset] = &[
    ColorPreset { name: "Red", rgb: [255, 32, 32] },
    ColorPreset { name: "Orange", rgb: [255, 110, 16] },
    ColorPreset { name: "Amber", rgb: [255, 185, 0] },
    ColorPreset { name: "Lime", rgb: [120, 255, 32] },
    ColorPreset { name: "Green", rgb: [0, 230, 80] },
    ColorPreset { name: "Cyan", rgb: [0, 225, 255] },
    ColorPreset { name: "Blue", rgb: [30, 110, 255] },
    ColorPreset { name: "Indigo", rgb: [110, 60, 255] },
    ColorPreset { name: "Purple", rgb: [190, 40, 255] },
    ColorPreset { name: "Magenta", rgb: [255, 20, 160] },
    ColorPreset { name: "Warm", rgb: [255, 210, 150] },
    ColorPreset { name: "White", rgb: [255, 255, 255] },
    ColorPreset { name: "Off", rgb: [0, 0, 0] },
];

const FPS_PRESETS: &[u32] = &[20, 30, 40, 50, 60];

fn rgb_u32([r, g, b]: [u8; 3]) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

fn contrasting_text([r, g, b]: [u8; 3]) -> u32 {
    let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    if luma > 135.0 { 0x111318 } else { 0xF3F4F6 }
}

fn key_width_units(usage: u8) -> f32 {
    match usage {
        0x2C => 4.2,       // Space
        0x2A => 1.8,       // Backspace
        0x28 => 1.8,       // Enter
        0xE1 => 1.9,       // Left Shift
        0xE5 => 1.4,       // Right Shift
        0x2B | 0x39 => 1.4, // Tab / Caps
        0xE0 | 0xE2 | 0xE3 => 1.15,
        _ => 1.0,
    }
}

struct XskbApp {
    worker: Arc<WorkerHandle>,
    snapshot: SharedState,
    solid_rgb: [u8; 3],
    brightness: f32,
    music_style: MusicStyle,
    music_fps: u32,
    music_sensitivity: f32,
}

impl XskbApp {
    fn new(worker: Arc<WorkerHandle>, cx: &mut Context<Self>) -> Self {
        let snapshot = worker.snapshot();

        // Poll worker state at ~40 Hz, notifying GPUI only when state changes.
        let worker_poll = Arc::clone(&worker);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(25))
                    .await;
                let latest = worker_poll.snapshot();
                let keep_going = this.update(cx, |this, cx| {
                    if latest.generation != this.snapshot.generation {
                        this.snapshot = latest;
                        cx.notify();
                    }
                });
                if keep_going.is_err() {
                    break;
                }
            }
        })
        .detach();

        Self {
            worker,
            snapshot,
            solid_rgb: [0, 225, 255],
            brightness: 0.85,
            music_style: MusicStyle::Rainbow,
            music_fps: 40,
            music_sensitivity: 1.0,
        }
    }

    fn send_solid(&self, burst: bool) {
        self.worker.send(WorkerCmd::SetSolid {
            rgb: self.solid_rgb,
            brightness: self.brightness,
            burst,
        });
    }

    fn update_solid_if_active(&self) {
        if self.snapshot.active_mode == ActiveMode::Solid {
            self.send_solid(false);
        }
    }

    fn update_music_if_active(&self) {
        if self.snapshot.active_mode == ActiveMode::Music {
            self.worker.send(WorkerCmd::UpdateMusic {
                style: self.music_style,
                fps: self.music_fps,
                sensitivity: self.music_sensitivity,
            });
        }
    }

    fn scaled_solid_rgb(&self) -> [u8; 3] {
        [
            (self.solid_rgb[0] as f32 * self.brightness).round() as u8,
            (self.solid_rgb[1] as f32 * self.brightness).round() as u8,
            (self.solid_rgb[2] as f32 * self.brightness).round() as u8,
        ]
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = self.snapshot.connected;
        let badge_bg = if connected { 0x133829 } else { 0x3D181A };
        let badge_dot = if connected { 0x22C55E } else { 0xEF4444 };
        let badge_text = if connected { "Connected" } else { "Disconnected" };

        div()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .py_3()
            .rounded_lg()
            .bg(rgb(0x161922))
            .border_1()
            .border_color(rgb(0x262B3A))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(rgb(0xF8FAFC))
                                    .child("XSKB Lighting Controller"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_full()
                                    .bg(rgb(badge_bg))
                                    .child(div().size(px(8.0)).rounded_full().bg(rgb(badge_dot)))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgb(0xE2E8F0))
                                            .child(badge_text),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .text_xs()
                            .text_color(rgb(0x94A3B8))
                            .child(format!("Device: {}", self.snapshot.product_name))
                            .child(format!("FW: {}", self.snapshot.firmware_version))
                            .child(format!("Info: {}", self.snapshot.app_info))
                            .child(format!("Path: {}", self.snapshot.hidraw_path)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("btn-reconnect")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(0x222736))
                            .border_1()
                            .border_color(rgb(0x333B52))
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(rgb(0xE2E8F0))
                            .cursor_pointer()
                            .child("Reconnect")
                            .on_click(cx.listener(|this, _, _, _| {
                                this.worker.send(WorkerCmd::Reconnect);
                            })),
                    )
                    .child(
                        div()
                            .id("btn-restore")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(0x7F1D1D))
                            .border_1()
                            .border_color(rgb(0xB91C1C))
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xFEE2E2))
                            .cursor_pointer()
                            .child("Off / Restore Stock")
                            .on_click(cx.listener(|this, _, _, _| {
                                this.worker.send(WorkerCmd::RestoreStock);
                            })),
                    ),
            )
    }

    fn render_keyboard_preview(&self) -> impl IntoElement {
        let matrix = &self.snapshot.matrix;
        let rows = active_rows(matrix);
        let cols = active_cols(matrix);
        let is_music = self.snapshot.active_mode == ActiveMode::Music;
        let is_stock = self.snapshot.active_mode == ActiveMode::Stock;

        let unit_px = 52.0f32;
        let key_h = 44.0f32;

        let mut row_elements = Vec::with_capacity(rows.len());
        for &r in &rows {
            let mut key_elements = Vec::with_capacity(cols.len());
            for &c in &cols {
                let usage = matrix[r][c];
                if usage == 0 {
                    // Preserve horizontal rhythm for empty matrix cells (e.g. around Spacebar).
                    key_elements.push(div().w(px(unit_px * 0.48)).h(px(key_h)).into_any_element());
                    continue;
                }

                let sent_rgb = self
                    .snapshot
                    .key_colors
                    .get(&usage)
                    .copied()
                    .unwrap_or([0, 0, 0]);
                let label = hid_key_label(usage);
                let label_text: SharedString = if label.is_empty() {
                    format!("{usage:02X}").into()
                } else {
                    label.into()
                };

                let is_lit = sent_rgb != [0, 0, 0];
                let key_bg = if is_stock {
                    rgb(0x202532)
                } else if is_lit {
                    rgb(rgb_u32(sent_rgb))
                } else {
                    rgb(0x1B1F2B)
                };

                let border_color = if is_lit && !is_stock {
                    rgb(rgb_u32(sent_rgb))
                } else {
                    rgb(0x2F364A)
                };

                let text_col = if is_stock || !is_lit {
                    0x94A3B8
                } else {
                    contrasting_text(sent_rgb)
                };

                let w = unit_px * key_width_units(usage);
                key_elements.push(
                    div()
                        .w(px(w))
                        .h(px(key_h))
                        .rounded_md()
                        .bg(key_bg)
                        .border_1()
                        .border_color(border_color)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(text_col))
                        .child(label_text)
                        .into_any_element(),
                );
            }

            row_elements.push(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .children(key_elements)
                    .into_any_element(),
            );
        }

        // Mini spectrum bar row under the keyboard when Music mode is running.
        let mut meter_bars = Vec::new();
        for (i, &lvl) in self.snapshot.band_levels.iter().enumerate() {
            let h = (lvl.clamp(0.0, 1.0) * 24.0).max(2.0);
            let bar_color = if is_music {
                if self.music_style == MusicStyle::Rainbow {
                    let c = crate::audio::hsv_to_rgb(
                        i as f32 / self.snapshot.band_levels.len().max(1) as f32,
                        0.9,
                        0.95,
                    );
                    rgb(rgb_u32(c))
                } else if lvl > 0.75 {
                    rgb(0xEF4444)
                } else if lvl > 0.45 {
                    rgb(0xFACC15)
                } else {
                    rgb(0x22C55E)
                }
            } else {
                rgb(0x2A3142)
            };
            meter_bars.push(
                div()
                    .flex_1()
                    .h(px(26.0))
                    .flex()
                    .items_end()
                    .child(div().w_full().h(px(h)).rounded_sm().bg(bar_color)),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(rgb(0x13161F))
            .border_1()
            .border_color(rgb(0x262B3A))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xCBD5E1))
                            .child("LIVE KEYBOARD PREVIEW"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x94A3B8))
                            .child(format!("Mode: {}", self.snapshot.active_mode.label())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1p5()
                    .py_2()
                    .px_3()
                    .rounded_lg()
                    .bg(rgb(0x0D1017))
                    .border_1()
                    .border_color(rgb(0x1E2433))
                    .children(row_elements),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .children(meter_bars),
            )
    }

    fn render_channel_bar(
        &self,
        id_prefix: &'static str,
        label: &'static str,
        value: u8,
        bar_hex: u32,
        channel_idx: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let steps = 16usize;
        let active_steps = ((value as usize) * steps + 127) / 255;

        let mut segs = Vec::with_capacity(steps);
        for s in 0..steps {
            let seg_val = (((s + 1) * 255) / steps).min(255) as u8;
            let lit = s < active_steps;
            let seg_id: SharedString = format!("{id_prefix}-seg-{s}").into();
            segs.push(
                div()
                    .id(seg_id)
                    .flex_1()
                    .h(px(14.0))
                    .rounded_sm()
                    .bg(if lit { rgb(bar_hex) } else { rgb(0x232938) })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.solid_rgb[channel_idx] = seg_val;
                        this.update_solid_if_active();
                        cx.notify();
                    })),
            );
        }

        let dec_id: SharedString = format!("{id_prefix}-dec").into();
        let inc_id: SharedString = format!("{id_prefix}-inc").into();

        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(22.0))
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(0xCBD5E1))
                    .child(label),
            )
            .child(
                div()
                    .id(dec_id)
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(rgb(0x222736))
                    .text_xs()
                    .text_color(rgb(0xE2E8F0))
                    .cursor_pointer()
                    .child("-")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.solid_rgb[channel_idx] = this.solid_rgb[channel_idx].saturating_sub(15);
                        this.update_solid_if_active();
                        cx.notify();
                    })),
            )
            .child(div().flex_1().flex().gap_0p5().children(segs))
            .child(
                div()
                    .id(inc_id)
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(rgb(0x222736))
                    .text_xs()
                    .text_color(rgb(0xE2E8F0))
                    .cursor_pointer()
                    .child("+")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.solid_rgb[channel_idx] = this.solid_rgb[channel_idx].saturating_add(15);
                        this.update_solid_if_active();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w(px(32.0))
                    .text_xs()
                    .text_color(rgb(0x94A3B8))
                    .child(format!("{value:3}")),
            )
    }

    fn render_solid_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.snapshot.active_mode == ActiveMode::Solid;
        let scaled = self.scaled_solid_rgb();
        let hex_label = format!("#{:02X}{:02X}{:02X}", scaled[0], scaled[1], scaled[2]);

        let mut swatch_els = Vec::with_capacity(PRESETS.len());
        for (i, preset) in PRESETS.iter().enumerate() {
            let selected = self.solid_rgb == preset.rgb;
            let swatch_rgb = preset.rgb;
            let id: SharedString = format!("preset-{i}").into();
            swatch_els.push(
                div()
                    .id(id)
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(rgb_u32(swatch_rgb)))
                    .border_2()
                    .border_color(if selected {
                        rgb(0xF8FAFC)
                    } else {
                        rgb(0x2A3142)
                    })
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(contrasting_text(swatch_rgb)))
                    .cursor_pointer()
                    .child(preset.name)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.solid_rgb = swatch_rgb;
                        if this.snapshot.active_mode == ActiveMode::Solid {
                            this.send_solid(false);
                        } else {
                            this.send_solid(true);
                        }
                        cx.notify();
                    })),
            );
        }

        // Brightness bar (10 steps: 10%..100%)
        let b_steps = 10usize;
        let b_active = (self.brightness * b_steps as f32).round() as usize;
        let mut b_segs = Vec::with_capacity(b_steps);
        for s in 0..b_steps {
            let val = (s + 1) as f32 / b_steps as f32;
            let lit = s < b_active;
            let id: SharedString = format!("bright-seg-{s}").into();
            b_segs.push(
                div()
                    .id(id)
                    .flex_1()
                    .h(px(14.0))
                    .rounded_sm()
                    .bg(if lit { rgb(0xF59E0B) } else { rgb(0x232938) })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.brightness = val;
                        this.update_solid_if_active();
                        cx.notify();
                    })),
            );
        }

        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(rgb(0x161922))
            .border_1()
            .border_color(if active { rgb(0x38BDF8) } else { rgb(0x262B3A) })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(0xF8FAFC))
                            .child("Solid Colour"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .size(px(18.0))
                                    .rounded_sm()
                                    .bg(rgb(rgb_u32(scaled)))
                                    .border_1()
                                    .border_color(rgb(0x475569)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xCBD5E1))
                                    .child(hex_label),
                            ),
                    ),
            )
            .child(div().flex().flex_wrap().gap_1p5().children(swatch_els))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(self.render_channel_bar("rgb-r", "R", self.solid_rgb[0], 0xEF4444, 0, cx))
                    .child(self.render_channel_bar("rgb-g", "G", self.solid_rgb[1], 0x22C55E, 1, cx))
                    .child(self.render_channel_bar("rgb-b", "B", self.solid_rgb[2], 0x3B82F6, 2, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(72.0))
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xCBD5E1))
                            .child("Brightness"),
                    )
                    .child(
                        div()
                            .id("bright-dec")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(rgb(0x222736))
                            .text_xs()
                            .text_color(rgb(0xE2E8F0))
                            .cursor_pointer()
                            .child("-")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.brightness = (this.brightness - 0.1).clamp(0.0, 1.0);
                                this.update_solid_if_active();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().flex().gap_0p5().children(b_segs))
                    .child(
                        div()
                            .id("bright-inc")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(rgb(0x222736))
                            .text_xs()
                            .text_color(rgb(0xE2E8F0))
                            .cursor_pointer()
                            .child("+")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.brightness = (this.brightness + 0.1).clamp(0.0, 1.0);
                                this.update_solid_if_active();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .w(px(36.0))
                            .text_xs()
                            .text_color(rgb(0x94A3B8))
                            .child(format!("{}%", (self.brightness * 100.0).round() as u32)),
                    ),
            )
            .child(
                div()
                    .id("btn-apply-solid")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x0284C7))
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(0xF0F9FF))
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .child(if active {
                        "Solid Mode Active — Resend Colour"
                    } else {
                        "Apply Solid Colour"
                    })
                    .on_click(cx.listener(|this, _, _, _| {
                        this.send_solid(true);
                    })),
            )
    }

    fn render_music_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.snapshot.active_mode == ActiveMode::Music;

        let mut fps_buttons = Vec::with_capacity(FPS_PRESETS.len());
        for &fps in FPS_PRESETS {
            let selected = self.music_fps == fps;
            let id: SharedString = format!("fps-{fps}").into();
            fps_buttons.push(
                div()
                    .id(id)
                    .px_2p5()
                    .py_1()
                    .rounded_md()
                    .bg(if selected { rgb(0x7C3AED) } else { rgb(0x222736) })
                    .border_1()
                    .border_color(if selected { rgb(0xA78BFA) } else { rgb(0x333B52) })
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(0xF8FAFC))
                    .cursor_pointer()
                    .child(format!("{fps}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.music_fps = fps;
                        this.update_music_if_active();
                        cx.notify();
                    })),
            );
        }

        // Sensitivity bar: 12 steps from 0.25x to 3.00x
        let s_steps = 12usize;
        let mut s_segs = Vec::with_capacity(s_steps);
        for s in 0..s_steps {
            let val = 0.25 + (s as f32) * 0.25;
            let lit = self.music_sensitivity + 0.01 >= val;
            let id: SharedString = format!("sens-seg-{s}").into();
            s_segs.push(
                div()
                    .id(id)
                    .flex_1()
                    .h(px(14.0))
                    .rounded_sm()
                    .bg(if lit { rgb(0xA855F7) } else { rgb(0x232938) })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.music_sensitivity = val;
                        this.update_music_if_active();
                        cx.notify();
                    })),
            );
        }

        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(rgb(0x161922))
            .border_1()
            .border_color(if active { rgb(0xA855F7) } else { rgb(0x262B3A) })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(0xF8FAFC))
                            .child("Music Reactive (PipeWire FFT)"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x94A3B8))
                            .child("15 bands · 40 Hz – 16 kHz"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(72.0))
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xCBD5E1))
                            .child("Style"),
                    )
                    .child(
                        div()
                            .id("style-rainbow")
                            .flex_1()
                            .py_1p5()
                            .rounded_md()
                            .bg(if self.music_style == MusicStyle::Rainbow {
                                rgb(0x7C3AED)
                            } else {
                                rgb(0x222736)
                            })
                            .border_1()
                            .border_color(if self.music_style == MusicStyle::Rainbow {
                                rgb(0xA78BFA)
                            } else {
                                rgb(0x333B52)
                            })
                            .flex()
                            .justify_center()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xF8FAFC))
                            .cursor_pointer()
                            .child("Rainbow")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.music_style = MusicStyle::Rainbow;
                                this.update_music_if_active();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("style-meter")
                            .flex_1()
                            .py_1p5()
                            .rounded_md()
                            .bg(if self.music_style == MusicStyle::Meter {
                                rgb(0x15803D)
                            } else {
                                rgb(0x222736)
                            })
                            .border_1()
                            .border_color(if self.music_style == MusicStyle::Meter {
                                rgb(0x4ADE80)
                            } else {
                                rgb(0x333B52)
                            })
                            .flex()
                            .justify_center()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xF8FAFC))
                            .cursor_pointer()
                            .child("Meter")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.music_style = MusicStyle::Meter;
                                this.update_music_if_active();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(72.0))
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xCBD5E1))
                            .child("FPS"),
                    )
                    .child(
                        div()
                            .id("fps-dec")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(rgb(0x222736))
                            .text_xs()
                            .text_color(rgb(0xE2E8F0))
                            .cursor_pointer()
                            .child("-")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.music_fps = this.music_fps.saturating_sub(5).max(10);
                                this.update_music_if_active();
                                cx.notify();
                            })),
                    )
                    .child(div().flex().gap_1p5().children(fps_buttons))
                    .child(
                        div()
                            .id("fps-inc")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(rgb(0x222736))
                            .text_xs()
                            .text_color(rgb(0xE2E8F0))
                            .cursor_pointer()
                            .child("+")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.music_fps = (this.music_fps + 5).min(120);
                                this.update_music_if_active();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x94A3B8))
                            .child(format!("{} fps", self.music_fps)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(72.0))
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xCBD5E1))
                            .child("Sensitivity"),
                    )
                    .child(
                        div()
                            .id("sens-dec")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(rgb(0x222736))
                            .text_xs()
                            .text_color(rgb(0xE2E8F0))
                            .cursor_pointer()
                            .child("-")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.music_sensitivity =
                                    (this.music_sensitivity - 0.25).clamp(0.25, 3.0);
                                this.update_music_if_active();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().flex().gap_0p5().children(s_segs))
                    .child(
                        div()
                            .id("sens-inc")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(rgb(0x222736))
                            .text_xs()
                            .text_color(rgb(0xE2E8F0))
                            .cursor_pointer()
                            .child("+")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.music_sensitivity =
                                    (this.music_sensitivity + 0.25).clamp(0.25, 3.0);
                                this.update_music_if_active();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .w(px(38.0))
                            .text_xs()
                            .text_color(rgb(0x94A3B8))
                            .child(format!("{:.2}x", self.music_sensitivity)),
                    ),
            )
            .child(
                div()
                    .mt_auto()
                    .id("btn-toggle-music")
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(if active { rgb(0x9333EA) } else { rgb(0x6D28D9) })
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(0xFAF5FF))
                    .flex()
                    .justify_center()
                    .cursor_pointer()
                    .child(if active {
                        "Stop Music & Restore Stock"
                    } else {
                        "Start Music Mode"
                    })
                    .on_click(cx.listener(|this, _, _, _| {
                        if this.snapshot.active_mode == ActiveMode::Music {
                            this.worker.send(WorkerCmd::RestoreStock);
                        } else {
                            this.worker.send(WorkerCmd::StartMusic {
                                style: this.music_style,
                                fps: this.music_fps,
                                sensitivity: this.music_sensitivity,
                            });
                        }
                    })),
            )
    }

    fn render_footer(&self) -> impl IntoElement {
        let status_color = if self.snapshot.error_message.is_some() {
            0xFCA5A5
        } else {
            0x94A3B8
        };
        let status_text = self
            .snapshot
            .error_message
            .clone()
            .unwrap_or_else(|| self.snapshot.status_message.clone());
        let backup_badge = if self.snapshot.backup_exists {
            format!("Backup OK: {}", self.snapshot.backup_location)
        } else {
            format!("Backup pending: {}", self.snapshot.backup_location)
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(0x13161F))
            .border_1()
            .border_color(rgb(0x222736))
            .text_xs()
            .child(div().text_color(rgb(status_color)).child(status_text))
            .child(div().text_color(rgb(0x64748B)).child(backup_badge))
    }
}

impl Render for XskbApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(rgb(0x0E1118))
            .text_color(rgb(0xF8FAFC))
            .child(self.render_header(cx))
            .child(self.render_keyboard_preview())
            .child(
                div()
                    .flex_1()
                    .flex()
                    .gap_3()
                    .child(self.render_solid_panel(cx))
                    .child(self.render_music_panel(cx)),
            )
            .child(self.render_footer())
    }
}

fn run_probe_cli() -> anyhow::Result<()> {
    let path = hid::find_hidraw()?;
    let mut dev = hid::HidDevice::open(&path)?;
    let probe = dev.startup_probe()?;
    println!("device: {}", probe.hidraw_path.display());
    println!("product: {}", probe.product_name);
    println!("firmware: {}", probe.firmware_version);
    println!("info: {} (mode: {})", probe.app_info, probe.mode_name);
    println!(
        "matrix: {} active rows x {} active cols ({} LEDs)",
        active_rows(&probe.matrix).len(),
        active_cols(&probe.matrix).len(),
        protocol::unique_leds(&probe.matrix).len()
    );
    println!(
        "lighting config flags=0x{:02x} mode=0x{:02x} effect=0x{:02x}",
        probe.config[37], probe.config[38], probe.config[39]
    );
    Ok(())
}

fn main() {
    if std::env::args().any(|a| a == "--probe") {
        if let Err(err) = run_probe_cli() {
            eprintln!("probe error: {err:#}");
            std::process::exit(1);
        }
        return;
    }

    let worker = WorkerHandle::spawn();

    // Restore stock lighting cleanly on SIGINT or SIGTERM.
    {
        let worker_sig = Arc::clone(&worker);
        let _ = ctrlc::set_handler(move || {
            worker_sig.shutdown_and_wait();
            std::process::exit(0);
        });
    }

    let worker_close = Arc::clone(&worker);
    let worker_quit = Arc::clone(&worker);
    let worker_view = Arc::clone(&worker);

    application().run(move |cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);

        cx.on_window_closed(move |cx, _| {
            if cx.windows().is_empty() {
                worker_close.shutdown_and_wait();
                cx.quit();
            }
        })
        .detach();

        cx.on_app_quit(move |_cx| {
            let w = Arc::clone(&worker_quit);
            async move {
                w.shutdown_and_wait();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1040.0), px(720.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("XSKB — Keyboard Lighting Controller".into()),
                    ..Default::default()
                }),
                app_id: Some("io.github.wincohax.Xskb".into()),
                ..Default::default()
            },
            move |_window, cx| {
                let w = Arc::clone(&worker_view);
                cx.new(move |cx| XskbApp::new(w, cx))
            },
        )
        .expect("failed to open XSKB window");

        cx.activate(true);
    });

    worker.shutdown_and_wait();
}

//! Audio capture (`pw-record`) and FFT spectrum mapping matching the `xskb.py` Python prototype.

use std::collections::HashMap;
use std::f32::consts::PI;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rustfft::{Fft, FftPlanner, num_complex::Complex};

use crate::protocol::{MATRIX_COLS, MATRIX_ROWS};

pub const SAMPLE_RATE: usize = 48_000;
pub const FFT_SIZE: usize = 1024;
pub const BAND_LO_HZ: f32 = 40.0;
pub const BAND_HI_HZ: f32 = 16_000.0;
pub const NOISE_FLOOR: f32 = 2.5e4;
pub const PEAK_DECAY: f32 = 0.995;
pub const LEVEL_DECAY: f32 = 0.85;
pub const LEVEL_BLEND: f32 = 0.15;
pub const GAMMA: f32 = 0.7;
pub const HUE_STEP: f32 = 0.002;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MusicStyle {
    Rainbow,
    Meter,
}

impl MusicStyle {
    pub fn label(self) -> &'static str {
        match self {
            MusicStyle::Rainbow => "Rainbow",
            MusicStyle::Meter => "Meter",
        }
    }
}

/// Compute `num_bands` log-spaced `(start_bin, end_bin)` ranges from `40 Hz` to `16 kHz`.
/// Matches the `xskb.py` Python prototype:
/// `edges = [lo * (hi / lo) ** (i / num_bands) for i in 0..=num_bands]`
/// `bins = [max(1, int(f * n / rate)) for f in edges]`
/// `band i = [bins[i], max(bins[i] + 1, bins[i + 1]))`
pub fn compute_band_bins(
    num_bands: usize,
    fft_size: usize,
    sample_rate: usize,
) -> Vec<(usize, usize)> {
    if num_bands == 0 {
        return Vec::new();
    }
    let max_bin = fft_size / 2;
    let ratio = BAND_HI_HZ / BAND_LO_HZ;
    let edges: Vec<usize> = (0..=num_bands)
        .map(|i| {
            let freq = BAND_LO_HZ * ratio.powf(i as f32 / num_bands as f32);
            let bin = (freq * fft_size as f32 / sample_rate as f32).floor() as usize;
            bin.clamp(1, max_bin)
        })
        .collect();

    (0..num_bands)
        .map(|i| {
            let a = edges[i].min(max_bin.saturating_sub(1));
            let b = edges[i + 1].max(a + 1).min(max_bin);
            (a, b)
        })
        .collect()
}

/// Convert HSV (`h` wraps in `[0, 1)`, `s` and `v` in `[0, 1]`) to 8-bit RGB,
/// matching Python's `colorsys.hsv_to_rgb`.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(1.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    if s <= 0.0 {
        let c = (v * 255.0) as u8;
        return [c, c, c];
    }
    let sector = (h * 6.0).floor();
    let i = (sector as i32) % 6;
    let f = h * 6.0 - sector;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

/// Stateful FFT spectrum analyzer matching the auto-gain, attack/decay, and noise
/// floor behaviour of the `xskb.py` Python prototype.
pub struct SpectrumAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    bins: Vec<(usize, usize)>,
    sample_buf: Vec<i16>,
    fft_scratch: Vec<Complex<f32>>,
    levels: Vec<f32>,
    peak: f32,
    hue0: f32,
}

impl SpectrumAnalyzer {
    pub fn new(num_bands: usize) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        let bins = compute_band_bins(num_bands, FFT_SIZE, SAMPLE_RATE);

        Self {
            fft,
            window,
            bins,
            sample_buf: vec![0i16; FFT_SIZE],
            fft_scratch: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            levels: vec![0.0; num_bands],
            peak: 1e-3,
            hue0: 0.0,
        }
    }

    /// Push newly captured mono `s16` samples, retaining the most recent `FFT_SIZE` samples.
    pub fn push_samples(&mut self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        if samples.len() >= FFT_SIZE {
            let start = samples.len() - FFT_SIZE;
            self.sample_buf.copy_from_slice(&samples[start..]);
        } else {
            let keep = FFT_SIZE - samples.len();
            self.sample_buf.copy_within(samples.len().., 0);
            self.sample_buf[keep..].copy_from_slice(samples);
        }
    }

    /// Run one FFT + auto-gain + attack/decay step and return the updated band levels `[0.0..=1.0]`.
    pub fn step(&mut self, sensitivity: f32) -> &[f32] {
        let sensitivity = sensitivity.clamp(0.1, 4.0);
        for (dst, (&s, &w)) in self
            .fft_scratch
            .iter_mut()
            .zip(self.sample_buf.iter().zip(self.window.iter()))
        {
            *dst = Complex::new(s as f32 * w, 0.0);
        }
        self.fft.process(&mut self.fft_scratch);

        let mut bands = Vec::with_capacity(self.bins.len());
        let mut max_band = 0.0f32;
        for &(a, b) in &self.bins {
            let count = (b - a).max(1) as f32;
            let sum_sq: f32 = self.fft_scratch[a..b]
                .iter()
                .map(|c| c.norm_sqr())
                .sum();
            let rms = (sum_sq / count).sqrt();
            if rms > max_band {
                max_band = rms;
            }
            bands.push(rms);
        }

        // At sensitivity = 1.0 this matches xskb.py identically:
        // peak = max(peak * 0.995, max(bands), 2.5e4)
        // v = min(1.0, (band / peak) ** 0.7)
        let effective_floor = NOISE_FLOOR / sensitivity;
        self.peak = (self.peak * PEAK_DECAY).max(max_band).max(effective_floor);

        for (lvl, band) in self.levels.iter_mut().zip(bands) {
            let norm = ((band / self.peak) * sensitivity).clamp(0.0, 1.0);
            let v = norm.powf(GAMMA);
            if v > *lvl {
                *lvl = v;
            } else {
                *lvl = *lvl * LEVEL_DECAY + v * LEVEL_BLEND;
            }
        }

        self.hue0 = (self.hue0 + HUE_STEP) % 1.0;
        &self.levels
    }

    /// Render the current spectrum levels onto the keyboard LED matrix.
    pub fn render_colors(
        &self,
        style: MusicStyle,
        rows: &[usize],
        cols: &[usize],
        matrix: &[[u8; MATRIX_COLS]; MATRIX_ROWS],
        leds: &[u8],
    ) -> HashMap<u8, [u8; 3]> {
        let mut colors: HashMap<u8, [u8; 3]> = leds.iter().map(|&idx| (idx, [0, 0, 0])).collect();
        let num_cols = cols.len().max(1);
        let num_rows = rows.len().max(1);

        for (ci, &c) in cols.iter().enumerate() {
            let band_level = self.levels.get(ci).copied().unwrap_or(0.0);
            let height = band_level * num_rows as f32;
            for (ri, &r) in rows.iter().rev().enumerate() {
                let idx = matrix[r][c];
                if idx == 0 {
                    continue;
                }
                let fill = (height - ri as f32).clamp(0.0, 1.0);
                if fill <= 0.0 {
                    continue;
                }
                let rgb = match style {
                    MusicStyle::Rainbow => {
                        hsv_to_rgb(self.hue0 + ci as f32 / num_cols as f32, 1.0, fill)
                    }
                    MusicStyle::Meter => {
                        let denom = (num_rows.saturating_sub(1)).max(1) as f32;
                        hsv_to_rgb(0.33 - 0.33 * ri as f32 / denom, 1.0, fill)
                    }
                };
                colors.insert(idx, rgb);
            }
        }
        colors
    }
}

/// Non-blocking PipeWire system-audio capture via `pw-record`.
pub struct PwCapture {
    child: Child,
    stdout: ChildStdout,
    pending: Vec<u8>,
}

impl PwCapture {
    pub fn start() -> Result<Self> {
        let mut child = Command::new("pw-record")
            .args([
                "-P",
                "stream.capture.sink=true",
                "-P",
                "node.latency=256/48000",
                "--rate",
                "48000",
                "--channels",
                "1",
                "--format",
                "s16",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn pw-record (is PipeWire installed?)")?;

        let stdout = child
            .stdout
            .take()
            .context("pw-record stdout was not piped")?;

        // Put stdout into non-blocking mode so our frame loop never stalls on silence.
        let fd = stdout.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        Ok(Self {
            child,
            stdout,
            pending: Vec::with_capacity(4096),
        })
    }

    /// Read any currently buffered PCM `i16` samples without blocking.
    pub fn read_samples(&mut self, out: &mut Vec<i16>) -> Result<()> {
        out.clear();
        let mut chunk = [0u8; 4096];
        loop {
            match self.stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => self.pending.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("reading from pw-record failed"),
            }
        }

        let usable = self.pending.len() - (self.pending.len() % 2);
        if usable > 0 {
            out.reserve(usable / 2);
            for &pair in self.pending[..usable].as_chunks::<2>().0 {
                out.push(i16::from_le_bytes(pair));
            }
            self.pending.drain(..usable);
        }

        if let Some(status) = self.child.try_wait()? {
            bail!("pw-record exited unexpectedly ({status}); is PipeWire running?");
        }
        Ok(())
    }
}

impl Drop for PwCapture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{FALLBACK_WIN68_MATRIX, active_cols, active_rows, unique_leds};

    #[test]
    fn band_mapping_15_bands_40hz_to_16khz() {
        let bins = compute_band_bins(15, FFT_SIZE, SAMPLE_RATE);
        assert_eq!(bins.len(), 15);

        // First edge: 40 Hz * 1024 / 48000 = 0.85 -> clamped to 1
        assert_eq!(bins[0].0, 1);

        // Last edge: 16000 Hz * 1024 / 48000 = 341.33 -> 341
        assert_eq!(bins[14].1, 341);

        // Every band is non-empty, monotonic in start bin, and strictly within Nyquist.
        for (i, &(a, b)) in bins.iter().enumerate() {
            assert!(a >= 1, "band {i} start bin {a} < 1");
            assert!(b > a, "band {i} empty range [{a}, {b})");
            assert!(b <= FFT_SIZE / 2, "band {i} end bin {b} exceeds Nyquist");
            if i > 0 {
                assert!(
                    a >= bins[i - 1].0,
                    "band {i} start {a} < prev start {}",
                    bins[i - 1].0
                );
            }
        }

        // High bands should be wider than low bands because spacing is logarithmic.
        let low_width = bins[0].1 - bins[0].0;
        let high_width = bins[14].1 - bins[14].0;
        assert!(high_width > low_width * 10);
    }

    #[test]
    fn silence_stays_dark_and_tone_lights_matching_band() {
        let mut analyzer = SpectrumAnalyzer::new(15);

        // Silence stays at zero level.
        analyzer.push_samples(&vec![0i16; FFT_SIZE]);
        let silence = analyzer.step(1.0);
        assert!(silence.iter().all(|&v| v < 1e-4));

        // 1 kHz full-scale sine wave should strongly excite a mid-frequency band.
        let tone: Vec<i16> = (0..FFT_SIZE)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                ((2.0 * PI * 1000.0 * t).sin() * 28000.0) as i16
            })
            .collect();
        analyzer.push_samples(&tone);
        let levels = analyzer.step(1.0);
        let (peak_band, &peak_val) = levels
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        // 1 kHz in log space [40..16000] is at log(25)/log(400) ~= 0.537 -> band ~8.
        assert!((6..=9).contains(&peak_band), "unexpected peak band {peak_band}");
        assert!(peak_val > 0.9, "expected strong level, got {peak_val}");

        // Rendering both styles produces lit keys on the WIN 68 matrix.
        let rows = active_rows(&FALLBACK_WIN68_MATRIX);
        let cols = active_cols(&FALLBACK_WIN68_MATRIX);
        let leds = unique_leds(&FALLBACK_WIN68_MATRIX);
        let rainbow = analyzer.render_colors(MusicStyle::Rainbow, &rows, &cols, &FALLBACK_WIN68_MATRIX, &leds);
        let meter = analyzer.render_colors(MusicStyle::Meter, &rows, &cols, &FALLBACK_WIN68_MATRIX, &leds);
        assert!(rainbow.values().any(|&rgb| rgb != [0, 0, 0]));
        assert!(meter.values().any(|&rgb| rgb != [0, 0, 0]));
    }
}

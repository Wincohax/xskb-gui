//! Background worker thread owning all HID I/O, PipeWire audio capture, and
//! frame pacing so the GPUI UI thread never blocks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::audio::{MusicStyle, PwCapture, SpectrumAnalyzer};
use crate::hid::{DeviceProbe, HidDevice, backup_path, find_hidraw};
use crate::protocol::{
    FALLBACK_WIN68_MATRIX, MATRIX_COLS, MATRIX_ROWS, active_cols, active_rows, unique_leds,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveMode {
    Stock,
    Solid,
    Music,
}

impl ActiveMode {
    pub fn label(self) -> &'static str {
        match self {
            ActiveMode::Stock => "Stock / Restored",
            ActiveMode::Solid => "Solid Colour",
            ActiveMode::Music => "Music Reactive",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SharedState {
    pub generation: u64,
    pub connected: bool,
    pub hidraw_path: String,
    pub product_name: String,
    pub firmware_version: String,
    pub app_info: String,
    pub mode_name: String,
    pub backup_location: String,
    pub backup_exists: bool,
    pub matrix: [[u8; MATRIX_COLS]; MATRIX_ROWS],
    pub active_mode: ActiveMode,
    pub key_colors: HashMap<u8, [u8; 3]>,
    pub band_levels: Vec<f32>,
    pub status_message: String,
    pub error_message: Option<String>,
}

impl Default for SharedState {
    fn default() -> Self {
        let backup_loc = backup_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "~/.config/xskb/lighting-backup.bin".to_string());
        let backup_exists = backup_path().map(|p| p.exists()).unwrap_or(false);
        Self {
            generation: 0,
            connected: false,
            hidraw_path: "Not connected".to_string(),
            product_name: "Spark/AULA WIN 68 HE MAX".to_string(),
            firmware_version: "—".to_string(),
            app_info: "—".to_string(),
            mode_name: "—".to_string(),
            backup_location: backup_loc,
            backup_exists,
            matrix: FALLBACK_WIN68_MATRIX,
            active_mode: ActiveMode::Stock,
            key_colors: HashMap::new(),
            band_levels: vec![0.0; 15],
            status_message: "Searching for keyboard...".to_string(),
            error_message: None,
        }
    }
}

impl SharedState {
    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn apply_probe(&mut self, probe: &DeviceProbe) {
        self.connected = true;
        self.hidraw_path = probe.hidraw_path.display().to_string();
        self.product_name = probe.product_name.clone();
        self.firmware_version = probe.firmware_version.clone();
        self.app_info = probe.app_info.clone();
        self.mode_name = probe.mode_name.clone();
        self.matrix = probe.matrix;
        self.backup_exists = backup_path().map(|p| p.exists()).unwrap_or(false);
        self.error_message = None;
        self.bump();
    }
}

#[derive(Debug, Clone)]
pub enum WorkerCmd {
    Reconnect,
    SetSolid {
        rgb: [u8; 3],
        brightness: f32,
        /// When true, resend 3 bursts like `xskb.py solid` (used on mode entry / explicit Apply).
        burst: bool,
    },
    StartMusic {
        style: MusicStyle,
        fps: u32,
        sensitivity: f32,
    },
    UpdateMusic {
        style: MusicStyle,
        fps: u32,
        sensitivity: f32,
    },
    RestoreStock,
    Shutdown,
}

pub struct WorkerHandle {
    tx: Sender<WorkerCmd>,
    pub state: Arc<Mutex<SharedState>>,
    shutdown_flag: Arc<AtomicBool>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl WorkerHandle {
    pub fn spawn() -> Arc<Self> {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(SharedState::default()));
        let shutdown_flag = Arc::new(AtomicBool::new(false));

        let state_bg = Arc::clone(&state);
        let flag_bg = Arc::clone(&shutdown_flag);

        let handle = thread::Builder::new()
            .name("xskb-worker".into())
            .spawn(move || run_worker(rx, state_bg, flag_bg))
            .expect("failed to spawn xskb-worker thread");

        Arc::new(Self {
            tx,
            state,
            shutdown_flag,
            join: Mutex::new(Some(handle)),
        })
    }

    pub fn send(&self, cmd: WorkerCmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn snapshot(&self) -> SharedState {
        self.state.lock().unwrap().clone()
    }

    /// Ask the worker to stop audio capture, restore stock lighting, and exit, then wait for it.
    pub fn shutdown_and_wait(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        let _ = self.tx.send(WorkerCmd::Shutdown);
        if let Some(handle) = self.join.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

struct MusicSession {
    capture: PwCapture,
    analyzer: SpectrumAnalyzer,
    style: MusicStyle,
    fps: u32,
    sensitivity: f32,
    rows: Vec<usize>,
    cols: Vec<usize>,
    leds: Vec<u8>,
    next_tick: Instant,
    sample_scratch: Vec<i16>,
}

struct WorkerCtx {
    dev: Option<HidDevice>,
    matrix: [[u8; MATRIX_COLS]; MATRIX_ROWS],
    host_mode_active: bool,
    music: Option<MusicSession>,
    state: Arc<Mutex<SharedState>>,
}

impl WorkerCtx {
    fn connect(&mut self) {
        self.music = None;
        match find_hidraw().and_then(HidDevice::open) {
            Ok(mut dev) => match dev.startup_probe() {
                Ok(probe) => {
                    self.matrix = probe.matrix;
                    self.dev = Some(dev);
                    let mut st = self.state.lock().unwrap();
                    st.apply_probe(&probe);
                    st.status_message = format!(
                        "Connected to {} on {}",
                        probe.product_name,
                        probe.hidraw_path.display()
                    );
                }
                Err(err) => {
                    self.dev = None;
                    let mut st = self.state.lock().unwrap();
                    st.connected = false;
                    st.hidraw_path = "Not connected".into();
                    st.error_message = Some(format!("Probe failed: {err}"));
                    st.status_message = "Device found, but startup probe failed".into();
                    st.bump();
                }
            },
            Err(err) => {
                self.dev = None;
                let mut st = self.state.lock().unwrap();
                st.connected = false;
                st.hidraw_path = "Not connected".into();
                st.error_message = Some(err.to_string());
                st.status_message = "No supported keyboard detected".into();
                st.bump();
            }
        }
    }

    fn ensure_host_mode(&mut self) -> bool {
        let Some(dev) = self.dev.as_mut() else {
            let mut st = self.state.lock().unwrap();
            st.error_message = Some("Keyboard is not connected".into());
            st.bump();
            return false;
        };
        if self.host_mode_active {
            return true;
        }
        {
            let mut st = self.state.lock().unwrap();
            st.status_message = "Backing up stock config and switching to host mode...".into();
            st.error_message = None;
            st.bump();
        }
        match dev.enter_host_mode() {
            Ok(_) => {
                self.host_mode_active = true;
                let mut st = self.state.lock().unwrap();
                st.backup_exists = backup_path().map(|p| p.exists()).unwrap_or(false);
                st.bump();
                true
            }
            Err(err) => {
                let mut st = self.state.lock().unwrap();
                st.error_message = Some(format!("Host mode switch failed: {err}"));
                st.status_message = "Could not switch keyboard to host mode".into();
                st.bump();
                false
            }
        }
    }

    fn apply_solid(&mut self, rgb: [u8; 3], brightness: f32, burst: bool) {
        self.music = None;
        let b = brightness.clamp(0.0, 1.0);
        let scaled = [
            (rgb[0] as f32 * b).round() as u8,
            (rgb[1] as f32 * b).round() as u8,
            (rgb[2] as f32 * b).round() as u8,
        ];
        let leds = unique_leds(&self.matrix);
        let pairs: Vec<(u8, [u8; 3])> = leds.iter().map(|&idx| (idx, scaled)).collect();

        // Always update the live preview immediately.
        {
            let mut st = self.state.lock().unwrap();
            st.active_mode = ActiveMode::Solid;
            st.key_colors = pairs.iter().copied().collect();
            st.band_levels.fill(0.0);
            st.bump();
        }

        if !self.ensure_host_mode() {
            return;
        }

        let Some(dev) = self.dev.as_mut() else {
            return;
        };

        let repeats = if burst { 3 } else { 1 };
        let gap = if burst {
            Duration::from_millis(2)
        } else {
            Duration::from_millis(1)
        };

        for r in 0..repeats {
            if let Err(err) = dev.send_frame(&pairs, gap) {
                let mut st = self.state.lock().unwrap();
                st.error_message = Some(format!("HID write failed: {err}"));
                st.connected = false;
                st.bump();
                self.dev = None;
                self.host_mode_active = false;
                return;
            }
            if burst && r + 1 < repeats {
                thread::sleep(Duration::from_millis(30));
            }
        }

        let mut st = self.state.lock().unwrap();
        st.error_message = None;
        st.status_message = format!(
            "Solid #{:02X}{:02X}{:02X} ({}% brightness, {} LEDs)",
            scaled[0],
            scaled[1],
            scaled[2],
            (b * 100.0).round() as u32,
            leds.len()
        );
        st.bump();
    }

    fn start_music(&mut self, style: MusicStyle, fps: u32, sensitivity: f32) {
        if !self.ensure_host_mode() {
            return;
        }
        let rows = active_rows(&self.matrix);
        let cols = active_cols(&self.matrix);
        let leds = unique_leds(&self.matrix);
        let capture = match PwCapture::start() {
            Ok(c) => c,
            Err(err) => {
                let mut st = self.state.lock().unwrap();
                st.error_message = Some(err.to_string());
                st.status_message = "Could not start PipeWire audio capture".into();
                st.bump();
                return;
            }
        };

        let fps = fps.clamp(10, 120);
        let analyzer = SpectrumAnalyzer::new(cols.len().max(1));
        self.music = Some(MusicSession {
            capture,
            analyzer,
            style,
            fps,
            sensitivity,
            rows,
            cols,
            leds,
            next_tick: Instant::now(),
            sample_scratch: Vec::with_capacity(2048),
        });

        let mut st = self.state.lock().unwrap();
        st.active_mode = ActiveMode::Music;
        st.error_message = None;
        st.status_message = format!(
            "Music mode ({}, {fps} FPS, sensitivity {:.2}x) via PipeWire",
            style.label(),
            sensitivity
        );
        st.bump();
    }

    fn update_music(&mut self, style: MusicStyle, fps: u32, sensitivity: f32) {
        if let Some(m) = self.music.as_mut() {
            m.style = style;
            m.fps = fps.clamp(10, 120);
            m.sensitivity = sensitivity;
            let mut st = self.state.lock().unwrap();
            st.status_message = format!(
                "Music mode ({}, {} FPS, sensitivity {:.2}x) via PipeWire",
                m.style.label(),
                m.fps,
                m.sensitivity
            );
            st.bump();
        }
    }

    fn step_music(&mut self) {
        let Some(m) = self.music.as_mut() else {
            return;
        };

        if let Err(err) = m.capture.read_samples(&mut m.sample_scratch) {
            self.music = None;
            let mut st = self.state.lock().unwrap();
            st.active_mode = ActiveMode::Stock;
            st.error_message = Some(err.to_string());
            st.status_message = "Music capture stopped".into();
            st.bump();
            return;
        }

        m.analyzer.push_samples(&m.sample_scratch);
        let levels = m.analyzer.step(m.sensitivity).to_vec();
        let color_map = m
            .analyzer
            .render_colors(m.style, &m.rows, &m.cols, &self.matrix, &m.leds);
        let pairs: Vec<(u8, [u8; 3])> = m
            .leds
            .iter()
            .map(|&idx| (idx, *color_map.get(&idx).unwrap_or(&[0, 0, 0])))
            .collect();

        if let Some(dev) = self.dev.as_mut() {
            if let Err(err) = dev.send_frame(&pairs, Duration::ZERO) {
                self.music = None;
                self.dev = None;
                self.host_mode_active = false;
                let mut st = self.state.lock().unwrap();
                st.connected = false;
                st.active_mode = ActiveMode::Stock;
                st.error_message = Some(format!("HID stream error: {err}"));
                st.status_message = "Lost connection to keyboard".into();
                st.bump();
                return;
            }
        }

        let mut st = self.state.lock().unwrap();
        st.key_colors = color_map;
        st.band_levels = levels;
        st.bump();

        let period = Duration::from_secs_f64(1.0 / m.fps as f64);
        m.next_tick += period;
        let now = Instant::now();
        if m.next_tick < now {
            m.next_tick = now;
        }
    }

    fn restore_stock(&mut self, is_shutdown: bool) {
        self.music = None;
        if let Some(dev) = self.dev.as_mut() {
            // On explicit RestoreStock button always restore; on Shutdown restore if we touched host mode.
            if !is_shutdown || self.host_mode_active {
                match dev.restore_stock() {
                    Ok(()) => {
                        self.host_mode_active = false;
                        let mut st = self.state.lock().unwrap();
                        st.active_mode = ActiveMode::Stock;
                        st.key_colors.clear();
                        st.band_levels.fill(0.0);
                        st.error_message = None;
                        st.status_message = "Restored stock keyboard lighting".into();
                        st.bump();
                    }
                    Err(err) => {
                        let mut st = self.state.lock().unwrap();
                        st.error_message = Some(format!("Restore failed: {err}"));
                        st.status_message = "Failed to restore stock lighting".into();
                        st.bump();
                    }
                }
            }
        } else if !is_shutdown {
            let mut st = self.state.lock().unwrap();
            st.active_mode = ActiveMode::Stock;
            st.key_colors.clear();
            st.band_levels.fill(0.0);
            st.error_message = Some("Keyboard is not connected".into());
            st.bump();
        }
    }
}

fn run_worker(rx: Receiver<WorkerCmd>, state: Arc<Mutex<SharedState>>, shutdown_flag: Arc<AtomicBool>) {
    let mut ctx = WorkerCtx {
        dev: None,
        matrix: FALLBACK_WIN68_MATRIX,
        host_mode_active: false,
        music: None,
        state,
    };

    ctx.connect();

    loop {
        if shutdown_flag.load(Ordering::SeqCst) {
            ctx.restore_stock(true);
            break;
        }

        // Pull next command (blocking when idle, timed when streaming music).
        let first_cmd = if let Some(m) = ctx.music.as_ref() {
            let wait = m.next_tick.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(cmd) => Some(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    ctx.restore_stock(true);
                    break;
                }
            }
        } else {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(cmd) => Some(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    ctx.restore_stock(true);
                    break;
                }
            }
        };

        // Drain bursty slider updates so we always act on the newest value immediately.
        let mut pending_cmds = Vec::new();
        if let Some(cmd) = first_cmd {
            pending_cmds.push(cmd);
        }
        loop {
            match rx.try_recv() {
                Ok(cmd) => pending_cmds.push(cmd),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    pending_cmds.push(WorkerCmd::Shutdown);
                    break;
                }
            }
        }

        // Coalesce consecutive SetSolid / UpdateMusic messages.
        let mut i = 0;
        let mut should_exit = false;
        while i < pending_cmds.len() {
            let cmd = pending_cmds[i].clone();
            match cmd {
                WorkerCmd::SetSolid { mut rgb, mut brightness, mut burst } => {
                    while i + 1 < pending_cmds.len() {
                        if let WorkerCmd::SetSolid {
                            rgb: r2,
                            brightness: b2,
                            burst: bu2,
                        } = pending_cmds[i + 1]
                        {
                            rgb = r2;
                            brightness = b2;
                            burst |= bu2;
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    ctx.apply_solid(rgb, brightness, burst);
                }
                WorkerCmd::UpdateMusic { mut style, mut fps, mut sensitivity } => {
                    while i + 1 < pending_cmds.len() {
                        if let WorkerCmd::UpdateMusic {
                            style: s2,
                            fps: f2,
                            sensitivity: se2,
                        } = pending_cmds[i + 1]
                        {
                            style = s2;
                            fps = f2;
                            sensitivity = se2;
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    ctx.update_music(style, fps, sensitivity);
                }
                WorkerCmd::StartMusic { style, fps, sensitivity } => {
                    ctx.start_music(style, fps, sensitivity);
                }
                WorkerCmd::RestoreStock => {
                    ctx.restore_stock(false);
                }
                WorkerCmd::Reconnect => {
                    ctx.connect();
                }
                WorkerCmd::Shutdown => {
                    ctx.restore_stock(true);
                    should_exit = true;
                    break;
                }
            }
            i += 1;
        }

        if should_exit {
            break;
        }

        if ctx.music.is_some() {
            ctx.step_music();
        }
    }
}

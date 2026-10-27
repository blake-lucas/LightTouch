//! A running device: the worker process for one session, its events, its frames and its performance numbers.
use crate::Waker;
use lighttouch_core::frames::{FrameMeta, FrameReader, segment_name};
use lighttouch_core::network::NetworkConfig;
use lighttouch_core::protocol::{Command, Event, NetworkArgs, NetworkStatus, StatusEvent, WorkerArgs};
use lighttouch_core::runtime::{BoardInfo, Contact, State};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Where a session is, as the UI shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// Checking the device's files before boot.
    Verifying {
        done: u64,
        total: u64,
    },
    Starting,
    Running,
    Paused,
    /// iOS is shutting down.
    Stopping,
    /// The worker has exited.
    Ended {
        clean: bool,
        dirty: bool,
        message: String,
    },
}

impl Phase {
    /// Booted or booting: anything but ended.
    pub fn active(&self) -> bool {
        !matches!(self, Self::Ended { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Verifying { .. } | Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Stopping => "Shutting down",
            Self::Ended { clean: true, .. } => "Off",
            Self::Ended { .. } => "Stopped unexpectedly",
        }
    }
}

pub struct Session {
    pub record_id: String,
    pub name: String,
    pub board: Option<BoardInfo>,
    pub phase: Phase,
    pub status: Option<StatusEvent>,
    /// The last command error or rejected input, for a notice.
    pub last_error: Option<String>,
    pub log_path: PathBuf,
    pub started_at: Instant,
    /// The guest's usbmux socket for app services, once the USB link is up.
    pub usb_socket: Option<PathBuf>,
    /// Why the USB link isn't available.
    pub usb_error: Option<String>,
    /// The session's networking as the worker reports it (`None` until it does, or when this build has none).
    pub network: Option<NetworkStatus>,
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Event>,
    frames: FrameReader,
    bgra: Vec<u8>,
    last_sequence: u64,
    pub stats: FrameStats,
    /// When each frame was published and shown, for the frame inspector.
    pub timeline: crate::inspector::Timeline,
    /// The detailed timing capture running now: its folder and when it began.
    capture: Option<(PathBuf, Instant)>,
    /// A pause was asked for (the worker confirms it with its next status).
    pause_requested: bool,
    tilt: crate::tilt::Tilt,
    /// The inspector's pause waits for the guest tools to name the app in front (they answer only while iOS runs):
    /// when it was asked.
    frontmost_asked: Option<Instant>,
    /// The app in front as the guest tools last named it (bundle id, name).
    pub frontmost: Option<(String, String)>,
    /// The overlay's detailed rows from the running capture, refreshed about once a second.
    pub live_timing: Option<crate::inspector::LiveTiming>,
    live_read: Option<Instant>,
    /// A profile recorded on request (Settings > Diagnostics): its folder and when it started.
    pub recording: Option<(PathBuf, Instant)>,
    last_motion: Option<Instant>,
    stop_requested: Option<Instant>,
}

pub struct StartOptions {
    pub library: PathBuf,
    pub record_id: String,
    pub name: String,
    pub engine: PathBuf,
    pub recover: bool,
    pub audio: bool,
    pub headphones: bool,
    pub muted: bool,
    pub log_dir: PathBuf,
    pub extra_env: Vec<(String, String)>,
    pub usb: Option<lighttouch_core::protocol::UsbArgs>,
    pub network: Option<NetworkArgs>,
}

impl Session {
    pub fn start(options: StartOptions, waker: Waker) -> Result<Self, String> {
        if !options.engine.is_file() {
            return Err(format!(
                "The emulator engine is missing ({}). Reinstall LightTouch or set LIGHTTOUCH_ENGINE.",
                options.engine.display()
            ));
        }
        let frames = FrameReader::create(&segment_name()).map_err(|e| format!("Couldn't set up the screen: {e}"))?;
        let args = WorkerArgs {
            library: options.library,
            record: options.record_id.clone(),
            engine: options.engine,
            frames: frames.name().to_string(),
            recover: options.recover,
            audio: options.audio,
            headphones: options.headphones,
            usb: options.usb.clone(),
            network: options.network.clone(),
        };
        std::fs::create_dir_all(&options.log_dir).ok();
        prune_logs(&options.log_dir, 10);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        let log_path = options
            .log_dir
            .join(format!("session-{stamp}-{}.log", &options.record_id[..8.min(options.record_id.len())]));
        let log = std::fs::File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
        let exe = lighttouch_core::paths::worker_exe()?;
        let mut command = std::process::Command::new(exe);
        command
            .arg("worker")
            .arg(serde_json::to_string(&args).unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        for (key, value) in &options.extra_env {
            command.env(key, value);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|e| format!("Couldn't start the device: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("session-events".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(event) = Event::parse(&line) {
                        if tx.send(event).is_err() {
                            break;
                        }
                        waker();
                    }
                }
                waker();
            })
            .map_err(|e| e.to_string())?;
        let mut session = Self {
            record_id: options.record_id,
            name: options.name,
            board: None,
            phase: Phase::Verifying { done: 0, total: 0 },
            status: None,
            last_error: None,
            log_path,
            started_at: Instant::now(),
            usb_socket: None,
            usb_error: None,
            network: None,
            child,
            stdin,
            events: rx,
            frames,
            bgra: Vec::new(),
            last_sequence: 0,
            stats: FrameStats::default(),
            timeline: Default::default(),
            capture: None,
            pause_requested: false,
            tilt: Default::default(),
            recording: None,
            live_timing: None,
            live_read: None,
            frontmost_asked: None,
            frontmost: None,
            last_motion: None,
            stop_requested: None,
        };
        if options.muted {
            session.send(&Command::Mute { muted: true });
        }
        Ok(session)
    }

    fn send(&mut self, command: &Command) {
        if let Some(stdin) = &mut self.stdin {
            if stdin.write_all(command.to_line().as_bytes()).and_then(|_| stdin.flush()).is_err() {
                self.stdin = None;
            }
        }
    }

    /// Apply pending worker events. Returns true when something changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    changed = true;
                    self.apply(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.phase.active() {
                        // The worker died without saying goodbye.
                        let _ = self.child.wait();
                        self.phase = Phase::Ended { clean: false, dirty: true, message: crash_message(&self.log_path) };
                        changed = true;
                    }
                    break;
                }
            }
        }
        changed
    }

    fn apply(&mut self, event: Event) {
        match event {
            Event::Admission { done, total } => self.phase = Phase::Verifying { done, total },
            Event::Board { info } => self.board = Some(info),
            Event::Started => self.phase = Phase::Starting,
            Event::Usb { socket, error } => {
                self.usb_socket = socket;
                self.usb_error = error;
            }
            Event::Status(status) => {
                self.stats.cpu(status.cpu_ns);
                self.status = Some(status);
                if self.phase.active() {
                    self.phase = match status.state {
                        State::Starting => Phase::Starting,
                        State::Running if self.stop_requested.is_some() => Phase::Stopping,
                        State::Running => Phase::Running,
                        State::Paused => Phase::Paused,
                        State::Stopping => Phase::Stopping,
                        State::Stopped | State::Failed => self.phase.clone(),
                    };
                }
            }
            Event::Frame { sequence, published_ns } => {
                self.stats.frame(sequence);
                if published_ns > 0 {
                    self.timeline.published(sequence, published_ns);
                }
            }
            Event::Error { command, message } => self.last_error = Some(format!("{command}: {message}")),
            Event::InputRejected { message } => self.last_error = Some(message),
            Event::GuestResult { state, status, output, .. } if self.frontmost_asked.is_some() => {
                let lines: Vec<&str> = output.trim_end().lines().collect();
                if state == 2 && status == 0 && lines.len() >= 2 && !lines[1].trim().is_empty() {
                    self.frontmost = Some((lines[0].trim().to_string(), lines[1].trim().to_string()));
                }
                self.pause_after_frontmost();
            }
            Event::Network(status) => self.network = Some(status),
            Event::GuestResult { .. } | Event::Screenshot { .. } => {}
            Event::Exit { clean, dirty, message } => {
                let _ = self.child.wait();
                self.phase = Phase::Ended { clean, dirty, message };
            }
        }
    }

    /// Copy the newest frame, converted to tightly packed RGBA, into `rgba` if it is newer than the last one read.
    pub fn read_frame(&mut self, rgba: &mut Vec<u8>) -> Option<FrameMeta> {
        let meta = self.frames.read(self.last_sequence, &mut self.bgra)?;
        self.last_sequence = meta.sequence;
        let (w, h, stride) = (meta.width as usize, meta.height as usize, meta.stride as usize);
        rgba.resize(w * h * 4, 0);
        for (row, out) in self.bgra.chunks(stride).take(h).zip(rgba.chunks_mut(w * 4)) {
            for (px, o) in row[..w * 4].chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                o[0] = px[2];
                o[1] = px[1];
                o[2] = px[0];
                o[3] = 255;
            }
        }
        self.stats.presented(meta);
        self.timeline.presented(meta.sequence, lighttouch_core::monotonic_ns());
        Some(meta)
    }

    pub fn touch(&mut self, contacts: Vec<Contact>) {
        if matches!(self.phase, Phase::Running) && !contacts.is_empty() {
            self.send(&Command::Touch { contacts });
        }
    }

    /// 0 Home, 1 Power, 2 Volume up, 3 Volume down.
    pub fn button(&mut self, button: u32, down: bool) {
        if matches!(self.phase, Phase::Running | Phase::Starting) {
            self.send(&Command::Button { button, down });
        }
    }

    /// Lean the device from the keyboard (tilt.rs). Called each frame while tilt controls are on: samples go out at
    /// up to 100 Hz while the guest runs, on boards with an accelerometer.
    pub fn tilt(&mut self, keys: crate::tilt::TiltKeys) {
        let Some(board) = &self.board else { return };
        if !matches!(self.phase, Phase::Running) || !board.has(BoardInfo::ACCELEROMETER) {
            return;
        }
        let gyro = board.has(BoardInfo::GYRO);
        let now = Instant::now();
        if self.last_motion.is_some_and(|t| now.duration_since(t) < Duration::from_millis(10)) {
            return;
        }
        self.last_motion = Some(now);
        let (acceleration, angular_velocity) = self.tilt.step(keys, now);
        self.send(&Command::Motion { acceleration: Some(acceleration), gyro: gyro.then_some(angular_velocity) });
    }

    /// Still leaning from the keyboard, or settling back.
    pub fn tilting(&self) -> bool {
        !self.tilt.at_rest()
    }

    pub fn cancel_input(&mut self) {
        if self.phase.active() {
            self.send(&Command::CancelInput);
        }
    }

    /// Pause for the frame inspector: first ask the guest tools which app is in front, then pause when they answer
    /// (or after 2.5 s; `kill_if_overdue` checks).
    pub fn pause_for_inspector(&mut self) {
        if !matches!(self.phase, Phase::Running) {
            return;
        }
        self.pause_requested = true;
        self.frontmost = None;
        self.frontmost_asked = Some(Instant::now());
        self.send(&Command::Guest {
            operation: lighttouch_core::sys::LT_GUEST_FRONTMOST,
            argument: String::new(),
            timeout_ms: 2000,
        });
    }

    fn pause_after_frontmost(&mut self) {
        if self.frontmost_asked.take().is_some() {
            self.set_paused(true);
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.frontmost_asked = None;
        if matches!(self.phase, Phase::Running | Phase::Paused) {
            self.pause_requested = paused;
            self.send(&Command::Pause { paused });
        }
    }

    /// Change the route, archive date or LAN access of the running session.
    pub fn set_network(&mut self, config: NetworkConfig) {
        if self.phase.active() {
            self.send(&Command::Network { config });
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.send(&Command::Mute { muted });
    }

    /// Ask iOS to shut down cleanly.
    /// Whether a shutdown has been asked for.
    pub fn stopping(&self) -> bool {
        self.stop_requested.is_some()
    }

    pub fn stop(&mut self) {
        if self.phase.active() && self.stop_requested.is_none() {
            self.stop_requested = Some(Instant::now());
            self.send(&Command::Stop);
            self.phase = Phase::Stopping;
        }
    }

    /// Save the newest guest frame as a PNG at `path`, encoded off the UI thread. Err when there's no frame yet.
    pub fn save_png(&self, path: &Path, done: impl FnOnce(Result<(), String>) + Send + 'static) -> Result<(), String> {
        let mut bgra = Vec::new();
        let meta = self.frames.read(0, &mut bgra).ok_or("no frame yet")?;
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            let (w, h, stride) = (meta.width as usize, meta.height as usize, meta.stride as usize);
            let mut rgb = Vec::with_capacity(w * h * 3);
            for row in bgra.chunks(stride).take(h) {
                for px in row[..w * 4].chunks_exact(4) {
                    rgb.extend_from_slice(&[px[2], px[1], px[0]]);
                }
            }
            let write = || -> Result<(), String> {
                let file = std::io::BufWriter::new(std::fs::File::create(&path).map_err(|e| e.to_string())?);
                let mut encoder = png::Encoder::new(file, meta.width, meta.height);
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.write_header().and_then(|mut w| w.write_image_data(&rgb)).map_err(|e| e.to_string())
            };
            done(write());
        });
        Ok(())
    }

    pub fn screenshot(&mut self, path: &Path) {
        self.send(&Command::Screenshot { path: path.to_path_buf() });
    }

    pub fn profile(&mut self, path: Option<PathBuf>) {
        self.send(&Command::Profile { path });
    }

    /// Detailed frame timing, as on Android: while `wanted` and the guest runs, an engine capture records into a new
    /// folder under `root`, restarted at a size or time limit. Only the newest two automatic captures are kept, and
    /// any the frame inspector claimed.
    pub fn keep_timing(&mut self, wanted: bool, root: &Path) {
        const MAX_BYTES: u64 = 64 << 20;
        const MAX_AGE: Duration = Duration::from_secs(600);
        let running = matches!(self.phase, Phase::Running) && !self.pause_requested;
        if let Some((dir, started)) = &self.capture {
            let size = std::fs::metadata(dir.join("events.tsv")).map_or(0, |m| m.len());
            if !wanted || !self.phase.active() || size > MAX_BYTES || started.elapsed() > MAX_AGE {
                self.profile(None);
                self.capture = None;
            }
        }
        match &self.capture {
            Some((dir, _)) if self.live_read.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) => {
                self.live_read = Some(Instant::now());
                let now = lighttouch_core::monotonic_ns();
                // Frames from the last 20 s only, so the rows clear once the guest stops drawing.
                let from = now.saturating_sub(20_000_000_000);
                let detail = crate::inspector::load_detail_tail(&dir.join("events.tsv"), from, 1 << 20);
                self.live_timing = crate::inspector::LiveTiming::of(&detail.breakdown, 2_000_000_000);
            }
            Some(_) => {}
            None => self.live_timing = None,
        }
        if wanted && running && self.capture.is_none() && self.recording.is_none() {
            let dir = root.join(format!("{:020}", lighttouch_core::monotonic_ns()));
            if std::fs::create_dir_all(&dir).is_ok() {
                self.profile(Some(dir.join("events.tsv")));
                self.capture = Some((dir.clone(), Instant::now()));
                let mut old: Vec<PathBuf> = std::fs::read_dir(root)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| *p != dir && !p.join("keep").exists())
                    .collect();
                old.sort();
                let excess = old.len().saturating_sub(1);
                for stale in old.into_iter().take(excess) {
                    let _ = std::fs::remove_dir_all(stale);
                }
            }
        }
    }

    /// Record an engine profile into a new folder under `root` until `stop_recording`, as Android's "Record a
    /// profile": the same capture detailed timing makes, so only while that is off. The folder is kept.
    pub fn start_recording(&mut self, root: &Path, stamp: &str) -> Result<(), String> {
        if !matches!(self.phase, Phase::Running) || self.capture.is_some() || self.recording.is_some() {
            return Err("the device isn't running, or a capture is already on".into());
        }
        let dir = root.join(format!("recorded {stamp}"));
        std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join("keep"), b"")).map_err(|e| e.to_string())?;
        self.profile(Some(dir.join("events.tsv")));
        self.recording = Some((dir, Instant::now()));
        Ok(())
    }

    /// Stop a requested recording; its folder.
    pub fn stop_recording(&mut self) -> Option<PathBuf> {
        let (dir, _) = self.recording.take()?;
        self.profile(None);
        Some(dir)
    }

    /// Stop the detailed timing capture and keep it (for the frame inspector); its events file.
    pub fn claim_timing(&mut self) -> Option<PathBuf> {
        let (dir, _) = self.capture.take()?;
        self.profile(None);
        let _ = std::fs::write(dir.join("keep"), b"");
        Some(dir.join("events.tsv"))
    }

    /// Whether the guest's display is off (iOS auto-locked or Sleep/Wake pressed).
    pub fn display_asleep(&self) -> bool {
        matches!(self.phase, Phase::Running) && self.status.is_some_and(|s| s.display_sleeping && s.engine_ready)
    }

    /// Last resort when the worker ignores Stop beyond the grace period: kill it. The device stays dirty.
    pub fn kill_if_overdue(&mut self) {
        if self.frontmost_asked.is_some_and(|at| at.elapsed() > Duration::from_millis(2500)) {
            self.pause_after_frontmost();
        }
        let grace = Duration::from_millis(
            (lighttouch_core::sys::LT_SHUTDOWN_AGENT_WAIT_MS + lighttouch_core::sys::LT_SHUTDOWN_GRACE_MS) as u64
                + 10_000,
        );
        if self.stop_requested.is_some_and(|at| at.elapsed() > grace) && self.phase.active() {
            let _ = self.child.kill();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.phase.active() {
            // The app is closing with iOS running: ask for a clean stop and let the worker finish on its own.
            self.send(&Command::Stop);
        }
    }
}

fn crash_message(log: &Path) -> String {
    let tail = std::fs::read_to_string(log)
        .ok()
        .and_then(|text| text.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
        .unwrap_or_default();
    if tail.is_empty() { "iOS stopped unexpectedly".into() } else { format!("iOS stopped unexpectedly: {tail}") }
}

fn prune_logs(dir: &Path, keep: usize) {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("session-")))
        .collect();
    logs.sort();
    while logs.len() >= keep {
        let _ = std::fs::remove_file(logs.remove(0));
    }
}

/// Frame timing for the performance overlay: frames published by the worker and their pacing.
#[derive(Default)]
pub struct FrameStats {
    /// Arrival times of published frames, newest last (5 s).
    arrivals: VecDeque<Instant>,
    /// Frame intervals in milliseconds, newest last (for the graph).
    pub intervals: VecDeque<f32>,
    last_cpu: Option<(u64, Instant)>,
    /// Worker CPU use, percent of one core (smoothed).
    pub cpu_percent: f32,
    /// Frame latency: worker publish to UI read, milliseconds (smoothed).
    pub latency_ms: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameSummary {
    pub fps: f32,
    pub frame_ms: f32,
    pub low_1_percent: f32,
    pub worst_ms: f32,
    pub idle: bool,
}

impl FrameStats {
    pub const GRAPH_SAMPLES: usize = 240;
    const IDLE_GAP: Duration = Duration::from_secs(1);

    fn frame(&mut self, _sequence: u64) {
        let now = Instant::now();
        if let Some(last) = self.arrivals.back() {
            let gap = now.duration_since(*last);
            if gap < Self::IDLE_GAP {
                self.intervals.push_back(gap.as_secs_f32() * 1000.0);
            } else {
                self.intervals.push_back(f32::NAN);
            }
            while self.intervals.len() > Self::GRAPH_SAMPLES {
                self.intervals.pop_front();
            }
        }
        self.arrivals.push_back(now);
        while self.arrivals.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(5)) {
            self.arrivals.pop_front();
        }
    }

    fn presented(&mut self, meta: FrameMeta) {
        if meta.published_ns > 0 {
            let latency = lighttouch_core::monotonic_ns().saturating_sub(meta.published_ns) as f32 / 1e6;
            self.latency_ms = if self.latency_ms == 0.0 { latency } else { self.latency_ms * 0.9 + latency * 0.1 };
        }
    }

    fn cpu(&mut self, cpu_ns: u64) {
        let now = Instant::now();
        if let Some((last_ns, last_at)) = self.last_cpu {
            let wall = now.duration_since(last_at).as_nanos() as f32;
            if wall > 0.0 && cpu_ns >= last_ns {
                let percent = (cpu_ns - last_ns) as f32 / wall * 100.0;
                self.cpu_percent = self.cpu_percent * 0.6 + percent * 0.4;
            }
        }
        self.last_cpu = Some((cpu_ns, now));
    }

    pub fn summary(&self) -> FrameSummary {
        let now = Instant::now();
        let Some(last) = self.arrivals.back() else { return FrameSummary { idle: true, ..Default::default() } };
        if now.duration_since(*last) > Self::IDLE_GAP {
            return FrameSummary { idle: true, ..Default::default() };
        }
        let recent: Vec<&Instant> =
            self.arrivals.iter().filter(|t| now.duration_since(**t) <= Duration::from_secs(1)).collect();
        let fps = recent.len() as f32;
        let mut intervals: Vec<f32> = self.intervals.iter().copied().filter(|v| v.is_finite()).collect();
        let take = (fps as usize).max(1).min(intervals.len());
        let frame_ms =
            if take > 0 { intervals[intervals.len() - take..].iter().sum::<f32>() / take as f32 } else { 0.0 };
        intervals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p99 = intervals.get(((intervals.len() as f32) * 0.99) as usize).copied().unwrap_or(0.0);
        FrameSummary {
            fps,
            frame_ms,
            low_1_percent: if p99 > 0.0 { 1000.0 / p99 } else { 0.0 },
            worst_ms: intervals.last().copied().unwrap_or(0.0),
            idle: false,
        }
    }
}

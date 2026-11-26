//! A running device: the worker process for one session, its events, its frames and its performance numbers.
use crate::Waker;
use lighttouch_core::frames::{FrameMeta, FrameReader, segment_name};
use lighttouch_core::network::NetworkConfig;
use lighttouch_core::protocol::{Command, Event, NetworkArgs, NetworkStatus, StatusEvent, WorkerArgs};
use lighttouch_core::runtime::{BoardInfo, Contact, State, StorageCounters};
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
    /// The logs folder, whose `engine` log (docs/LOGS.md) holds what the worker and the engine printed.
    pub log_dir: PathBuf,
    /// Copies the worker's stderr into the engine log; finishes once the worker and its children have exited.
    log_copier: Option<std::thread::JoinHandle<()>>,
    pub started_at: Instant,
    /// The guest's usbmux socket for app services, once the USB link is up.
    pub usb_socket: Option<PathBuf>,
    /// Why the USB link isn't available.
    pub usb_error: Option<String>,
    /// The session's networking as the worker reports it (`None` until it does, or when this build has none).
    pub network: Option<NetworkStatus>,
    /// The modem's state (devices with one), as the worker last reported it.
    pub modem: Option<lighttouch_core::carrier::ModemStatus>,
    /// What the modem, or the core before it, refused last (cleared by the next call or text).
    pub modem_error: Option<String>,
    /// Texts the device sent this session (number, text), oldest first.
    pub sent_texts: Vec<(String, String)>,
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
    /// The engine's periodic storage sync is held for an app install (`hold_storage_sync`).
    storage_sync_held: bool,
    tilt: crate::tilt::Tilt,
    /// The inspector's pause waits for the guest tools to name the app in front (they answer only while iOS runs):
    /// when it was asked.
    frontmost_asked: Option<Instant>,
    /// The app in front as the guest tools last named it (bundle id, name).
    pub frontmost: Option<(String, String)>,
    /// A frontmost-app question asked for controller presets (`watch_frontmost`), which doesn't pause.
    frontmost_watched: Option<Instant>,
    /// The overlay's detailed rows from the running capture, refreshed about once a second.
    pub live_timing: Option<crate::inspector::LiveTiming>,
    live_read: Option<Instant>,
    /// A profile recorded on request (Settings > Diagnostics): its folder and when it started.
    pub recording: Option<(PathBuf, Instant)>,
    last_motion: Option<Instant>,
    /// The last accelerometer sample sent: the engine holds it until the next.
    last_acceleration: Option<[f64; 3]>,
    stop_requested: Option<Instant>,
    /// Force stop: the worker was killed on purpose, so its exit is not a crash.
    forced: bool,
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
    pub microphone: bool,
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
            microphone: options.microphone,
            usb: options.usb.clone(),
            network: options.network.clone(),
        };
        // Each start begins a new engine log; the one before stays as its previous generation.
        let mut log = lighttouch_core::logs::CappedLog::start(&options.log_dir, lighttouch_core::logs::ENGINE)
            .map_err(|e| format!("Couldn't start the device's log in {}: {e}", options.log_dir.display()))?;
        let t = lighttouch_core::local_time();
        let _ = log.line(
            format!(
                "LightTouch {} ({}): {} {} ({}) at {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                env!("CARGO_PKG_VERSION"),
                lighttouch_core::COMMIT,
                if options.recover { "recovering" } else { "starting" },
                options.name,
                &options.record_id[..8.min(options.record_id.len())],
                t.year,
                t.month,
                t.day,
                t.hour,
                t.minute,
                t.second
            )
            .as_bytes(),
        );
        let exe = lighttouch_core::paths::worker_exe()?;
        let mut command = std::process::Command::new(exe);
        command
            .arg("worker")
            .arg(serde_json::to_string(&args).unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
        // Through a pipe this process reads: a worker that crashes still delivers its last lines.
        let log_copier = child
            .stderr
            .take()
            .map(|stderr| lighttouch_core::logs::copy_lines(stderr, std::sync::Arc::new(std::sync::Mutex::new(log))));
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
            log_dir: options.log_dir,
            log_copier,
            started_at: Instant::now(),
            usb_socket: None,
            usb_error: None,
            network: None,
            modem: None,
            modem_error: None,
            sent_texts: Vec::new(),
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
            storage_sync_held: false,
            tilt: Default::default(),
            recording: None,
            live_timing: None,
            live_read: None,
            frontmost_asked: None,
            frontmost: None,
            frontmost_watched: None,
            last_motion: None,
            last_acceleration: None,
            stop_requested: None,
            forced: false,
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
                        let message = if self.forced {
                            "Force stopped before iOS finished shutting down. The next start checks the device's storage \
                             first."
                                .to_string()
                        } else {
                            self.wait_for_log();
                            crash_message(&self.log_dir)
                        };
                        self.phase = Phase::Ended { clean: false, dirty: true, message };
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
                self.stats.storage(status.storage);
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
            // The modem's refusals show in the header too, wherever the call or text was asked for.
            Event::Error { command, message } if command == "modem" || command == "reload_carrier" => {
                self.last_error = Some(format!("Carrier: {message}"));
                self.modem_error = Some(message)
            }
            Event::Error { command, message } => self.last_error = Some(format!("{command}: {message}")),
            Event::Modem { status } => {
                let before = self.modem_error.clone();
                crate::carrier::take_status(&mut self.modem, &mut self.modem_error, &mut self.sent_texts, status);
                if self.modem_error != before {
                    if let Some(refusal) = &self.modem_error {
                        self.last_error = Some(format!("Carrier: {refusal}"));
                    }
                }
            }
            Event::InputRejected { message } => self.last_error = Some(message),
            Event::GuestResult { state, status, output, .. } if self.frontmost_asked.is_some() => {
                let lines: Vec<&str> = output.trim_end().lines().collect();
                if state == 2 && status == 0 && lines.len() >= 2 && !lines[1].trim().is_empty() {
                    self.frontmost = Some((lines[0].trim().to_string(), lines[1].trim().to_string()));
                }
                self.pause_after_frontmost();
            }
            Event::GuestResult { state, status, output, .. } if self.frontmost_watched.is_some() => {
                // Turned away (the guest tools aren't up yet, while iOS boots): ask again in a few seconds.
                if state != 3 {
                    self.frontmost_watched = None;
                }
                let lines: Vec<&str> = output.trim_end().lines().collect();
                if state == 2 && status == 0 && lines.len() >= 2 && !lines[1].trim().is_empty() {
                    self.frontmost = Some((lines[0].trim().to_string(), lines[1].trim().to_string()));
                }
            }
            Event::Network(status) => self.network = Some(status),
            Event::GuestResult { .. } | Event::Screenshot { .. } => {}
            Event::Exit { clean, dirty, message } => {
                let _ = self.child.wait();
                self.phase = Phase::Ended { clean, dirty, message };
            }
        }
    }

    /// Have the next `read_frame` copy the newest frame even if it was read before (a device back on screen whose
    /// guest isn't drawing shows its last picture at once).
    pub fn redraw(&mut self) {
        self.last_sequence = 0;
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

    /// Send touch contacts in order. The engine takes at most eight a batch and each contact id once (it rejects
    /// the whole batch otherwise), but a frame can hold several steps of one finger (Windows reports a touchscreen
    /// finger's moves faster than frames, a swipe's last move comes with its lift): a repeated id, or a ninth
    /// contact, starts the next batch.
    pub fn touch(&mut self, contacts: Vec<Contact>) {
        if !matches!(self.phase, Phase::Running) {
            return;
        }
        for batch in touch_batches(contacts) {
            self.send(&Command::Touch { contacts: batch });
        }
    }

    /// 0 Home, 1 Power, 2 Volume up, 3 Volume down.
    pub fn button(&mut self, button: u32, down: bool) {
        if matches!(self.phase, Phase::Running | Phase::Starting) {
            self.send(&Command::Button { button, down });
        }
    }

    /// Lean the device from the keyboard (tilt.rs), from upright as the desktop shows it (`rotation`). Called each
    /// frame while tilt controls are on: samples go out at up to 100 Hz while the guest runs, on boards with an
    /// accelerometer.
    pub fn tilt(&mut self, keys: crate::tilt::TiltKeys, rotation: crate::rotation::Rotation) {
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
        let (acceleration, angular_velocity) = self.tilt.step(keys, rotation, now);
        self.last_acceleration = Some(acceleration);
        self.send(&Command::Motion { acceleration: Some(acceleration), gyro: gyro.then_some(angular_velocity) });
    }

    /// The device at rest, upright as the desktop shows it, unless that's the last sample sent: called each frame
    /// while nothing else moves it, so starting the device or turning it on the desktop turns it for iOS too.
    pub fn rest(&mut self, rotation: crate::rotation::Rotation) {
        let Some(board) = &self.board else { return };
        if !matches!(self.phase, Phase::Running) || !board.has(BoardInfo::ACCELEROMETER) {
            return;
        }
        let acceleration = crate::tilt::rest(rotation);
        if self.last_acceleration == Some(acceleration) {
            return;
        }
        let gyro = board.has(BoardInfo::GYRO);
        self.last_motion = Some(Instant::now());
        self.last_acceleration = Some(acceleration);
        self.send(&Command::Motion { acceleration: Some(acceleration), gyro: gyro.then_some([0.0; 3]) });
    }

    /// A motion sample from elsewhere (a controller): acceleration in m/s², and the gyroscope's turn in rad/s for
    /// boards that have one, while the guest runs, on boards with an accelerometer. Every one is sent (at most one a
    /// frame): the engine holds the last it took, so a dropped one (the level a released binding sends) would leave
    /// the device leaning; the worker passes on only the newest of those queued.
    pub fn motion(&mut self, acceleration: [f64; 3], angular_velocity: [f64; 3]) {
        let Some(board) = &self.board else { return };
        if !matches!(self.phase, Phase::Running) || !board.has(BoardInfo::ACCELEROMETER) {
            return;
        }
        let gyro = board.has(BoardInfo::GYRO);
        self.last_motion = Some(Instant::now());
        self.last_acceleration = Some(acceleration);
        self.send(&Command::Motion { acceleration: Some(acceleration), gyro: gyro.then_some(angular_velocity) });
    }

    /// Ask which app is in front, without pausing (`frontmost` holds the answer). Skipped while a question is out.
    pub fn watch_frontmost(&mut self) {
        let waiting = |t: Option<Instant>| t.is_some_and(|at| at.elapsed() < Duration::from_secs(5));
        if !matches!(self.phase, Phase::Running) || waiting(self.frontmost_watched) || self.frontmost_asked.is_some() {
            return;
        }
        self.frontmost_watched = Some(Instant::now());
        self.send(&Command::Guest {
            operation: lighttouch_core::sys::LT_GUEST_FRONTMOST,
            argument: String::new(),
            timeout_ms: 2000,
        });
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

    /// The device's carrier settings were saved: the worker writes what changed to the modem.
    /// Hold the engine's periodic storage sync while apps install, release it after: the sync made the guest's writes
    /// wait (the upload went in bursts), and an install cut short is lost anyway. Sends only changes.
    pub fn hold_storage_sync(&mut self, hold: bool) {
        if hold != self.storage_sync_held && self.phase.active() {
            self.storage_sync_held = hold;
            self.send(&Command::HoldStorageSync { hold });
        }
    }

    pub fn reload_carrier(&mut self) {
        if self.phase.active() {
            self.send(&Command::ReloadCarrier);
        }
    }

    /// A call, an answer or hang-up from the other end, or a text, for the modem.
    pub fn modem(&mut self, action: lighttouch_core::carrier::ModemAction) {
        if self.phase.active() {
            self.modem_error = None;
            self.send(&Command::Modem { action });
        }
    }

    pub fn set_microphone(&mut self, on: bool) {
        self.send(&Command::Microphone { on });
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

    /// Stop is taking too long: end the worker now instead of waiting out the grace period. The device stays dirty
    /// and is checked on its next start, as after the grace period's own kill.
    pub fn force_stop(&mut self) {
        if self.phase.active() {
            self.stop_requested.get_or_insert_with(Instant::now);
            self.forced = true;
            let _ = self.child.kill();
        }
    }

    /// Last resort when the worker ignores Stop beyond the grace period: kill it. The device stays dirty.
    /// Give the engine log a moment to take the worker's last lines (it ends when the worker's stderr closes).
    fn wait_for_log(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.log_copier.as_ref().is_some_and(|c| !c.is_finished()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.log_copier.as_ref().is_some_and(|c| c.is_finished()) {
            let _ = self.log_copier.take().map(|c| c.join());
        }
    }

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

fn crash_message(log_dir: &Path) -> String {
    let newest = lighttouch_core::logs::newest_part(log_dir, lighttouch_core::logs::ENGINE.name);
    let tail = lighttouch_core::logs::read_tail(&newest, 16 << 10)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .and_then(|text| {
            text.lines().rev().find(|l| !l.trim().is_empty() && !l.starts_with("----- [log]")).map(str::to_string)
        })
        .unwrap_or_default();
    if tail.is_empty() { "iOS stopped unexpectedly".into() } else { format!("iOS stopped unexpectedly: {tail}") }
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
    /// Storage counter readings over the last second, oldest first.
    storage_samples: VecDeque<(StorageCounters, Instant)>,
    /// The guest's storage traffic over the last second; `None` from engines without the counters.
    pub storage: Option<StorageRates>,
}

/// Guest storage per second, from the engine's NAND counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct StorageRates {
    pub read_bytes: f64,
    pub read_ops: f64,
    pub write_bytes: f64,
    pub write_ops: f64,
    pub erase_ops: f64,
    /// Host time spent serving NAND operations, percent of wall time.
    pub busy_percent: f64,
    /// Host time per operation, microseconds; 0 while idle.
    pub us_per_op: f64,
}

impl StorageRates {
    pub fn between(old: &StorageCounters, new: &StorageCounters, seconds: f64) -> Self {
        let d = |a: u64, b: u64| b.saturating_sub(a) as f64;
        let ops = d(old.read_ops, new.read_ops) + d(old.write_ops, new.write_ops) + d(old.erase_ops, new.erase_ops);
        let ns = d(old.read_ns, new.read_ns) + d(old.write_ns, new.write_ns) + d(old.erase_ns, new.erase_ns);
        Self {
            read_bytes: d(old.read_bytes, new.read_bytes) / seconds,
            read_ops: d(old.read_ops, new.read_ops) / seconds,
            write_bytes: d(old.write_bytes, new.write_bytes) / seconds,
            write_ops: d(old.write_ops, new.write_ops) / seconds,
            erase_ops: d(old.erase_ops, new.erase_ops) / seconds,
            busy_percent: ns / (seconds * 1e7),
            us_per_op: if ops > 0.0 { ns / ops / 1e3 } else { 0.0 },
        }
    }
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

    fn storage(&mut self, counters: Option<StorageCounters>) {
        let Some(counters) = counters else {
            self.storage = None;
            return;
        };
        let now = Instant::now();
        self.storage_samples.push_back((counters, now));
        // Rates over the last second: steadier than one 250 ms status apart, and still live.
        while self.storage_samples.len() > 2 && now.duration_since(self.storage_samples[1].1) >= Duration::from_secs(1)
        {
            self.storage_samples.pop_front();
        }
        let (old, at) = self.storage_samples[0];
        let seconds = now.duration_since(at).as_secs_f64();
        if seconds > 0.1 {
            self.storage = Some(StorageRates::between(&old, &counters, seconds));
        }
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

/// `contacts` split, in order, into batches the engine takes: at most eight, no contact id twice.
pub fn touch_batches(contacts: Vec<Contact>) -> Vec<Vec<Contact>> {
    let mut out: Vec<Vec<Contact>> = Vec::new();
    for c in contacts {
        match out.last_mut() {
            Some(b) if b.len() < lighttouch_core::sys::LT_MAX_CONTACTS && !b.iter().any(|x| x.id == c.id) => b.push(c),
            _ => out.push(vec![c]),
        }
    }
    out
}

#[cfg(test)]
mod touch_tests {
    use super::*;

    #[test]
    fn repeated_ids_and_ninth_contacts_start_a_batch() {
        let c = |id, phase| Contact { id, phase, x: 0.5, y: 0.5 };
        // One finger moved twice and lifted in a frame, a second finger alongside.
        let ids = |b: &Vec<Contact>| b.iter().map(|c| c.id).collect::<Vec<_>>();
        let batches = touch_batches(vec![c(3, 1), c(4, 1), c(3, 1), c(3, 2), c(4, 2)]);
        assert_eq!(batches.iter().map(ids).collect::<Vec<_>>(), vec![vec![3, 4], vec![3], vec![3, 4]]);
        let many = touch_batches((0..10).map(|i| c(i, 0)).collect());
        assert_eq!(many.iter().map(Vec::len).collect::<Vec<_>>(), vec![8, 2]);
        assert!(touch_batches(Vec::new()).is_empty());
    }
}

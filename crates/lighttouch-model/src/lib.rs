//! Everything the LightTouch app does, without any UI toolkit: the device list, adding devices, running one,
//! settings and background work. A frontend renders the public state and calls the methods; `tick()` once per
//! frame applies background results. `Waker` lets background threads ask the frontend to redraw.
pub mod carrier;
pub mod crash;
pub mod diagnostics;
pub mod gamepad;
pub mod inspector;
pub mod network;
pub mod notices;
pub mod prepare;
pub mod queue;
pub mod real_size;
pub mod rotation;
pub mod session;
pub mod settings;
pub mod store;
pub mod task;
pub mod tilt;
pub mod transfer;

use lighttouch_catalog::catalog::{device_subtitle, model_name};
use lighttouch_catalog::{Bootrom, GuestTools};
use lighttouch_core::app_log;
use lighttouch_core::library::{Library, Record};
use lighttouch_core::paths::{DataDirs, Resources};
use session::{Phase, Session, StartOptions};
use settings::Settings;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use task::{Progress, Task};

/// Results a background thread hands back to `tick`.
type Channel<T> = (std::sync::mpsc::Sender<T>, std::sync::mpsc::Receiver<T>);

/// Ask the frontend to redraw (called from background threads).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A saved device as the UI shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub board: String,
    pub build: String,
    /// "iPod touch 2G", or the board when the catalog does not know it.
    pub model: String,
    /// "iPod touch 2G · iOS 3.1.3".
    pub subtitle: String,
    pub selected: bool,
    /// Not shut down cleanly last time: starting it means recovering.
    pub dirty: bool,
    /// The boot ROM it needs is missing.
    pub missing_bootrom: bool,
    pub has_guest_tools: bool,
    pub record: Record,
}

impl Device {
    fn from_record(record: Record) -> Self {
        let needs_rom = Bootrom::for_board(&record.board).is_some();
        Self {
            id: record.id.clone(),
            name: record.name.clone(),
            board: record.board.clone(),
            build: record.build.clone(),
            model: model_name(&record.board, &record.build).map(str::to_string).unwrap_or_else(|| record.board.clone()),
            subtitle: device_subtitle(&record.board, &record.build),
            selected: record.selected,
            dirty: record.dirty,
            missing_bootrom: needs_rom && record.bootrom_path.is_none(),
            has_guest_tools: record.guest_package_path.is_some(),
            record,
        }
    }
}

/// What the current background operation is.
#[derive(Debug, Clone, PartialEq)]
pub enum Activity {
    /// Preparing a device from an IPSW (the firmware id).
    Preparing(String),
    /// Exporting a device (its name) to a device file.
    Exporting { name: String, dest: PathBuf },
    /// Importing a device file.
    Importing(PathBuf),
}

/// A finished background operation, for the UI to react to (e.g. show "ready").
#[derive(Debug, Clone, PartialEq)]
pub enum Finished {
    /// `timings`: (stage, seconds) of a preparation, for its "Details".
    DeviceAdded {
        id: String,
        name: String,
        selected: bool,
        timings: Vec<(String, f64)>,
    },
    /// A device written to a device file (transfer.rs).
    DeviceExported {
        name: String,
        path: PathBuf,
        bytes: u64,
        seconds: f64,
    },
    /// A device file added as a new device.
    DeviceImported {
        id: String,
        name: String,
        selected: bool,
        seconds: f64,
    },
    Failed {
        activity: Activity,
        message: String,
    },
}

pub struct Model {
    pub dirs: DataDirs,
    pub resources: Resources,
    pub settings: Settings,
    pub devices: Vec<Device>,
    pub session: Option<Session>,
    /// A message to show briefly ("Renamed to …").
    pub notice: Option<(String, Instant)>,
    /// A problem to show until dismissed.
    pub error: Option<String>,
    /// Legacy Store and app installs for the current device.
    pub store: store::Store,
    /// Game controllers and keyboard bindings (gamepad.rs).
    pub gamepads: gamepad::Gamepads,
    library: Option<Library>,
    /// The carrier settings of the device last asked about (carrier.rs).
    carrier: carrier::Cache,
    task: Option<(Activity, Task<Finished>)>,
    finished: Option<Finished>,
    /// Screenshots being written: the folder, and whether it worked.
    screenshots: (std::sync::mpsc::Sender<Result<PathBuf, String>>, std::sync::mpsc::Receiver<Result<PathBuf, String>>),
    /// A preparation LightTouch closed during, to offer again (taken by the UI).
    pub interrupted: Option<prepare::PrepareRequest>,
    /// The GPU LightTouch's window draws with, for exported logs (set by the frontend).
    pub gpu: Option<String>,
    /// A log export in progress, and how the last one went (the zip, or why not).
    exporting: bool,
    exports: Channel<Result<PathBuf, String>>,
    /// The error last written to the app log, so each shows there once.
    logged_error: Option<String>,
    waker: Waker,
}

impl Model {
    pub fn new(dirs: DataDirs, resources: Resources, waker: Waker) -> Self {
        let settings = Settings::load(&dirs.settings());
        let store = store::Store::new(dirs.cache().join("store"), waker.clone());
        let gamepads = gamepad::Gamepads::new(
            dirs.root.join("controllers"),
            resources.dir.clone(),
            Some(dirs.logs().join("controllers.log")),
            waker.clone(),
            settings.controller_preset.as_deref(),
        );
        let mut model = Self {
            store,
            gamepads,
            dirs,
            resources,
            settings,
            devices: Vec::new(),
            session: None,
            notice: None,
            error: None,
            library: None,
            carrier: None,
            task: None,
            finished: None,
            screenshots: std::sync::mpsc::channel(),
            interrupted: None,
            gpu: None,
            exporting: false,
            exports: std::sync::mpsc::channel(),
            logged_error: None,
            waker,
        };
        remove_old_logs(&model.dirs.logs());
        model.interrupted = prepare::recover_interrupted(&model.dirs);
        match Library::open(&model.dirs.library()) {
            Ok(library) => model.library = Some(library),
            Err(e) => model.error = Some(format!("Couldn't open your devices: {e}")),
        }
        model.refresh();
        for d in &model.devices {
            app_log!(
                "device {}: {} ({} {}){}{}",
                &d.id[..8.min(d.id.len())],
                d.subtitle,
                d.board,
                d.build,
                if d.selected { ", selected" } else { "" },
                if d.dirty { ", not shut down cleanly" } else { "" }
            );
        }
        if let Some(request) = &model.interrupted {
            app_log!("a preparation of {} was interrupted when LightTouch last closed", request.firmware_id);
        }
        model
    }

    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// Apply background results. Call once per frame.
    pub fn tick(&mut self) {
        while let Ok(result) = self.screenshots.1.try_recv() {
            match result {
                Ok(dir) => self.notify(format!("Screenshot saved to {}", dir.display())),
                Err(e) => self.error = Some(format!("Couldn't save the screenshot: {e}")),
            }
        }
        let mut ended = false;
        if let Some(session) = &mut self.session {
            session.poll();
            session.kill_if_overdue();
            session.keep_timing(self.settings.detailed_timing, &self.dirs.profiles());
            if let Some(error) = session.last_error.take() {
                self.notice = Some((error, Instant::now()));
            }
            if let Phase::Ended { clean, dirty, message } = &session.phase {
                app_log!(
                    "{} ended after {:.0} s: clean={clean} dirty={dirty}{}",
                    session.name,
                    session.started_at.elapsed().as_secs_f64(),
                    if message.is_empty() { String::new() } else { format!(": {message}") }
                );
                if !*clean && !message.is_empty() {
                    self.error = Some(message.clone());
                }
                ended = true;
            }
        }
        if ended {
            // Keep the ended session around only long enough to report; the record's dirty flag shows the rest.
            self.session = None;
            self.refresh();
        }
        if let Some((activity, task)) = &mut self.task {
            if let Some(result) = task.take_result() {
                let activity = activity.clone();
                self.task = None;
                let finished = result.unwrap_or_else(|message| Finished::Failed { activity, message });
                match &finished {
                    Finished::DeviceAdded { name, timings, .. } => {
                        app_log!("preparation done: added {name} in {:.0} s", timings.last().map_or(0.0, |(_, s)| *s))
                    }
                    Finished::Failed { activity: Activity::Preparing(id), message } => {
                        app_log!("preparation of {id} failed: {message}")
                    }
                    Finished::Failed { activity, message } => app_log!("{activity:?} failed: {message}"),
                    Finished::DeviceExported { .. } | Finished::DeviceImported { .. } => {}
                }
                if let Finished::DeviceAdded { name, selected: false, .. } = &finished {
                    self.notice =
                        Some((format!("Added {name}; stop the running device to switch to it"), Instant::now()));
                }
                self.finished = Some(finished);
                self.refresh();
            }
        }
        let current = self.current().map(|d| (d.id.clone(), d.board.clone(), d.build.clone()));
        let live = self.session.as_ref().filter(|s| s.phase.active());
        let socket = live.and_then(|s| s.usb_socket.clone());
        let usb_problem = live.and_then(|s| {
            s.usb_error.clone().or_else(|| {
                (!self.resources.usbmuxd().is_file())
                    .then(|| "App installs aren't included in this build (its USB bridge is missing).".to_string())
            })
        });
        self.store.follow(
            current.as_ref().map(|(i, b, v)| (i.as_str(), b.as_str(), v.as_str())),
            socket.as_deref(),
            usb_problem.as_deref(),
        );
        self.store.tick();
        while let Ok(result) = self.exports.1.try_recv() {
            self.exporting = false;
            match result {
                Ok(path) => {
                    app_log!("logs exported");
                    self.notify(format!("Logs saved to {}", path.display()));
                }
                Err(e) => self.error = Some(e),
            }
        }
        if self.error != self.logged_error {
            if let Some(e) = &self.error {
                app_log!("error shown: {e}");
            }
            self.logged_error = self.error.clone();
        }
        if self.notice.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(4)) {
            self.notice = None;
        }
    }

    /// The last finished background operation, once.
    pub fn take_finished(&mut self) -> Option<Finished> {
        self.finished.take()
    }

    pub fn refresh(&mut self) {
        if let Some(library) = &self.library {
            match library.records() {
                Ok(records) => self.devices = records.into_iter().map(Device::from_record).collect(),
                Err(e) => self.error = Some(format!("Couldn't read your devices: {e}")),
            }
        }
    }

    pub fn selected(&self) -> Option<&Device> {
        self.devices.iter().find(|d| d.selected)
    }

    /// The device the session runs, else the selected one.
    pub fn current(&self) -> Option<&Device> {
        match &self.session {
            Some(s) => self.devices.iter().find(|d| d.id == s.record_id),
            None => self.selected(),
        }
    }

    /// The running device's emulator log (its session's) and serial console, for "why did the last app quit".
    pub fn device_logs(&self) -> crash::DeviceLogs {
        crash::DeviceLogs {
            engine: self.session.as_ref().map(|s| s.log_dir.clone()),
            serial: self.current().map(|d| d.record.state_path.join("serial.log")),
        }
    }

    pub fn session_active(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.phase.active())
    }

    pub fn busy(&self) -> Option<(&Activity, Progress)> {
        self.task.as_ref().map(|(a, t)| (a, t.progress()))
    }

    /// Whether the user cancelled the running background operation (it is winding down).
    pub fn task_cancelled(&self) -> bool {
        self.task.as_ref().is_some_and(|(_, t)| t.is_cancelled())
    }

    pub fn cancel_task(&mut self) {
        if let Some((_, task)) = &self.task {
            task.cancel();
        }
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    fn library(&self) -> Result<&Library, String> {
        self.library.as_ref().ok_or_else(|| "The device library is unavailable".to_string())
    }

    pub fn select(&mut self, id: &str) {
        if self.session_active() {
            return;
        }
        let result = self.library().and_then(|l| l.select(id).map_err(|e| e.to_string()));
        match result {
            Ok(()) => self.refresh(),
            Err(e) => self.error = Some(e),
        }
    }

    pub fn rename(&mut self, id: &str, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let result = self.library().and_then(|l| l.rename(id, &name).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                if let Some(session) = &mut self.session {
                    if session.record_id == id {
                        session.name = name.clone();
                    }
                }
                self.notify(format!("Renamed to {name}"));
                self.refresh();
            }
            Err(e) => self.error = Some(format!("Couldn't rename it: {e}")),
        }
    }

    pub fn remove(&mut self, id: &str) {
        if self.session.as_ref().is_some_and(|s| s.record_id == id) {
            return;
        }
        let result = self.library().and_then(|l| l.remove(id).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(self.dirs.pairing(id));
                let _ = std::fs::remove_dir_all(self.dirs.network().join(id));
                self.notify("Device deleted");
                self.refresh();
            }
            Err(e) => self.error = Some(format!("Couldn't delete it: {e}")),
        }
    }

    /// Start the selected device. `recover` cold-boots state that was not shut down cleanly.
    pub fn start(&mut self, recover: bool) {
        if self.session_active() {
            return;
        }
        let Some(device) = self.selected().cloned() else {
            self.error = Some("Select a device first".into());
            return;
        };
        // Hand over newer bundled guest tools; iOS installs them during this boot.
        let tools = GuestTools::new(self.resources.guest_tools());
        if let Some(pack) = tools.newer_than(device.record.guest_package_path.as_deref(), &device.board, &device.build)
        {
            if let Ok(library) = self.library() {
                match library.set_guest_package(&device.id, &pack) {
                    Ok(_) => app_log!("guest tools for {} updated to {}", device.name, pack.display()),
                    Err(e) => app_log!("guest tools update for {} failed: {e}", device.name),
                }
            }
        }
        // iOS's console starts over each boot: keep the one before, as the engine log does.
        let serial = device.record.state_path.join("serial.log");
        if serial.is_file() {
            let _ = std::fs::rename(&serial, device.record.state_path.join("serial.prev.log"));
        }
        app_log!(
            "{} {} ({} {} {})",
            if recover || device.dirty { "recovering" } else { "starting" },
            device.name,
            device.model,
            device.board,
            device.build
        );
        let options = StartOptions {
            library: self.dirs.library(),
            record_id: device.id.clone(),
            name: device.name.clone(),
            engine: self.resources.engine(),
            recover: recover || device.dirty,
            audio: true,
            headphones: self.settings.headphones,
            muted: self.settings.muted,
            microphone: self.settings.microphone,
            log_dir: self.dirs.logs(),
            // Settings → Fewer pauses when apps open: the engine's code-cache flushes as an app opens and once it
            // settles cost a pause each but are worth ~5% in games (docs/PERFORMANCE.md).
            extra_env: if self.settings.fewer_app_pauses {
                vec![("LT_FLUSH_ON_APP".into(), "off".into()), ("LT_FLUSH_SETTLE".into(), "off".into())]
            } else {
                Vec::new()
            },
            usb: usb_args(&self.dirs, &self.resources, &device.id),
            network: Some(lighttouch_core::protocol::NetworkArgs {
                config: self.settings.network.config(),
                directory: self.dirs.network().join(&device.id),
                guest_tools: self.resources.guest_tools(),
            }),
        };
        match Session::start(options, self.waker.clone()) {
            Ok(session) => {
                self.error = None;
                self.session = Some(session);
            }
            Err(e) => self.error = Some(e),
        }
        self.refresh();
    }

    /// Save LightTouch's logs as a zip at `dest` in the background (Settings › Diagnostics › Export logs, and the
    /// preparation pages' Save logs). A notice says where when it is done.
    pub fn export_logs(&mut self, dest: PathBuf) {
        if self.exporting {
            return;
        }
        let running = self
            .session
            .as_ref()
            .filter(|s| s.phase.active())
            .map(|s| (s.record_id.clone(), s.phase.label().to_string()));
        let request = diagnostics::ExportRequest {
            dirs: self.dirs.clone(),
            devices: self.devices.clone(),
            current: self.current().map(|d| d.id.clone()),
            running,
            settings: self.settings.clone(),
            gpu: self.gpu.clone(),
            guest_socket: self.store.guest_socket(),
        };
        self.exporting = true;
        self.notify("Saving the logs…");
        let (tx, waker) = (self.exports.0.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let _ = tx.send(diagnostics::export(&request, &dest).map(|_| dest));
            waker();
        });
    }

    /// Whether a log export is running.
    pub fn exporting_logs(&self) -> bool {
        self.exporting
    }

    pub fn stop(&mut self) {
        if self.store.mutating() {
            self.notify("An app is being installed; stop the device once it finishes.");
            return;
        }
        if let Some(session) = &mut self.session {
            session.stop();
        }
    }

    pub fn force_stop(&mut self) {
        if let Some(session) = &mut self.session {
            session.force_stop();
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        if let Some(session) = &mut self.session {
            session.set_paused(paused);
        }
    }

    /// Pause the guest and freeze its last two minutes of frames for the frame inspector. The flag says whether this
    /// paused it, so closing the inspector resumes it.
    pub fn inspect(&mut self) -> Option<(inspector::Snapshot, bool)> {
        let session = self.session.as_mut()?;
        let pause = matches!(session.phase, Phase::Running);
        if pause {
            session.pause_for_inspector();
        }
        let events = session.claim_timing();
        let mut snapshot = session.timeline.snapshot(&session.name, Some(session.log_dir.clone()));
        snapshot.events = events;
        Some((snapshot, pause))
    }

    /// Save an inspector range as a report folder; returns it.
    pub fn save_frame_report(&mut self, snapshot: &inspector::Snapshot, from: u64, to: u64) -> Option<PathBuf> {
        let app = self.session.as_ref().and_then(|s| s.frontmost.as_ref()).map(|(_, name)| name.clone());
        match inspector::save_report(snapshot, app.as_deref(), from, to, &self.dirs.reports(), &chrono_like_stamp()) {
            Ok(folder) => Some(folder),
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.settings.muted = muted;
        self.save_settings();
        if let Some(session) = &mut self.session {
            session.set_muted(muted);
        }
    }

    pub fn set_microphone(&mut self, on: bool) {
        self.settings.microphone = on;
        self.save_settings();
        if let Some(session) = &mut self.session {
            session.set_microphone(on);
        }
    }

    /// Save Settings → Network and apply it to the running session, including turning networking on or off.
    pub fn set_network(&mut self, network: network::NetworkSettings) {
        if let Err(e) = network.validate(network::Day::today()) {
            self.error = Some(e);
            return;
        }
        if network == self.settings.network {
            return;
        }
        self.settings.network = network;
        self.save_settings();
        let config = self.settings.network.config();
        if let Some(session) = &mut self.session {
            session.set_network(config);
        }
    }

    pub fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.dirs.settings()) {
            self.error = Some(format!("Couldn't save settings: {e}"));
        }
    }

    /// Install `pack` (an .itpack) into the selected device, as Android's "Update guest tools": iOS installs it at
    /// the device's next start.
    pub fn update_guest_tools(&mut self, pack: &Path) {
        let Some(device) = self.selected().cloned() else { return };
        if self.session_active() {
            self.error = Some(format!("Stop {} first", device.name));
            return;
        }
        let result =
            self.library().and_then(|library| library.set_guest_package(&device.id, pack).map_err(|e| e.to_string()));
        match result {
            Ok(_) => {
                app_log!("guest tools for {} replaced with {}", device.name, pack.display());
                self.notify("Guest tools updated; they install at the next start");
                self.refresh();
            }
            Err(e) => self.error = Some(format!("Couldn't update the guest tools: {e}")),
        }
    }

    /// Start or stop recording an engine profile of the running device (Settings > Diagnostics).
    pub fn toggle_recording(&mut self) {
        let Some(session) = &mut self.session else { return };
        if let Some(dir) = session.stop_recording() {
            self.notify(format!("Profile saved to {}", dir.display()));
        } else if let Err(e) = session.start_recording(&self.dirs.profiles(), &chrono_like_stamp()) {
            self.error = Some(format!("Couldn't record a profile: {e}"));
        }
    }

    /// While a profile records: how long, and how big so far.
    pub fn recording_status(&self) -> Option<String> {
        let (dir, started) = self.session.as_ref()?.recording.as_ref()?;
        let bytes = std::fs::metadata(dir.join("events.tsv")).map_or(0, |m| m.len());
        Some(format!("Recording… {} s, {:.1} MB", started.elapsed().as_secs(), bytes as f64 / 1e6))
    }

    /// Save the current guest screen into the Pictures folder.
    pub fn screenshot(&mut self) {
        let dir = self.dirs.screenshots();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error = Some(format!("Couldn't save the screenshot: {e}"));
            return;
        }
        let stamp = chrono_like_stamp();
        let path = dir.join(format!("LightTouch {stamp}.png"));
        let Some(session) = &self.session else { return };
        let (tx, waker) = (self.screenshots.0.clone(), self.waker.clone());
        if let Err(e) = session.save_png(&path, move |result| {
            let _ = tx.send(result.map(|_| dir));
            waker();
        }) {
            self.error = Some(format!("Couldn't save the screenshot: {e}"));
        }
    }

    /// Prepare a device from an IPSW in the background.
    pub fn prepare(&mut self, request: prepare::PrepareRequest) {
        if self.task.is_some() {
            return;
        }
        let context = prepare::PrepareContext {
            dirs: self.dirs.clone(),
            resources: self.resources.clone(),
            devices: self.devices.clone(),
            select: !self.session_active(),
            waker: self.waker.clone(),
        };
        app_log!("preparing {} (jailbreak: {})", request.firmware_id, request.jailbreak);
        let activity = Activity::Preparing(request.firmware_id.clone());
        let task = Task::spawn(self.waker.clone(), move |reporter| prepare::run(context, request, reporter));
        self.task = Some((activity, task));
    }

    /// Export a stopped device to a device file at `dest` in the background (its right-click menu's Export…).
    pub fn export_device(&mut self, id: &str, dest: PathBuf) {
        if self.task.is_some() {
            return;
        }
        let Some(device) = self.devices.iter().find(|d| d.id == id).cloned() else { return };
        if self.session.as_ref().is_some_and(|s| s.record_id == id && s.phase.active()) {
            self.error = Some(format!("Stop {} before exporting it", device.name));
            return;
        }
        app_log!("exporting {} to {}", device.name, dest.display());
        let activity = Activity::Exporting { name: device.name.clone(), dest: dest.clone() };
        let library = self.dirs.library();
        let task = Task::spawn(self.waker.clone(), move |reporter| {
            transfer::export(&library, &device.id, &device.name, &dest, reporter)
        });
        self.task = Some((activity, task));
    }

    /// Add the device in a device file (another LightTouch's export) in the background.
    pub fn import_device(&mut self, source: PathBuf) {
        if self.task.is_some() {
            return;
        }
        app_log!("importing a device from {}", source.display());
        let activity = Activity::Importing(source.clone());
        let library = self.dirs.library();
        let select = !self.session_active();
        let task =
            Task::spawn(self.waker.clone(), move |reporter| transfer::import(&library, &source, select, reporter));
        self.task = Some((activity, task));
    }

    pub fn dismiss_error(&mut self) {
        self.error = None;
    }
}

/// "2026-10-24 at 21.05.33" in local-ish time without a date library: UTC is fine for file names.
pub(crate) fn chrono_like_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let (days, rem) = (secs / 86400, secs % 86400);
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if mo <= 2 { 1 } else { 0 };
    format!("{y:04}-{mo:02}-{d:02} at {h:02}.{m:02}.{s:02}")
}

/// Logs from before docs/LOGS.md: a session log per device start, and firmwarekit's own.
fn remove_old_logs(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if (name.starts_with("session-") && name.ends_with(".log")) || name == "firmwarekit.log" {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The USB link for a session of `record`, when this build has the bridge.
pub fn usb_args(dirs: &DataDirs, resources: &Resources, record: &str) -> Option<lighttouch_core::protocol::UsbArgs> {
    let daemon = resources.usbmuxd();
    let info = lighttouch_core::library::Library::open(&dirs.library()).and_then(|l| l.record(record)).ok();
    daemon.is_file().then(|| lighttouch_core::protocol::UsbArgs {
        daemon,
        pairing: dirs.pairing(record),
        log: dirs.logs().join(format!("usbmuxd-{}.log", &record[..8.min(record.len())])),
        app_pairs: info.as_ref().is_some_and(|r| r.board == "n42ap"),
        pairing_seed: info.as_ref().map(|r| r.base_path.join("pairing")).filter(|p| p.is_dir()),
    })
}

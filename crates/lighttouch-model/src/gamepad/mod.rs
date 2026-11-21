//! Game controllers and keyboard bindings: presets of what each input does (`preset`), the step that turns inputs
//! into touches, buttons and motion (`mapper`), and the controllers themselves (`sdl`). The UI shows and edits the
//! presets; this runs the active one every frame.
#[cfg(target_os = "linux")]
pub mod linux_access;
pub mod mapper;
pub mod preset;
pub mod sdl;

use crate::session::Session;
use mapper::{Inputs, Mapper, Merged, View};
use preset::Preset;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Gamepads {
    shared: Arc<Mutex<sdl::Pads>>,
    started: bool,
    resources: std::path::PathBuf,
    log: Option<std::path::PathBuf>,
    waker: crate::Waker,
    store: preset::Store,
    pub presets: Vec<Preset>,
    /// The preset in use (an index into `presets`); `None` turns bindings off.
    pub active: Option<usize>,
    pub mapper: Mapper,
    /// Moving spots around on the screen: bindings don't act meanwhile.
    pub editing: bool,
    /// The step sent motion this frame, so the arrow-key tilt stays out of its way.
    pub motion_active: bool,
    view: Option<View>,
    /// What a Motion binding is doing, as last noted in the controllers log.
    motion_note: Option<String>,
    /// The last front app a preset was picked for, and when the guest was last asked.
    app: Option<String>,
    asked: Option<Instant>,
    pub error: Option<String>,
}

impl Gamepads {
    /// `log`: where the controller thread notes what SDL and each controller report.
    pub fn new(
        dir: std::path::PathBuf,
        resources: std::path::PathBuf,
        log: Option<std::path::PathBuf>,
        waker: crate::Waker,
        active: Option<&str>,
    ) -> Self {
        // Nothing is set up until the player makes a preset: no spots or pointer on the screen out of the box.
        let store = preset::Store::new(dir);
        let presets = store.load_all();
        let active = active.and_then(|name| presets.iter().position(|p| p.name == name));
        let mut gamepads = Self {
            shared: Arc::default(),
            started: false,
            resources,
            log,
            waker,
            store,
            presets,
            active,
            mapper: Mapper::default(),
            editing: false,
            motion_active: false,
            view: None,
            motion_note: None,
            app: None,
            asked: None,
            error: None,
        };
        if gamepads.active.is_some() {
            gamepads.start();
        }
        gamepads
    }

    /// Load SDL and start reading controllers (once; the controller window and an active preset call it).
    pub fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let waker = self.waker.clone();
        sdl::start(self.resources.clone(), self.log.clone(), self.shared.clone(), Arc::new(move || waker()));
    }

    pub fn pads(&self) -> sdl::Pads {
        self.shared.lock().unwrap().clone()
    }

    pub fn active_preset(&self) -> Option<&Preset> {
        self.active.and_then(|i| self.presets.get(i))
    }

    /// The preset in use has a look binding the mouse steers: a click on the screen locks the mouse to the game.
    pub fn mouse_look(&self) -> bool {
        self.active_preset()
            .is_some_and(|p| p.bindings.iter().any(|b| matches!(b.action, preset::Action::Look { mouse: true, .. })))
    }

    /// The input that, held, frees the locked mouse to touch the screen (the mouse-look binding's).
    pub fn mouse_free(&self) -> Option<preset::Input> {
        self.active_preset()?.bindings.iter().find_map(|b| match &b.action {
            preset::Action::Look { mouse: true, free, .. } => free.clone(),
            _ => None,
        })
    }

    /// The name of the preset in use, for settings.
    pub fn active_name(&self) -> Option<String> {
        self.active_preset().map(|p| p.name.clone())
    }

    /// Switch presets (or turn bindings off), lifting whatever the old one held.
    pub fn set_active(&mut self, index: Option<usize>, session: Option<&mut Session>) {
        self.release(session);
        if index != self.active {
            self.mapper.forget_tilt_level();
        }
        self.active = index.filter(|&i| i < self.presets.len());
        if self.active.is_some() {
            self.start();
        }
    }

    /// Save preset `index` after an edit.
    pub fn save(&mut self, index: usize) {
        if let Some(p) = self.presets.get(index) {
            if let Err(e) = self.store.save(p) {
                self.error = Some(format!("Couldn't save the preset: {e}"));
            }
        }
    }

    /// A new preset (a copy of `from` if given), saved and made active. Names are kept unique.
    pub fn add(&mut self, from: Option<Preset>, session: Option<&mut Session>) -> usize {
        let mut p = from.unwrap_or_else(|| Preset::new("New preset"));
        p.name = self.unique_name(&p.name);
        self.presets.push(p);
        let index = self.presets.len() - 1;
        self.save(index);
        self.set_active(Some(index), session);
        index
    }

    pub fn unique_name(&self, base: &str) -> String {
        let base = base.trim();
        let base = if base.is_empty() { "Preset" } else { base };
        let taken = |n: &str| self.presets.iter().any(|p| p.name.eq_ignore_ascii_case(n));
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap()
    }

    pub fn rename(&mut self, index: usize, name: &str) {
        let name = name.trim();
        if name.is_empty() || self.presets.get(index).is_none_or(|p| p.name == name) {
            return;
        }
        let name = self.unique_name(name);
        let old = std::mem::replace(&mut self.presets[index].name, name);
        self.store.remove(&old);
        self.save(index);
    }

    pub fn remove(&mut self, index: usize, session: Option<&mut Session>) {
        if index >= self.presets.len() {
            return;
        }
        if self.active == Some(index) {
            self.set_active(None, session);
        }
        let p = self.presets.remove(index);
        self.store.remove(&p.name);
        self.active = self.active.map(|a| if a > index { a - 1 } else { a });
    }

    /// The preset in use was edited: its fingers may no longer belong to anything.
    pub fn changed(&mut self, session: Option<&mut Session>) {
        // Fingers go; the device's lean stays (a removed Motion binding levels on the next step).
        let view = self.view.unwrap_or(View { rotation: Default::default(), turned: false, aspect: 1.0 });
        let out = self.mapper.release_fingers(self.active.and_then(|i| self.presets.get(i)), &view);
        if let Some(session) = session {
            send(session, out);
        }
        if let Some(i) = self.active {
            self.save(i);
        }
    }

    /// The bindings move the device, or hold it where a controller's sensors left it: the keyboard's tilt and the
    /// resting pose keep off.
    pub fn holds_motion(&self) -> bool {
        self.motion_active || self.mapper.holds_motion()
    }

    /// Lift every finger and button the bindings hold.
    pub fn release(&mut self, session: Option<&mut Session>) {
        let view = self.view.unwrap_or(View { rotation: Default::default(), turned: false, aspect: 1.0 });
        let out = self.mapper.release(self.active.and_then(|i| self.presets.get(i)), &view);
        if let Some(session) = session {
            send(session, out);
        }
        self.motion_active = false;
    }

    /// One frame: read the controllers, `keys` and the locked mouse's movement since the last (`mouse`), and act on
    /// the device. `listening` is false while the window is in the background (or the device's screen isn't
    /// showing). Returns true when another step is due soon.
    pub fn step(
        &mut self,
        keys: &HashSet<String>,
        mouse: (f32, f32),
        view: View,
        listening: bool,
        session: Option<&mut Session>,
    ) -> bool {
        let Some(session) = session else {
            self.mapper = Mapper::default();
            return false;
        };
        let running = matches!(session.phase, crate::session::Phase::Running);
        if self.view.is_some_and(|v| v != view) {
            // Turned or resized: fingers held at the old place would jump.
            self.release(Some(&mut *session));
        }
        self.view = Some(view);
        self.follow_app(session);
        let pads = self.shared.lock().unwrap().pads.clone();
        self.note_motion(&pads, keys, listening, running, session);
        let Some(index) = self.active.filter(|_| listening && running) else {
            self.release(Some(session));
            return false;
        };
        let mut merged = Merged::from(&pads);
        merged.mouse = mouse;
        if self.editing {
            // The mouse places the bindings; the controller's motion still reaches a running game.
            let out =
                self.mapper.step_motion(&self.presets[index], &Inputs { pad: &merged, keys }, &view, Instant::now());
            self.motion_active = out.motion.is_some();
            let busy = out.busy;
            send(session, out);
            return busy;
        }
        let out = self.mapper.step(&self.presets[index], &Inputs { pad: &merged, keys }, &view, Instant::now());
        self.motion_active = out.motion.is_some();
        let held = merged.buttons != 0 || merged.axes.iter().any(|a| a.abs() > 0.05) || !keys.is_empty();
        let busy = out.busy || held;
        send(session, out);
        busy
    }

    /// Note in the controllers log what the active preset's Motion binding is doing, when that changes: whether the
    /// controller's motion reaches the device, and if not, why.
    fn note_motion(
        &mut self,
        pads: &[sdl::Pad],
        keys: &HashSet<String>,
        listening: bool,
        running: bool,
        session: &Session,
    ) {
        let binding = self
            .active
            .and_then(|i| self.presets.get(i))
            .and_then(|p| p.bindings.iter().find(|b| matches!(b.action, preset::Action::Motion { .. })));
        let note = binding.filter(|_| running).map(|binding| {
            let sensing = pads.iter().find(|p| p.accel.is_some());
            let merged = Merged::from(pads);
            if !session.board.as_ref().is_some_and(|b| b.has(lighttouch_core::runtime::BoardInfo::ACCELEROMETER)) {
                "not sent: this device has no accelerometer".to_string()
            } else if !listening {
                "not sent: LightTouch isn't in front (\"In the background\" is off)".to_string()
            } else if let Some(pad) = sensing {
                let held = binding.with.as_ref().is_none_or(|w| (Inputs { pad: &merged, keys }).held(w));
                if held {
                    let gyro = if pad.gyro.is_some() { "with" } else { "without" };
                    format!("sent from {} ({gyro} its gyroscope)", pad.name)
                } else {
                    "not sent: waiting for its hold button".to_string()
                }
            } else {
                let pads: Vec<String> = pads.iter().map(|p| format!("{} {:?}", p.name, p.sensors)).collect();
                format!("not sent: no controller reports its accelerometer ({})", pads.join(", "))
            }
        });
        if note != self.motion_note {
            if let (Some(n), Some(path)) = (&note, &self.log) {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
                    let _ = writeln!(f, "Motion binding: {n}");
                }
            }
            self.motion_note = note;
        }
    }

    /// Switch to the preset linked to the app in front, every few seconds, when any preset names apps.
    fn follow_app(&mut self, session: &mut Session) {
        if !self.presets.iter().any(|p| !p.apps.is_empty()) {
            return;
        }
        if self.asked.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
            self.asked = Some(Instant::now());
            session.watch_frontmost();
        }
        let Some((bundle, _)) = session.frontmost.clone() else { return };
        if self.app.as_deref() == Some(bundle.as_str()) {
            return;
        }
        self.app = Some(bundle.clone());
        if let Some(i) = self.presets.iter().position(|p| p.apps.iter().any(|a| a == &bundle)) {
            if self.active != Some(i) {
                self.set_active(Some(i), Some(session));
            }
        }
    }

    /// The app in front, if the guest said (for linking a preset to it).
    pub fn front_app(&self, session: Option<&Session>) -> Option<(String, String)> {
        session.and_then(|s| s.frontmost.clone())
    }
}

fn send(session: &mut Session, out: mapper::Output) {
    // Session::touch splits them into batches the engine takes.
    session.touch(out.contacts);
    for (button, down) in out.buttons {
        session.button(button, down);
    }
    if let Some((acceleration, gyro)) = out.motion {
        session.motion(acceleration, gyro);
    }
}

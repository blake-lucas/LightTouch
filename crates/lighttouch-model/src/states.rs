//! Save states of the device on screen: the list the States menu shows, saving the running device, and loading a
//! state, which ends a running session without an iOS shutdown and starts a fresh one from the state (the engine
//! starts once per worker). The core checks a state against the device and the engine before anything changes
//! (Portable/src/States.cpp).
//!
//! A load is seamless: from the moment it is asked until the state's first frames are on screen the model reports
//! the device as running and `restoring` holds the state's picture, which the screen shows with the load's progress
//! instead of the session ending, a new one starting and iOS booting.
use crate::Model;
use crate::session::Phase;
use lighttouch_core::library::SaveState;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The current device's states, and a load waiting for its running session to end.
#[derive(Default)]
pub(crate) struct Cache {
    /// The record the list is of; `None` until read.
    record: Option<String>,
    list: Vec<SaveState>,
    /// A state to load into the current device, asked of its running worker (`check_state`) and then waiting for it to
    /// end (`discard`).
    pending: Option<PendingLoad>,
    restoring: Option<Restoring>,
}

/// A state being loaded into the device on screen, shown as its picture until the state runs.
#[derive(Debug, Clone)]
pub struct Restoring {
    pub state: String,
    /// The state's screen when it was saved, when it has one.
    pub thumbnail: Option<PathBuf>,
    /// When the restored device first reported running.
    ran: Option<Instant>,
    since: Instant,
}

/// How long the picture stays once the state runs, so its first frame is up before the picture goes.
const SETTLE: Duration = Duration::from_millis(350);

pub(crate) struct PendingLoad {
    record: String,
    state: String,
    /// The worker said the state would load, and was told to end.
    discarding: bool,
}

impl Model {
    /// The current device's save states, newest first.
    pub fn states(&mut self) -> &[SaveState] {
        let current = self.current().map(|d| d.id.clone());
        if self.states.record != current {
            self.states.record = current.clone();
            self.states.list = match (current, self.library.as_ref()) {
                (Some(id), Some(library)) => library.states(&id).unwrap_or_else(|e| {
                    lighttouch_core::app_log!("couldn't list the save states of {}: {e}", &id[..8.min(id.len())]);
                    Vec::new()
                }),
                _ => Vec::new(),
            };
        }
        &self.states.list
    }

    /// Read the list again (after a save, a delete, a rename).
    fn reload_states(&mut self) {
        self.states.record = None;
    }

    /// Whether the running device can be saved now.
    pub fn can_save_state(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| matches!(s.phase, Phase::Running | Phase::Paused) && s.state_job.is_none())
    }

    /// Whether a state could be loaded now: the device is off, or running (not starting, stopping or saving).
    pub fn can_load_state(&self) -> bool {
        self.states.pending.is_none()
            && match &self.session {
                None => self.selected().is_some(),
                Some(s) => matches!(s.phase, Phase::Running | Phase::Paused) && s.state_job.is_none(),
            }
    }

    /// Save the running device as a new state (`None`: named after the date and time).
    pub fn save_state(&mut self, name: Option<String>) {
        if !self.can_save_state() {
            return;
        }
        if let Some(session) = self.session.as_mut() {
            lighttouch_core::app_log!("saving a state of {}", session.name);
            session.save_state(name.filter(|n| !n.trim().is_empty()));
        }
    }

    /// Load a state into the current device: at once when it is off, else once its running session has agreed the
    /// state would load and ended without an iOS shutdown (what the guest did since is replaced by the state).
    pub fn load_state(&mut self, state: &str) {
        if !self.can_load_state() {
            return;
        }
        let thumbnail = self.states().iter().find(|s| s.id == state).and_then(|s| s.thumbnail.clone());
        self.states.restoring =
            Some(Restoring { state: state.to_string(), thumbnail, ran: None, since: Instant::now() });
        match &mut self.session {
            Some(session) => {
                lighttouch_core::app_log!(
                    "loading state {} into running {}",
                    &state[..8.min(state.len())],
                    session.name
                );
                session.check_state(state);
                self.states.pending = Some(PendingLoad {
                    record: session.record_id.clone(),
                    state: state.to_string(),
                    discarding: false,
                });
            }
            None => self.start_from_state(state),
        }
    }

    pub fn delete_state(&mut self, state: &str) {
        let Some(id) = self.current().map(|d| d.id.clone()) else { return };
        match self.library().and_then(|l| l.delete_state(&id, state).map_err(|e| e.to_string())) {
            Ok(()) => self.notify("Save state deleted"),
            Err(e) => self.error = Some(format!("Couldn't delete the save state: {e}")),
        }
        self.reload_states();
    }

    pub fn rename_state(&mut self, state: &str, name: &str) {
        let Some(id) = self.current().map(|d| d.id.clone()) else { return };
        if let Err(e) = self.library().and_then(|l| l.rename_state(&id, state, name.trim()).map_err(|e| e.to_string()))
        {
            self.error = Some(format!("Couldn't rename the save state: {e}"));
        }
        self.reload_states();
    }

    /// A save or load under way on the device on screen, for its progress.
    pub fn state_job(&self) -> Option<&crate::session::StateJob> {
        self.session.as_ref().and_then(|s| s.state_job.as_ref())
    }

    /// Whether a load waits for the running session to end.
    pub fn state_load_pending(&self) -> bool {
        self.states.pending.is_some()
    }

    /// The state being loaded into the device on screen, until it runs.
    pub fn restoring(&self) -> Option<&Restoring> {
        self.states.restoring.as_ref()
    }

    /// The device's phase as the user sees it: running throughout a load.
    pub fn shown_phase(&self) -> Option<Phase> {
        if self.states.restoring.is_some() {
            return Some(Phase::Running);
        }
        self.session.as_ref().map(|s| s.phase.clone())
    }

    /// Follow a load until the state runs (or fails: its message then shows as any start's would).
    fn tick_restoring(&mut self) {
        let Some(restoring) = &mut self.states.restoring else { return };
        if self.states.pending.is_some() {
            return; // the running session is still ending
        }
        match self.session.as_ref().map(|s| (&s.phase, s.state_job.is_some())) {
            Some((Phase::Running, false)) => {
                let ran = *restoring.ran.get_or_insert_with(Instant::now);
                if ran.elapsed() >= SETTLE {
                    lighttouch_core::app_log!(
                        "restored state {} after {:.1} s",
                        &restoring.state[..8.min(restoring.state.len())],
                        restoring.since.elapsed().as_secs_f64()
                    );
                    self.states.restoring = None;
                }
            }
            Some((Phase::Ended { .. }, _)) | None => self.states.restoring = None,
            _ => {}
        }
    }

    /// Follow saves and loads (called from `tick` before a session that ended is dropped).
    pub(crate) fn tick_states(&mut self) {
        self.tick_restoring();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if let Some(state) = session.state_saved.take() {
            lighttouch_core::app_log!("saved state {} ({} bytes)", &state.id[..8.min(state.id.len())], state.bytes);
            self.notice = Some((format!("Saved \u{201c}{}\u{201d}", state.name), std::time::Instant::now()));
            self.reload_states();
        }
        let Some(session) = self.session.as_mut() else { return };
        if let Some(message) = session.state_failed.take() {
            self.error = Some(message);
        }
        let Some(pending) = &mut self.states.pending else { return };
        if session.record_id != pending.record {
            self.states.pending = None;
            return;
        }
        if !pending.discarding {
            match session.state_checked.take() {
                Some((id, None)) if id == pending.state => {
                    pending.discarding = true;
                    session.discard();
                }
                Some((id, Some(problem))) if id == pending.state => {
                    self.states.pending = None;
                    self.states.restoring = None;
                    self.error = Some(problem);
                }
                _ => {}
            }
        }
    }

    /// After the session ended: a load that waited for it starts now. Returns true when it took over the ending.
    pub(crate) fn state_session_ended(&mut self) -> bool {
        let discarded = self.session.as_ref().is_some_and(|s| s.discarded);
        let Some(pending) = self.states.pending.take() else { return discarded };
        if pending.discarding {
            self.session = None;
            self.refresh();
            if self.selected().is_some_and(|d| d.id == pending.record) {
                self.start_from_state(&pending.state);
            }
            return true;
        }
        discarded
    }

    fn start_from_state(&mut self, state: &str) {
        self.start_with(true, Some(state.to_string()));
    }
}

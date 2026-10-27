//! The Legacy Store's install queue, with no UI or guest in it (a port of the Android InstallQueue). Items are
//! fetched one at a time and installed one at a time, in the order they were added; a fetch may overlap the install
//! before it, but only one fetched file waits at once (an IPA can be hundreds of megabytes). `spawn` starts a step's
//! thread; tests run steps by hand. `on_change` fires after every change, outside the queue's lock.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueState {
    Pending,
    Downloading,
    Ready,
    Installing,
    Done,
    Failed,
}

/// One app in the install queue, as the store shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueItem {
    pub id: u64,
    pub key: String,
    pub name: String,
    pub state: QueueState,
    pub phase: String,
    pub progress: Option<f32>,
    pub error: Option<String>,
    /// iOS has started installing it: from here it can't be cancelled, and the device shouldn't stop.
    pub committed: bool,
    /// The whole item's progress (see `stages`); it only moves forward.
    pub overall: f32,
    /// Items added while others were still going share a batch; one added to an idle queue starts the next.
    pub batch: u64,
}

impl QueueItem {
    /// Still has work to do.
    pub fn active(&self) -> bool {
        !matches!(self.state, QueueState::Done | QueueState::Failed)
    }

    /// Until iOS starts installing it, copying the app onto the device can still stop.
    pub fn cancellable(&self) -> bool {
        self.active() && !self.committed
    }
}

/// How an item's `overall` splits over its stages, so one bar can follow it from the first downloaded byte to
/// iOS's last step without starting over at each: download, copy onto the device, then iOS's own install.
pub mod stages {
    pub const COPY: f32 = 0.4; // downloaded
    pub const IOS: f32 = 0.7; // on the device; iOS installs from here
    pub const UNPACKED: f32 = 0.95; // iOS's unpacking is most of its time; its remaining steps share the rest

    pub fn download(fraction: f32) -> f32 {
        COPY * fraction
    }
    pub fn copy(fraction: f32) -> f32 {
        COPY + (IOS - COPY) * fraction
    }
    pub fn unpack(fraction: f32) -> f32 {
        IOS + (UNPACKED - IOS) * fraction
    }
    /// iOS's own install percentage; unpacking ends at 20.
    pub fn ios_percent(percent: u64) -> f32 {
        UNPACKED + (1.0 - UNPACKED) * ((percent as f32 - 20.0) / 80.0).clamp(0.0, 1.0)
    }
}

/// How far along the running batch is, 0 to 1: its finished items (installed or failed) count whole. Items from
/// earlier batches, still listed until cleared, don't count, so a new install starts the ring at nothing.
pub fn fraction(items: &[QueueItem]) -> f32 {
    let Some(batch) = items.iter().filter(|i| i.active()).map(|i| i.batch).max() else { return 0.0 };
    let current: Vec<&QueueItem> = items.iter().filter(|i| i.batch == batch).collect();
    let sum: f32 = current.iter().map(|i| if i.active() { i.overall.clamp(0.0, 1.0) } else { 1.0 }).sum();
    sum / current.len() as f32
}

/// What a running step reports back to its item.
pub trait Report {
    fn update(&self, phase: &str, fraction: Option<f32>);
    fn rename(&self, name: &str);
    /// The whole item's progress (see `stages`).
    fn overall(&self, fraction: f32);
    /// iOS has started installing: from here the install can't be cancelled.
    fn commit(&self);
}

/// The two steps of an install. Errors are messages fit to show the user.
pub trait Work<P>: Send + Sync + 'static {
    /// Download (or copy) the app's file; stop with an error once `cancelled` returns true.
    fn fetch(&self, payload: &P, report: &dyn Report, cancelled: &dyn Fn() -> bool) -> Result<PathBuf, String>;
    /// Install the fetched file; stop with an error once `cancelled` returns true, until `Report::commit`.
    fn install(
        &self,
        payload: &P,
        file: &PathBuf,
        report: &dyn Report,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), String>;
    /// The fetched file is no longer needed (installed, failed or cancelled).
    fn discard(&self, file: &PathBuf);
}

struct Entry<P> {
    item: QueueItem,
    payload: Arc<P>,
    file: Option<PathBuf>,
    cancelled: Arc<AtomicBool>,
}

struct State<P> {
    entries: Vec<Entry<P>>,
    next_id: u64,
    batch: u64,
    fetching: bool,
    installing: bool,
}

impl<P> State<P> {
    /// The batch an item added now joins: the running one, or a new one when nothing is left to do.
    fn next_batch(&mut self) -> u64 {
        if !self.entries.iter().any(|e| e.item.active()) {
            self.batch += 1;
        }
        self.batch
    }
}

type Spawn = dyn Fn(&'static str, Box<dyn FnOnce() + Send>) + Send + Sync;

pub struct InstallQueue<P: Send + Sync + 'static> {
    state: Arc<Mutex<State<P>>>,
    work: Arc<dyn Work<P>>,
    spawn: Arc<Spawn>,
    on_change: Arc<dyn Fn() + Send + Sync>,
}

impl<P: Send + Sync + 'static> Clone for InstallQueue<P> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            work: self.work.clone(),
            spawn: self.spawn.clone(),
            on_change: self.on_change.clone(),
        }
    }
}

const INSTALLED: &str = "Installed. Find it on the Home screen.";

impl<P: Send + Sync + 'static> InstallQueue<P> {
    /// Steps run on their own threads.
    pub fn new(work: impl Work<P>, on_change: impl Fn() + Send + Sync + 'static) -> Self {
        Self::with_spawn(work, on_change, |name, body| {
            let _ = std::thread::Builder::new().name(name.into()).spawn(body);
        })
    }

    pub fn with_spawn(
        work: impl Work<P>,
        on_change: impl Fn() + Send + Sync + 'static,
        spawn: impl Fn(&'static str, Box<dyn FnOnce() + Send>) + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                entries: Vec::new(),
                next_id: 1,
                batch: 0,
                fetching: false,
                installing: false,
            })),
            work: Arc::new(work),
            spawn: Arc::new(spawn),
            on_change: Arc::new(on_change),
        }
    }

    pub fn snapshot(&self) -> Vec<QueueItem> {
        self.state.lock().unwrap().entries.iter().map(|e| e.item.clone()).collect()
    }

    pub fn active(&self) -> bool {
        self.state.lock().unwrap().entries.iter().any(|e| e.item.active())
    }

    /// False when the same app is already waiting or running; a finished attempt at it is replaced.
    pub fn enqueue(&self, key: &str, name: &str, payload: P) -> bool {
        {
            let mut state = self.state.lock().unwrap();
            if state.entries.iter().any(|e| e.item.key == key && e.item.active()) {
                return false;
            }
            state.entries.retain(|e| e.item.key != key);
            let batch = state.next_batch();
            let id = state.next_id;
            state.next_id += 1;
            state.entries.push(Entry {
                item: QueueItem {
                    id,
                    key: key.into(),
                    name: name.into(),
                    state: QueueState::Pending,
                    phase: "Waiting".into(),
                    progress: None,
                    error: None,
                    committed: false,
                    overall: 0.0,
                    batch,
                },
                payload: Arc::new(payload),
                file: None,
                cancelled: Arc::new(AtomicBool::new(false)),
            });
        }
        self.changed();
        true
    }

    /// Pending and fetched items leave the queue; a download or a copy onto the device stops and leaves. Once iOS
    /// is installing it can't be taken back.
    pub fn cancel(&self, id: u64) {
        let mut discard = None;
        {
            let mut state = self.state.lock().unwrap();
            let Some(index) = state.entries.iter().position(|e| e.item.id == id) else { return };
            let entry = &mut state.entries[index];
            match entry.item.state {
                QueueState::Pending => {
                    state.entries.remove(index);
                }
                QueueState::Ready => {
                    discard = entry.file.take();
                    state.entries.remove(index);
                }
                QueueState::Downloading => {
                    entry.cancelled.store(true, Ordering::SeqCst);
                    entry.item.phase = "Cancelling".into();
                }
                QueueState::Installing if !entry.item.committed => {
                    entry.cancelled.store(true, Ordering::SeqCst);
                    entry.item.phase = "Cancelling".into();
                }
                _ => {}
            }
        }
        if let Some(file) = discard {
            self.work.discard(&file);
        }
        self.changed();
    }

    pub fn retry(&self, id: u64) {
        {
            let mut state = self.state.lock().unwrap();
            let Some(index) = state.entries.iter().position(|e| e.item.id == id && e.item.state == QueueState::Failed)
            else {
                return;
            };
            // Back of the line, like any newly added app.
            let batch = state.next_batch();
            let mut entry = state.entries.remove(index);
            entry.cancelled.store(false, Ordering::SeqCst);
            entry.item = QueueItem {
                state: QueueState::Pending,
                phase: "Waiting".into(),
                progress: None,
                error: None,
                committed: false,
                overall: 0.0,
                batch,
                ..entry.item
            };
            state.entries.push(entry);
        }
        self.changed();
    }

    pub fn dismiss(&self, id: u64) {
        self.state.lock().unwrap().entries.retain(|e| e.item.id != id || e.item.active());
        self.changed();
    }

    pub fn clear_finished(&self) {
        self.state.lock().unwrap().entries.retain(|e| e.item.active());
        self.changed();
    }

    fn changed(&self) {
        (self.on_change)();
        self.pump();
    }

    fn update(&self, id: u64, change: impl FnOnce(&mut QueueItem)) {
        if let Some(entry) = self.state.lock().unwrap().entries.iter_mut().find(|e| e.item.id == id) {
            change(&mut entry.item);
        }
        (self.on_change)();
    }

    /// Start whichever steps can run now.
    fn pump(&self) {
        let (mut fetch, mut install) = (None, None);
        {
            let mut state = self.state.lock().unwrap();
            if !state.installing {
                if let Some(entry) = state.entries.iter_mut().find(|e| e.item.state == QueueState::Ready) {
                    entry.item.state = QueueState::Installing;
                    entry.item.phase = "Installing".into();
                    entry.item.progress = None;
                    install = Some(entry.item.id);
                }
                if install.is_some() {
                    state.installing = true;
                }
            }
            // One fetched file waits at most; the install before it may still be running.
            if !state.fetching && !state.entries.iter().any(|e| e.item.state == QueueState::Ready) {
                if let Some(entry) = state.entries.iter_mut().find(|e| e.item.state == QueueState::Pending) {
                    entry.item.state = QueueState::Downloading;
                    entry.item.phase = "Downloading".into();
                    entry.item.progress = Some(0.0);
                    fetch = Some(entry.item.id);
                }
                if fetch.is_some() {
                    state.fetching = true;
                }
            }
        }
        if fetch.is_some() || install.is_some() {
            (self.on_change)();
        }
        if let Some(id) = install {
            let queue = self.clone();
            (self.spawn)("store-install", Box::new(move || queue.run_install(id)));
        }
        if let Some(id) = fetch {
            let queue = self.clone();
            (self.spawn)("store-download", Box::new(move || queue.run_fetch(id)));
        }
    }

    fn entry_parts(&self, id: u64) -> Option<(Arc<P>, Arc<AtomicBool>)> {
        let state = self.state.lock().unwrap();
        state.entries.iter().find(|e| e.item.id == id).map(|e| (e.payload.clone(), e.cancelled.clone()))
    }

    fn run_fetch(&self, id: u64) {
        let Some((payload, cancelled)) = self.entry_parts(id) else { return };
        let report = Reporter { queue: self.clone(), id };
        let result = self.work.fetch(&payload, &report, &|| cancelled.load(Ordering::SeqCst));
        let mut discard = None;
        {
            let mut state = self.state.lock().unwrap();
            state.fetching = false;
            if let Some(index) = state.entries.iter().position(|e| e.item.id == id) {
                if cancelled.load(Ordering::SeqCst) {
                    discard = result.ok();
                    state.entries.remove(index);
                } else {
                    let entry = &mut state.entries[index];
                    match result {
                        Ok(file) => {
                            entry.file = Some(file);
                            entry.item.state = QueueState::Ready;
                            entry.item.phase = "Waiting to install".into();
                            entry.item.progress = None;
                            entry.item.overall = entry.item.overall.max(stages::COPY);
                        }
                        Err(error) => fail(&mut entry.item, error),
                    }
                }
            } else {
                discard = result.ok();
            }
        }
        if let Some(file) = discard {
            self.work.discard(&file);
        }
        self.changed();
    }

    fn run_install(&self, id: u64) {
        let Some((payload, cancelled)) = self.entry_parts(id) else { return };
        let file = {
            let mut state = self.state.lock().unwrap();
            state.entries.iter_mut().find(|e| e.item.id == id).and_then(|e| e.file.take())
        };
        let committed = {
            let state = self.state.clone();
            move || state.lock().unwrap().entries.iter().find(|e| e.item.id == id).is_some_and(|e| e.item.committed)
        };
        let result = match &file {
            Some(file) => {
                let report = Reporter { queue: self.clone(), id };
                self.work.install(&payload, file, &report, &|| cancelled.load(Ordering::SeqCst) && !committed())
            }
            None => Err("The downloaded app went missing.".into()),
        };
        if let Some(file) = &file {
            self.work.discard(file);
        }
        {
            let mut state = self.state.lock().unwrap();
            state.installing = false;
            if let Some(index) = state.entries.iter().position(|e| e.item.id == id) {
                let entry = &mut state.entries[index];
                if cancelled.load(Ordering::SeqCst) && !entry.item.committed {
                    state.entries.remove(index);
                } else {
                    match result {
                        Ok(()) => {
                            entry.item.state = QueueState::Done;
                            entry.item.phase = INSTALLED.into();
                            entry.item.progress = Some(1.0);
                            entry.item.overall = 1.0;
                        }
                        Err(error) => fail(&mut entry.item, error),
                    }
                }
            }
        }
        self.changed();
    }
}

fn fail(item: &mut QueueItem, error: String) {
    item.state = QueueState::Failed;
    item.phase.clear();
    item.progress = None;
    item.error = Some(error);
}

struct Reporter<P: Send + Sync + 'static> {
    queue: InstallQueue<P>,
    id: u64,
}

impl<P: Send + Sync + 'static> Report for Reporter<P> {
    fn update(&self, phase: &str, fraction: Option<f32>) {
        self.queue.update(self.id, |item| {
            item.phase = phase.into();
            item.progress = fraction;
        });
    }
    fn rename(&self, name: &str) {
        self.queue.update(self.id, |item| item.name = name.into());
    }
    fn overall(&self, fraction: f32) {
        self.queue.update(self.id, |item| item.overall = item.overall.max(fraction.clamp(0.0, 1.0)));
    }
    fn commit(&self) {
        self.queue.update(self.id, |item| {
            item.committed = true;
            item.overall = item.overall.max(stages::IOS);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    type Tasks = Arc<Mutex<Vec<(&'static str, Box<dyn FnOnce() + Send>)>>>;

    /// Steps wait in `tasks` until the test runs them, so each state in between can be looked at.
    #[derive(Default)]
    struct Log {
        fetched: Vec<String>,
        installed: Vec<String>,
        discarded: Vec<PathBuf>,
        failures: HashSet<String>,
        ignore_cancel: bool,
    }

    struct TestWork(Arc<Mutex<Log>>);

    impl Work<String> for TestWork {
        fn fetch(&self, payload: &String, _: &dyn Report, cancelled: &dyn Fn() -> bool) -> Result<PathBuf, String> {
            let mut log = self.0.lock().unwrap();
            if !log.ignore_cancel && cancelled() {
                return Err("Download cancelled.".into());
            }
            if log.failures.contains(&format!("fetch:{payload}")) {
                return Err("No connection".into());
            }
            log.fetched.push(payload.clone());
            Ok(PathBuf::from(format!("/tmp/{payload}/App.ipa")))
        }
        fn install(
            &self,
            payload: &String,
            _: &PathBuf,
            report: &dyn Report,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<(), String> {
            let commit = self.0.lock().unwrap().failures.contains(&format!("commit:{payload}"));
            if commit {
                report.commit();
            }
            if cancelled() {
                return Err("Cancelled".into());
            }
            let mut log = self.0.lock().unwrap();
            if log.failures.contains(&format!("install:{payload}")) {
                return Err("Install failed".into());
            }
            log.installed.push(payload.clone());
            Ok(())
        }
        fn discard(&self, file: &PathBuf) {
            self.0.lock().unwrap().discarded.push(file.clone());
        }
    }

    struct Harness {
        queue: InstallQueue<String>,
        tasks: Tasks,
        log: Arc<Mutex<Log>>,
    }

    impl Harness {
        fn new() -> Self {
            let tasks: Tasks = Arc::default();
            let log: Arc<Mutex<Log>> = Arc::default();
            let pending = tasks.clone();
            let queue = InstallQueue::with_spawn(
                TestWork(log.clone()),
                || {},
                move |name, body| pending.lock().unwrap().push((name, body)),
            );
            Self { queue, tasks, log }
        }
        fn run(&self, name: &str) {
            let body = {
                let mut tasks = self.tasks.lock().unwrap();
                let index = tasks.iter().position(|(n, _)| *n == name).expect("no such task");
                tasks.remove(index).1
            };
            body();
        }
        fn run_all(&self) {
            loop {
                let next = self.tasks.lock().unwrap().first().map(|(n, _)| *n);
                match next {
                    Some(name) => self.run(name),
                    None => break,
                }
            }
        }
        fn item(&self, key: &str) -> QueueItem {
            self.queue.snapshot().into_iter().find(|i| i.key == key).unwrap()
        }
        fn state(&self, key: &str) -> QueueState {
            self.item(key).state
        }
        fn add(&self, keys: &[&str]) {
            for k in keys {
                self.queue.enqueue(k, k, k.to_string());
            }
        }
        fn keys(&self) -> Vec<String> {
            self.queue.snapshot().into_iter().map(|i| i.key).collect()
        }
        fn installed(&self) -> Vec<String> {
            self.log.lock().unwrap().installed.clone()
        }
        fn discarded(&self) -> Vec<PathBuf> {
            self.log.lock().unwrap().discarded.clone()
        }
        fn fail(&self, what: &str) {
            self.log.lock().unwrap().failures.insert(what.into());
        }
    }

    fn file(key: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/{key}/App.ipa"))
    }

    #[test]
    fn one_app_goes_through_download_and_install() {
        let h = Harness::new();
        h.add(&["a"]);
        assert_eq!(h.state("a"), QueueState::Downloading);
        h.run("store-download");
        assert_eq!(h.state("a"), QueueState::Installing); // the installer was free
        h.run("store-install");
        assert_eq!(h.state("a"), QueueState::Done);
        assert_eq!(h.installed(), ["a"]);
        assert_eq!(h.discarded(), [file("a")]);
        assert!(!h.queue.active());
    }

    #[test]
    fn copying_onto_the_device_can_be_cancelled_until_ios_installs() {
        let h = Harness::new();
        h.add(&["a"]);
        h.run("store-download");
        let a = h.item("a");
        assert!(a.cancellable());
        h.queue.cancel(a.id);
        h.run("store-install");
        assert!(h.keys().is_empty());
        assert!(h.installed().is_empty());
        assert_eq!(h.discarded(), [file("a")]);
        assert!(!h.queue.active());
    }

    #[test]
    fn a_cancel_after_ios_started_installing_is_too_late() {
        let h = Harness::new();
        h.fail("commit:a");
        h.add(&["a"]);
        h.run("store-download");
        h.queue.cancel(h.item("a").id); // before the step commits
        h.run("store-install"); // commits, then sees no cancel
        let a = h.item("a");
        assert_eq!(a.state, QueueState::Done);
        assert!(a.committed && !a.cancellable());
    }

    #[test]
    fn overall_progress_never_goes_back_across_stages() {
        let h = Harness::new();
        h.add(&["a"]);
        h.run("store-download");
        assert!((h.item("a").overall - stages::COPY).abs() < 1e-4);
        h.run("store-install");
        assert!((h.item("a").overall - 1.0).abs() < 1e-4);
    }

    #[test]
    fn download_overlaps_install_but_only_one_file_waits() {
        let h = Harness::new();
        h.add(&["a", "b", "c"]);
        h.run("store-download");
        assert_eq!((h.state("a"), h.state("b")), (QueueState::Installing, QueueState::Downloading));
        h.run("store-download");
        assert_eq!((h.state("b"), h.state("c")), (QueueState::Ready, QueueState::Pending));
        h.run("store-install");
        assert_eq!(
            (h.state("a"), h.state("b"), h.state("c")),
            (QueueState::Done, QueueState::Installing, QueueState::Downloading)
        );
        h.run("store-download");
        h.run("store-install");
        h.run("store-install");
        assert_eq!(h.installed(), ["a", "b", "c"]);
    }

    #[test]
    fn installs_run_one_at_a_time() {
        let h = Harness::new();
        h.add(&["a", "b"]);
        h.run("store-download");
        h.run("store-download");
        assert_eq!(h.tasks.lock().unwrap().iter().filter(|(n, _)| *n == "store-install").count(), 1);
    }

    #[test]
    fn cancel_removes_pending_and_stops_downloads() {
        let h = Harness::new();
        h.add(&["a", "b"]);
        h.queue.cancel(h.item("b").id);
        assert_eq!(h.keys(), ["a"]);
        h.queue.cancel(h.item("a").id); // downloading: flagged, then dropped when its step ends
        assert_eq!(h.item("a").phase, "Cancelling");
        h.run("store-download");
        assert!(h.keys().is_empty());
        assert!(h.log.lock().unwrap().fetched.is_empty());
    }

    #[test]
    fn cancelled_after_fetch_discards_the_file() {
        let h = Harness::new();
        h.log.lock().unwrap().ignore_cancel = true; // a fetch that finished as the cancel arrived hands its file back
        h.add(&["a"]);
        h.queue.cancel(h.item("a").id);
        h.run("store-download");
        assert!(h.keys().is_empty());
        assert_eq!(h.discarded(), [file("a")]);
        assert!(h.installed().is_empty());
    }

    #[test]
    fn failed_items_keep_their_reason_and_retry_at_the_back() {
        let h = Harness::new();
        h.fail("fetch:a");
        h.add(&["a", "b"]);
        h.run("store-download");
        assert_eq!(h.state("a"), QueueState::Failed);
        assert_eq!(h.item("a").error.as_deref(), Some("No connection"));
        assert_eq!(h.state("b"), QueueState::Downloading); // the next app carries on
        h.log.lock().unwrap().failures.clear();
        h.queue.retry(h.item("a").id);
        assert_eq!(h.keys(), ["b", "a"]);
        assert_eq!(h.item("a").batch, h.item("b").batch); // b was still going: same batch
        assert_eq!(h.item("a").error, None);
        h.run_all();
        assert_eq!(h.installed(), ["b", "a"]);
    }

    #[test]
    fn install_failure_frees_the_installer_and_discards_the_file() {
        let h = Harness::new();
        h.fail("install:a");
        h.add(&["a", "b"]);
        h.run("store-download");
        h.run("store-download");
        h.run("store-install");
        assert_eq!(h.state("a"), QueueState::Failed);
        assert_eq!(h.item("a").error.as_deref(), Some("Install failed"));
        assert_eq!(h.state("b"), QueueState::Installing);
        assert!(h.discarded().contains(&file("a")));
    }

    #[test]
    fn an_app_already_in_the_queue_is_not_added_again() {
        let h = Harness::new();
        assert!(h.queue.enqueue("a", "a", "a".into()));
        assert!(!h.queue.enqueue("a", "a", "a".into()));
        h.run_all();
        assert_eq!(h.state("a"), QueueState::Done);
        assert!(h.queue.enqueue("a", "a", "a".into())); // a finished one is replaced
        assert_eq!(h.keys().len(), 1);
    }

    #[test]
    fn finished_items_stay_until_dismissed() {
        let h = Harness::new();
        h.add(&["a", "b"]);
        h.run_all();
        assert_eq!(h.keys().len(), 2);
        h.queue.dismiss(h.item("a").id);
        assert_eq!(h.keys(), ["b"]);
        h.queue.clear_finished();
        assert!(h.keys().is_empty());
    }

    #[test]
    fn an_install_added_to_an_idle_queue_starts_a_new_batch() {
        let h = Harness::new();
        h.add(&["a"]);
        h.run_all();
        h.add(&["b", "c"]);
        assert_ne!(h.item("a").batch, h.item("b").batch);
        assert_eq!(h.item("b").batch, h.item("c").batch);
        // a, installed earlier, is still listed but the ring starts from nothing.
        assert_eq!(fraction(&h.queue.snapshot()), 0.0);
    }

    #[test]
    fn the_ring_follows_overall_progress() {
        let item = |state, overall| QueueItem {
            id: 1,
            key: "k".into(),
            name: "App".into(),
            state,
            phase: String::new(),
            progress: None,
            error: None,
            committed: false,
            overall,
            batch: 1,
        };
        assert_eq!(fraction(&[]), 0.0);
        assert!((fraction(&[item(QueueState::Downloading, stages::download(0.5))]) - 0.2).abs() < 1e-3);
        assert!((fraction(&[item(QueueState::Installing, stages::copy(0.5))]) - 0.55).abs() < 1e-3);
        let mixed = [
            item(QueueState::Done, 0.0),
            item(QueueState::Failed, 0.0),
            item(QueueState::Pending, 0.0),
            item(QueueState::Pending, 0.0),
        ];
        assert!((fraction(&mixed) - 0.5).abs() < 1e-3);
        // An earlier batch's finished installs don't count toward a new one.
        let earlier = QueueItem { batch: 0, ..item(QueueState::Done, 1.0) };
        assert_eq!(fraction(&[earlier.clone(), item(QueueState::Pending, 0.0)]), 0.0);
        assert_eq!(fraction(&[earlier]), 0.0);
        assert!((stages::download(1.0) - stages::copy(0.0)).abs() < 1e-4);
        assert!((stages::copy(1.0) - stages::unpack(0.0)).abs() < 1e-4);
        assert!((stages::unpack(1.0) - stages::ios_percent(20)).abs() < 1e-4);
        assert!((stages::ios_percent(100) - 1.0).abs() < 1e-4);
    }
}

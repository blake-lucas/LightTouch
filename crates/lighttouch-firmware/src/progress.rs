//! The create command's stdout contract (one JSON object a line: begin, step, progress, warning, done, error) and
//! the progress events: one per second while a step runs, plus a 1.0 when it ends. The fraction is measured where
//! the step can say (bytes hashed) and otherwise follows the step's plan: serial-log milestones of the one-shot boots,
//! and elapsed time against the expected seconds in between. It never goes back and stays below 1 until the step is
//! over (Swift's Preparer.PrepareEvent and Recipe/StepProgress.swift).
//!
//! ```ignore
//! let p = StepProgress::new(Some(work), major, emit);   // heartbeat thread until p.stop()
//! p.next(3, "Writing the NAND");                        // ends the previous step (1.0), emits the step event
//! p.measure(Some(bytes.fraction_fn()));                 // optional, for this step only
//! p.finish();                                           // the last step's 1.0
//! ```
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One line of the preparer's stdout.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `seconds`: each step's expected duration, for weighting the overall bar (omitted when empty).
    Begin { steps: usize, seconds: Vec<f64> },
    Step { index: usize, name: String },
    Progress { fraction: f64, detail: Option<String> },
    Warning(String),
    Done { lock: String },
    Error { code: String, message: String, piece: Option<String> },
}

/// A number as Foundation's JSONSerialization writes it: whole values without a fraction.
fn number(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v}") }
}

fn string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

impl Event {
    /// The event's line: keys sorted, slashes unescaped, as the Swift tool wrote them.
    pub fn json(&self) -> String {
        let fields: Vec<(&str, String)> = match self {
            Event::Begin { steps, seconds } => {
                let mut f = vec![("event", string("begin"))];
                if !seconds.is_empty() {
                    let list: Vec<String> = seconds.iter().map(|s| number(*s)).collect();
                    f.push(("seconds", format!("[{}]", list.join(","))));
                }
                f.push(("steps", steps.to_string()));
                f
            }
            Event::Step { index, name } => vec![("event", string("step")), ("index", index.to_string()), ("name", string(name))],
            Event::Progress { fraction, detail } => {
                let mut f = Vec::new();
                if let Some(d) = detail {
                    f.push(("detail", string(d)));
                }
                f.push(("event", string("progress")));
                f.push(("fraction", number(*fraction)));
                f
            }
            Event::Warning(m) => vec![("event", string("warning")), ("message", string(m))],
            Event::Done { lock } => vec![("event", string("done")), ("lock", string(lock))],
            Event::Error { code, message, piece } => {
                let mut f = vec![("code", string(code)), ("event", string("error")), ("message", string(message))];
                if let Some(p) = piece {
                    f.push(("piece", string(p)));
                }
                f
            }
        };
        let body: Vec<String> = fields.into_iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
        format!("{{{}}}", body.join(","))
    }

    /// The contract's error event for an error `create` returns.
    pub fn error(e: &crate::error::Error) -> Event {
        Event::Error { code: e.code.as_str().to_string(), message: e.message.clone(), piece: e.piece.clone() }
    }
}

pub type Emit = Arc<dyn Fn(Event) + Send + Sync>;

/// A line in a serial log under the work directory, `at` seconds into the step. `file` ending in "-" is a prefix: the
/// newest match (keybag-1.log, keybag-2.log on a retry).
#[derive(Debug, Clone)]
pub struct Milestone {
    pub file: &'static str,
    pub marker: &'static str,
    pub at: f64,
    pub text: &'static str,
}

const fn m(file: &'static str, marker: &'static str, at: f64, text: &'static str) -> Milestone {
    Milestone { file, marker, at, text }
}

/// What one step expects: its seconds on a recent Mac, what to say, and (boots) the serial-log lines it passes.
#[derive(Debug, Clone)]
pub struct StepPlan {
    pub seconds: f64,
    pub text: String,
    pub milestones: Vec<Milestone>,
}

/// The step that boots the prepared flash once so its first boot is done (it_seal halts it).
pub const SEAL_STEP: &str = "Finishing setup";

impl StepPlan {
    fn new(seconds: f64, text: &str, milestones: Vec<Milestone>) -> StepPlan {
        StepPlan { seconds, text: text.to_string(), milestones }
    }

    /// The plan of the step `name`; `major`, the firmware's iOS major version, picks the seal and keybag boots' (6.x
    /// and 7.x take several times as long). Timings: firmwarekit create of 7B500 and 8C148 on an M4 Max, the seal and
    /// keybag boots of 5.x-7.x as measured (Recipe/StepProgress.swift has the runs).
    pub fn plan(name: &str, major: u32) -> StepPlan {
        let seal = "seal.log";
        match (name, major) {
            (SEAL_STEP, 7..) => StepPlan::new(331.0, "First start: loading iOS", vec![
                m(seal, "FTL_Open", 4.0, "First start: opening the flash"),
                m(seal, "launchd[1] has started", 8.0, "First start: starting iOS"),
                m(seal, "AirPort: Link Up", 60.0, "First start: starting iOS"),
                m(seal, "SpringBoard[", 204.0, "First start: starting the Home screen"),
                m(seal, "lockdown says", 239.0, "First start: starting the Home screen"),
                m(seal, "it_seal:", 328.0, "First start: shutting down"),
            ]),
            (SEAL_STEP, 6) => StepPlan::new(128.0, "First start: loading iOS", vec![
                m(seal, "FTL_Open", 4.0, "First start: opening the flash"),
                m(seal, "launchd[1] has started", 9.0, "First start: starting iOS"),
                m(seal, "AirPort: Link Up", 42.0, "First start: starting iOS"),
                m(seal, "it_prefs:", 68.0, "First start: setting up iOS"),
                m(seal, "it_seal:", 125.0, "First start: shutting down"),
            ]),
            (SEAL_STEP, 5) => StepPlan::new(95.0, "First start: loading iOS", vec![
                m(seal, "FTL_Open", 1.0, "First start: opening the flash"),
                m(seal, "launchd[1] has started", 2.0, "First start: starting iOS"),
                m(seal, "it_prefs:", 21.0, "First start: setting up iOS"),
                m(seal, "it_seal:", 87.0, "First start: shutting down"),
            ]),
            (SEAL_STEP, _) => StepPlan::new(68.0, "First start: loading iOS", vec![
                m(seal, "CXT is not valid", 1.0, "First start: indexing the flash"),
                m(seal, "FTL_Open", 6.0, "First start: opening the flash"),
                m(seal, "launchd[1] has started", 19.0, "First start: starting iOS"),
                m(seal, "SpringBoard[", 30.0, "First start: starting the Home screen"),
                m(seal, "it_prefs:", 41.0, "First start: starting the Home screen"),
                m(seal, "it_seal:", 65.0, "First start: shutting down"),
            ]),
            ("Creating the data-protection keybag", 6) => StepPlan::new(49.0, "Encryption setup: starting", vec![
                m("keybag-", "FTL_Open", 6.0, "Encryption setup: opening the flash"),
                m("keybag-", "it_keybag:", 23.0, "Encryption setup: creating the keybag"),
            ]),
            ("Creating the data-protection keybag", _) => StepPlan::new(6.0, "Encryption setup: starting", vec![
                m("keybag-", "FTL_Open", 2.0, "Encryption setup: opening the flash"),
                m("keybag-", "it_keybag:", 4.0, "Encryption setup: creating the keybag"),
            ]),
            ("Booting the restore ramdisk", _) => StepPlan::new(40.0, "Encryption setup: starting", vec![
                m("keybag.log", "FTL_Open", 10.0, "Encryption setup: opening the flash"),
                m("keybag.log", "it_keybag:", 30.0, "Encryption setup: creating the keybag"),
            ]),
            ("Verifying the IPSW", _) => StepPlan::new(2.0, "Checking the IPSW’s checksum", vec![]),
            ("Decrypting the firmware", _) => StepPlan::new(5.0, "Decrypting the firmware", vec![]),
            ("Writing the identity and boot image", _) => StepPlan::new(1.0, "Writing the boot image", vec![]),
            ("Building the system and data volumes", _) => StepPlan::new(12.0, "Building the system and data volumes", vec![]),
            ("Writing the NAND", _) => StepPlan::new(8.0, "Writing the flash image", vec![]),
            ("Writing the lock", _) => StepPlan::new(4.0, "Packing the prepared flash", vec![]),
            ("Writing the identity, NOR and boot files", _) => StepPlan::new(2.0, "Writing the NOR and boot files", vec![]),
            ("Building the system volume", _) => StepPlan::new(20.0, "Building the system volume", vec![]),
            _ => StepPlan::new(10.0, name, vec![]),
        }
    }

    /// 0..<1 at `elapsed` s: the last milestone seen (`seen[i]`, when it was first seen) plus the time since. Time
    /// alone runs on past milestones a release never prints (7.x has no it_prefs line) but never reaches the last
    /// one: only the boot's own halt finishes the step. On by the clock for 80% of the way to the next milestone,
    /// then ever more slowly (same slope at the join): a slower host (a phone, 3-5x a Mac) never sees the bar stop.
    pub fn fraction(&self, elapsed: f64, seen: &[Option<f64>]) -> f64 {
        let reached = seen.iter().rposition(Option::is_some);
        let last_at = self.milestones.last().map(|m| m.at).unwrap_or(self.seconds);
        let from = reached.map(|i| self.milestones[i].at).unwrap_or(0.0);
        let since = reached.and_then(|i| seen[i]).unwrap_or(0.0);
        let to = match reached {
            Some(i) if i + 1 < self.milestones.len() => last_at,
            Some(_) => self.seconds,
            None => last_at,
        };
        let x = (elapsed - since).max(0.0);
        let gap = (to - from).max(0.0);
        let (knee, tail) = (0.8 * gap, 0.19 * gap);
        let t = from + if x <= knee || tail <= 0.0 { x.min(knee) } else { knee + tail * (1.0 - (-(x - knee) / tail).exp()) };
        (t / self.seconds).min(0.99)
    }
}

struct State {
    plan: Option<StepPlan>,
    text: String,
    started: Instant,
    last: f64,
    seen: Vec<Option<f64>>,
    measured: Option<Box<dyn Fn() -> f64 + Send>>,
}

/// The heartbeat: a progress event every second for the current step.
pub struct StepProgress {
    emit: Emit,
    work: Option<PathBuf>,
    major: u32,
    state: Arc<Mutex<State>>,
    stop: Mutex<Option<(mpsc::Sender<()>, std::thread::JoinHandle<()>)>>,
}

impl StepProgress {
    /// `work` holds the one-shots' serial logs.
    pub fn new(work: Option<PathBuf>, major: u32, emit: Emit) -> Arc<StepProgress> {
        let p = Arc::new(StepProgress {
            emit,
            work,
            major,
            state: Arc::new(Mutex::new(State {
                plan: None,
                text: String::new(),
                started: Instant::now(),
                last: 0.0,
                seen: Vec::new(),
                measured: None,
            })),
            stop: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel::<()>();
        let weak = Arc::downgrade(&p);
        let handle = std::thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs(1)) {
                let Some(p) = weak.upgrade() else { break };
                let mut s = p.state.lock().unwrap_or_else(|e| e.into_inner());
                p.tick(&mut s);
            }
        });
        *p.stop.lock().unwrap_or_else(|e| e.into_inner()) = Some((tx, handle));
        p
    }

    /// This step's own fraction (0...1), e.g. bytes hashed; the plan's time estimate otherwise.
    pub fn measure(&self, f: Option<Box<dyn Fn() -> f64 + Send>>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).measured = f;
    }

    /// Ends the current step with 1.0, then emits the step event and the new step's first progress.
    pub fn next(&self, index: usize, name: &str) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.end(&mut s);
        (self.emit)(Event::Step { index, name: name.to_string() });
        let plan = StepPlan::plan(name, self.major);
        s.text = plan.text.clone();
        s.seen = vec![None; plan.milestones.len()];
        s.plan = Some(plan);
        s.started = Instant::now();
        s.last = 0.0;
        s.measured = None;
        self.tick(&mut s);
    }

    /// The last step's 1.0.
    pub fn finish(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.end(&mut s);
    }

    /// Stops the heartbeat; no events after it returns.
    pub fn stop(&self) {
        if let Some((tx, handle)) = self.stop.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
            let _ = handle.join();
        }
    }

    fn detail(text: &str, started: Instant) -> String {
        format!("{text} — {} s", started.elapsed().as_secs())
    }

    fn end(&self, s: &mut State) {
        if s.plan.take().is_some() {
            (self.emit)(Event::Progress { fraction: 1.0, detail: Some(Self::detail(&s.text, s.started)) });
        }
    }

    fn tick(&self, s: &mut State) {
        let Some(plan) = s.plan.clone() else { return };
        let elapsed = s.started.elapsed().as_secs_f64();
        if let Some(work) = &self.work {
            let mut logs: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
            for (i, m) in plan.milestones.iter().enumerate() {
                if s.seen[i].is_none() {
                    let serial = logs.entry(m.file).or_insert_with(|| serial_log(work, m.file));
                    if serial.contains(m.marker) {
                        s.seen[i] = Some(elapsed);
                    }
                }
                if s.seen[i].is_some() {
                    s.text = m.text.to_string();
                }
            }
        }
        let estimate = match &s.measured {
            Some(f) => f(),
            None => plan.fraction(elapsed, &s.seen),
        };
        let f = s.last.max(estimate.min(0.99));
        s.last = f;
        (self.emit)(Event::Progress { fraction: (f * 1000.0).round() / 1000.0, detail: Some(Self::detail(&s.text, s.started)) });
    }
}

impl Drop for StepProgress {
    fn drop(&mut self) {
        if let Some((tx, _)) = self.stop.get_mut().map(|s| s.take()).ok().flatten() {
            let _ = tx.send(());
        }
    }
}

/// A serial log's text ("" until it exists, read as Latin-1); a "prefix-" names the newest of prefix*.log.
pub fn serial_log(work: &Path, file: &str) -> String {
    let name = if file.ends_with('-') {
        let mut names: Vec<String> = std::fs::read_dir(work)
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.retain(|n| n.starts_with(file) && n.ends_with(".log"));
        // localizedStandardCompare: numbers by value (keybag-10 after keybag-9)
        names.sort_by_key(|n| {
            let digits: String = n[file.len()..].chars().take_while(char::is_ascii_digit).collect();
            (digits.parse::<u64>().unwrap_or(0), n.clone())
        });
        match names.pop() {
            Some(n) => n,
            None => return String::new(),
        }
    } else {
        file.to_string()
    };
    std::fs::read(work.join(name)).map(|b| b.iter().map(|&c| c as char).collect()).unwrap_or_default()
}

/// Bytes done of a total, as a step's measure.
pub struct ByteCount {
    total: u64,
    done: AtomicU64,
}

impl ByteCount {
    pub fn new(total: u64) -> Arc<ByteCount> {
        Arc::new(ByteCount { total, done: AtomicU64::new(0) })
    }
    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
    pub fn fraction(&self) -> f64 {
        if self.total > 0 { self.done.load(Ordering::Relaxed) as f64 / self.total as f64 } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_match_the_swift_tool() {
        assert_eq!(
            Event::Begin { steps: 6, seconds: vec![2.0, 5.0, 2.0, 20.0, 8.0, 3.0] }.json(),
            r#"{"event":"begin","seconds":[2,5,2,20,8,3],"steps":6}"#
        );
        assert_eq!(
            Event::Step { index: 1, name: "Verifying the IPSW".into() }.json(),
            r#"{"event":"step","index":1,"name":"Verifying the IPSW"}"#
        );
        assert_eq!(
            Event::Progress { fraction: 0.664, detail: Some("First start: setting up iOS — 61 s".into()) }.json(),
            r#"{"detail":"First start: setting up iOS — 61 s","event":"progress","fraction":0.664}"#
        );
        assert_eq!(Event::Done { lock: "device.lock.json".into() }.json(), r#"{"event":"done","lock":"device.lock.json"}"#);
        assert_eq!(
            Event::Error { code: "internal".into(), message: "a/b".into(), piece: None }.json(),
            r#"{"code":"internal","event":"error","message":"a/b"}"#
        );
    }

    #[test]
    fn fraction_follows_milestones_and_never_finishes() {
        let p = StepPlan::plan(SEAL_STEP, 6);
        assert_eq!(p.fraction(0.0, &[None; 5]), 0.0);
        let early = p.fraction(10.0, &[None; 5]);
        assert!(early > 0.0 && early < 0.1);
        let seen = [Some(4.0), Some(9.0), Some(42.0), Some(68.0), Some(125.0)];
        assert!(p.fraction(1e6, &seen) <= 0.99);
    }
}

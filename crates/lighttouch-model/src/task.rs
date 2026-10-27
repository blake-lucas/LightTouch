//! Long operations (adding a device, downloading, preparing) on their own threads, with progress the UI can show
//! and a cancel flag the work checks.
use crate::Waker;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    /// What is happening now, in plain words ("Copying the system").
    pub phase: String,
    pub done: u64,
    pub total: u64,
    /// The whole task's share done, when the work is several phases (see [`Reporter::band`]); `done`/`total` are
    /// then the current phase's own.
    pub overall: Option<f32>,
    band: Option<(f32, f32)>,
}

impl Progress {
    /// 0..1 across the whole task, or `None` while the total is unknown.
    pub fn fraction(&self) -> Option<f32> {
        self.overall.or_else(|| (self.total > 0).then(|| (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32))
    }
}

/// Handed to the work: report progress, check for cancellation.
#[derive(Clone)]
pub struct Reporter {
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    waker: Waker,
}

impl Reporter {
    /// Update progress; returns `false` once the user cancelled.
    pub fn report(&self, phase: &str, done: u64, total: u64) -> bool {
        {
            let mut p = self.progress.lock().unwrap();
            if p.phase != phase || p.done != done || p.total != total {
                p.phase.clear();
                p.phase.push_str(phase);
                p.done = done;
                p.total = total;
            }
            if let Some((start, end)) = p.band {
                let phase = if total > 0 { (done as f64 / total as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
                // Never backwards: a phase that starts over (a check after its download) holds the bar.
                p.overall = Some(p.overall.unwrap_or(0.0).max(start + (end - start) * phase));
            }
        }
        (self.waker)();
        !self.cancelled()
    }

    /// The share of the whole task that the following reports fill, `start..end` of 0..1, so a task of several
    /// phases shows one bar that never restarts.
    pub fn band(&self, start: f32, end: f32) {
        let mut p = self.progress.lock().unwrap();
        p.band = Some((start, end));
        p.overall = Some(p.overall.unwrap_or(0.0).max(start));
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub struct Task<T> {
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<T, String>>>,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(waker: Waker, work: impl FnOnce(&Reporter) -> Result<T, String> + Send + 'static) -> Self {
        let progress = Arc::new(Mutex::new(Progress::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let reporter = Reporter { progress: progress.clone(), cancel: cancel.clone(), waker: waker.clone() };
        let handle = std::thread::Builder::new()
            .name("task".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&reporter)))
                    .unwrap_or_else(|_| Err("something went wrong (internal error)".into()));
                waker();
                result
            })
            .expect("spawn task thread");
        Self { progress, cancel, handle: Some(handle) }
    }

    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The result once the work has finished.
    pub fn take_result(&mut self) -> Option<Result<T, String>> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            self.handle.take().map(|h| h.join().unwrap_or_else(|_| Err("internal error".into())))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reporter() -> Reporter {
        Reporter {
            progress: Arc::new(Mutex::new(Progress::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            waker: Arc::new(|| {}),
        }
    }

    #[test]
    fn phases_in_bands_make_one_bar_that_never_goes_back() {
        let r = reporter();
        let fraction = || r.progress.lock().unwrap().fraction().unwrap();
        r.band(0.0, 0.4);
        r.report("Downloading the IPSW", 50, 100);
        assert_eq!(fraction(), 0.2);
        r.report("Downloading the IPSW", 100, 100);
        // the next phase starts at its own 0 but the bar holds where it was
        r.band(0.4, 0.9);
        r.report("Unpacking", 0, 10);
        assert_eq!(fraction(), 0.4);
        r.report("Unpacking", 5, 10);
        assert!((fraction() - 0.65).abs() < 1e-6);
        // the phase's own numbers stay for the page's "X of Y" text
        let progress = r.progress.lock().unwrap().clone();
        assert_eq!((progress.done, progress.total), (5, 10));
        r.band(0.9, 1.0);
        r.report("Adding the device", 0, 0);
        assert_eq!(fraction(), 0.9);
    }

    #[test]
    fn without_bands_the_fraction_is_the_phase_own() {
        let r = reporter();
        r.report("Copying", 1, 4);
        assert_eq!(r.progress.lock().unwrap().fraction(), Some(0.25));
        r.report("Starting", 0, 0);
        assert_eq!(r.progress.lock().unwrap().fraction(), None);
    }
}

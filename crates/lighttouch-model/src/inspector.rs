//! The frame inspector, as on Android: a session keeps when each guest frame was published and shown; pressing
//! Inspect pauses the guest and freezes the last two minutes into a `Snapshot`, whose ranges the UI charts and
//! `save_report` writes out.
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// A frame after a gap this long follows a static screen: its interval is idle time, not render time.
pub const IDLE_GAP_MS: f32 = 1000.0;
/// What a snapshot keeps; older frames are outside what anyone pressed the button for.
const WINDOW_NS: u64 = 120_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Published {
    sequence: u64,
    published_ns: u64,
    /// When the window read it; 0 if a newer frame replaced it first.
    presented_ns: u64,
}

/// Frames as they happen, ~136 s at 60 fps.
#[derive(Default)]
pub struct Timeline {
    frames: VecDeque<Published>,
}

impl Timeline {
    const CAPACITY: usize = 8192;

    pub(crate) fn published(&mut self, sequence: u64, published_ns: u64) {
        if self.frames.len() == Self::CAPACITY {
            self.frames.pop_front();
        }
        self.frames.push_back(Published { sequence, published_ns, presented_ns: 0 });
    }

    pub(crate) fn presented(&mut self, sequence: u64, now_ns: u64) {
        if let Some(f) = self.frames.iter_mut().rev().take(8).find(|f| f.sequence == sequence) {
            f.presented_ns = now_ns;
        }
    }

    /// The last two minutes, frozen.
    pub fn snapshot(&self, device: &str, log: Option<PathBuf>) -> Snapshot {
        let snapshot_ns = lighttouch_core::monotonic_ns();
        let kept: Vec<&Published> =
            self.frames.iter().filter(|f| snapshot_ns.saturating_sub(f.published_ns) <= WINDOW_NS).collect();
        let frames = kept
            .windows(2)
            .map(|w| Frame {
                sequence: w[1].sequence,
                published_ns: w[1].published_ns,
                presented_ns: w[1].presented_ns,
                interval_ms: w[1].published_ns.saturating_sub(w[0].published_ns) as f32 / 1e6,
            })
            .collect();
        Snapshot { frames, snapshot_ns, device: device.to_string(), log, events: None, detail: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub sequence: u64,
    pub published_ns: u64,
    pub presented_ns: u64,
    /// Since the previous frame.
    pub interval_ms: f32,
}

impl Frame {
    pub fn idle(&self) -> bool {
        self.interval_ms >= IDLE_GAP_MS
    }
}

pub struct Snapshot {
    pub frames: Vec<Frame>,
    pub snapshot_ns: u64,
    pub device: String,
    /// The session's log, whose end goes in a report.
    pub log: Option<PathBuf>,
    /// The detailed timing capture the inspector claimed, if detailed timing was on.
    pub events: Option<PathBuf>,
    /// Read from `events` (`load_detail`) once the capture has been flushed.
    pub detail: Option<Detail>,
}

/// The performance overlay's detailed rows, as on Android: the newest couple of seconds of the live capture. The
/// engine writes captures through a 64 KB buffer, so what's on disk trails the guest by a few seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveTiming {
    pub gl_ms_per_frame: f32,
    pub bql_ms_per_frame: f32,
    /// Guest code running, of wall time.
    pub run_percent: f32,
    /// The vCPU thread on a host core, of wall time; None where the engine can't measure it (macOS).
    pub vcpu_percent: Option<f32>,
}

impl LiveTiming {
    /// From the breakdowns ending in the `window_ns` before the newest one; None without any.
    pub fn of(breakdown: &[Breakdown], window_ns: u64) -> Option<Self> {
        let newest = breakdown.iter().map(|b| b.end_ns).max()?;
        let recent: Vec<&Breakdown> = breakdown.iter().filter(|b| b.end_ns + window_ns >= newest).collect();
        let wall: f32 = recent.iter().map(|b| b.interval_ms).sum();
        if recent.is_empty() || wall <= 0.0 {
            return None;
        }
        let n = recent.len() as f32;
        let sum = |f: fn(&Breakdown) -> f32| recent.iter().map(|b| f(b)).sum::<f32>();
        Some(Self {
            gl_ms_per_frame: sum(|b| b.gl_ms) / n,
            bql_ms_per_frame: sum(|b| b.bql_ms) / n,
            run_percent: sum(|b| b.run_ms) / wall * 100.0,
            vcpu_percent: Some(sum(|b| b.vcpu_cpu_ms)).filter(|ms| *ms > 0.0).map(|ms| ms / wall * 100.0),
        })
    }
}

/// Where one frame interval went, from detailed timing (deltas of the engine's F records, ms).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Breakdown {
    pub end_ns: u64,
    pub interval_ms: f32,
    /// Guest code: the vCPU running.
    pub run_ms: f32,
    /// The vCPU halted or outside guest code.
    pub outside_ms: f32,
    /// Waiting for the engine lock.
    pub bql_ms: f32,
    /// The vCPU thread on a host core.
    pub vcpu_cpu_ms: f32,
    pub gl_ms: f32,
    pub gl_present_ms: f32,
    pub main_cpu_ms: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Gl,
    Wait,
    Hold,
    Callback,
}

impl EventKind {
    pub fn label(self) -> &'static str {
        match self {
            EventKind::Gl => "GL call",
            EventKind::Wait => "Lock wait",
            EventKind::Hold => "Lock hold",
            EventKind::Callback => "Main loop",
        }
    }
}

/// A slow engine event from the capture (the engine records those over 0.5 ms).
#[derive(Debug, Clone, PartialEq)]
pub struct SlowEvent {
    pub start_ns: u64,
    pub end_ns: u64,
    pub kind: EventKind,
    pub label: String,
}

impl SlowEvent {
    pub fn ms(&self) -> f32 {
        self.end_ns.saturating_sub(self.start_ns) as f32 / 1e6
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Detail {
    pub breakdown: Vec<Breakdown>,
    pub events: Vec<SlowEvent>,
}

impl Detail {
    /// The record nearest a frame's publication (they come from the same engine event).
    pub fn breakdown_for(&self, frame: &Frame) -> Option<Breakdown> {
        self.breakdown
            .iter()
            .min_by_key(|b| b.end_ns.abs_diff(frame.published_ns))
            .filter(|b| b.end_ns.abs_diff(frame.published_ns) < 8_000_000)
            .copied()
    }

    pub fn events_in(&self, from: u64, to: u64) -> Vec<&SlowEvent> {
        self.events.iter().filter(|e| e.end_ns >= from && e.start_ns <= to).collect()
    }
}

/// Read the tail of a capture (at most 24 MiB) from `from_ns` on into per-frame deltas and slow events, as the Android
/// inspector does. F records: `F ns ? run outside bql-outside bql-inside vcpu-cpu ? gl gl-present main-cpu`; G names a
/// GL slot; S a GL call (start end slot); W/H a lock wait/hold (start end file line); C a main loop callback.
pub fn load_detail(path: &Path, from_ns: u64) -> Detail {
    load_detail_tail(path, from_ns, 24 << 20)
}

/// `load_detail` over only the last `tail` bytes of the capture.
pub fn load_detail_tail(path: &Path, from_ns: u64, tail: u64) -> Detail {
    use std::io::{BufRead, Read, Seek, SeekFrom};
    let mut detail = Detail::default();
    let Ok(mut file) = std::fs::File::open(path) else { return detail };
    let length = file.metadata().map_or(0, |m| m.len());
    let start = length.saturating_sub(tail);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return detail;
    }
    let mut reader = std::io::BufReader::with_capacity(1 << 16, file.take(u64::MAX));
    let mut line = String::new();
    if start > 0 {
        let _ = reader.read_line(&mut line); // the partial first line
    }
    let mut names = std::collections::HashMap::new();
    let mut gl = Vec::new();
    let mut previous: Option<[u64; 9]> = None;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let v: Vec<&str> = line.trim_end().split(' ').collect();
        let num = |i: usize| v.get(i).and_then(|s| s.parse::<u64>().ok());
        match v.first().copied() {
            Some("F") if v.len() >= 12 => {
                let parsed: Option<Vec<u64>> = [1, 3, 4, 5, 6, 7, 9, 10, 11].iter().map(|i| num(*i)).collect();
                let Some(f) = parsed else { continue };
                let f: [u64; 9] = f.try_into().unwrap();
                if let Some(p) = previous {
                    if f[0] >= from_ns {
                        let d = |i: usize| f[i].saturating_sub(p[i]) as f32 / 1e6;
                        detail.breakdown.push(Breakdown {
                            end_ns: f[0],
                            interval_ms: d(0),
                            run_ms: d(1),
                            outside_ms: d(2),
                            bql_ms: d(3) + d(4),
                            vcpu_cpu_ms: d(5),
                            gl_ms: d(6),
                            gl_present_ms: d(7),
                            main_cpu_ms: d(8),
                        });
                    }
                }
                previous = Some(f);
            }
            Some("G") if v.len() >= 7 => {
                if let Some(slot) = num(2) {
                    names.insert(slot, v[6..].join(" "));
                }
            }
            Some("S") if v.len() >= 4 => {
                if let (Some(s), Some(e), Some(slot)) = (num(1), num(2), num(3)) {
                    if e >= from_ns {
                        gl.push((s, e, slot));
                    }
                }
            }
            Some(kind @ ("W" | "H")) if v.len() >= 5 => {
                if let (Some(s), Some(e)) = (num(1), num(2)) {
                    if e >= from_ns {
                        let file = v[3].rsplit(['/', '\\']).next().unwrap_or(v[3]);
                        detail.events.push(SlowEvent {
                            start_ns: s,
                            end_ns: e,
                            kind: if kind == "W" { EventKind::Wait } else { EventKind::Hold },
                            label: format!("{file}:{}", v[4]),
                        });
                    }
                }
            }
            Some("C") if v.len() >= 4 => {
                if let (Some(s), Some(e)) = (num(1), num(2)) {
                    if e >= from_ns {
                        detail.events.push(SlowEvent {
                            start_ns: s,
                            end_ns: e,
                            kind: EventKind::Callback,
                            label: v[3].to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    for (s, e, slot) in gl {
        let label = names.get(&slot).cloned().unwrap_or_else(|| format!("GL call {slot}"));
        detail.events.push(SlowEvent { start_ns: s, end_ns: e, kind: EventKind::Gl, label });
    }
    detail.events.sort_by_key(|e| e.start_ns);
    detail
}

impl Snapshot {
    pub fn start_ns(&self) -> u64 {
        self.frames.first().map_or(self.snapshot_ns, |f| f.published_ns)
    }

    pub fn end_ns(&self) -> u64 {
        self.frames.last().map_or(self.snapshot_ns, |f| f.published_ns)
    }

    /// The drawn (not idle) frames published in `from..=to`.
    pub fn frames_in(&self, from: u64, to: u64) -> Vec<Frame> {
        self.frames.iter().filter(|f| (from..=to).contains(&f.published_ns) && !f.idle()).copied().collect()
    }

    /// The worst frame of the last ten seconds, with a second either side.
    pub fn initial_range(&self) -> (u64, u64) {
        let (start, end) = (self.start_ns(), self.end_ns());
        let drawn: Vec<&Frame> = self.frames.iter().filter(|f| !f.idle()).collect();
        let recent: Vec<&Frame> = drawn.iter().copied().filter(|f| end - f.published_ns < 10_000_000_000).collect();
        let pool = if !recent.is_empty() {
            recent
        } else if !drawn.is_empty() {
            drawn
        } else {
            self.frames.iter().collect()
        };
        match pool.into_iter().max_by(|a, b| a.interval_ms.total_cmp(&b.interval_ms)) {
            Some(worst) => (
                worst.published_ns.saturating_sub(1_000_000_000).max(start),
                (worst.published_ns + 1_000_000_000).min(end),
            ),
            None => (start, end),
        }
    }
}

/// A range's statistics as the inspector shows them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RangeStats {
    pub frames: usize,
    pub seconds: f32,
    pub avg_fps: f32,
    pub avg_ms: f32,
    pub low_fps: f32,
    pub worst_ms: f32,
    /// Frames over 50 ms.
    pub hitches: usize,
    pub buckets: [usize; 7],
}

impl RangeStats {
    pub const BUCKET_LIMITS: [f32; 6] = [16.7, 20.0, 25.0, 33.4, 50.0, 100.0];
    pub const BUCKET_LABELS: [&'static str; 7] =
        ["≤16.7 ms", "≤20 ms", "≤25 ms", "≤33 ms", "≤50 ms", "≤100 ms", ">100 ms"];

    pub fn of(frames: &[Frame]) -> Self {
        if frames.is_empty() {
            return Self::default();
        }
        let ms: Vec<f32> = frames.iter().map(|f| f.interval_ms).collect();
        let total: f32 = ms.iter().sum();
        let mut sorted = ms.clone();
        sorted.sort_by(f32::total_cmp);
        let p99 = sorted[((sorted.len() - 1) as f32 * 0.99) as usize];
        let mut buckets = [0; 7];
        for v in &ms {
            buckets[Self::BUCKET_LIMITS.iter().position(|limit| v <= limit).unwrap_or(6)] += 1;
        }
        Self {
            frames: ms.len(),
            seconds: total / 1000.0,
            avg_fps: ms.len() as f32 * 1000.0 / total,
            avg_ms: total / ms.len() as f32,
            low_fps: 1000.0 / p99,
            worst_ms: *sorted.last().unwrap(),
            hitches: ms.iter().filter(|v| **v > 50.0).count(),
            buckets,
        }
    }
}

/// Write the range `from..=to` as a report folder in `dir`: summary.txt, selection.tsv (its frames), frames.tsv (the
/// whole snapshot), report.json, and the end of the session log. Returns the folder.
/// `app`: the iOS app in front when the inspector opened, as the guest tools named it; the report is named for it.
pub fn save_report(
    snapshot: &Snapshot,
    app: Option<&str>,
    from: u64,
    to: u64,
    dir: &Path,
    stamp: &str,
) -> Result<PathBuf, String> {
    let fail = |e: std::io::Error| format!("Couldn't save the report: {e}");
    // Named for the app it measured, so reports from different games tell apart at a glance.
    let subject = app.filter(|a| !a.is_empty()).unwrap_or(&snapshot.device);
    let name: String = subject.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).take(40).collect();
    let name = name.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    let folder = dir.join(format!("frames-{name}-{}", stamp.replace([' ', '.'], "-")));
    std::fs::create_dir_all(&folder).map_err(fail)?;
    let selected = snapshot.frames_in(from, to);
    let stats = RangeStats::of(&selected);
    let at = |ns: u64| ns.saturating_sub(snapshot.start_ns()) as f64 / 1e9;

    let mut summary = String::new();
    let _ = writeln!(summary, "LightTouch frame report  {stamp} UTC");
    let _ = writeln!(
        summary,
        "LightTouch {} on {}, device: {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        snapshot.device
    );
    if let Some(app) = app {
        let _ = writeln!(summary, "App in front: {app}");
    }
    let _ =
        writeln!(summary, "Selection: {:.3} s to {:.3} s of the timeline (monotonic {from}..{to})", at(from), at(to));
    let _ = writeln!(
        summary,
        "Frames {} over {:.2} s: average {:.1} fps ({:.2} ms), 1% low {:.1} fps, worst {:.1} ms, {} frames over 50 ms",
        stats.frames, stats.seconds, stats.avg_fps, stats.avg_ms, stats.low_fps, stats.worst_ms, stats.hitches
    );
    let _ = writeln!(summary, "Frame times:");
    for (label, count) in RangeStats::BUCKET_LABELS.iter().zip(stats.buckets) {
        let _ = writeln!(summary, "  {label:<9} {count}");
    }
    let detail = snapshot.detail.as_ref();
    let _ = writeln!(
        summary,
        "{}",
        if detail.is_some() {
            "Detailed timing: on (selection.tsv has the per-frame breakdown, events.tsv the raw records)"
        } else {
            "Detailed timing: off (turn it on in Settings > Diagnostics for per-frame GL, CPU and lock timing)"
        }
    );
    if let Some(detail) = detail {
        let mut slow = detail.events_in(from, to);
        slow.sort_by(|a, b| b.ms().total_cmp(&a.ms()));
        if !slow.is_empty() {
            let _ = writeln!(summary, "Slowest events:");
            for e in slow.iter().take(20) {
                let _ = writeln!(summary, "  {:7.2} ms  {:<9} {}", e.ms(), e.kind.label(), e.label);
            }
        }
    }
    std::fs::write(folder.join("summary.txt"), summary).map_err(fail)?;

    let table = |frames: &[Frame]| {
        let mut out = String::from("# sequence published-ns presented-ns interval-ms");
        if detail.is_some() {
            out.push_str(" run-ms outside-ms bql-ms vcpu-cpu-ms gl-ms gl-present-ms main-cpu-ms");
        }
        out.push('\n');
        for f in frames {
            let _ = write!(out, "{} {} {} {:.3}", f.sequence, f.published_ns, f.presented_ns, f.interval_ms);
            if let Some(b) = detail.and_then(|d| d.breakdown_for(f)) {
                let _ = write!(
                    out,
                    " {:.3} {:.3} {:.3} {:.3} {:.3} {:.3} {:.3}",
                    b.run_ms, b.outside_ms, b.bql_ms, b.vcpu_cpu_ms, b.gl_ms, b.gl_present_ms, b.main_cpu_ms
                );
            }
            out.push('\n');
        }
        out
    };
    // The raw records from a second before the range to a second after.
    if let (Some(_), Some(events)) = (detail, &snapshot.events) {
        if let Ok(text) = std::fs::read_to_string(events) {
            let window = from.saturating_sub(1_000_000_000)..=to + 1_000_000_000;
            let kept: String = text
                .lines()
                .filter(|l| {
                    l.starts_with('#')
                        || l.split(' ').nth(1).and_then(|t| t.parse::<u64>().ok()).is_some_and(|t| window.contains(&t))
                })
                .flat_map(|l| [l, "\n"])
                .collect();
            std::fs::write(folder.join("events.tsv"), kept).map_err(fail)?;
        }
    }
    std::fs::write(folder.join("selection.tsv"), table(&selected)).map_err(fail)?;
    std::fs::write(folder.join("frames.tsv"), table(&snapshot.frames)).map_err(fail)?;
    let report = serde_json::json!({"from": from, "to": to, "frames": stats.frames, "averageFps": stats.avg_fps,
        "lowFps": stats.low_fps, "worstMs": stats.worst_ms, "hitches": stats.hitches, "detailed": detail.is_some()});
    std::fs::write(folder.join("report.json"), serde_json::to_string_pretty(&report).unwrap_or_default() + "\n")
        .map_err(fail)?;
    if let Some(log) = snapshot.log.as_ref().and_then(|p| std::fs::read(p).ok()) {
        let tail = &log[log.len().saturating_sub(2 << 20)..];
        std::fs::write(folder.join("session-tail.log"), tail).map_err(fail)?;
    }
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(intervals_ms: &[u64]) -> Snapshot {
        let mut timeline = Timeline::default();
        let mut t = lighttouch_core::monotonic_ns() - 30_000_000_000;
        timeline.published(1, t);
        for (i, ms) in intervals_ms.iter().enumerate() {
            t += ms * 1_000_000;
            timeline.published(i as u64 + 2, t);
        }
        timeline.snapshot("iPod touch 2G", None)
    }

    #[test]
    fn stats_and_range() {
        let s = snapshot(&[16, 17, 16, 70, 16, 2000, 17, 16]);
        assert_eq!(s.frames.len(), 8);
        let all = s.frames_in(s.start_ns(), s.end_ns());
        assert_eq!(all.len(), 7, "the frame after the 2 s gap is idle");
        let stats = RangeStats::of(&all);
        assert_eq!(stats.hitches, 1);
        assert_eq!(stats.worst_ms, 70.0);
        assert_eq!(stats.buckets.iter().sum::<usize>(), 7);
        let (from, to) = s.initial_range();
        let worst = s.frames.iter().find(|f| f.interval_ms == 70.0).unwrap();
        assert!(from <= worst.published_ns && worst.published_ns <= to);
    }

    #[test]
    fn detail_records() {
        let path = std::env::temp_dir().join(format!("lt-events-{}.tsv", std::process::id()));
        let text = "# header\n\
            F 1000000000 0 100 200 0 0 300 0 0 0 50\n\
            G 0 7 0 0 0 glDrawArrays\n\
            S 1005000000 1006000000 7\n\
            W 1008000000 1010000000 ../system/cpus.c 120\n\
            F 1016000000 0 10000100 2200200 1000000 500000 9000300 0 3000000 100000 2000050\n";
        std::fs::write(&path, text).unwrap();
        let d = load_detail(&path, 0);
        assert_eq!(d.breakdown.len(), 1);
        let b = d.breakdown[0];
        assert_eq!((b.interval_ms, b.run_ms, b.bql_ms, b.gl_ms, b.main_cpu_ms), (16.0, 10.0, 1.5, 3.0, 2.0));
        assert_eq!(d.events.len(), 2);
        assert_eq!(d.events[0].label, "glDrawArrays");
        assert_eq!(d.events[1].label, "cpus.c:120");
        assert_eq!(d.events[1].kind, EventKind::Wait);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn report() {
        let s = snapshot(&[16, 17, 50, 16]);
        let dir = std::env::temp_dir().join(format!("lt-inspector-{}", std::process::id()));
        let folder = save_report(&s, None, s.start_ns(), s.end_ns(), &dir, "2026-10-25 at 06.30.00").unwrap();
        let summary = std::fs::read_to_string(folder.join("summary.txt")).unwrap();
        assert!(summary.contains("Frames 4 over"), "{summary}");
        assert_eq!(std::fs::read_to_string(folder.join("selection.tsv")).unwrap().lines().count(), 5);
        assert!(folder.ends_with("frames-iPod-touch-2G-2026-10-25-at-06-30-00"), "{}", folder.display());
        let folder =
            save_report(&s, Some("Doodle Jump"), s.start_ns(), s.end_ns(), &dir, "2026-10-25 at 06.31.00").unwrap();
        assert!(folder.ends_with("frames-Doodle-Jump-2026-10-25-at-06-31-00"), "{}", folder.display());
        assert!(std::fs::read_to_string(folder.join("summary.txt")).unwrap().contains("App in front: Doodle Jump"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

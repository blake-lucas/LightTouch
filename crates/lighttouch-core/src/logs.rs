//! The logs LightTouch keeps for problem reports (docs/LOGS.md): each is written as it happens, keeps the start of a
//! run (its head) and its newest output (two tail segments), and keeps the run before it as its previous generation,
//! so reopening LightTouch or restarting a device after a crash doesn't erase the crash.
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A capped log: its name in the logs folder and its sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    pub name: &'static str,
    /// Bytes of the run's start kept in `NAME.log`.
    pub head: u64,
    /// Bytes per tail segment; two are kept (`NAME.tail.1.log`, `NAME.tail.log`).
    pub segment: u64,
}

/// LightTouch itself, from when it opens.
pub const APP: Spec = Spec { name: "app", head: 256 << 10, segment: 256 << 10 };
/// The device worker and the engine inside it, from a device start.
pub const ENGINE: Spec = Spec { name: "engine", head: 1 << 20, segment: 1 << 20 };
/// A device preparation: its steps, firmwarekit and its one-shot boots.
pub const PREPARE: Spec = Spec { name: "prepare", head: 1 << 20, segment: 1 << 20 };

/// The files of the log `stem` ("engine", "engine.prev") in reading order: head, older tail, newer tail.
pub fn parts(stem: &str) -> [String; 3] {
    [format!("{stem}.log"), format!("{stem}.tail.1.log"), format!("{stem}.tail.log")]
}

/// Start a new generation of `name`: the previous one is deleted and the current one becomes the previous.
pub fn new_generation(dir: &Path, name: &str) -> io::Result<()> {
    let previous = format!("{name}.prev");
    for part in parts(&previous) {
        match std::fs::remove_file(dir.join(part)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    for (current, old) in parts(name).iter().zip(parts(&previous)) {
        match std::fs::rename(dir.join(current), dir.join(old)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// One run of a capped log, written line by line straight to disk.
pub struct CappedLog {
    dir: PathBuf,
    spec: Spec,
    file: File,
    /// Bytes in the file being written.
    written: u64,
    /// 0 while writing the head, then the tail segment's number.
    segment: u32,
}

impl CappedLog {
    /// Start a new generation of `spec` in `dir` and open its head.
    pub fn start(dir: &Path, spec: Spec) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        new_generation(dir, spec.name)?;
        let file = File::create(dir.join(format!("{}.log", spec.name)))?;
        Ok(Self { dir: dir.to_path_buf(), spec, file, written: 0, segment: 0 })
    }

    /// The file being written now.
    pub fn path(&self) -> PathBuf {
        let [head, _, tail] = parts(self.spec.name);
        self.dir.join(if self.segment == 0 { head } else { tail })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append one line (a newline is added when it has none). A line longer than a segment is cut.
    pub fn line(&mut self, line: &[u8]) -> io::Result<()> {
        let limit = if self.segment == 0 { self.spec.head } else { self.spec.segment };
        if self.written >= limit {
            self.next_segment()?;
        }
        let limit = if self.segment == 0 { self.spec.head } else { self.spec.segment } as usize;
        let body = &line[..line.len().min(limit)];
        self.file.write_all(body)?;
        self.written += body.len() as u64;
        if body.last() != Some(&b'\n') {
            self.file.write_all(b"\n")?;
            self.written += 1;
        }
        Ok(())
    }

    fn next_segment(&mut self) -> io::Result<()> {
        let [_, older, newer] = parts(self.spec.name);
        if self.segment > 0 {
            std::fs::rename(self.dir.join(&newer), self.dir.join(&older))?;
        }
        self.segment += 1;
        self.file = File::create(self.dir.join(&newer))?;
        let t = crate::local_time();
        let marker =
            format!("----- [log] tail segment {}, {:02}:{:02}:{:02} -----\n", self.segment, t.hour, t.minute, t.second);
        self.file.write_all(marker.as_bytes())?;
        self.written = marker.len() as u64;
        Ok(())
    }
}

impl Write for CappedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.line(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A capped log shared between the thread copying into it and whoever adds its own lines.
pub type SharedLog = Arc<Mutex<CappedLog>>;

/// Copy `reader` (a child's stderr) into `log` line by line until it ends. The returned handle finishes when the
/// child and everything it started have closed the pipe.
pub fn copy_lines(reader: impl Read + Send + 'static, log: SharedLog) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("log-{}", log.lock().map(|l| l.spec.name).unwrap_or("copy")))
        .spawn(move || {
            let mut reader = BufReader::new(reader);
            let mut line = Vec::new();
            loop {
                line.clear();
                // Bounded, so a child printing without newlines can't grow this without end.
                match (&mut reader).take(64 << 10).read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Ok(mut log) = log.lock() {
                            let _ = log.line(&line);
                        }
                    }
                }
            }
        })
        .expect("spawn a log thread")
}

/// The log `stem` ("engine", "engine.prev") as one text: its head and tail segments joined. `None` when it has no
/// files.
pub fn stitch(dir: &Path, stem: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut any = false;
    for part in parts(stem) {
        if let Ok(bytes) = std::fs::read(dir.join(part)) {
            if any && !out.is_empty() && out.last() != Some(&b'\n') {
                out.push(b'\n');
            }
            out.extend_from_slice(&bytes);
            any = true;
        }
    }
    any.then_some(out)
}

/// The last `max` bytes of `bytes`, from the start of a line.
pub fn tail(bytes: &[u8], max: usize) -> &[u8] {
    if bytes.len() <= max {
        return bytes;
    }
    let cut = &bytes[bytes.len() - max..];
    match cut.iter().position(|&b| b == b'\n') {
        Some(i) if i + 1 < cut.len() => &cut[i + 1..],
        _ => cut,
    }
}

/// The last `max` bytes of the file at `path`, from the start of a line, without reading the rest.
pub fn read_tail(path: &Path, max: u64) -> Option<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let start = length.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    if start == 0 {
        return Some(bytes);
    }
    Some(match bytes.iter().position(|&b| b == b'\n') {
        Some(i) if i + 1 < bytes.len() => bytes[i + 1..].to_vec(),
        _ => bytes,
    })
}

/// The newest file of the log `stem`: its newer tail segment once it has one, else its head.
pub fn newest_part(dir: &Path, stem: &str) -> PathBuf {
    let [head, _, tail] = parts(stem);
    let tail = dir.join(tail);
    if tail.is_file() { tail } else { dir.join(head) }
}

static APP_LOG: Mutex<Option<CappedLog>> = Mutex::new(None);

/// Open the app log for this run (a new generation), with `header` as its first lines.
pub fn start_app_log(dir: &Path, header: &str) -> io::Result<()> {
    let mut log = CappedLog::start(dir, APP)?;
    for line in header.lines() {
        log.line(line.as_bytes())?;
    }
    *APP_LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(log);
    Ok(())
}

/// One line in the app log, stamped with the local time; also printed to stderr for a terminal. Before the app log
/// is open (the CLI, the worker) it is only printed.
pub fn app(message: &str) {
    let t = crate::local_time();
    let line = format!("{:02}:{:02}:{:02} {message}", t.hour, t.minute, t.second);
    eprintln!("{line}");
    if let Some(log) = APP_LOG.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = log.line(line.as_bytes());
    }
}

/// `app_log!("Started {}", name)`: a formatted line in the app log.
#[macro_export]
macro_rules! app_log {
    ($($arg:tt)*) => { $crate::logs::app(&format!($($arg)*)) };
}

/// Replace private paths in a log's text: each `(path, stand-in)` in order, longest first, with either slash.
pub fn scrub(text: &str, paths: &[(PathBuf, &str)]) -> String {
    let mut pairs: Vec<(String, &str)> = Vec::new();
    for (path, stand_in) in paths {
        let raw = path.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
        if raw.len() < 2 {
            continue;
        }
        pairs.push((raw.replace('\\', "/"), stand_in));
        pairs.push((raw.replace('/', "\\"), stand_in));
    }
    pairs.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    let mut out = text.to_string();
    for (path, stand_in) in pairs {
        if out.contains(&path) {
            out = out.replace(&path, stand_in);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lt-logs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const SMALL: Spec = Spec { name: "t", head: 100, segment: 100 };

    fn line(i: usize) -> String {
        format!("line {i:04} {}", "x".repeat(30))
    }

    #[test]
    fn a_run_keeps_its_head_and_two_tail_segments() {
        let d = dir("rotate");
        let mut log = CappedLog::start(&d, SMALL).unwrap();
        for i in 0..40 {
            log.line(line(i).as_bytes()).unwrap();
        }
        let head = std::fs::read_to_string(d.join("t.log")).unwrap();
        assert!(head.starts_with("line 0000"), "the head is the start of the run: {head}");
        assert!(head.len() <= 100 + 41, "the head stops near its size: {}", head.len());
        let newer = std::fs::read_to_string(d.join("t.tail.log")).unwrap();
        let older = std::fs::read_to_string(d.join("t.tail.1.log")).unwrap();
        assert!(newer.contains("line 0039"), "the newest line is kept: {newer}");
        assert!(newer.starts_with("----- [log] tail segment "), "{newer}");
        assert!(older.starts_with("----- [log] tail segment "), "{older}");
        let total: u64 = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
        assert!(total <= 3 * (100 + 41) + 2 * 50, "the run stays within head + 2 segments: {total}");
        assert_eq!(log.path(), d.join("t.tail.log"));
        let stitched = String::from_utf8(stitch(&d, "t").unwrap()).unwrap();
        let first = stitched.find("line 0000").unwrap();
        let marker = stitched.find("----- [log] tail segment").unwrap();
        let last = stitched.find("line 0039").unwrap();
        assert!(first < marker && marker < last, "head, then tail in order:\n{stitched}");
        assert!(!stitched.contains("line 0005\n") || head.contains("line 0005"), "the middle is left out");
    }

    #[test]
    fn a_new_generation_keeps_one_previous_run() {
        let d = dir("generations");
        for run in 0..3 {
            let mut log = CappedLog::start(&d, SMALL).unwrap();
            for i in 0..10 {
                log.line(format!("run {run} {}", line(i)).as_bytes()).unwrap();
            }
        }
        let current = String::from_utf8(stitch(&d, "t").unwrap()).unwrap();
        let previous = String::from_utf8(stitch(&d, "t.prev").unwrap()).unwrap();
        assert!(current.starts_with("run 2 ") && !current.contains("run 1 "), "{current}");
        assert!(previous.starts_with("run 1 ") && !previous.contains("run 2 "), "{previous}");
        assert!(!std::fs::read_dir(&d).unwrap().any(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            name.contains("prev.prev")
        }));
        let text = std::fs::read_dir(&d).unwrap().map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap());
        assert!(!text.into_iter().any(|t| t.contains("run 0 ")), "the run before the previous is gone");
        // A short run after a long one leaves none of the long one's tail in the current generation.
        let mut log = CappedLog::start(&d, SMALL).unwrap();
        log.line(b"short").unwrap();
        assert_eq!(stitch(&d, "t").unwrap(), b"short\n");
        assert!(stitch(&d, "t.prev").unwrap().starts_with(b"run 2 "));
    }

    #[test]
    fn copying_a_pipe_keeps_lines_whole() {
        let d = dir("copy");
        let log = Arc::new(Mutex::new(CappedLog::start(&d, SMALL).unwrap()));
        let input: Vec<u8> = (0..5).flat_map(|i| format!("{}\n", line(i)).into_bytes()).chain(*b"no newline").collect();
        copy_lines(std::io::Cursor::new(input), log.clone()).join().unwrap();
        let text = String::from_utf8(stitch(&d, "t").unwrap()).unwrap();
        assert!(text.lines().filter(|l| l.starts_with("line ")).all(|l| l.len() == line(0).len()), "{text}");
        assert!(text.ends_with("no newline\n"), "{text}");
        assert!(stitch(&d, "missing").is_none());
    }

    #[test]
    fn tails_start_at_a_line() {
        assert_eq!(tail(b"one\ntwo\nthree\n", 8), b"three\n");
        assert_eq!(tail(b"short", 8), b"short");
        let d = dir("tail");
        std::fs::write(d.join("f"), "one\ntwo\nthree\n").unwrap();
        assert_eq!(read_tail(&d.join("f"), 8).unwrap(), b"three\n");
        assert_eq!(read_tail(&d.join("f"), 100).unwrap(), b"one\ntwo\nthree\n");
    }

    #[test]
    fn scrubbing_hides_the_home_and_data_folders() {
        let paths =
            [(PathBuf::from("/home/someone/.local/share/LightTouch"), "<data>"), (PathBuf::from("/home/someone"), "~")];
        let text = "staged '/home/someone/.local/share/LightTouch/library/x' from /home/someone/Downloads/a.ipsw";
        assert_eq!(scrub(text, &paths), "staged '<data>/library/x' from ~/Downloads/a.ipsw");
        let windows = [(PathBuf::from(r"C:\Users\someone"), "~")];
        assert_eq!(scrub(r"C:\Users\someone\a and C:/Users/someone/b", &windows), r"~\a and ~/b");
    }
}

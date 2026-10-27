//! The preparation's boots (the keybag and seal one-shots): the engine run headless on one QEMU command line, in a
//! process of its own (`firmwarekit oneshot`), while the serial log and the clock are watched here.
//!
//!   firmwarekit oneshot LightTouch -M <machine,...> -serial file:<log> ...   (argv as boot.rs makes it)
//!   firmwarekit oneshot '{"mode":{"machines":{}}}'                         (the machine listing)
//!
//! The helper is this program itself unless `create --helper` names another launcher. A helper quits QEMU cleanly
//! (storage flushed) on SIGTERM; `firmwarekit oneshot` also on its stdin closing (LT_ONESHOT_STDIN=1), which is how
//! a Windows parent asks, having no SIGTERM.
use crate::boot::{Board, DeviceInfo};
use crate::error::{Code, Error, Result};
use std::ffi::{CString, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How to start a helper: a program and the arguments before the QEMU argv.
#[derive(Debug, Clone)]
pub struct Helper {
    pub program: PathBuf,
    pub prefix: Vec<OsString>,
    /// This program's own oneshot mode: stopped by closing its stdin as well as by SIGTERM.
    pub own: bool,
}

impl Helper {
    /// `create --helper`'s program, else this program's `oneshot` mode.
    pub fn new(helper: Option<&Path>) -> Result<Helper> {
        match helper {
            Some(p) => Ok(Helper { program: p.to_path_buf(), prefix: Vec::new(), own: false }),
            None => Ok(Helper {
                program: std::env::current_exe().map_err(|e| crate::internal!("this program's path: {e}"))?,
                prefix: vec!["oneshot".into()],
                own: true,
            }),
        }
    }

    fn command(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.prefix).args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        c
    }

    /// Every machine the helper's emulator library runs.
    pub fn machines(&self) -> Result<Vec<DeviceInfo>> {
        let out = self
            .command(&[r#"{"mode":{"machines":{}}}"#.to_string()])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| crate::internal!("{}: {e}", self.program.display()))?;
        if !out.status.success() {
            let name = self.program.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            return Err(crate::internal!("{name} machines exited {}", out.status.code().unwrap_or(-1)));
        }
        #[derive(serde::Deserialize)]
        struct Listing {
            machines: Vec<DeviceInfo>,
        }
        let listing: Listing =
            serde_json::from_slice(&out.stdout).map_err(|e| crate::internal!("the machine listing: {e}"))?;
        Ok(listing.machines)
    }

    /// The emulator's facts about `board`.
    pub fn hardware(&self, board: Board) -> Result<DeviceInfo> {
        let name = self.program.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let machines = self.machines().map_err(|e| {
            eprintln!("machines: {}", e.message);
            crate::internal!("{name}'s emulator library has no machine for {}", board.name())
        })?;
        machines
            .into_iter()
            .find(|m| m.board == board.name())
            .ok_or_else(|| crate::internal!("{name}'s emulator library has no machine for {}", board.name()))
    }
}

/// How a one-shot ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OneShot {
    /// The guest halted (QEMU exited by itself), rather than being stopped at the marker or the timeout.
    pub exited: bool,
    pub exit_code: i32,
    pub marker: bool,
    pub seconds: f64,
}

fn read_latin1(path: &Path) -> String {
    std::fs::read(path).map(|b| b.iter().map(|&c| c as char).collect()).unwrap_or_default()
}

/// Whether `pattern` (a regular expression of the few forms the recipes use: literal text with `\s*` between
/// words, as FTL_Open\s*\[OK\]) matches `text`.
fn pattern_matches(text: &str, pattern: &str) -> bool {
    // The recipes' patterns are literal pieces joined by \s*: match them in order with any whitespace between.
    let pieces: Vec<String> = pattern.split(r"\s*").map(|p| p.replace(r"\[", "[").replace(r"\]", "]").replace(r"\.", ".")).collect();
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(i) = text[start..].find(pieces[0].as_str()) {
        let mut at = start + i + pieces[0].len();
        let mut ok = true;
        for p in &pieces[1..] {
            while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            if text[at..].starts_with(p.as_str()) {
                at += p.len();
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            return true;
        }
        start += i + 1;
    }
    false
}

fn terminate(child: &mut Child) {
    drop(child.stdin.take()); // our own helper quits on its stdin closing
    #[cfg(unix)]
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
}

/// One boot of `argv` (which routes -serial to `serial`), stopped at `stop` (text) or `stop_pattern` in the serial log
/// (its newlines removed: other kernel messages interleave with the line), or at `timeout` seconds. `during` runs on
/// its own thread once the helper is started (the iPod keybag's gdbstub handoff); if it fails, the helper is stopped
/// and its error returned.
#[allow(clippy::too_many_arguments)]
pub fn run(
    helper: &Helper,
    argv: &[String],
    serial: &Path,
    stop: Option<&str>,
    stop_pattern: Option<&str>,
    timeout: f64,
    log: &dyn Fn(&str),
    during: Option<Box<dyn FnOnce() -> Result<()> + Send>>,
) -> Result<(OneShot, String)> {
    let mut command = helper.command(argv);
    command.env("IT_TVOUT_READY", "1").stdout(Stdio::from(std::io::stderr())).stderr(Stdio::inherit());
    if helper.own {
        command.env("LT_ONESHOT_STDIN", "1").stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let start = Instant::now();
    let mut child = command.spawn().map_err(|e| Error {
        code: Code::OneshotFailed,
        message: format!("{}: {e}", helper.program.display()),
        piece: None,
    })?;
    let (failed_tx, failed_rx) = std::sync::mpsc::channel::<Error>();
    let worker = during.map(|f| {
        std::thread::spawn(move || {
            if let Err(e) = f() {
                let _ = failed_tx.send(e);
            }
        })
    });
    let (mut marker, mut stopping) = (false, false);
    let mut deadline: Option<Instant> = None;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(e) = failed_rx.try_recv() {
            terminate(&mut child);
            let _ = child.wait();
            return Err(e);
        }
        if !marker && (stop.is_some() || stop_pattern.is_some()) {
            let text = read_latin1(serial);
            if stop.is_some_and(|s| text.contains(s)) {
                marker = true;
            }
            if let Some(p) = stop_pattern {
                if pattern_matches(&text.replace('\n', ""), p) {
                    marker = true;
                }
            }
        }
        if !stopping && (marker || start.elapsed().as_secs_f64() > timeout) {
            stopping = true;
            terminate(&mut child); // the helper quits QEMU, which flushes the NAND and NOR
            deadline = Some(Instant::now() + Duration::from_secs(60));
        }
        if deadline.is_some_and(|d| Instant::now() > d) {
            let _ = child.kill();
        }
    };
    if let Some(w) = worker {
        let _ = w.join();
    }
    if let Ok(e) = failed_rx.try_recv() {
        return Err(e);
    }
    let r = OneShot { exited: !stopping, exit_code: status.code().unwrap_or(-1), marker, seconds: start.elapsed().as_secs_f64() };
    log(&format!(
        "one-shot: {} after {:.0} s (exit {})",
        if r.marker { "marker" } else if r.exited { "halted" } else { "timed out" },
        r.seconds,
        r.exit_code
    ));
    Ok((r, read_latin1(serial)))
}

// ---- `firmwarekit oneshot`: the engine in this process

/// qemu-ios contrib/ios-app/qemu-ios-ui.h
#[repr(C)]
struct QemuIosDeviceInfo {
    machine: *const std::ffi::c_char,
    board: *const std::ffi::c_char,
    screen_width: i32,
    screen_height: i32,
    screen_scale: i32,
    default_orientation: i32,
    has_cellular: bool,
    has_usb_host: bool,
    has_compass: bool,
    has_usb_charger: bool,
    panel_min: i32,
    panel_max_width: i32,
    panel_max_height: i32,
    panel_width_step: i32,
    panel_max_pixels: i32,
}

fn engine_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LTM_QEMU_DYLIB") {
        return PathBuf::from(p);
    }
    let name = if cfg!(windows) {
        "libqemu-arm.dll"
    } else if cfg!(target_os = "macos") {
        "libqemu-arm.dylib"
    } else {
        "libqemu-arm.so"
    };
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    dir.join(name)
}

fn load(path: &Path) -> std::result::Result<libloading::Library, String> {
    #[cfg(unix)]
    {
        use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
        unsafe { Library::open(Some(path), RTLD_NOW | RTLD_LOCAL) }.map(Into::into).map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        use libloading::os::windows::{Library, LOAD_WITH_ALTERED_SEARCH_PATH};
        // its dependencies beside it
        unsafe { Library::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH) }.map(Into::into).map_err(|e| e.to_string())
    }
}

fn c_str(p: *const std::ffi::c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// The engine's threads on the fast cores only, as the app's runtime does for a running device: Android's scheduler
/// otherwise leaves the second TCG thread and the main loop on the little cores. No-op unless the slowest core has
/// under half the biggest's capacity (desktops); LT_ENGINE_CPUS=off skips it. Android is its own target_os.
fn prefer_fast_cores() {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if std::env::var("LT_ENGINE_CPUS").as_deref() == Ok("off") {
            return;
        }
        let mut capacity = Vec::new();
        for n in 0..libc::CPU_SETSIZE as usize {
            if !Path::new(&format!("/sys/devices/system/cpu/cpu{n}")).exists() {
                break;
            }
            let c = std::fs::read_to_string(format!("/sys/devices/system/cpu/cpu{n}/cpu_capacity"))
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .unwrap_or(0);
            capacity.push(c);
        }
        let biggest = capacity.iter().copied().max().unwrap_or(0);
        let smallest = capacity.iter().copied().filter(|&c| c > 0).min().unwrap_or(i64::MAX);
        if capacity.is_empty() || biggest <= 0 || smallest * 2 > biggest {
            return;
        }
        unsafe {
            let mut fast: libc::cpu_set_t = std::mem::zeroed();
            for (cpu, &c) in capacity.iter().enumerate() {
                if c * 2 >= biggest {
                    libc::CPU_SET(cpu, &mut fast);
                }
            }
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &fast);
        }
    }
}

/// Android Dynamic Performance Framework for the vCPU threads, as the app's runtime does for a running device
/// (performance_hint.h), but for a boot that has no frames and wants to finish soonest: every 20 ms, the vCPUs' CPU
/// time in that window against a 10 ms target, so a busy vCPU reads as behind and the power HAL raises its clocks and
/// placement (2.3x on a Pixel's guest; without it a Pixel's iPod touch 3G seal boot ran past 10 minutes), and an idle
/// one is left alone. API 33+, resolved at run time; does nothing without hint sessions (the Poco's HAL).
/// LT_ADPF=off skips it.
#[cfg(target_os = "android")]
fn performance_hint() {
    use std::ffi::c_void;
    type GetManager = unsafe extern "C" fn() -> *mut c_void;
    type CreateSession = unsafe extern "C" fn(*mut c_void, *const i32, usize, i64) -> *mut c_void;
    type ReportWork = unsafe extern "C" fn(*mut c_void, i64) -> i32;
    // QEMU names its vCPU threads "CPU n/TCG" as they start.
    fn vcpu_tids() -> Vec<i32> {
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else { return Vec::new() };
        tasks
            .flatten()
            .filter_map(|t| {
                let name = std::fs::read_to_string(t.path().join("comm")).ok()?;
                let name = name.trim_end_matches('\n');
                (name.len() > 4 && name.starts_with("CPU ") && name.ends_with("/TCG"))
                    .then(|| t.file_name().to_str()?.parse().ok())?
            })
            .take(8)
            .collect()
    }
    // Another thread's CPU clock: MAKE_THREAD_CPUCLOCK(tid, CPUCLOCK_SCHED). None once a vCPU has gone (QEMU stopping).
    fn cpu_ns(tids: &[i32]) -> Option<i64> {
        let mut total = 0i64;
        for &tid in tids {
            let mut value: libc::timespec = unsafe { std::mem::zeroed() };
            let clock = ((!(tid as u32) << 3) | 6) as libc::clockid_t;
            if unsafe { libc::clock_gettime(clock, &mut value) } != 0 {
                return None;
            }
            total += value.tv_sec as i64 * 1_000_000_000 + value.tv_nsec as i64;
        }
        Some(total)
    }
    let Ok(android) = (unsafe { libloading::Library::new("libandroid.so") }) else { return };
    // Never unloaded: asking for the manager starts libandroid's binder thread, and closing the library on the way
    // out (no hint sessions, as on the Poco) destroyed a mutex that thread still locks, aborting the boot a second
    // in ("pthread_mutex_lock called on a destroyed mutex"; "keybag boot: halted without the keybag").
    let android: &'static libloading::Library = Box::leak(Box::new(android));
    let symbols = unsafe {
        (
            android.get::<GetManager>(b"APerformanceHint_getManager"),
            android.get::<CreateSession>(b"APerformanceHint_createSession"),
            android.get::<ReportWork>(b"APerformanceHint_reportActualWorkDuration"),
        )
    };
    let (Ok(manager), Ok(create), Ok(report)) = symbols else {
        eprintln!("firmwarekit oneshot: no performance hints (API 33+ only)");
        return;
    };
    let manager = unsafe { manager() };
    if manager.is_null() {
        eprintln!("firmwarekit oneshot: no performance hints on this device");
        return;
    }
    let mut tids = Vec::new();
    for _ in 0..60 {
        tids = vcpu_tids();
        if !tids.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    if tids.is_empty() {
        return;
    }
    let session = unsafe { create(manager, tids.as_ptr(), tids.len(), 10_000_000) };
    if session.is_null() {
        eprintln!("firmwarekit oneshot: no performance hints (the power HAL has no hint sessions)");
        return;
    }
    eprintln!("firmwarekit oneshot: performance hints for {} vCPU thread(s)", tids.len());
    let Some(mut last) = cpu_ns(&tids) else { return };
    loop {
        std::thread::sleep(Duration::from_millis(20));
        let Some(now) = cpu_ns(&tids) else { break };
        if now > last {
            unsafe { report(session, now - last) };
        }
        last = now;
    }
}

#[cfg(unix)]
static WAKE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

#[cfg(unix)]
extern "C" fn on_signal(_: i32) {
    let fd = WAKE.load(std::sync::atomic::Ordering::Relaxed);
    if fd >= 0 {
        let c = 1u8;
        unsafe { libc::write(fd, &c as *const u8 as *const _, 1) };
    }
}

/// `firmwarekit oneshot ARGV...`: the engine ($LTM_QEMU_DYLIB, else libqemu-arm beside this program) on one QEMU
/// command line until it exits; SIGTERM, SIGINT or (LT_ONESHOT_STDIN=1) stdin's end quits QEMU as the app does
/// (storage flushed). Returns qemu_ios_main's status; 70 when the engine can't be loaded.
pub fn engine_main(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("usage: firmwarekit oneshot argv0 qemu-arguments...");
        return 64;
    }
    let path = engine_path();
    let lib = match load(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("firmwarekit oneshot: {e}");
            return 70;
        }
    };
    if args.len() == 1 && args[0].starts_with('{') && args[0].contains("\"machines\"") {
        let Ok(info_at) = (unsafe { lib.get::<unsafe extern "C" fn(i32) -> *const QemuIosDeviceInfo>(b"qemu_ios_device_info_at") })
        else {
            return 70;
        };
        let mut machines = Vec::new();
        for i in 0.. {
            let d = unsafe { info_at(i) };
            if d.is_null() {
                break;
            }
            let d = unsafe { &*d };
            machines.push(serde_json::json!({
                "machine": c_str(d.machine), "board": c_str(d.board), "screenWidth": d.screen_width,
                "screenHeight": d.screen_height, "screenScale": d.screen_scale,
                "defaultOrientation": d.default_orientation, "hasCellular": d.has_cellular,
                "hasUSBHost": d.has_usb_host, "hasCompass": d.has_compass, "panelMin": d.panel_min,
                "panelMaxWidth": d.panel_max_width, "panelMaxHeight": d.panel_max_height,
                "panelWidthStep": d.panel_width_step, "panelMaxPixels": d.panel_max_pixels,
            }));
        }
        println!("{}", serde_json::json!({"dylibPath": path.to_string_lossy(), "machines": machines}));
        return 0;
    }
    let symbols = unsafe {
        (
            lib.get::<unsafe extern "C" fn(i32, *mut *mut std::ffi::c_char) -> i32>(b"qemu_ios_main"),
            lib.get::<unsafe extern "C" fn(*const std::ffi::c_void, *const std::ffi::c_void)>(b"qemu_ios_ui_attach"),
            lib.get::<unsafe extern "C" fn()>(b"qemu_ios_ui_quit"),
        )
    };
    let (Ok(qemu_main), Ok(attach), Ok(quit)) = symbols else {
        eprintln!("firmwarekit oneshot: {} lacks the qemu_ios entry points", path.display());
        return 70;
    };
    let quit: unsafe extern "C" fn() = *quit;
    prefer_fast_cores(); // before any thread starts, so they all inherit it
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if rx.recv().is_ok() {
            unsafe { quit() };
        }
    });
    #[cfg(unix)]
    {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return 71;
        }
        WAKE.store(fds[1], std::sync::atomic::Ordering::Relaxed);
        let read_end = fds[0];
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut c = 0u8;
            while unsafe { libc::read(read_end, &mut c as *mut u8 as *mut _, 1) } != 1 {}
            let _ = tx.send(());
        });
        unsafe {
            // A mask is inherited across exec: a parent's worker thread may block SIGTERM.
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGINT);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(i32) as usize;
            libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        }
    }
    if std::env::var("LT_ONESHOT_STDIN").as_deref() == Ok("1") {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut sink = [0u8; 64];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
            let _ = tx.send(());
        });
    }
    unsafe { attach(std::ptr::null(), std::ptr::null()) };
    #[cfg(target_os = "android")]
    if std::env::var("LT_ADPF").as_deref() != Ok("off") {
        std::thread::spawn(performance_hint);
    }
    let owned: Vec<CString> = args.iter().map(|a| CString::new(a.as_str()).unwrap_or_default()).collect();
    let mut argv: Vec<*mut std::ffi::c_char> = owned.iter().map(|a| a.as_ptr() as *mut _).collect();
    argv.push(std::ptr::null_mut());
    let status = unsafe { qemu_main(owned.len() as i32, argv.as_mut_ptr()) };
    eprintln!("firmwarekit oneshot: QEMU returned {status}");
    std::mem::forget(lib); // QEMU's threads may outlive main's return until exit
    status
}

#[cfg(test)]
mod tests {
    #[test]
    fn ftl_pattern() {
        assert!(super::pattern_matches("x FTL_Open [OK] y", r"FTL_Open\s*\[OK\]"));
        assert!(super::pattern_matches("FTL_Open[OK]", r"FTL_Open\s*\[OK\]"));
        assert!(!super::pattern_matches("FTL_Open [FAIL]", r"FTL_Open\s*\[OK\]"));
    }
}

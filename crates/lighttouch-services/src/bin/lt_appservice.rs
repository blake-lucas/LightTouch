//! Android's guest app-services helper (liblt_appservice.so in the APK): a short-lived, private child of the app
//! that talks only to the app's own usbmuxd-qemu (`--socket`, an owner-only Unix socket), never to a system usbmuxd
//! or a real device. The same services as the desktop app, from this crate.
//!
//! ```text
//! lt_appservice info|list|crashlog --socket MUX.sock
//! lt_appservice install --socket MUX.sock --ipa PATH --bundle-id ID
//! lt_appservice uninstall --socket MUX.sock --bundle-id ID
//! lt_appservice syslog --socket MUX.sock --seconds 1..600
//! ```
//!
//! Output is JSON lines on stdout (`GuestApps.kt` reads them):
//! - `{"target":{"product_type":"iPod2,1","ios":"3.1.3"}}`, always first;
//! - install: `{"phase":"upload","percent":N}`, `{"phase":"mutation","status":…}` just before the guest's
//!   installer starts, `{"phase":"install","status":…,"percent":N|null}` (plus `unpacked`/`unpacked_total` while
//!   it unpacks);
//! - syslog: `{"syslog_start":{…}}`, then `{"syslog_chunk":{"begin_ns","end_ns","bytes","hex"}}` per read;
//! - `{"result":{…}}` and exit 0, or `{"error":"…"}` and exit 1.
//!
//! The IPA is already checked by the app (architecture, permissions), as the desktop's `ipa::prepare` does.
//! `LT_PARENT_DEATH_SIGNAL=1` (with `LT_PARENT_PID`) ends the helper when the app process dies.
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    helper::main()
}

/// Only Android runs the helper; elsewhere the app calls the crate directly.
#[cfg(not(unix))]
fn main() {
    eprintln!("lt_appservice runs on Android and other Unix systems only");
    std::process::exit(1);
}

#[cfg(unix)]
mod helper {
    use lighttouch_services::apps::{Guest, InstallProgress};
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::path::Path;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    /// How long the guest may take to come up (attach, pair, answer lockdown) before the request fails.
    const CONNECT_WINDOW: Duration = Duration::from_secs(20);

    fn emit(value: Value) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    }

    fn fail(message: impl Into<String>) -> ExitCode {
        emit(json!({ "error": message.into() }));
        ExitCode::from(1)
    }

    fn usage(detail: &str) -> ExitCode {
        fail(format!(
            "{detail} Usage: info|list|crashlog --socket PATH, install --socket PATH --ipa PATH --bundle-id ID, uninstall \
             --socket PATH --bundle-id ID, or syslog --socket PATH --seconds 1..600"
        ))
    }

    /// CLOCK_MONOTONIC in ns: the clock the app's frame logs use, so syslog chunks line up with them.
    #[allow(clippy::useless_conversion)] // time_t and long are 32-bit on 32-bit targets
    fn monotonic_ns() -> i64 {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime writes the timespec it is given.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        i64::from(ts.tv_sec) * 1_000_000_000 + i64::from(ts.tv_nsec)
    }

    /// What iOS accepts as a bundle identifier and a command line carries without quoting.
    fn bundle_id_valid(id: &str) -> bool {
        !id.is_empty() && id.len() < 256 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    }

    /// The socket must be ours alone: absolute, a socket owned by us with no group or other bits, in a folder that is
    /// ours alone too.
    fn socket_private(path: &str) -> bool {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let path = Path::new(path);
        let uid = unsafe { libc::geteuid() };
        let mine = |m: &std::fs::Metadata| m.uid() == uid && m.mode() & 0o077 == 0;
        path.is_absolute()
            && path.as_os_str().len() < 108
            && std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket() && mine(&m))
            && path.parent().and_then(|p| std::fs::metadata(p).ok()).is_some_and(|m| m.is_dir() && mine(&m))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn parent_death_guard() {
        if std::env::var("LT_PARENT_DEATH_SIGNAL").as_deref() != Ok("1") {
            return;
        }
        // SAFETY: prctl and getppid have no memory effects here.
        unsafe {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                std::process::exit(1);
            }
            let parent = libc::getppid();
            let expected = std::env::var("LT_PARENT_PID").ok().and_then(|p| p.parse::<i32>().ok());
            if parent == 1 || expected.is_some_and(|p| p != parent) {
                std::process::exit(1);
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn parent_death_guard() {}

    struct Args {
        action: String,
        socket: Option<String>,
        ipa: Option<String>,
        bundle_id: Option<String>,
        seconds: Option<String>,
    }

    fn parse(argv: &[String]) -> Result<Args, ExitCode> {
        let Some(action) = argv.first() else { return Err(usage("Missing action.")) };
        let mut a = Args { action: action.clone(), socket: None, ipa: None, bundle_id: None, seconds: None };
        let mut i = 1;
        while i < argv.len() {
            let slot = match argv[i].as_str() {
                "--socket" => &mut a.socket,
                "--ipa" => &mut a.ipa,
                "--bundle-id" => &mut a.bundle_id,
                "--seconds" => &mut a.seconds,
                "--verbose" => {
                    i += 1;
                    continue;
                }
                _ => return Err(usage("Unknown argument.")),
            };
            if i + 1 >= argv.len() || slot.is_some() {
                return Err(usage("Missing or repeated option value."));
            }
            *slot = Some(argv[i + 1].clone());
            i += 2;
        }
        Ok(a)
    }

    pub fn main() -> ExitCode {
        parent_death_guard();
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let a = match parse(&argv) {
            Ok(a) => a,
            Err(code) => return code,
        };
        let action = a.action.as_str();
        if !["info", "list", "crashlog", "install", "uninstall", "syslog"].contains(&action) {
            return usage("Unknown action.");
        }
        let Some(socket) = a.socket.as_deref() else { return usage("--socket is required.") };
        match action {
            "install" if a.ipa.as_deref().is_none_or(str::is_empty) => return fail("Choose an IPA."),
            "install" if a.bundle_id.as_deref().is_none_or(str::is_empty) => {
                return fail("The app's bundle identifier is required.");
            }
            "uninstall" if a.ipa.is_some() || a.bundle_id.is_none() => {
                return usage("uninstall requires --bundle-id and takes no --ipa.");
            }
            "uninstall" if !bundle_id_valid(a.bundle_id.as_deref().unwrap_or("")) => {
                return usage("Invalid bundle identifier.");
            }
            "install" | "uninstall" => {}
            _ if a.ipa.is_some() || a.bundle_id.is_some() => {
                return usage("--ipa/--bundle-id apply to install and uninstall only.");
            }
            _ => {}
        }
        let seconds = match (action, a.seconds.as_deref()) {
            ("syslog", Some(text)) => match text.parse::<u64>() {
                Ok(s @ 1..=600) if text.bytes().all(|b| b.is_ascii_digit()) => s,
                _ => return usage("syslog requires --seconds 1..600."),
            },
            ("syslog", None) => return usage("syslog requires --seconds 1..600."),
            (_, Some(_)) => return usage("--seconds applies to syslog only."),
            _ => 0,
        };
        if !socket_private(socket) {
            return fail("A private owner-only guest usbmux socket is required.");
        }

        let mut guest = match Guest::connect(Path::new(socket), CONNECT_WINDOW) {
            Ok(guest) => guest,
            Err(e) => return fail(e.0),
        };
        match guest.product() {
            Ok((product, ios)) => emit(json!({ "target": { "product_type": product, "ios": ios } })),
            Err(e) => return fail(format!("{} Try again after iOS finishes starting.", e.0)),
        }
        let result = match action {
            "info" => Ok(json!({ "ready": true })),
            "list" => list(&mut guest),
            "crashlog" => crashlog(&mut guest),
            "uninstall" => uninstall(&mut guest, a.bundle_id.as_deref().unwrap_or("")),
            "install" => {
                install(&mut guest, Path::new(a.ipa.as_deref().unwrap_or("")), a.bundle_id.as_deref().unwrap_or(""))
            }
            _ => syslog(&mut guest, seconds),
        };
        match result {
            Ok(result) => {
                emit(json!({ "result": result }));
                ExitCode::SUCCESS
            }
            Err(message) => fail(message),
        }
    }

    fn list(guest: &mut Guest) -> Result<Value, String> {
        let apps = guest.installed().map_err(|e| format!("Listing the installed apps failed: {}", e.0))?;
        let apps: Vec<Value> = apps
            .into_iter()
            .map(|app| {
                let mut entry = json!({ "bundle_id": app.bundle_id, "name": app.name, "version": app.version });
                if let Some(short) = app.short_version {
                    entry["short_version"] = json!(short);
                }
                entry
            })
            .collect();
        Ok(json!({ "apps": apps }))
    }

    fn crashlog(guest: &mut Guest) -> Result<Value, String> {
        match guest.last_crash() {
            Ok(Some((name, text))) => Ok(json!({ "name": name, "text": text })),
            Ok(None) => Ok(json!({ "name": null })),
            Err(e) => Err(format!("Reading the crash reports failed: {}", e.0)),
        }
    }

    /// The guest's installer answers once the app and its data are gone; the app's deadline bounds a guest that never
    /// answers.
    fn uninstall(guest: &mut Guest, bundle_id: &str) -> Result<Value, String> {
        emit(json!({ "phase": "mutation", "status": "uninstalling" }));
        guest.uninstall(bundle_id).map_err(|e| format!("Removing {bundle_id} failed: {}", e.0))?;
        Ok(json!({ "bundle_id": bundle_id, "uninstalled": true }))
    }

    fn install(guest: &mut Guest, ipa: &Path, bundle_id: &str) -> Result<Value, String> {
        match std::fs::metadata(ipa) {
            Ok(m) if m.is_file() && m.len() > 0 => {}
            Ok(_) => return Err("The IPA cannot be read: not a non-empty regular file".into()),
            Err(e) => return Err(format!("The IPA cannot be read: {e}")),
        }
        // Unpacking progress follows the guest's free space against this; without it the bar has no total.
        let unpacked = lighttouch_catalog::ipa::unpacked_size(ipa).ok().filter(|&n| n > 0);
        let mut last_percent = None;
        let app = guest
            .install(ipa, bundle_id, unpacked, &mut |p| {
                match p {
                    InstallProgress::Uploading { sent, total } => {
                        let percent = sent * 100 / total.max(1);
                        if last_percent != Some(percent) {
                            last_percent = Some(percent);
                            emit(json!({ "phase": "upload", "percent": percent }));
                        }
                    }
                    InstallProgress::Submitted => emit(
                        json!({ "phase": "mutation", "status": "Installing; wait for completion before stopping iOS" }),
                    ),
                    InstallProgress::Installing { status, percent } => {
                        emit(json!({ "phase": "install", "status": status, "percent": percent }))
                    }
                    InstallProgress::Unpacking { done, total } => emit(json!({
                        "phase": "install", "status": "ExtractingPackage", "percent": null,
                        "unpacked": done, "unpacked_total": total,
                    })),
                }
                true
            })
            .map_err(|e| e.0)?;
        Ok(json!({ "bundle_id": app.bundle_id, "version": app.version, "installed": true }))
    }

    /// Up to `seconds` of the guest's system log as it comes, hex-encoded (its lines are NUL-separated), each chunk
    /// stamped with the monotonic times around its read.
    fn syslog(guest: &mut Guest, seconds: u64) -> Result<Value, String> {
        let mut conn = guest
            .system_log_conn(Duration::from_secs(1))
            .map_err(|e| format!("Connecting to the system log failed: {}", e.0))?;
        let started = monotonic_ns();
        let deadline = Instant::now() + Duration::from_secs(seconds);
        emit(json!({ "syslog_start": { "monotonic_ns": started, "seconds": seconds } }));
        let (mut chunks, mut bytes) = (0u64, 0u64);
        let mut data = [0u8; 8192];
        while Instant::now() < deadline {
            let before = monotonic_ns();
            match conn.read(&mut data) {
                Ok(0) => return Err("The system log relay closed; the capture is incomplete.".into()),
                Ok(n) => {
                    let hex: String = data[..n].iter().map(|b| format!("{b:02x}")).collect();
                    emit(
                        json!({ "syslog_chunk": { "begin_ns": before, "end_ns": monotonic_ns(), "bytes": n, "hex": hex } }),
                    );
                    chunks += 1;
                    bytes += n as u64;
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(format!("Reading the system log failed ({e}); the capture is incomplete.")),
            }
        }
        Ok(json!({ "complete": true, "begin_ns": started, "end_ns": monotonic_ns(), "chunks": chunks, "bytes": bytes }))
    }
}

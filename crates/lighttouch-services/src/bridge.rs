//! The session's private USB link: the bundled QEMU-only usbmuxd listens where the engine dials when the guest
//! brings its USB port up (`IT_USB_TCP`), and serves usbmux clients. On Linux and macOS both are Unix sockets in an
//! owner-only folder; on Windows the daemon (`lighttouch-usbmuxd.exe`) picks two loopback TCP ports and reports them,
//! and the addresses are `127.0.0.1:port`. Pairing records persist per device record in `pairing`. The daemon must
//! be listening before the engine starts.
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Bridge {
    child: Child,
    /// The Unix sockets' folder (none on Windows).
    dir: PathBuf,
    /// Windows: the daemon's control pipe (`stop` retires it).
    #[cfg(windows)]
    control: Option<std::process::ChildStdin>,
    /// Where usbmux clients connect.
    pub mux_socket: PathBuf,
    /// What the engine dials (the value for `IT_USB_TCP`).
    pub guest_socket: PathBuf,
}

impl Bridge {
    /// Start the daemon and wait (up to 5 s) for both sockets. `log` receives its output.
    pub fn start(daemon: &Path, pairing: &Path, log: &Path) -> Result<Self> {
        if !daemon.is_file() {
            return Err(Error("Guest app services are not included in this build.".into()));
        }
        #[cfg(windows)]
        {
            Self::start_windows(daemon, pairing, log)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (pairing, log);
            Err(Error("Guest app services are not available on this system yet.".into()))
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(pairing)?;
            std::fs::set_permissions(pairing, std::fs::Permissions::from_mode(0o700))?;
            sweep(daemon);
            // Socket paths are limited to 104 bytes (macOS), so they go in a short temporary folder.
            let dir = std::env::temp_dir().join(format!("{PREFIX}{}-{:08x}", std::process::id(), nonce() as u32));
            std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
            let mux_socket = dir.join("mux.sock");
            let guest_socket = dir.join("qemu.sock");
            if let Some(parent) = log.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let out = std::fs::File::create(log)?;
            let mut command = Command::new(daemon);
            command
                .arg("-f")
                .arg("-S")
                .arg(&mux_socket)
                .arg("-C")
                .arg(pairing)
                .arg("-P")
                .arg(dir.join("daemon.pid"))
                // LIGHTTOUCH_SELF_PAIR: the app pairs, as it must on Windows (lockdown.rs), so the daemon doesn't.
                .args(std::env::var_os("LIGHTTOUCH_SELF_PAIR").map(|_| "-p"))
                .env("USBMUXD_QEMU_ADDR", &guest_socket)
                .stdin(Stdio::null())
                .stdout(out.try_clone()?)
                .stderr(out);
            // Its libraries sit beside it or in ../lib (AppImage, Linux packages).
            if let Some(bin) = daemon.parent() {
                let mut paths = vec![bin.to_path_buf(), bin.join("../lib")];
                if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
                    paths.extend(std::env::split_paths(&existing));
                }
                if let Ok(joined) = std::env::join_paths(paths) {
                    command.env("LD_LIBRARY_PATH", joined);
                }
            }
            // The daemon goes when this process does, however it ends. Elsewhere `sweep` retires it later.
            #[cfg(target_os = "linux")]
            unsafe {
                use std::os::unix::process::CommandExt;
                command.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
            let child = command.spawn().map_err(|e| Error(format!("The guest USB bridge couldn't start: {e}")))?;
            lighttouch_core::job::bind_to_this_process(&child);
            let mut bridge = Self { child, dir, mux_socket, guest_socket };
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if let Ok(Some(status)) = bridge.child.try_wait() {
                    return Err(Error(format!("The guest USB bridge exited ({status}); see {}", log.display())));
                }
                if is_socket(&bridge.mux_socket) && is_socket(&bridge.guest_socket) {
                    return Ok(bridge);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            bridge.stop();
            Err(Error("The guest USB bridge didn't open its sockets within 5 seconds.".into()))
        }
    }

    /// `lighttouch-usbmuxd.exe <pairing>`: it prints `DAEMON {json}` with the ports it listens on once ready, and
    /// retires on `stop` on its stdin. The session identity in `LTW_SESSION_*` is echoed back in that line.
    #[cfg(windows)]
    fn start_windows(daemon: &Path, pairing: &Path, log: &Path) -> Result<Self> {
        use std::io::{BufRead, BufReader};
        use std::os::windows::process::CommandExt;
        std::fs::create_dir_all(pairing)?;
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let profile = pairing.file_name().and_then(|n| n.to_str()).unwrap_or("device").to_string();
        let token = format!("{:016x}{:016x}", nonce(), nonce());
        let mut command = Command::new(daemon);
        command
            .arg(pairing)
            .env_remove("IT_USB_TCP")
            .env_remove("USBMUXD_SOCKET_ADDRESS")
            .env_remove("USBMUXD_QEMU_ADDR")
            .env("LTW_SESSION_PROFILE", &profile)
            .env("LTW_SESSION_TOKEN", &token)
            .env("LTW_SESSION_GENERATION", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(log)?)
            .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        // Its DLLs sit beside it; Windows looks there first.
        let mut child = command.spawn().map_err(|e| Error(format!("The guest USB bridge couldn't start: {e}")))?;
        lighttouch_core::job::bind_to_this_process(&child);
        let control = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| Error("The guest USB bridge has no output".into()))?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("usb-bridge-output".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .ok();
        let mut bridge =
            Self { child, dir: PathBuf::new(), control, mux_socket: PathBuf::new(), guest_socket: PathBuf::new() };
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => {
                    let Some(json) = line.strip_prefix("DAEMON ") else { continue };
                    let ready: serde_json::Value =
                        serde_json::from_str(json).map_err(|e| Error(format!("The guest USB bridge said: {e}")))?;
                    let port = |key: &str| ready.get(key).and_then(|v| v.as_u64()).filter(|p| (1..=65535).contains(p));
                    let ours = ready.get("format").and_then(|v| v.as_str()) == Some("lighttouch-windows-usbmux")
                        && ready.get("token").and_then(|v| v.as_str()) == Some(token.as_str());
                    match (ours, port("client_port"), port("guest_port")) {
                        (true, Some(client), Some(guest)) => {
                            bridge.mux_socket = PathBuf::from(format!("127.0.0.1:{client}"));
                            bridge.guest_socket = PathBuf::from(format!("127.0.0.1:{guest}"));
                            return Ok(bridge);
                        }
                        _ => break,
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Ok(Some(status)) = bridge.child.try_wait() {
                        return Err(Error(format!("The guest USB bridge exited ({status}); see {}", log.display())));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        bridge.stop();
        Err(Error(format!("The guest USB bridge didn't start; see {}", log.display())))
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn stop(&mut self) {
        // Windows: it retires on "stop" (or when its control pipe closes).
        #[cfg(windows)]
        if let Some(mut control) = self.control.take() {
            use std::io::Write;
            let _ = control.write_all(b"stop\n").and_then(|()| control.flush());
        }
        if self.alive() {
            #[cfg(unix)]
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            }
            // A daemon that never saw the guest exits on SIGTERM in ~100 ms. Once the guest has connected it can't:
            // the engine's end of the link stays open after the runtime is destroyed, and the daemon's device
            // teardown waits out a 30 s USB read, so it gets killed whatever the grace. A long one only held up
            // every Stop (it was 2 s, more than iOS takes to shut down).
            #[cfg(unix)]
            let grace = Duration::from_millis(300);
            #[cfg(not(unix))]
            let grace = Duration::from_secs(2);
            let deadline = Instant::now() + grace;
            while self.alive() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // Only the files we expect; anything else stays as evidence.
        if !self.dir.as_os_str().is_empty() {
            for name in ["mux.sock", "qemu.sock", "daemon.pid"] {
                let _ = std::fs::remove_file(self.dir.join(name));
            }
            let _ = std::fs::remove_dir(&self.dir);
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop();
    }
}

const PREFIX: &str = "lighttouch-usb-";

/// Retire bridges whose worker died without stopping them (killed, or crashed): remove their folders, and stop
/// their daemon if it still runs, which happens where the daemon can't be tied to its parent's life (macOS).
#[cfg(unix)]
fn sweep(daemon: &Path) {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(owner) = name.to_str().and_then(|n| n.strip_prefix(PREFIX)).and_then(|n| n.split('-').next()) else {
            continue;
        };
        let Ok(owner) = owner.parse::<i32>() else { continue };
        if owner <= 0 || alive(owner) {
            continue;
        }
        let dir = entry.path();
        let pid = std::fs::read_to_string(dir.join("daemon.pid")).ok().and_then(|p| p.trim().parse::<i32>().ok());
        // Only a process that is still this daemon: a reused pid belongs to something else.
        if let Some(pid) = pid.filter(|&pid| pid > 0 && alive(pid) && runs(pid, daemon)) {
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        for name in ["mux.sock", "qemu.sock", "daemon.pid"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let _ = std::fs::remove_dir(&dir);
    }
}

#[cfg(unix)]
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// Whether `pid` runs the executable at `path`.
#[cfg(unix)]
fn runs(pid: i32, path: &Path) -> bool {
    let Ok(expected) = path.canonicalize() else { return false };
    #[cfg(target_os = "linux")]
    let actual = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    #[cfg(target_os = "macos")]
    let actual = {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        (n > 0).then(|| {
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..n as usize]))
        })
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let actual: Option<PathBuf> = None;
    actual.and_then(|a| a.canonicalize().ok()).is_some_and(|a| a == expected)
}

#[cfg(unix)]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

fn nonce() -> u64 {
    let mut bytes = [0u8; 8];
    let _ = openssl::rand::rand_bytes(&mut bytes);
    u64::from_le_bytes(bytes)
}

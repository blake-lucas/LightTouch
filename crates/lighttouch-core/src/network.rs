//! The guest's web proxy (`Portable/network`, shared with Android): one per running device, in the worker. The engine
//! forwards the guest's `10.0.2.100:3128` to its private socket. Direct internet connects to websites with modern TLS
//! and the host's trusted roots; the Wayback Machine route fetches dated captures. Its CA (the identity the guest is
//! told to trust) lives in the device's private folder and never leaves it; only the public certificate goes in.
use crate::ffi::{Error, Result, chars, cpath, cstring};
use crate::runtime::NetworkMode;
use lighttouch_sys as sys;
use serde::{Deserialize, Serialize};
use std::ffi::c_char;
use std::path::{Path, PathBuf};

/// Route, archive date and LAN access, as the proxy and the engine's NAT apply them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub mode: NetworkMode,
    /// `yyyyMMdd`, a calendar day; used by the archive route.
    pub archive_date: String,
    pub local_network: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self { mode: NetworkMode::Off, archive_date: "20090909".into(), local_network: false }
    }
}

/// The proxy's counters for this session. Connections count as soon as they reach it, before any HTTP or TLS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    pub connections: u64,
    pub active: u64,
    /// Finished connections, errors included.
    pub completed: u64,
    pub failed: u64,
    /// "Idle — no web connections in progress" and the like.
    pub summary: String,
    pub last_result: String,
    pub last_result_error: bool,
}

pub struct Proxy {
    raw: *mut sys::lt_net_proxy,
}

// The backend locks its own state; configure, activity and serve may run on any thread.
unsafe impl Send for Proxy {}
unsafe impl Sync for Proxy {}

fn failed(buffer: &[c_char]) -> Error {
    Error::new(sys::LT_IO, chars(buffer))
}

impl Proxy {
    /// A proxy for one device. `directory` is its private folder (created owner-only), holding the CA it reuses on
    /// every start. Networking off makes no proxy.
    pub fn create(directory: &Path, config: &NetworkConfig) -> Result<Self> {
        let mode = config.mode.raw();
        if mode == 0 {
            return Err(Error::invalid("Networking is off"));
        }
        let (dir, date) = (cpath(directory)?, cstring(&config.archive_date)?);
        let mut error = [0 as c_char; 512];
        let raw = unsafe {
            sys::lt_net_create(
                dir.as_ptr(),
                mode,
                date.as_ptr(),
                config.local_network as u32,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if raw.is_null() { Err(failed(&error)) } else { Ok(Self { raw }) }
    }

    /// Applies to requests the guest makes from now on; in-flight ones keep theirs. The engine's NAT follows
    /// separately (`Runtime::network_policy`).
    pub fn configure(&self, config: &NetworkConfig) -> Result<()> {
        let date = cstring(&config.archive_date)?;
        let mut error = [0 as c_char; 512];
        let status = unsafe {
            sys::lt_net_configure(
                self.raw,
                config.mode.raw(),
                date.as_ptr(),
                config.local_network as u32,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if status == 0 { Ok(()) } else { Err(failed(&error)) }
    }

    /// Serve the engine's forward on the Unix socket at `path` (made owner-only; removed when the proxy goes).
    pub fn listen(&self, path: &Path) -> Result<()> {
        let path = cpath(path)?;
        let mut error = [0 as c_char; 512];
        let status = unsafe { sys::lt_net_listen(self.raw, path.as_ptr(), error.as_mut_ptr(), error.len()) };
        if status == 0 { Ok(()) } else { Err(failed(&error)) }
    }

    /// The public CA certificate (DER) for the guest's trust store.
    pub fn certificate(&self) -> Vec<u8> {
        let size = unsafe { sys::lt_net_certificate(self.raw, std::ptr::null_mut(), 0) };
        let mut der = vec![0u8; size];
        unsafe { sys::lt_net_certificate(self.raw, der.as_mut_ptr(), der.len()) };
        der
    }

    /// The CA's SHA-256 fingerprint, `AB:CD:…`.
    pub fn fingerprint(&self) -> String {
        let mut out = [0 as c_char; 128];
        unsafe { sys::lt_net_fingerprint(self.raw, out.as_mut_ptr(), out.len()) };
        chars(&out)
    }

    pub fn activity(&self) -> Activity {
        let mut a: sys::lt_net_activity = unsafe { std::mem::zeroed() };
        unsafe { sys::lt_net_activity_read(self.raw, &mut a) };
        Activity {
            connections: a.connections,
            active: a.active,
            completed: a.completed,
            failed: a.failed,
            summary: chars(&a.summary),
            last_result: chars(&a.last_result),
            last_result_error: a.last_result_error != 0,
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        // Closes the listener and waits for connections in progress (their sockets are shut down first).
        unsafe { sys::lt_net_destroy(self.raw) }
    }
}

/// A real calendar day in `yyyyMMdd` from 1996 on, by the backend's rule (no upper bound: the app adds "not after
/// today").
pub fn valid_date(date: &str) -> bool {
    cstring(date).is_ok_and(|d| unsafe { sys::lt_net_valid_date(d.as_ptr()) } != 0)
}

/// Where OpenSSL finds the host's trusted roots for the proxy's upstream TLS, when its built-in defaults may have
/// none: packaged builds compile OpenSSL in, whose default folder doesn't exist on users' machines. The result goes
/// in `SSL_CERT_FILE` before the proxy is made. `None` leaves OpenSSL's defaults (or a user's own `SSL_CERT_FILE` /
/// `SSL_CERT_DIR`) alone. `cache` is a private folder for an exported copy (macOS, Windows).
pub fn trust_roots(cache: &Path) -> Option<PathBuf> {
    if std::env::var_os("SSL_CERT_FILE").is_some() || std::env::var_os("SSL_CERT_DIR").is_some() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = cache;
        // Debian/Ubuntu/Arch, Fedora/RHEL, openSUSE, Alpine and others.
        [
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
            "/etc/ssl/cert.pem",
        ]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
    }
    #[cfg(target_os = "macos")]
    {
        // The system's root store, as Keychain Access shows it; /etc/ssl/cert.pem (Apple's LibreSSL bundle) if that
        // fails.
        let out = std::process::Command::new("/usr/bin/security")
            .args(["find-certificate", "-a", "-p", "/System/Library/Keychains/SystemRootCertificates.keychain"])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success() && o.stdout.windows(27).any(|w| w == b"-----BEGIN CERTIFICATE-----"));
        let exported = out.and_then(|o| {
            let path = cache.join("system-roots.pem");
            std::fs::create_dir_all(cache).and_then(|_| write_atomically(&path, &o.stdout)).ok().map(|_| path)
        });
        exported.or_else(|| Some(PathBuf::from("/etc/ssl/cert.pem")).filter(|p| p.is_file()))
    }
    #[cfg(windows)]
    {
        let path = cache.join("system-roots.pem");
        std::fs::create_dir_all(cache).ok()?;
        let target = cpath(&path).ok()?;
        let mut error = [0 as c_char; 256];
        let count = unsafe { sys::lt_net_export_system_roots(target.as_ptr(), error.as_mut_ptr(), error.len()) };
        if count <= 0 {
            eprintln!("network: system roots: {}", chars(&error));
        }
        (count > 0).then_some(path)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = cache;
        None
    }
}

#[cfg(target_os = "macos")]
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let partial = path.with_extension("pem.partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(partial, path)
}

/// A private folder for the proxy's socket, removed on drop. Socket paths are limited to 104 bytes (macOS), so it
/// is a short one in the temporary folder rather than in the data folder.
pub struct SocketDir {
    pub dir: PathBuf,
    pub socket: PathBuf,
}

const SOCKET_PREFIX: &str = "lighttouch-net-";

impl SocketDir {
    pub fn create() -> Result<Self> {
        sweep();
        let io = |e: std::io::Error| Error::new(sys::LT_IO, format!("proxy socket folder: {e}"));
        let base = std::env::temp_dir();
        #[cfg(unix)]
        let base = if base.as_os_str().len() > 70 { PathBuf::from("/tmp") } else { base };
        let mut attempt = 0u64;
        loop {
            let nonce = (crate::monotonic_ns() ^ (attempt << 40)) as u32;
            let dir = base.join(format!("{SOCKET_PREFIX}{}-{nonce:08x}", std::process::id()));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&dir) {
                Ok(()) => {
                    let socket = dir.join("proxy.sock");
                    if socket.as_os_str().len() >= 104 {
                        let _ = std::fs::remove_dir(&dir);
                        return Err(Error::new(sys::LT_IO, "The temporary folder's path is too long for a socket"));
                    }
                    return Ok(Self { dir, socket });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 8 => attempt += 1,
                Err(e) => return Err(io(e)),
            }
        }
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Remove socket folders left by workers that died without cleaning up.
fn sweep() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(SOCKET_PREFIX))
            .and_then(|n| n.split('-').next())
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() || process_alive(pid) {
            continue;
        }
        let _ = std::fs::remove_file(entry.path().join("proxy.sock"));
        let _ = std::fs::remove_dir(entry.path());
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    pid > 0
        && pid <= i32::MAX as u32
        && unsafe {
            libc::kill(pid as i32, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    unsafe { CloseHandle(handle) };
    true
}

/// Standard base64 with padding, for guest operation arguments.
pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n =
            (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected);
        }
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn backend_calendar() {
        assert!(valid_date("19960101"));
        assert!(valid_date("20000229"));
        assert!(!valid_date("19951231"));
        assert!(!valid_date("19000229"));
        assert!(!valid_date("20090230"));
        assert!(!valid_date("2009090"));
        assert!(!valid_date("2009-9-9"));
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lt-net-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn off_makes_no_proxy() {
        let dir = temp("off");
        assert!(Proxy::create(&dir, &NetworkConfig::default()).is_err());
        assert!(!dir.exists(), "no identity is created while networking is off");
    }

    /// A device's proxy identity is made once and kept, on every platform (Windows saves it with its own calls).
    #[test]
    fn proxy_identity_is_saved_and_reused() {
        let dir = temp("identity");
        let config = NetworkConfig { mode: NetworkMode::Direct, ..Default::default() };
        let proxy = Proxy::create(&dir, &config).unwrap();
        let fingerprint = proxy.fingerprint();
        assert!(!fingerprint.is_empty() && !proxy.certificate().is_empty());
        drop(proxy);
        assert!(dir.join("proxy-ca.pem").is_file());
        let again = Proxy::create(&dir, &config).unwrap();
        assert_eq!(again.fingerprint(), fingerprint, "the device keeps its CA");
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The identity persists per device, and a request the policy refuses (host loopback) is counted without any
    /// outbound traffic.
    #[cfg(unix)]
    #[test]
    fn proxy_identity_and_activity() {
        use std::io::{Read, Write};
        use std::os::unix::fs::PermissionsExt;
        let dir = temp("proxy");
        let config = NetworkConfig { mode: NetworkMode::Direct, ..Default::default() };
        let proxy = Proxy::create(&dir, &config).unwrap();
        let der = proxy.certificate();
        assert!(der.len() > 100 && der[0] == 0x30);
        let fingerprint = proxy.fingerprint();
        assert_eq!(fingerprint.len(), 32 * 3 - 1);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);

        let socket = SocketDir::create().unwrap();
        proxy.listen(&socket.socket).unwrap();
        assert_eq!(proxy.activity().connections, 0);
        let mut stream = std::os::unix::net::UnixStream::connect(&socket.socket).unwrap();
        stream.write_all(b"GET http://127.0.0.1:9/ HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        let activity = proxy.activity();
        assert_eq!((activity.connections, activity.completed, activity.failed), (1, 1, 1));
        assert!(activity.last_result_error);

        // Live changes validate; an invalid date is refused and the old configuration stays.
        let bad = NetworkConfig { mode: NetworkMode::Archive, archive_date: "20090230".into(), local_network: false };
        assert!(proxy.configure(&bad).is_err());
        let archive = NetworkConfig { archive_date: "20090909".into(), ..bad };
        proxy.configure(&archive).unwrap();
        drop(proxy);
        assert!(!socket.socket.exists());

        let again = Proxy::create(&dir, &config).unwrap();
        assert_eq!(again.fingerprint(), fingerprint, "the device keeps its CA");
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

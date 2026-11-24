//! App management on a running guest: what it is, which apps it has, its newest crash report, and installing an
//! IPA (staged over AFC, then the installation proxy). Ported from the Android app service (lt_appservice.c).
use crate::afc::{Afc, Mode};
use crate::instproxy::{self, InstallStatus, InstalledApp};
use crate::lockdown::{Conn, Lockdown};
use crate::usbmux::{Mux, MuxDevice};
use crate::{Error, Result};
use lighttouch_catalog::Target;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const AFC: &str = "com.apple.afc";
const INSTPROXY: &str = "com.apple.mobile.installation_proxy";
const CRASH_REPORTS: &str = "com.apple.crashreportcopymobile";
const SYSLOG: &str = "com.apple.syslog_relay";
/// What the system log keeps of a launch: the lines that explain one (dyld's complaints, SpringBoard or the kernel
/// killing an app, a missing service). The Android app service keeps the same.
const LOG_KEYWORDS: &[&str] = &[
    "dyld",
    "libobjc",
    "SpringBoard",
    "backboardd",
    "launchd",
    "Jetsam",
    "watchdog",
    "EXC_",
    "Termination",
    "code signature",
    "sandbox",
    "assert",
    "abnormal",
    "not found",
    "no such file",
    "unable to",
    "failed",
    "error",
];
const STAGING: &str = "PublicStaging";
/// One AFC write each, answered before the next: at 64 KiB the iPod touch 2G spent ~20 ms of every write waiting.
const UPLOAD_CHUNK: usize = 1 << 20;
/// AFC sessions writing the IPA at once (LT_UPLOAD_AFC overrides, for A/B; 1 is the old single stream). Each 1 MiB
/// write waits for the guest to store it; several in flight keep the USB link busy meanwhile (docs/STORAGE.md).
const UPLOAD_SESSIONS: u64 = 8;
/// No status and no unpacking progress for this long: give up.
const INSTALL_IDLE: Duration = Duration::from_secs(90);
/// Plus the unpacked size at `UNPACK_FLOOR_BPS`.
const INSTALL_ABSOLUTE: Duration = Duration::from_secs(600);
const UNPACK_FLOOR_BPS: u64 = 256 << 10;
const SPACE_POLL: Duration = Duration::from_secs(2);
const TIMEOUT: Duration = Duration::from_secs(20);
/// An AFC service's replies: the iPhone 5's afcd, launched on demand, can take longer than `TIMEOUT` to answer its
/// first request on a busy host (seen: no answer in 20 s while the emulated A6 started it).
const AFC_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq)]
pub enum InstallProgress {
    Uploading {
        sent: u64,
        total: u64,
    },
    /// The guest has the package and is installing it; from here the install can't be stopped.
    Submitted,
    Installing {
        status: String,
        percent: Option<u64>,
    },
    /// Files landing while the guest unpacks (it reports nothing itself then).
    Unpacking {
        done: u64,
        total: u64,
    },
}

pub struct Guest {
    mux: Mux,
    device: MuxDevice,
    lockdown: Lockdown,
}

impl Guest {
    /// Find the guest and open a lockdown session, retrying for `window` while it is still starting.
    pub fn connect(socket: &Path, window: Duration) -> Result<Self> {
        let mux = Mux::new(socket);
        let deadline = Instant::now() + window;
        loop {
            let attempt = mux.devices().and_then(|list| {
                let device = list.into_iter().next().ok_or_else(|| {
                    Error("The guest isn't connected yet; try again once iOS has finished starting.".into())
                })?;
                let lockdown = Lockdown::session(&mux, &device, TIMEOUT)?;
                Ok(Self { mux: mux.clone(), device, lockdown })
            });
            match attempt {
                Ok(guest) => return Ok(guest),
                Err(e) if Instant::now() + Duration::from_millis(500) > deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    }

    pub fn target(&mut self) -> Result<Target> {
        let product = self.lockdown.string_value("ProductType")?;
        let ios = self.lockdown.string_value("ProductVersion")?;
        Target::of(&product, &ios).ok_or_else(|| Error(format!("App installs don't support this device ({product}).")))
    }

    /// Start a service. lockdownd drops a session left idle (as across a long upload); a fresh one recovers it.
    /// When iOS restarts or its USB link drops, the guest comes back under a new usbmux id: wait for it.
    fn service(&mut self, name: &str, timeout: Duration) -> Result<Conn> {
        let mut last = Error(format!("couldn't start {name}"));
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(500 * attempt));
                if let Err(e) = self.reattach(Duration::from_secs(30)) {
                    last = e;
                    continue;
                }
                match Lockdown::session(&self.mux, &self.device, TIMEOUT) {
                    Ok(lockdown) => self.lockdown = lockdown,
                    Err(e) => {
                        last = e;
                        continue;
                    }
                }
            }
            match self.lockdown.service(name, timeout) {
                Ok(conn) => return Ok(conn),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// A service connection for this crate's other modules (devinstall).
    pub(crate) fn service_conn(&mut self, name: &str, timeout: Duration) -> Result<Conn> {
        self.service(name, timeout)
    }

    /// Look the guest up again by its UDID, waiting up to `window` for it to reattach.
    fn reattach(&mut self, window: Duration) -> Result<()> {
        let deadline = Instant::now() + window;
        loop {
            let found = self.mux.devices()?.into_iter().find(|d| d.udid == self.device.udid);
            if let Some(device) = found {
                self.device = device;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error("The guest disconnected and didn't come back.".into()));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    pub fn installed(&mut self) -> Result<Vec<InstalledApp>> {
        let mut conn = self.service(INSTPROXY, Duration::from_secs(90))?;
        instproxy::browse_user_apps(&mut conn)
    }

    /// Remove `bundle_id` and its data from the guest.
    pub fn uninstall(&mut self, bundle_id: &str) -> Result<()> {
        let mut conn = self.service(INSTPROXY, Duration::from_secs(120))?;
        instproxy::uninstall(&mut conn, bundle_id)
    }

    /// The newest crash report: its file name and text (the readable report inside a .plist when it is one).
    pub fn last_crash(&mut self) -> Result<Option<(String, String)>> {
        self.last_crash_named("")
    }

    /// The newest crash report whose name contains `part` (developer runs: `lighttouch guest SOCKET crash PART`).
    pub fn last_crash_named(&mut self, part: &str) -> Result<Option<(String, String)>> {
        let mut afc = Afc::new(self.service(CRASH_REPORTS, AFC_TIMEOUT)?);
        let newest = afc
            .read_dir("/")?
            .into_iter()
            .filter(|n| {
                n.contains(part)
                    && !n.starts_with('.')
                    && !n.starts_with("LatestCrash")
                    && (n.ends_with(".plist") || n.ends_with(".ips") || n.ends_with(".crash"))
            })
            // Names end in _YYYY-MM-DD-HHMMSS_device.ext: compare from the date.
            .max_by(|a, b| date_part(a).cmp(date_part(b)));
        let Some(name) = newest else { return Ok(None) };
        let handle = afc.open(&format!("/{name}"), Mode::ReadOnly)?;
        let mut text = Vec::new();
        while text.len() < 256 << 10 {
            let chunk = afc.read(handle, 16 << 10)?;
            if chunk.is_empty() {
                break;
            }
            text.extend_from_slice(&chunk);
        }
        let _ = afc.close(handle);
        let text = String::from_utf8_lossy(&text).into_owned();
        Ok(Some((name, report_body(&text))))
    }

    /// Up to `window` of the guest's own system log, trimmed to the lines that explain a launch. It is what an app
    /// writes in the seconds before iOS writes its crash report, and the only record of a launch that never got far
    /// enough to crash. `cancelled` returning true ends it early.
    pub fn system_log(&mut self, window: Duration, cancelled: &dyn Fn() -> bool) -> Result<String> {
        // Its read timeout is how often the deadline and `cancelled` are checked.
        let mut conn = self.service(SYSLOG, Duration::from_secs(1))?;
        let deadline = Instant::now() + window;
        let mut text = Vec::new();
        let mut chunk = [0u8; 16 << 10];
        while Instant::now() < deadline && !cancelled() {
            match conn.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    text.extend_from_slice(&chunk[..n]);
                    // Bounded: an idle launch prints little, a busy one prints faster than anything reads it.
                    if text.len() > 512 << 10 {
                        text.drain(..text.len() - (256 << 10));
                    }
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(Error(format!("the system log: {e}"))),
            }
        }
        // Lines end in a newline, each message in a NUL.
        let text = String::from_utf8_lossy(&text);
        let lines: Vec<&str> = text.split(['\0', '\n']).map(str::trim).filter(|l| !l.is_empty()).collect();
        let explained: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| {
                let lower = l.to_lowercase();
                LOG_KEYWORDS.iter().any(|k| lower.contains(&k.to_lowercase()))
            })
            .collect();
        let chosen = if explained.is_empty() {
            &lines[lines.len().saturating_sub(40)..]
        } else {
            &explained[explained.len().saturating_sub(60)..]
        };
        Ok(chosen.join("\n"))
    }

    /// The guest's raw system log into `out` until `cancelled` (developer runs: `lighttouch run --syslog FILE`).
    pub fn stream_system_log(&mut self, out: &mut dyn std::io::Write, cancelled: &dyn Fn() -> bool) -> Result<()> {
        let mut conn = self.service(SYSLOG, Duration::from_secs(1))?;
        let mut chunk = [0u8; 16 << 10];
        while !cancelled() {
            match conn.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let text: Vec<u8> = chunk[..n].iter().map(|&b| if b == 0 { b'\n' } else { b }).collect();
                    out.write_all(&text)?;
                    out.flush()?;
                }
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(Error(format!("the system log: {e}"))),
            }
        }
        Ok(())
    }

    /// Install `ipa` (checked by `lighttouch_catalog::ipa::prepare`, which also gives its `unpacked` size) and
    /// confirm `bundle_id` is then listed. `progress` returns false to cancel, which only works until the package
    /// is submitted.
    pub fn install(
        &mut self,
        ipa: &Path,
        bundle_id: &str,
        unpacked: Option<u64>,
        progress: &mut dyn FnMut(InstallProgress) -> bool,
    ) -> Result<InstalledApp> {
        let mut afc = Afc::new(self.service(AFC, AFC_TIMEOUT)?);
        let total = std::fs::metadata(ipa)?.len();
        let available = afc.free_bytes()?;
        let needed = total * 2 + (16 << 20);
        if available < needed {
            return Err(Error(format!(
                "The guest needs {} MB free for this app; it has {} MB.",
                needed >> 20,
                available >> 20
            )));
        }
        afc.make_dir(STAGING)?;
        let remote = format!("{STAGING}/lighttouch-{:016x}.ipa", nonce());
        let result = self.stage_and_install(&mut afc, ipa, &remote, total, unpacked, progress);
        let _ = afc.remove(&remote);
        result?;
        // The status alone isn't proof: check the installed list.
        let apps = self.installed()?;
        apps.into_iter()
            .find(|a| a.bundle_id == bundle_id)
            .ok_or_else(|| Error("The guest reported the install complete, but the app isn't in its list.".into()))
    }

    /// Copy `ipa` to `remote` over `UPLOAD_SESSIONS` AFC sessions at once, each writing its own contiguous part of
    /// the file: one session waits for the guest's reply to every 1 MiB write, so several keep the link busy.
    fn upload(
        &mut self,
        afc: &mut Afc,
        ipa: &Path,
        remote: &str,
        total: u64,
        progress: &mut dyn FnMut(InstallProgress) -> bool,
    ) -> Result<()> {
        use std::io::{Seek, SeekFrom};
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        let sessions =
            std::env::var("LT_UPLOAD_AFC").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(UPLOAD_SESSIONS);
        let sessions = sessions.clamp(1, 16).min(total.div_ceil(UPLOAD_CHUNK as u64).max(1));
        let handle = afc.open(remote, Mode::WriteOnly)?;
        // Extra sessions are a speed-up only: one that doesn't start (an iPhone 5's afcd is slow to launch) leaves
        // the upload to the sessions that did.
        let mut others = Vec::new();
        for _ in 1..sessions {
            let Ok(conn) = self.service(AFC, TIMEOUT) else { break };
            let mut extra = Afc::new(conn);
            let Ok(h) = extra.open(remote, Mode::ReadWrite) else { break };
            others.push((extra, h));
        }
        let sessions = others.len() as u64 + 1;
        // Each session takes the next 1 MiB as soon as its last write is answered, so every write lands within a few
        // MiB of the file's end. Fixed parts (LT_UPLOAD_SPLIT=parts, for A/B) had the later sessions write hundreds of
        // MiB past it, and iOS filled that gap before answering: single writes took seconds while the upload began.
        let parts = std::env::var("LT_UPLOAD_SPLIT").is_ok_and(|v| v == "parts");
        let part = total.div_ceil(sessions).div_ceil(UPLOAD_CHUNK as u64) * UPLOAD_CHUNK as u64;
        let (next, sent, abort) = (AtomicU64::new(0), AtomicU64::new(0), AtomicBool::new(false));
        // LT_UPLOAD_TRACE=1: one stderr line per write (session, offset, how long the guest took to answer).
        let trace = std::env::var_os("LT_UPLOAD_TRACE").is_some();
        let began = std::time::Instant::now();
        let write_part = |afc: &mut Afc, handle: u64, index: u64| -> Result<()> {
            let (mut at, end) = if parts { (index * part, ((index + 1) * part).min(total)) } else { (0, total) };
            let mut file = std::fs::File::open(ipa)?;
            let mut buffer = vec![0u8; UPLOAD_CHUNK];
            // Where this session's handle writes next: a fresh handle starts at 0.
            let mut position = 0;
            loop {
                if abort.load(Ordering::Relaxed) {
                    return Err(Error("Cancelled.".into()));
                }
                if !parts {
                    at = next.fetch_add(UPLOAD_CHUNK as u64, Ordering::Relaxed);
                }
                if at >= end {
                    return Ok(());
                }
                let n = (end - at).min(UPLOAD_CHUNK as u64) as usize;
                if position != at {
                    afc.seek(handle, at)?;
                }
                file.seek(SeekFrom::Start(at))?;
                file.read_exact(&mut buffer[..n])?;
                let asked = began.elapsed();
                afc.write(handle, &buffer[..n])?;
                if trace {
                    let done = began.elapsed();
                    eprintln!(
                        "upload-trace {:.3} {index} {at} {:.1}",
                        done.as_secs_f64(),
                        (done - asked).as_secs_f64() * 1e3
                    );
                }
                at += n as u64;
                position = at;
                sent.fetch_add(n as u64, Ordering::Relaxed);
            }
        };
        let first = &mut *afc;
        let run = |afc: &mut Afc, handle: u64, index: u64| {
            let r = write_part(afc, handle, index);
            if r.is_err() {
                abort.store(true, Ordering::Relaxed);
            }
            r
        };
        let result = std::thread::scope(|scope| {
            let abort_ref = &abort;
            let run = &run;
            let mut workers = vec![scope.spawn(move || run(first, handle, 0))];
            for (i, (extra, h)) in others.iter_mut().enumerate() {
                workers.push(scope.spawn(move || run(extra, *h, i as u64 + 1)));
            }
            // This thread reports everyone's progress, and stops the others when one fails or the user cancels.
            progress(InstallProgress::Uploading { sent: 0, total });
            while !workers.iter().all(|w| w.is_finished()) {
                std::thread::sleep(Duration::from_millis(100));
                if !progress(InstallProgress::Uploading { sent: sent.load(Ordering::Relaxed), total }) {
                    abort_ref.store(true, Ordering::Relaxed);
                }
            }
            // The first real failure, not the "Cancelled." the other sessions stop with once one fails.
            let mut result = Ok(());
            for w in workers {
                let r = w.join().unwrap_or_else(|_| Err(Error("upload thread panicked".into())));
                let stopped = |r: &Result<()>| matches!(r, Err(e) if e.0 == "Cancelled.");
                if result.is_ok() || (stopped(&result) && r.is_err() && !stopped(&r)) {
                    result = r;
                }
            }
            result
        });
        let _ = progress(InstallProgress::Uploading { sent: sent.load(Ordering::Relaxed), total });
        for (mut extra, h) in others {
            let _ = extra.close(h);
        }
        let closed = afc.close(handle);
        result?;
        closed
    }

    fn stage_and_install(
        &mut self,
        afc: &mut Afc,
        ipa: &Path,
        remote: &str,
        total: u64,
        unpacked_total: Option<u64>,
        progress: &mut dyn FnMut(InstallProgress) -> bool,
    ) -> Result<()> {
        self.upload(afc, ipa, remote, total, progress)?;

        let mut conn = self.service(INSTPROXY, Duration::from_secs(3600))?;
        progress(InstallProgress::Submitted);
        instproxy::submit_install(&mut conn, remote)?;
        // Replies arrive on their own thread so the guest's free space can be watched meanwhile: iOS reports
        // nothing while it unpacks, which takes minutes for a large app.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let status = instproxy::install_status(&mut conn);
                let more = matches!(status, Ok(InstallStatus::Progress { .. }));
                if tx.send(status).is_err() || !more {
                    break;
                }
            }
        });
        let started = Instant::now();
        let deadline = started + INSTALL_ABSOLUTE + Duration::from_secs(unpacked_total.unwrap_or(0) / UNPACK_FLOOR_BPS);
        let mut last_activity = started;
        let free_start = afc.free_bytes().unwrap_or(0);
        let mut free_last = free_start;
        let mut next_poll = started + SPACE_POLL;
        let mut status_now = String::new();
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Ok(InstallStatus::Complete)) => return Ok(()),
                Ok(Ok(InstallStatus::Progress { status, percent })) => {
                    last_activity = Instant::now();
                    status_now = status.clone();
                    progress(InstallProgress::Installing { status, percent });
                }
                Ok(Err(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Error("The guest closed the install without a result.".into()));
                }
            }
            let now = Instant::now();
            if now >= next_poll {
                next_poll = now + SPACE_POLL;
                if let Ok(free) = afc.free_bytes() {
                    if free != free_last {
                        free_last = free;
                        last_activity = now;
                        if let (Some(total), "ExtractingPackage") = (unpacked_total, status_now.as_str()) {
                            progress(InstallProgress::Unpacking {
                                done: free_start.saturating_sub(free).min(total),
                                total,
                            });
                        }
                    }
                }
            }
            if now >= deadline || now - last_activity > INSTALL_IDLE {
                return Err(Error("The install timed out; check the guest before trying again.".into()));
            }
        }
    }
}

fn date_part(name: &str) -> &str {
    name.find("_20").map(|i| &name[i..]).unwrap_or("")
}

/// A .plist crash report wraps the readable report in a `description` string.
fn report_body(text: &str) -> String {
    let Some(start) = text.find("<key>description</key>") else { return text.to_string() };
    let rest = &text[start..];
    let Some(open) = rest.find("<string>") else { return text.to_string() };
    let body = &rest[open + 8..];
    let Some(end) = body.find("</string>") else { return text.to_string() };
    body[..end].replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn nonce() -> u64 {
    let mut bytes = [0u8; 8];
    let _ = openssl::rand::rand_bytes(&mut bytes);
    u64::from_le_bytes(bytes)
}

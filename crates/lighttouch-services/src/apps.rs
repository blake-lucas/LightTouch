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
const STAGING: &str = "PublicStaging";
/// One AFC write each, answered before the next: at 64 KiB the iPod touch 2G spent ~20 ms of every write waiting.
const UPLOAD_CHUNK: usize = 1 << 20;
/// No status and no unpacking progress for this long: give up.
const INSTALL_IDLE: Duration = Duration::from_secs(90);
/// Plus the unpacked size at `UNPACK_FLOOR_BPS`.
const INSTALL_ABSOLUTE: Duration = Duration::from_secs(600);
const UNPACK_FLOOR_BPS: u64 = 256 << 10;
const SPACE_POLL: Duration = Duration::from_secs(2);
const TIMEOUT: Duration = Duration::from_secs(20);

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

    /// The newest crash report: its file name and text (the readable report inside a .plist when it is one).
    pub fn last_crash(&mut self) -> Result<Option<(String, String)>> {
        let mut afc = Afc::new(self.service(CRASH_REPORTS, TIMEOUT)?);
        let newest = afc
            .read_dir("/")?
            .into_iter()
            .filter(|n| {
                !n.starts_with('.')
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
        let mut afc = Afc::new(self.service(AFC, TIMEOUT)?);
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

    fn stage_and_install(
        &mut self,
        afc: &mut Afc,
        ipa: &Path,
        remote: &str,
        total: u64,
        unpacked_total: Option<u64>,
        progress: &mut dyn FnMut(InstallProgress) -> bool,
    ) -> Result<()> {
        let handle = afc.open(remote, Mode::WriteOnly)?;
        let upload = (|| -> Result<()> {
            let mut file = std::fs::File::open(ipa)?;
            let mut buffer = vec![0u8; UPLOAD_CHUNK];
            let mut sent = 0u64;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    return Ok(());
                }
                afc.write(handle, &buffer[..read])?;
                sent += read as u64;
                if !progress(InstallProgress::Uploading { sent, total }) {
                    return Err(Error("Cancelled.".into()));
                }
            }
        })();
        let closed = afc.close(handle);
        upload?;
        closed?;

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

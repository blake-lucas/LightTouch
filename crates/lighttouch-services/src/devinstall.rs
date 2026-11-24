//! Install experiments (docs/STORAGE.md, "Faster installs"), developer tooling only: an app directory, unpacked on the
//! host, uploaded into `PublicStaging` over several AFC connections at once (each its own afcd), then installed with
//! the installation proxy as a Developer package, the way Xcode installs a build. Reached from
//! `lighttouch run ID --install-dir DIR.app --afc N` and `examples/devinstall.rs` (Android, under run-as).
use crate::afc::{Afc, Mode};
use crate::apps::Guest;
use crate::instproxy::{self, InstallStatus};
use crate::{Error, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CHUNK: usize = 1 << 20;

/// What one experiment took.
#[derive(Debug, Default)]
pub struct DevInstallReport {
    pub files: usize,
    pub dirs: usize,
    pub bytes: u64,
    pub skipped_links: usize,
    /// AFC requests sent (make dir, open, write, close), all connections.
    pub requests: u64,
    pub upload_s: f64,
    pub install_s: f64,
    /// What `PublicStaging` held after the install (the staged directory gone: installd moved or removed it).
    pub staging_after: Vec<String>,
}

fn walk(root: &Path, rel: &str, dirs: &mut Vec<String>, files: &mut Vec<(String, PathBuf, u64)>, links: &mut usize) {
    let Ok(read) = std::fs::read_dir(root.join(rel)) else { return };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let child = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_symlink() {
            *links += 1;
        } else if kind.is_dir() {
            dirs.push(child.clone());
            walk(root, &child, dirs, files, links);
        } else if let Ok(meta) = e.metadata() {
            files.push((child, e.path(), meta.len()));
        }
    }
}

impl Guest {
    /// Upload `app` (an unpacked `.app` directory) to `PublicStaging/<name>.app` over `connections` AFC sessions,
    /// biggest files first, then install it as a Developer package. `say` gets progress lines.
    pub fn install_unpacked(
        &mut self,
        app: &Path,
        connections: usize,
        say: &(dyn Fn(String) + Sync),
    ) -> Result<DevInstallReport> {
        let name = app.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !name.ends_with(".app") {
            return Err(Error("--install-dir takes an unpacked .app directory".into()));
        }
        let (mut dirs, mut files, mut links) = (Vec::new(), Vec::new(), 0);
        walk(app, "", &mut dirs, &mut files, &mut links);
        files.sort_by(|a, b| b.2.cmp(&a.2));
        let mut report = DevInstallReport {
            files: files.len(),
            dirs: dirs.len(),
            bytes: files.iter().map(|f| f.2).sum(),
            skipped_links: links,
            ..Default::default()
        };
        let remote = format!("PublicStaging/{name}");
        let requests = Arc::new(AtomicU64::new(0));
        let mut sessions = Vec::new();
        for _ in 0..connections.max(1) {
            sessions.push(self.afc_session()?);
        }
        say("uploading 0%".into());
        let started = Instant::now();
        {
            let afc = &mut sessions[0];
            afc.make_dir("PublicStaging")?;
            let _ = afc.remove(&remote);
            afc.make_dir(&remote)?;
            for d in &dirs {
                afc.make_dir(&format!("{remote}/{d}"))?;
            }
            requests.fetch_add(dirs.len() as u64 + 3, Ordering::Relaxed);
        }
        let queue = Arc::new(Mutex::new(files.into_iter().rev().collect::<Vec<_>>())); // pop() = biggest
        let sent = Arc::new(AtomicU64::new(0));
        let total = report.bytes;
        let failure: Mutex<Option<Error>> = Mutex::new(None);
        std::thread::scope(|scope| {
            for mut afc in sessions.drain(..) {
                let (queue, sent, requests, failure, remote) = (&queue, &sent, &requests, &failure, &remote);
                scope.spawn(move || {
                    let mut buffer = vec![0u8; CHUNK];
                    loop {
                        let Some((rel, path, _)) = queue.lock().unwrap().pop() else { return };
                        let result = (|| -> Result<()> {
                            use std::io::Read;
                            let mut file = std::fs::File::open(&path)?;
                            let handle = afc.open(&format!("{remote}/{rel}"), Mode::WriteOnly)?;
                            requests.fetch_add(2, Ordering::Relaxed);
                            loop {
                                let n = file.read(&mut buffer)?;
                                if n == 0 {
                                    break;
                                }
                                afc.write(handle, &buffer[..n])?;
                                requests.fetch_add(1, Ordering::Relaxed);
                                let before = sent.fetch_add(n as u64, Ordering::Relaxed);
                                if (before * 4 / total.max(1)) != ((before + n as u64) * 4 / total.max(1)) {
                                    say(format!("uploading {}%", (before + n as u64) * 100 / total.max(1) / 25 * 25));
                                }
                            }
                            afc.close(handle)
                        })();
                        if let Err(e) = result {
                            failure.lock().unwrap().get_or_insert(e);
                            queue.lock().unwrap().clear();
                            return;
                        }
                    }
                });
            }
        });
        if let Some(e) = failure.into_inner().unwrap() {
            return Err(e);
        }
        report.upload_s = started.elapsed().as_secs_f64();
        report.requests = requests.load(Ordering::Relaxed);
        say("uploading 100%".into());
        let install_started = Instant::now();
        let mut conn = self.instproxy_session()?;
        instproxy::submit_install_as(&mut conn, &remote, Some("Developer"))?;
        loop {
            match instproxy::install_status(&mut conn)? {
                InstallStatus::Complete => break,
                InstallStatus::Progress { status, percent } => say(format!("{status} {percent:?}")),
            }
        }
        report.install_s = install_started.elapsed().as_secs_f64();
        let mut afc = self.afc_session()?;
        report.staging_after = afc.read_dir("PublicStaging").unwrap_or_default();
        Ok(report)
    }

    fn afc_session(&mut self) -> Result<Afc> {
        Ok(Afc::new(self.service_conn("com.apple.afc", Duration::from_secs(90))?))
    }

    fn instproxy_session(&mut self) -> Result<crate::lockdown::Conn> {
        self.service_conn("com.apple.mobile.installation_proxy", Duration::from_secs(3600))
    }
}

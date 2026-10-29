//! Preparing a device from an IPSW: get the IPSW (download, or a file the user has), the boot ROM when the board
//! needs one, run `firmwarekit create` and follow its progress, then move its output into the library as a new
//! device. Lean on purpose: no backups or retries; if it fails the user prepares again.
use crate::task::Reporter;
use crate::{Finished, Waker};
use lighttouch_catalog::catalog::{CATALOG_JSON, Catalog, default_device_name};
use lighttouch_catalog::download::{self, DownloadError};
use lighttouch_catalog::{Bootrom, GuestTools};
use lighttouch_core::library::{ImportSource, Library};
use lighttouch_core::paths::{DataDirs, Resources};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum IpswSource {
    /// Download it (ipsw.me, the catalog's links, the Internet Archive).
    Download,
    /// A file the user chose; checked against the catalog, used where it is.
    File(PathBuf),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PrepareRequest {
    pub firmware_id: String,
    pub ipsw: IpswSource,
    /// A boot ROM file the user chose; otherwise one is reused or downloaded when the board needs it.
    pub bootrom: Option<PathBuf>,
    pub name: Option<String>,
    /// Install the jailbreak (afc2 + Cydia) during preparation.
    #[serde(default)]
    pub jailbreak: bool,
}

/// Whether this build can prepare devices, and why not.
pub fn availability(resources: &Resources) -> Result<(), String> {
    if !resources.firmwarekit().is_file() {
        return Err("This build of LightTouch has no device preparation tool (firmwarekit).".into());
    }
    if !GuestTools::new(resources.guest_tools()).can_prepare() {
        return Err("This build of LightTouch has no device preparation helpers (guest tools).".into());
    }
    if !resources.engine().is_file() {
        return Err("The emulator engine is missing.".into());
    }
    Ok(())
}

/// A boot ROM already on this computer: one a saved device carries, or one downloaded before.
pub fn known_bootrom(dirs: &DataDirs, devices: &[crate::Device], rom: Bootrom) -> Option<PathBuf> {
    let saved = dirs.root.join("bootroms").join(rom.label());
    std::iter::once(saved)
        .chain(devices.iter().filter_map(|d| d.record.bootrom_path.clone()))
        .find(|p| rom.matches_file(p))
}

const NOTE: &str = "pending.json";

/// The request a preparation keeps in `prepared/pending.json` while it runs, the file locked. Ending normally
/// (done, failed or cancelled) removes it; finding it unlocked at the next start means LightTouch closed mid-way.
struct PendingNote {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl PendingNote {
    fn write(dirs: &DataDirs, request: &PrepareRequest) -> Option<Self> {
        use std::io::Write;
        let path = dirs.prepared().join(NOTE);
        std::fs::create_dir_all(dirs.prepared()).ok()?;
        let mut file = std::fs::File::create(&path).ok()?;
        file.try_lock().ok()?;
        file.write_all(&serde_json::to_vec(request).ok()?).ok()?;
        Some(Self { path, file: Some(file) })
    }
}

impl Drop for PendingNote {
    fn drop(&mut self) {
        drop(self.file.take()); // closed first: Windows won't delete an open file
        let _ = std::fs::remove_file(&self.path);
    }
}

/// At startup: the request of a preparation LightTouch closed during, if any, after clearing what it left (its
/// half-built job folders and firmwarekit's cache). Nothing is touched while another LightTouch still prepares.
pub fn recover_interrupted(dirs: &DataDirs) -> Option<PrepareRequest> {
    use std::io::Read;
    let jobs = dirs.prepared();
    let mut request = None;
    if let Ok(mut file) = std::fs::File::options().read(true).write(true).open(jobs.join(NOTE)) {
        if file.try_lock().is_err() {
            return None;
        }
        let mut text = String::new();
        let _ = file.read_to_string(&mut text);
        request = serde_json::from_str(&text).ok();
        drop(file);
        let _ = std::fs::remove_file(jobs.join(NOTE));
    }
    // Nothing adopts a job folder later, so any left over is from a run that never finished.
    let leftovers: Vec<PathBuf> = std::fs::read_dir(&jobs)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("job-"))
        .map(|e| e.path())
        .collect();
    let cache = dirs.cache().join("firmwarekit");
    if !leftovers.is_empty() || cache.exists() {
        std::thread::spawn(move || {
            leftovers.iter().for_each(|job| remove_tree(job));
            let _ = std::fs::remove_dir_all(cache);
        });
    }
    request
}

pub(crate) struct PrepareContext {
    pub dirs: DataDirs,
    pub resources: Resources,
    pub devices: Vec<crate::Device>,
    pub select: bool,
    pub waker: Waker,
}

pub(crate) fn run(ctx: PrepareContext, request: PrepareRequest, reporter: &Reporter) -> Result<Finished, String> {
    let catalog = Catalog::get();
    let firmware = catalog.find(&request.firmware_id).ok_or("That iOS version isn't in the catalog")?.clone();
    availability(&ctx.resources)?;
    let _note = PendingNote::write(&ctx.dirs, &request);
    let started = Instant::now();
    let mut timings: Vec<(String, f64)> = Vec::new();

    // One bar for the whole preparation: each phase fills a share by its usual time, so the bar never starts over.
    let downloading = matches!(request.ipsw, IpswSource::Download);
    let keybag_from = firmware.recipe.as_ref().and_then(|r| r.keybag_ramdisk_from.clone());
    let seconds = [
        if downloading { 40.0 } else { 8.0 },
        if keybag_from.is_some() { 40.0 } else { 0.0 },
        if firmware.board == "n72ap" { 40.0 } else { 150.0 },
        4.0,
    ];
    let sum: f32 = seconds.iter().sum();
    let mut at = 0.0;
    let [ipsw_band, sibling_band, create_band, adopt_band] = seconds.map(|s| {
        at += s;
        ((at - s) / sum, at / sum)
    });
    // A download's check afterwards takes the last part of its share.
    let fetch_band = |(start, end): (f32, f32), download: bool, phase: &str| {
        let split = start + (end - start) * 0.85;
        match (download, phase == "Checking") {
            (false, _) => (start, end),
            (true, false) => (start, split),
            (true, true) => (split, end),
        }
    };

    // 1. The IPSW.
    let mut cancel_check = |phase: &str, done: u64, total: u64| -> bool {
        let (start, end) = fetch_band(ipsw_band, downloading, phase);
        reporter.band(start, end);
        let text = match phase {
            "Checking" => "Checking the IPSW".to_string(),
            other => match other.strip_prefix("Downloading from ") {
                Some(host) => format!("Downloading the IPSW from {host}"),
                None => other.to_string(),
            },
        };
        reporter.report(&text, done, total)
    };
    let (ipsw, downloaded) = match &request.ipsw {
        IpswSource::File(path) => {
            download::verify_ipsw(&firmware, path, &mut cancel_check).map_err(describe)?;
            (path.clone(), false)
        }
        IpswSource::Download => {
            (download::download_ipsw(&firmware, &ctx.dirs.downloads(), &mut cancel_check).map_err(describe)?, true)
        }
    };
    // 1b. A build whose restore ramdisk has no published key boots its keybag one-shot from a sibling's.
    let sibling = match keybag_from.as_deref() {
        None => None,
        Some(from) => {
            let sib =
                catalog.find(from).ok_or_else(|| format!("{from} (its keybag ramdisk) isn't in the catalog"))?.clone();
            let mut sibling_check = |phase: &str, done: u64, total: u64| -> bool {
                let (start, end) = fetch_band(sibling_band, true, phase);
                reporter.band(start, end);
                let text = match phase {
                    other if other.starts_with("Downloading from ") => format!(
                        "Downloading iOS {} {} for its restore ramdisk",
                        sib.version,
                        &other["Downloading ".len()..]
                    ),
                    "Checking" => format!("Checking iOS {}'s IPSW", sib.version),
                    other => other.to_string(),
                };
                reporter.report(&text, done, total)
            };
            Some(download::download_ipsw(&sib, &ctx.dirs.downloads(), &mut sibling_check).map_err(describe)?)
        }
    };
    timings.push(("Getting the IPSW".into(), started.elapsed().as_secs_f64()));

    // 2. The boot ROM, when the board starts from one.
    let bootrom = match Bootrom::for_board(&firmware.board) {
        None => None,
        Some(rom) => Some(match &request.bootrom {
            Some(path) if rom.matches_file(path) => path.clone(),
            Some(_) => return Err(format!("That boot file isn't the {} boot ROM.", rom.label())),
            None => match known_bootrom(&ctx.dirs, &ctx.devices, rom) {
                Some(path) => path,
                None => {
                    reporter.report("Downloading the boot ROM", 0, 0);
                    let dir = ctx.dirs.root.join("bootroms");
                    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                    let target = dir.join(rom.label());
                    rom.fetch(&target)
                        .map_err(|e| format!("Couldn't download the boot ROM ({e}). Choose the file instead."))?;
                    target
                }
            },
        }),
    };
    if reporter.cancelled() {
        return Err("Cancelled".into());
    }

    // 3. firmwarekit create, into the library's own folder so the result is moved in, not copied.
    let jobs = ctx.dirs.prepared();
    std::fs::create_dir_all(&jobs).map_err(|e| format!("{}: {e}", jobs.display()))?;
    let out = jobs.join(format!("job-{:x}", lighttouch_core::monotonic_ns()));
    let cache = ctx.dirs.cache().join("firmwarekit");
    let catalog_file = ctx.dirs.cache().join("firmware-catalog.json");
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    std::fs::write(&catalog_file, CATALOG_JSON).map_err(|e| e.to_string())?;
    reporter.band(create_band.0, create_band.1);
    let result = run_firmwarekit(
        &ctx,
        &firmware.id,
        &ipsw,
        sibling.as_deref(),
        bootrom.as_deref(),
        &out,
        &cache,
        &catalog_file,
        reporter,
        &mut timings,
        request.jailbreak,
    );
    let _ = std::fs::remove_dir_all(&cache);
    if let Err(e) = result {
        remove_tree(&out);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(out.join("work"));

    // 4. Adopt the output as a new device.
    reporter.band(adopt_band.0, adopt_band.1);
    reporter.report("Adding the device", 0, 0);
    let adopt_started = Instant::now();
    let library = Library::open(&ctx.dirs.library()).map_err(|e| e.to_string())?;
    let taken: Vec<&str> = ctx.devices.iter().map(|d| d.name.as_str()).collect();
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_device_name(&firmware.board, &firmware.build, taken.iter().copied()));
    let tools = GuestTools::new(ctx.resources.guest_tools());
    let source = ImportSource {
        path: out.clone(),
        name: Some(name.clone()),
        bootrom: bootrom.clone(),
        guest_package: tools.for_device(&firmware.board, &firmware.build),
    };
    let record = library
        .adopt_prepared(&source, &mut |phase, done, total| {
            reporter.report(
                if phase == "verifying" { "Checking the new device" } else { "Adding the device" },
                done,
                total,
            )
        })
        .map_err(|e| {
            remove_tree(&out);
            format!("Couldn't add the prepared device: {e}")
        })?;
    timings.push(("Adding the device".into(), adopt_started.elapsed().as_secs_f64()));
    if ctx.select {
        let _ = library.select(&record.id);
    }
    if downloaded {
        let _ = std::fs::remove_file(&ipsw);
    }
    if let Some(sibling) = &sibling {
        let _ = std::fs::remove_file(sibling);
    }
    timings.push(("Total".into(), started.elapsed().as_secs_f64()));
    let _ = &ctx.waker;
    Ok(Finished::DeviceAdded { id: record.id, name, selected: ctx.select, timings })
}

#[allow(clippy::too_many_arguments)]
fn run_firmwarekit(
    ctx: &PrepareContext,
    firmware_id: &str,
    ipsw: &Path,
    sibling: Option<&Path>,
    bootrom: Option<&Path>,
    out: &Path,
    cache: &Path,
    catalog: &Path,
    reporter: &Reporter,
    timings: &mut Vec<(String, f64)>,
    jailbreak: bool,
) -> Result<(), String> {
    let engine = ctx.resources.engine();
    let mut command = Command::new(ctx.resources.firmwarekit());
    command
        .arg("create")
        .arg("--catalog")
        .arg(catalog)
        .arg("--id")
        .arg(firmware_id)
        .arg("--ipsw")
        .arg(ipsw)
        .arg("--out")
        .arg(out)
        .arg("--guest-tools")
        .arg(ctx.resources.guest_tools())
        .arg("--cache")
        .arg(cache)
        .arg("--language")
        .arg(language())
        .arg("--skip-setup");
    if let Some(rom) = bootrom {
        command.arg("--bootrom").arg(rom);
    }
    if jailbreak {
        command.arg("--jailbreak");
    }
    if let Some(sibling) = sibling {
        command.arg("--sibling-ipsw").arg(sibling);
    }
    // firmwarekit boots the engine for its one-shots itself (`firmwarekit oneshot`, this engine library).
    command.env("LTM_QEMU_DYLIB", &engine).env("TMPDIR", ctx.dirs.cache());
    if let Some(dir) = engine.parent() {
        let mut paths = vec![dir.to_path_buf()];
        if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
            paths.extend(std::env::split_paths(&existing));
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            command.env("LD_LIBRARY_PATH", joined);
        }
    }
    std::fs::create_dir_all(ctx.dirs.logs()).ok();
    let log_path = ctx.dirs.logs().join("firmwarekit.log");
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::from(log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    // It ends with LightTouch (and its one-shot helper with it, on stdin closing): the next start clears up.
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| format!("Couldn't start firmwarekit: {e}"))?;
    lighttouch_core::job::bind_to_this_process(&child);
    let stdout = child.stdout.take().unwrap();

    // Follow the event lines; cancellation kills the tool (it cleans up its children).
    let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    });
    let mut weights: Vec<u64> = Vec::new();
    let mut step = 0usize;
    let mut step_name = String::from("Starting");
    let mut step_started = Instant::now();
    let mut error: Option<String> = None;
    let mut done_event = false;
    loop {
        if reporter.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cancelled".into());
        }
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(event) => match event["event"].as_str().unwrap_or("") {
                "begin" => {
                    weights = event["seconds"]
                        .as_array()
                        .map(|a| a.iter().map(|s| (s.as_f64().unwrap_or(1.0).max(1.0) * 1000.0) as u64).collect())
                        .unwrap_or_default();
                }
                "step" => {
                    if step > 0 {
                        timings.push((step_name.clone(), step_started.elapsed().as_secs_f64()));
                    }
                    step = event["index"].as_u64().unwrap_or(1) as usize;
                    step_name = event["name"].as_str().unwrap_or("Working").to_string();
                    step_started = Instant::now();
                    report_step(reporter, &weights, step, 0.0, &step_name);
                }
                "progress" => {
                    let fraction = event["fraction"].as_f64().unwrap_or(0.0);
                    let text = event["detail"].as_str().map(str::to_string).unwrap_or_else(|| step_name.clone());
                    report_step(reporter, &weights, step, fraction, &text);
                }
                "error" => error = event["message"].as_str().map(str::to_string),
                "done" => done_event = true,
                _ => {}
            },
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if step > 0 {
        timings.push((step_name, step_started.elapsed().as_secs_f64()));
    }
    if status.success() && done_event {
        Ok(())
    } else {
        Err(error.unwrap_or_else(|| {
            let tail = std::fs::read_to_string(&log_path)
                .ok()
                .and_then(|t| t.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
                .unwrap_or_default();
            format!("Preparation failed ({status}). {tail}")
        }))
    }
}

/// Overall progress from step weights: steps before the current one count in full.
fn report_step(reporter: &Reporter, weights: &[u64], step: usize, fraction: f64, text: &str) {
    let total: u64 = weights.iter().sum::<u64>().max(1);
    let before: u64 = weights.iter().take(step.saturating_sub(1)).sum();
    let current = weights.get(step.saturating_sub(1)).copied().unwrap_or(0);
    let done = before + (current as f64 * fraction.clamp(0.0, 1.0)) as u64;
    reporter.report(text, done, total);
}

/// The user's language as BCP 47 ("en-US"), for the new system's locale.
fn language() -> String {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            let tag = value.split('.').next().unwrap_or("").replace('_', "-");
            if !tag.is_empty() && tag != "C" && tag != "POSIX" {
                return tag;
            }
        }
    }
    "en-US".into()
}

fn describe(e: DownloadError) -> String {
    match e {
        DownloadError::Cancelled => "Cancelled".into(),
        DownloadError::Failed(m) => m,
    }
}

fn remove_tree(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

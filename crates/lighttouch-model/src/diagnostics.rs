//! Settings › Diagnostics › Export logs, and the preparation pages' Save logs: LightTouch's logs (docs/LOGS.md) and
//! a summary of this computer and its devices in one zip for a problem report, with private paths taken out.
use crate::Device;
use crate::settings::Settings;
use lighttouch_core::logs;
use lighttouch_core::paths::DataDirs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What an export reads, gathered on the UI thread.
#[derive(Clone)]
pub struct ExportRequest {
    pub dirs: DataDirs,
    pub devices: Vec<Device>,
    /// The device whose console and description go in: the running one, else the selected one.
    pub current: Option<String>,
    /// The running device's id and what it is doing ("Running", "Paused").
    pub running: Option<(String, String)>,
    pub settings: Settings,
    /// The GPU LightTouch's own window draws with.
    pub gpu: Option<String>,
    /// The running device's guest tools socket, for its newest crash report.
    pub guest_socket: Option<PathBuf>,
}

/// How much of iOS's console goes in, from its end.
const SERIAL_TAIL: u64 = 1 << 20;
/// How much of the USB bridge's and the controllers' logs goes in, from their ends.
const OTHER_TAIL: u64 = 1 << 20;
/// How long the guest tools get to hand over the newest crash report.
const CRASH_WAIT: Duration = Duration::from_secs(8);

/// "LightTouch-logs-20261120-140322.zip", in local time.
pub fn file_name() -> String {
    let t = lighttouch_core::local_time();
    format!("LightTouch-logs-{:04}{:02}{:02}-{:02}{:02}{:02}.zip", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

/// Write the zip to `dest`. Slow parts (asking iOS for its crash report) are bounded; call it off the UI thread.
pub fn export(request: &ExportRequest, dest: &Path) -> Result<(), String> {
    let logs_dir = request.dirs.logs();
    let mut c = Collected::default();
    let add = |c: &mut Collected, name: &str, bytes: Option<Vec<u8>>, why: &str| match bytes {
        Some(bytes) => c.entries.push((name.to_string(), bytes)),
        None => c.missing.push(format!("{name}: {why}")),
    };

    for (spec, none, one) in [
        (logs::APP, "LightTouch's log couldn't be opened", "LightTouch has opened once since logs began"),
        (logs::ENGINE, "no device has started since logs began", "a device has started once since logs began"),
        (logs::PREPARE, "no device has been prepared since logs began", "one preparation since logs began"),
    ] {
        add(&mut c, &format!("{}.log", spec.name), logs::stitch(&logs_dir, spec.name), none);
        let previous = logs::stitch(&logs_dir, &format!("{}.prev", spec.name));
        add(&mut c, &format!("{}-previous.log", spec.name), previous, one);
    }

    // A device started while another ran logs in a folder of its own (`Model::start`).
    let mut folders: Vec<_> = std::fs::read_dir(&logs_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("device-") && e.path().is_dir())
        .collect();
    folders.sort_by_key(|e| e.file_name());
    for folder in folders {
        let name = folder.file_name().to_string_lossy().into_owned();
        if let Some(bytes) = logs::stitch(&folder.path(), logs::ENGINE.name) {
            c.entries.push((format!("{name}/engine.log"), bytes));
        }
    }

    let current = request.current.as_ref().and_then(|id| request.devices.iter().find(|d| &d.id == id));
    let state = current.map(|d| d.record.state_path.clone());
    add(
        &mut c,
        "guest-serial.log",
        state.as_ref().and_then(|s| logs::read_tail(&s.join("serial.log"), SERIAL_TAIL)),
        "no device selected, or it hasn't started",
    );
    add(
        &mut c,
        "guest-serial-previous.log",
        state.as_ref().and_then(|s| logs::read_tail(&s.join("serial.prev.log"), SERIAL_TAIL)),
        "the device has started once since logs began",
    );

    let crash = request.guest_socket.clone().map(last_crash);
    match crash {
        Some(Ok(Some((name, body)))) => {
            c.entries.push(("guest/last-crash.txt".into(), format!("{name}\n\n{body}").into()))
        }
        Some(Ok(None)) => c.missing.push("guest/last-crash.txt: iOS has no crash reports".into()),
        Some(Err(e)) => c.missing.push(format!("guest/last-crash.txt: {e}")),
        None => c.missing.push("guest/last-crash.txt: the device isn't running".into()),
    }

    if let Some(device) = current {
        let record_dir = device.record.state_path.parent().map(Path::to_path_buf);
        add(
            &mut c,
            "device/record.json",
            record_dir.as_ref().and_then(|d| std::fs::read(d.join("record.json")).ok()),
            "unreadable",
        );
        add(
            &mut c,
            "device/device.lock.json",
            std::fs::read(device.record.base_path.join("device.lock.json")).ok(),
            "unreadable",
        );
    } else {
        c.missing.push("device/: no device selected".into());
    }

    let usbmuxd = newest(&logs_dir, "usbmuxd-", ".log");
    add(
        &mut c,
        "usbmuxd.log",
        usbmuxd.and_then(|p| logs::read_tail(&p, OTHER_TAIL)),
        "the USB bridge hasn't run since logs began",
    );
    add(
        &mut c,
        "controllers.log",
        logs::read_tail(&logs_dir.join("controllers.log"), OTHER_TAIL),
        "LightTouch hasn't looked for controllers this run",
    );

    // Paths with the user's name: the devices' folder (Settings → Storage) and the data folder first (they are
    // usually inside the home folder).
    let mut private = Vec::new();
    if let Some(library) = request.dirs.custom_library() {
        private.push((library.to_path_buf(), "<devices>"));
    }
    private.push((request.dirs.root.clone(), "<data>"));
    if let Some(home) = home_dir() {
        private.push((home, "~"));
    }
    for (_, bytes) in &mut c.entries {
        let text = String::from_utf8_lossy(bytes);
        let scrubbed = logs::scrub(&text, &private);
        if scrubbed != text {
            *bytes = scrubbed.into_bytes();
        }
    }

    let summary = logs::scrub(&summary(request, &c.entries, &c.missing), &private);
    write_zip(dest, std::iter::once(("summary.txt".to_string(), summary.into_bytes())).chain(c.entries))
}

#[derive(Default)]
struct Collected {
    entries: Vec<(String, Vec<u8>)>,
    /// Files left out, each with why.
    missing: Vec<String>,
}

fn write_zip(dest: &Path, entries: impl Iterator<Item = (String, Vec<u8>)>) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("Couldn't save the logs to {}: {e}", dest.display());
    let partial = dest.with_extension("zip.partial");
    let file = std::fs::File::create(&partial).map_err(|e| fail(&e))?;
    let mut zip = zip::ZipWriter::new(file);
    let t = lighttouch_core::local_time();
    let mut options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    if let Ok(when) = zip::DateTime::from_date_and_time(
        t.year.clamp(1980, 2107) as u16,
        t.month as u8,
        t.day as u8,
        t.hour as u8,
        t.minute as u8,
        t.second as u8,
    ) {
        options = options.last_modified_time(when);
    }
    for (name, bytes) in entries {
        zip.start_file(name, options).map_err(|e| fail(&e))?;
        zip.write_all(&bytes).map_err(|e| fail(&e))?;
    }
    zip.finish().map_err(|e| fail(&e))?;
    let _ = std::fs::remove_file(dest); // Windows won't rename over a file
    std::fs::rename(&partial, dest).map_err(|e| fail(&e))
}

/// The newest crash report inside iOS, if its guest tools answer in time.
fn last_crash(socket: PathBuf) -> Result<Option<(String, String)>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = lighttouch_services::apps::Guest::connect(&socket, Duration::from_secs(5))
            .and_then(|mut guest| guest.last_crash())
            .map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    rx.recv_timeout(CRASH_WAIT).unwrap_or_else(|_| Err("the guest tools didn't answer in time".into()))
}

/// The newest file in `dir` named `prefix…suffix`.
fn newest(dir: &Path, prefix: &str, suffix: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix)))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, p)| p)
}

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.as_os_str().len() > 1)
}

fn size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1 << 20 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1 << 20) as f64)
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn summary(request: &ExportRequest, entries: &[(String, Vec<u8>)], missing: &[String]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let t = lighttouch_core::local_time();
    let _ = writeln!(out, "LightTouch logs");
    let _ = writeln!(
        out,
        "Saved {:04}-{:02}-{:02} {:02}:{:02}:{:02} (local time)\n",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    let _ = writeln!(
        out,
        "LightTouch: {} (commit {}), desktop, {}",
        env!("CARGO_PKG_VERSION"),
        lighttouch_core::COMMIT,
        std::env::consts::OS
    );
    for (label, value) in lighttouch_core::host::describe(&request.dirs.root, &request.dirs.library()) {
        let _ = writeln!(out, "{label}: {value}");
    }
    if let Some(gpu) = &request.gpu {
        let _ = writeln!(out, "GPU (LightTouch's window): {gpu}");
    }
    let engine = entries.iter().find(|(n, _)| n == "engine.log").map(|(_, b)| String::from_utf8_lossy(b));
    if let Some(line) = engine.as_deref().and_then(|t| t.lines().find(|l| l.contains("[gles] host GL up:"))) {
        let _ = writeln!(out, "GPU (the engine): {}", line.trim_start_matches("[gles] host GL up:").trim());
    }

    let s = &request.settings;
    let _ = writeln!(out, "\nSettings that change how devices run:");
    let _ = writeln!(out, "  Fewer pauses when apps open: {}", on_off(s.fewer_app_pauses));
    let _ = writeln!(out, "  Headphones: {}", on_off(s.headphones));
    let _ = writeln!(out, "  Microphone: {}", on_off(s.microphone));
    let _ =
        writeln!(out, "  Network: {}", if s.network.enabled { format!("{:?}", s.network.route) } else { "off".into() });
    let _ = writeln!(out, "  Controls: {}", if s.controller_preset.is_some() { "a set is in use" } else { "none" });
    let _ = writeln!(out, "  Detailed frame timing: {}", on_off(s.detailed_timing));
    let _ = writeln!(out, "  Rotation: {:?}", s.rotation);
    let _ = writeln!(out, "  Screen scaling: {:?}, real size: {:?}", s.scaling, s.real_size);

    let _ = writeln!(out, "\nDevices ({}):", request.devices.len());
    for d in &request.devices {
        let lock = std::fs::read_to_string(d.record.base_path.join("device.lock.json")).ok();
        let lock_json = lock.as_deref().and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok());
        let jailbroken = lock.as_deref().map(|t| {
            let t = t.to_ascii_lowercase();
            t.contains("cydia") || t.contains("\"jailbreak\"")
        });
        let prepared_tools =
            lock_json.as_ref().and_then(|j| j["guest_package"]["version"].as_str().map(str::to_string));
        let serial = d
            .record
            .guest_package_path
            .as_deref()
            .and_then(|p| lighttouch_catalog::guest_tools::serial(p, &d.board, &d.build).ok());
        let mut state = Vec::new();
        if d.selected {
            state.push("selected".to_string());
        }
        if let Some((id, phase)) = &request.running
            && id == &d.id
        {
            state.push(phase.to_ascii_lowercase());
        }
        if d.dirty {
            state.push("not shut down cleanly".into());
        }
        let tool = lock_json
            .as_ref()
            .and_then(|j| j["tool"]["version"].as_str().or(j["tool"]["name"].as_str()).map(str::to_string));
        let _ = writeln!(
            out,
            "  - {}: {} ({} {}); jailbroken: {}; guest tools: {}; prepared by firmwarekit {}{}{}",
            d.name,
            d.subtitle,
            d.board,
            d.build,
            match jailbroken {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            },
            serial.map_or("none".to_string(), |s| format!("serial {s}")),
            tool.as_deref().unwrap_or("?"),
            prepared_tools.map(|v| format!(" with guest tools {v}")).unwrap_or_default(),
            if state.is_empty() { String::new() } else { format!("; {}", state.join(", ")) },
        );
    }
    if request.devices.is_empty() {
        let _ = writeln!(out, "  none");
    }

    let _ = writeln!(out, "\nFiles:");
    for (name, bytes) in entries {
        let _ = writeln!(out, "  {name}: {}", size(bytes.len()));
    }
    if !missing.is_empty() {
        let _ = writeln!(out, "\nNot included:");
        for line in missing {
            let _ = writeln!(out, "  {line}");
        }
    }
    let _ = writeln!(
        out,
        "\nLeft out on purpose: your home folder is shown as ~ and LightTouch's data folder as <data>; network settings, \
         proxy identities, USB pairing records, IPSWs, firmware, device storage and apps are never included."
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_export_holds_the_logs_with_private_paths_hidden() {
        let root = std::env::temp_dir().join(format!("lt-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = DataDirs::new(root.clone());
        let logs_dir = dirs.logs();
        let mut engine = logs::CappedLog::start(&logs_dir, logs::ENGINE).unwrap();
        engine.line(format!("[IT_DIRECT] staged iBoot '{}/library/x'", root.display()).as_bytes()).unwrap();
        engine.line(b"[gles] host GL up: 4.6 / Some GPU, present by readback (guest-api=2)").unwrap();
        drop(engine);
        let mut app = logs::CappedLog::start(&logs_dir, logs::APP).unwrap();
        app.line(b"started").unwrap();
        drop(app);
        let request = ExportRequest {
            dirs,
            devices: Vec::new(),
            current: None,
            running: None,
            settings: Settings::default(),
            gpu: Some("Test renderer".into()),
            guest_socket: None,
        };
        let dest = root.join("out.zip");
        export(&request, &dest).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        let names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect();
        assert_eq!(names[0], "summary.txt");
        assert!(names.contains(&"engine.log".to_string()) && names.contains(&"app.log".to_string()), "{names:?}");
        assert!(!names.contains(&"engine-previous.log".to_string()));
        let read = |zip: &mut zip::ZipArchive<std::fs::File>, name: &str| {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut zip.by_name(name).unwrap(), &mut text).unwrap();
            text
        };
        let engine = read(&mut zip, "engine.log");
        assert!(engine.contains("'<data>/library/x'"), "{engine}");
        assert!(!engine.contains(&root.display().to_string()));
        let summary = read(&mut zip, "summary.txt");
        assert!(summary.contains("GPU (the engine): 4.6 / Some GPU"), "{summary}");
        assert!(summary.contains("GPU (LightTouch's window): Test renderer"));
        assert!(summary.contains("engine-previous.log: a device has started once"), "{summary}");
        assert!(file_name().starts_with("LightTouch-logs-") && file_name().ends_with(".zip"));
        let _ = std::fs::remove_dir_all(&root);
    }
}

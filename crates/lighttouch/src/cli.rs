//! Developer commands, mainly for headless tests:
//!
//! ```text
//! lighttouch list
//! lighttouch prepare <firmware id> [--ipsw FILE] [--rom FILE] [--name NAME]
//! lighttouch run <record id> [--seconds N] [--recover] [--shot FILE.ppm] [--script FILE]
//!                [--network direct|archive[:yyyyMMdd]] [--lan] [--no-headphones]
//! ```
//!
//! `run` starts a worker like the app does, prints its events, takes an optional screenshot of the last frame, then
//! shuts iOS down cleanly. A script line is `<seconds> <command json>`, e.g. `40 {"cmd":"button","button":0,"down":true}`
//! or, live, `60 {"cmd":"network","config":{"mode":"archive","archive_date":"20090909","local_network":false}}`.
use lighttouch_core::frames::{FrameReader, segment_name};
use lighttouch_core::library::Library;
use lighttouch_core::network::NetworkConfig;
use lighttouch_core::paths::{DataDirs, Resources, data_root};
use lighttouch_core::protocol::{Command, Event, NetworkArgs, WorkerArgs};
use lighttouch_core::runtime::NetworkMode;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::time::{Duration, Instant};

pub fn handles(command: &str) -> bool {
    matches!(command, "list" | "run" | "prepare")
}

pub fn main(args: &[String]) -> i32 {
    let dirs = DataDirs::new(data_root());
    let result = match args[0].as_str() {
        "list" => list(&dirs),
        "run" => run(&dirs, &args[1..]),
        "prepare" => prepare(dirs, &args[1..]),
        _ => unreachable!(),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn list(dirs: &DataDirs) -> Result<(), String> {
    let library = Library::open(&dirs.library()).map_err(|e| e.to_string())?;
    for r in library.records().map_err(|e| e.to_string())? {
        println!(
            "{}{} {:<28} {} {}{}{}",
            if r.selected { "*" } else { " " },
            r.id,
            r.name,
            r.board,
            r.build,
            if r.dirty { " (needs recovery)" } else { "" },
            if r.guest_package_path.is_some() { "" } else { " (no guest tools)" }
        );
    }
    Ok(())
}

fn run(dirs: &DataDirs, args: &[String]) -> Result<(), String> {
    let record = args.first().ok_or("run needs a record id")?.clone();
    let seconds: f64 = option(args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(120.0);
    let shot = option(args, "--shot").map(PathBuf::from);
    let script = match option(args, "--script") {
        Some(path) => parse_script(&std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?)?,
        None => Vec::new(),
    };
    let frames = FrameReader::create(&segment_name()).map_err(|e| format!("frame memory: {e}"))?;
    let action = match (option(args, "--install"), option(args, "--store")) {
        (Some(path), _) => Some(ServiceAction::Install(PathBuf::from(path))),
        (_, Some(query)) => Some(ServiceAction::Store(query)),
        _ => args.iter().any(|a| a == "--services").then_some(ServiceAction::Report),
    };
    let services = action.is_some();
    let action = action.unwrap_or(ServiceAction::Report);
    let usb = if services { lighttouch_model::usb_args(dirs, &Resources::locate(), &record) } else { None };
    if services && usb.is_none() {
        return Err("--services needs usbmuxd-qemu (LIGHTTOUCH_USBMUXD)".into());
    }
    let network = match option(args, "--network") {
        Some(value) => {
            let (mode, date) = value.split_once(':').unwrap_or((value.as_str(), "20090909"));
            let mode = match mode {
                "off" => NetworkMode::Off,
                "direct" => NetworkMode::Direct,
                "archive" => NetworkMode::Archive,
                _ => return Err("--network is off, direct or archive[:yyyyMMdd]".into()),
            };
            if !lighttouch_core::network::valid_date(date) {
                return Err(format!("{date} isn't a yyyyMMdd date from 1996 on"));
            }
            let config =
                NetworkConfig { mode, archive_date: date.into(), local_network: args.iter().any(|a| a == "--lan") };
            Some(NetworkArgs {
                config,
                directory: dirs.network().join(&record),
                guest_tools: Resources::locate().guest_tools(),
            })
        }
        None => None,
    };
    let worker_args = WorkerArgs {
        library: dirs.library(),
        usb,
        network,
        record,
        engine: Resources::locate().engine(),
        frames: frames.name().to_string(),
        recover: args.iter().any(|a| a == "--recover"),
        audio: !args.iter().any(|a| a == "--no-audio"),
        headphones: !args.iter().any(|a| a == "--no-headphones"),
    };
    let mut child = spawn_worker(&worker_args)?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(event) = Event::parse(&line) {
                if tx.send(event).is_err() {
                    break;
                }
            }
        }
    });
    let started = Instant::now();
    let mut stop_sent = false;
    let mut script = script.into_iter().peekable();
    let mut frame_count = 0u64;
    let mut last_frame_log = Instant::now();
    let mut last_status_log = Instant::now();
    loop {
        while let Some((at, _)) = script.peek() {
            if started.elapsed().as_secs_f64() < *at {
                break;
            }
            let (_, command) = script.next().unwrap();
            send(&mut stdin, &command);
        }
        if !stop_sent && started.elapsed().as_secs_f64() >= seconds {
            // A missing frame is reported, but the guest still gets its shutdown.
            if let Some(Err(e)) = shot.as_ref().map(|path| save_frame(&frames, path)) {
                eprintln!("[{:6.1}] couldn't save the frame: {e}", started.elapsed().as_secs_f64());
            }
            eprintln!("[{:6.1}] requesting shutdown", started.elapsed().as_secs_f64());
            send(&mut stdin, &Command::Stop);
            stop_sent = true;
        }
        // Wake for the next script line on time: lines sent in a burst reach the engine closer together than a
        // person's input would.
        let next = script.peek().map_or(0.1, |(at, _)| (at - started.elapsed().as_secs_f64()).clamp(0.001, 0.1));
        match rx.recv_timeout(Duration::from_secs_f64(next)) {
            Ok(Event::Frame { .. }) => {
                frame_count += 1;
                if last_frame_log.elapsed() >= Duration::from_secs(10) {
                    eprintln!("[{:6.1}] {frame_count} frames", started.elapsed().as_secs_f64());
                    last_frame_log = Instant::now();
                }
            }
            Ok(Event::Status(status)) => {
                if last_status_log.elapsed() >= Duration::from_secs(10) {
                    eprintln!("[{:6.1}] status {:?}", started.elapsed().as_secs_f64(), status);
                    last_status_log = Instant::now();
                }
            }
            Ok(Event::Usb { socket: Some(socket), .. }) => {
                eprintln!("[{:6.1}] usb bridge at {}", started.elapsed().as_secs_f64(), socket.display());
                let (action, scratch) = (action.clone(), dirs.cache().join("services"));
                std::thread::spawn(move || probe_services(socket, started, action, scratch));
            }
            Ok(Event::Exit { clean, dirty, message }) => {
                eprintln!("[{:6.1}] exit clean={clean} dirty={dirty} {message}", started.elapsed().as_secs_f64());
                break;
            }
            Ok(event) => eprintln!("[{:6.1}] {}", started.elapsed().as_secs_f64(), event.to_line().trim_end()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    eprintln!("worker exited with {status}; {frame_count} frames");
    Ok(())
}

pub fn spawn_worker(args: &WorkerArgs) -> Result<Child, String> {
    let exe = lighttouch_core::paths::worker_exe()?;
    std::process::Command::new(exe)
        .arg("worker")
        .arg(serde_json::to_string(args).unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("start the session process: {e}"))
}

fn send(stdin: &mut ChildStdin, command: &Command) {
    let _ = stdin.write_all(command.to_line().as_bytes());
    let _ = stdin.flush();
}

fn parse_script(text: &str) -> Result<Vec<(f64, Command)>, String> {
    let mut lines = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (at, json) = line.split_once(' ').ok_or_else(|| format!("bad script line: {line}"))?;
        let at: f64 = at.parse().map_err(|_| format!("bad time: {line}"))?;
        let command: Command = serde_json::from_str(json).map_err(|e| format!("{e}: {line}"))?;
        lines.push((at, command));
    }
    // In time order, whatever order they were written in (a stable sort keeps same-time lines as written).
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(lines)
}

/// Write the newest frame as a binary PPM.
fn save_frame(frames: &FrameReader, path: &PathBuf) -> Result<(), String> {
    let mut pixels = Vec::new();
    let meta = frames.read(0, &mut pixels).ok_or("no frame to save")?;
    let mut out = format!("P6\n{} {}\n255\n", meta.width, meta.height).into_bytes();
    for row in pixels.chunks(meta.stride as usize).take(meta.height as usize) {
        for px in row[..meta.width as usize * 4].chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    std::fs::write(path, out).map_err(|e| e.to_string())?;
    eprintln!("saved {}x{} frame to {}", meta.width, meta.height, path.display());
    Ok(())
}

/// Prepare a device from an IPSW through the app's own pipeline, printing its progress.
fn prepare(dirs: DataDirs, args: &[String]) -> Result<(), String> {
    use lighttouch_model::prepare::{IpswSource, PrepareRequest};
    let firmware_id = args.first().ok_or("prepare needs a firmware id, e.g. n72ap-7E18")?.clone();
    let request = PrepareRequest {
        firmware_id,
        ipsw: option(args, "--ipsw").map(|p| IpswSource::File(PathBuf::from(p))).unwrap_or(IpswSource::Download),
        bootrom: option(args, "--rom").map(PathBuf::from),
        name: option(args, "--name"),
    };
    let mut model = lighttouch_model::Model::new(dirs, Resources::locate(), std::sync::Arc::new(|| {}));
    lighttouch_model::prepare::availability(&model.resources)?;
    model.prepare(request);
    let mut last = String::new();
    loop {
        model.tick();
        if let Some(finished) = model.take_finished() {
            match finished {
                lighttouch_model::Finished::DeviceAdded { name, id, timings, .. } => {
                    for (stage, seconds) in timings {
                        eprintln!("  {stage:<44} {seconds:7.1} s");
                    }
                    println!("added {name} ({id})");
                    return Ok(());
                }
                lighttouch_model::Finished::Failed { message, .. } => return Err(message),
            }
        }
        if let Some((_, progress)) = model.busy() {
            let percent = progress.fraction().map(|f| format!(" {:.0}%", f * 100.0)).unwrap_or_default();
            let line = format!("{}{percent}", progress.phase);
            if line != last {
                eprintln!("{line}");
                last = line;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// What `run` does over the USB link once it is up.
#[derive(Clone)]
enum ServiceAction {
    Report,
    Install(PathBuf),
    Store(String),
}

/// Once the USB link is up: connect to the guest, report it, its apps and its newest crash report, then install a
/// local IPA or the first Legacy Store result for a query.
fn probe_services(socket: PathBuf, started: Instant, action: ServiceAction, scratch: PathBuf) {
    use lighttouch_catalog::{ipa, store};
    use lighttouch_services::apps::{Guest, InstallProgress};
    let at = || started.elapsed().as_secs_f64();
    let say = |text: String| eprintln!("[{:6.1}] services: {text}", at());
    // Wait for the guest to attach (it boots first), then for its session.
    let mux = lighttouch_services::usbmux::Mux::new(&socket);
    while mux.devices().map(|l| l.is_empty()).unwrap_or(true) {
        if at() > 180.0 {
            return say("the guest never attached".into());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    say("attached".into());
    let mut guest = match Guest::connect(&socket, Duration::from_secs(60)) {
        Ok(guest) => guest,
        Err(e) => return say(e.to_string()),
    };
    let target = match guest.target() {
        Ok(t) => t,
        Err(e) => return say(format!("target: {e}")),
    };
    say(format!("{} iOS {} {} family {}", target.product_type, target.ios, target.arch, target.family));
    match guest.installed() {
        Ok(apps) => {
            say(format!("{} user apps", apps.len()));
            for app in apps {
                eprintln!("    {} {:?} {:?}", app.bundle_id, app.name, app.short_version.or(app.version));
            }
        }
        Err(e) => say(format!("apps: {e}")),
    }
    match guest.last_crash() {
        Ok(Some((name, text))) => {
            say(format!("newest crash {name} ({} bytes)", text.len()));
            if std::env::var_os("LIGHTTOUCH_CRASH_TEXT").is_some() {
                eprintln!("{text}");
            }
        }
        Ok(None) => say("no crash reports".into()),
        Err(e) => say(format!("crash reports: {e}")),
    }
    std::fs::create_dir_all(&scratch).ok();
    let (source, downloaded) = match action {
        ServiceAction::Report => return,
        ServiceAction::Install(path) => (path, None),
        ServiceAction::Store(query) => {
            let apps = match store::search(&query, &target) {
                Ok(apps) => apps,
                Err(e) => return say(format!("store: {e}")),
            };
            say(format!("store: {} results for {query:?}", apps.len()));
            let Some(app) = apps.into_iter().find(|a| a.incompatibility().is_none()) else {
                return say("store: nothing compatible".into());
            };
            say(format!(
                "store: {} {} ({}), {:?} bytes",
                app.name,
                app.version.clone().unwrap_or_default(),
                app.ipa_id,
                app.size
            ));
            match store::versions(&app, &target) {
                Ok(copies) => say(format!(
                    "store: {} versions run here, newest {:?}",
                    copies.len(),
                    copies.first().map(|c| &c.version)
                )),
                Err(e) => say(format!("store: versions: {e}")),
            }
            let mut last = 0;
            match store::download(&app, &target, &scratch, &mut |done, total| {
                let percent = total.map(|t| done * 100 / t.max(1)).unwrap_or(0);
                if percent / 25 != last / 25 {
                    say(format!("downloading {percent}%"));
                }
                last = percent;
                true
            }) {
                Ok(path) => {
                    let dir = path.parent().map(Path::to_path_buf);
                    (path, dir)
                }
                Err(e) => return say(format!("store: download: {e}")),
            }
        }
    };
    let copy = scratch.join("install.ipa");
    let result = ipa::prepare(&source, &copy, &target).map_err(|e| e.to_string()).and_then(|info| {
        say(format!(
            "checked {} {} ({:?}), {} bytes unpacked",
            info.name, info.bundle_id, info.short_version, info.unpacked
        ));
        let mut last = String::new();
        guest
            .install(&copy, &info.bundle_id, Some(info.unpacked), &mut |p| {
                let line = match p {
                    InstallProgress::Uploading { sent, total } => {
                        format!("uploading {}%", sent * 100 / total.max(1) / 25 * 25)
                    }
                    InstallProgress::Submitted => "submitted".into(),
                    InstallProgress::Installing { status, percent } => format!("{status} {percent:?}"),
                    InstallProgress::Unpacking { done, total } => {
                        format!("unpacking {}%", done * 100 / total.max(1) / 25 * 25)
                    }
                };
                if line != last {
                    say(line.clone());
                    last = line;
                }
                true
            })
            .map_err(|e| e.to_string())
    });
    let _ = std::fs::remove_file(&copy);
    if let Some(dir) = downloaded {
        let _ = std::fs::remove_dir_all(dir);
    }
    match result {
        Ok(app) => say(format!("installed {app:?}")),
        Err(e) => say(format!("install failed: {e}")),
    }
}

//! Developer hook for checking the UI where nothing can capture the screen (over ssh on macOS or Windows):
//! `LIGHTTOUCH_UI_SHOT=file.png` saves the window after `LIGHTTOUCH_UI_SHOT_AFTER` seconds (default 3), then closes
//! it. `LIGHTTOUCH_UI_PAGE` (settings, prepare, store, screen-scaling, scaling-full for its full screen preview) opens that page first; `LIGHTTOUCH_UI_START` starts the selected
//! device (`LIGHTTOUCH_UI_FULLSCREEN=1` full screen). `LIGHTTOUCH_UI_SIZE=WxH` sets the window's size, and `LIGHTTOUCH_UI_PAGE=add` opens Add a device,
//! `LIGHTTOUCH_UI_DIALOG=stop` the Stop dialog (`controls` the Controls one), `LIGHTTOUCH_UI_ERROR=text` shows an error,
//! `LIGHTTOUCH_UI_FIRMWARE=id` opens Prepare at that firmware's last step (with `LIGHTTOUCH_UI_IPSW=file`
//! it starts preparing, on the device page, or on its own page with `LIGHTTOUCH_UI_PAGE=prepare`; with
//! `LIGHTTOUCH_UI_PICK=file` it is chosen as from the file dialog). `LIGHTTOUCH_UI_EXPORT=file.zip` exports the logs (Settings › Diagnostics) as the window is
//! saved, and closes once they are written. `LIGHTTOUCH_UI_TRANSFER=export:ID:FILE` or `import:FILE` starts a device
//! export or import (no file dialog). `LIGHTTOUCH_UI_LIBRARY=DIR` asks what to do with DIR as the devices' folder
//! (Settings → Storage), `move:DIR` moves them there. `LIGHTTOUCH_UI_LOG_SCREEN=1` logs the guest screen's size and place in physical
//! pixels each time it changes (Screen scaling checks).
use super::{App, Page};
use eframe::egui;
use lighttouch_model::queue::QueueState;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct DevShot {
    path: PathBuf,
    at: Instant,
    requested: bool,
    saved: Arc<AtomicBool>,
    after: Duration,
    install: Option<PathBuf>,
    /// The queued install, until it ends, and the last state reported.
    installing: Option<(u64, String)>,
}

impl DevShot {
    pub fn from_env() -> Option<Self> {
        let path = PathBuf::from(std::env::var_os("LIGHTTOUCH_UI_SHOT")?);
        let after = std::env::var("LIGHTTOUCH_UI_SHOT_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(3.0);
        let install = std::env::var_os("LIGHTTOUCH_UI_INSTALL").map(PathBuf::from);
        let after = Duration::from_secs_f64(after);
        Some(Self {
            path,
            // An install sets the time once it ends.
            at: Instant::now() + if install.is_some() { Duration::from_secs(86400) } else { after },
            requested: false,
            saved: Arc::new(AtomicBool::new(false)),
            after,
            install,
            installing: None,
        })
    }
}

/// Whether to log where the guest screen is drawn (`LIGHTTOUCH_UI_LOG_SCREEN`).
pub fn log_screen() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LIGHTTOUCH_UI_LOG_SCREEN").is_some())
}

pub fn window_size() -> Option<[f32; 2]> {
    let text = std::env::var("LIGHTTOUCH_UI_SIZE").ok()?;
    let (w, h) = text.split_once('x')?;
    Some([w.parse().ok()?, h.parse().ok()?])
}

pub fn start_page() -> Option<Page> {
    match std::env::var("LIGHTTOUCH_UI_PAGE").ok()?.as_str() {
        "add" => Some(Page::Prepare),
        "settings" => Some(Page::Settings),
        "licenses" => Some(Page::Licenses),
        "network" => Some(Page::Network),
        "carrier" => Some(Page::Carrier),
        "screen-scaling" | "ipad-size" | "scaling-full" => Some(Page::ScreenScaling),
        "prepare" => Some(Page::Prepare),
        "store" => Some(Page::Store),
        _ => None,
    }
}

/// Start the selected device as the window opens, when asked to.
pub fn start(app: &mut App) {
    if std::env::var_os("LIGHTTOUCH_UI_START").is_some() {
        app.model.start(false);
    }
    // LIGHTTOUCH_UI_FULLSCREEN=1 shows the device page full screen (the window is the display under Xvfb).
    if std::env::var_os("LIGHTTOUCH_UI_FULLSCREEN").is_some() {
        app.fullscreen = true;
    }
    // LIGHTTOUCH_UI_PAGE=scaling-full opens Screen scaling's full screen preview.
    if std::env::var("LIGHTTOUCH_UI_PAGE").is_ok_and(|p| p == "scaling-full") {
        app.screen_scaling.fullscreen = Some(Instant::now());
    }
    if let Some(firmware) = std::env::var("LIGHTTOUCH_UI_FIRMWARE")
        .ok()
        .and_then(|id| lighttouch_catalog::catalog::Catalog::get().find(&id).cloned())
    {
        app.prepare.product = Some(firmware.product_type.clone());
        app.prepare.firmware = Some(firmware.id.clone());
        app.prepare.step = super::prepare::Step::Files;
        app.page = Page::Prepare;
        // LIGHTTOUCH_UI_PICK=file chooses it as the file dialog would (its check, then what it is).
        app.prepare.dev_pick = std::env::var_os("LIGHTTOUCH_UI_PICK").map(PathBuf::from);
        // LIGHTTOUCH_UI_IPSW=file also starts preparing from it, and shows the device page meanwhile.
        if let Some(ipsw) = std::env::var_os("LIGHTTOUCH_UI_IPSW").map(PathBuf::from) {
            app.prepare.ipsw_file = Some(ipsw.clone());
            app.model.prepare(lighttouch_model::prepare::PrepareRequest {
                firmware_id: firmware.id.clone(),
                ipsw: lighttouch_model::prepare::IpswSource::File(ipsw),
                bootrom: None,
                name: None,
                jailbreak: false,
                ipas: Vec::new(),
                no_game_center: false,
                unchecked_ipsw: false,
            });
            app.prepare.started = Some(Instant::now());
            app.prepare.step = super::prepare::Step::Progress;
            // LIGHTTOUCH_UI_PAGE=prepare stays on the preparation page (its progress, then its result).
            if start_page() != Some(Page::Prepare) {
                app.page = Page::Device;
            }
        }
    }
    // LIGHTTOUCH_UI_TRANSFER=export:ID:FILE or import:FILE starts that export or import without a file dialog.
    if let Ok(spec) = std::env::var("LIGHTTOUCH_UI_TRANSFER") {
        match spec.split_once(':') {
            Some(("export", rest)) => {
                if let Some((id, file)) = rest.split_once(':') {
                    app.model.export_device(id, PathBuf::from(file));
                }
            }
            Some(("import", file)) => app.model.import_device(PathBuf::from(file)),
            _ => eprintln!("LIGHTTOUCH_UI_TRANSFER is export:ID:FILE or import:FILE"),
        }
    }
    // LIGHTTOUCH_UI_LIBRARY=DIR asks what to do with DIR as Settings → Storage's Change does; move:DIR moves there.
    if let Some(spec) = std::env::var_os("LIGHTTOUCH_UI_LIBRARY").map(PathBuf::from) {
        match spec.to_str().and_then(|s| s.strip_prefix("move:")) {
            Some(dir) => app.model.move_library(PathBuf::from(dir)),
            None => super::storage::offer(app, &spec),
        }
    }
    if let Ok(error) = std::env::var("LIGHTTOUCH_UI_ERROR") {
        app.model.error = Some(error);
    }
    match std::env::var("LIGHTTOUCH_UI_DIALOG").as_deref() {
        Ok("stop") => app.dialog = Some(super::Dialog::Stop),
        Ok("controls") => app.dialog = Some(super::Dialog::Controls),
        _ => {}
    }
}

pub fn tick(app: &mut App, ctx: &egui::Context) {
    // LIGHTTOUCH_UI_SWITCH=ID: once the device on screen has run 20 s, switch to ID and start it too (several devices
    // at once); LIGHTTOUCH_UI_SWITCH_BACK=1 switches back to the first 40 s later.
    static SWITCHED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    static FROM: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static BACK: AtomicBool = AtomicBool::new(false);
    if let Ok(id) = std::env::var("LIGHTTOUCH_UI_SWITCH") {
        let secs = |name: &str, default: u64| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
        let first = app
            .model
            .session
            .as_ref()
            .filter(|s| s.started_at.elapsed() > Duration::from_secs(secs("LIGHTTOUCH_UI_SWITCH_AFTER", 20)));
        if SWITCHED.get().is_none() && first.is_some_and(|s| s.record_id != id) {
            let back = first.map(|s| s.record_id.clone()).unwrap_or_default();
            let _ = SWITCHED.set(Instant::now());
            eprintln!("switch: to {id}, leaving {back} running");
            app.model.select(&id);
            if std::env::var_os("LIGHTTOUCH_UI_SWITCH_STOPPED").is_none() {
                app.model.start(false);
            }
            let _ = FROM.set(back);
        }
        if let Some(at) = SWITCHED.get() {
            if std::env::var_os("LIGHTTOUCH_UI_SWITCH_BACK").is_some()
                && at.elapsed() > Duration::from_secs(secs("LIGHTTOUCH_UI_SWITCH_BACK_AFTER", 40))
                && !BACK.swap(true, Ordering::AcqRel)
            {
                let back = FROM.get().cloned().unwrap_or_default();
                eprintln!("switch: back to {back}");
                app.model.select(&back);
            }
        }
    }
    let Some(shot) = &mut app.dev_shot else { return };
    if shot.install.is_some() && app.model.store.guest_ready() {
        let path = shot.install.take().unwrap();
        eprintln!("install: queueing {}", path.display());
        app.model.store.install_file(path);
        shot.installing = app.model.store.queue.last().map(|item| (item.id, String::new()));
    }
    if let Some((id, last)) = &mut shot.installing {
        if let Some(item) = app.model.store.queue.iter().find(|item| item.id == *id) {
            let now = format!("{:?} {}", item.state, item.phase);
            if now != *last {
                eprintln!("install: {now}{}", item.error.as_deref().map(|e| format!(": {e}")).unwrap_or_default());
                *last = now;
            }
            if matches!(item.state, QueueState::Done | QueueState::Failed) {
                shot.installing = None;
                shot.at = Instant::now() + shot.after;
            }
        }
    }
    if shot.saved.load(Ordering::Acquire) && !app.model.exporting_logs() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        return;
    }
    if !shot.requested && Instant::now() >= shot.at {
        shot.requested = true;
        if let Some(zip) = std::env::var_os("LIGHTTOUCH_UI_EXPORT") {
            app.model.export_logs(PathBuf::from(zip));
        }
        let (path, saved, waker) = (shot.path.clone(), shot.saved.clone(), ctx.clone());
        ctx.request_screenshot(move |image| {
            let [w, h] = image.size;
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8) {
                Ok(()) => eprintln!("saved the window to {}", path.display()),
                Err(e) => eprintln!("couldn't save {}: {e}", path.display()),
            }
            saved.store(true, Ordering::Release);
            waker.request_repaint();
        });
    }
    ctx.request_repaint_after(Duration::from_millis(100));
}

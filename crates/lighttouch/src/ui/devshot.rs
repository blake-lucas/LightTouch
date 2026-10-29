//! Developer hook for checking the UI where nothing can capture the screen (over ssh on macOS or Windows):
//! `LIGHTTOUCH_UI_SHOT=file.png` saves the window after `LIGHTTOUCH_UI_SHOT_AFTER` seconds (default 3), then closes
//! it. `LIGHTTOUCH_UI_PAGE` (settings, prepare, store) opens that page first; `LIGHTTOUCH_UI_START` starts the selected
//! device. `LIGHTTOUCH_UI_SIZE=WxH` sets the window's size, and `LIGHTTOUCH_UI_PAGE=add` opens Add a device,
//! `LIGHTTOUCH_UI_DIALOG=stop` the Stop dialog (`controls` the Controls one), `LIGHTTOUCH_UI_ERROR=text` shows an error,
//! `LIGHTTOUCH_UI_FIRMWARE=id` opens Prepare at that firmware's last step (with `LIGHTTOUCH_UI_IPSW=file`
//! it starts preparing).
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
    if let Some(firmware) = std::env::var("LIGHTTOUCH_UI_FIRMWARE")
        .ok()
        .and_then(|id| lighttouch_catalog::catalog::Catalog::get().find(&id).cloned())
    {
        app.prepare.product = Some(firmware.product_type.clone());
        app.prepare.firmware = Some(firmware.id.clone());
        app.prepare.step = super::prepare::Step::Files;
        app.page = Page::Prepare;
        // LIGHTTOUCH_UI_IPSW=file also starts preparing from it, and shows the device page meanwhile.
        if let Some(ipsw) = std::env::var_os("LIGHTTOUCH_UI_IPSW").map(PathBuf::from) {
            app.prepare.ipsw_file = Some(ipsw.clone());
            app.model.prepare(lighttouch_model::prepare::PrepareRequest {
                firmware_id: firmware.id.clone(),
                ipsw: lighttouch_model::prepare::IpswSource::File(ipsw),
                bootrom: None,
                name: None,
                jailbreak: false,
            });
            app.prepare.started = Some(Instant::now());
            app.prepare.step = super::prepare::Step::Progress;
            app.page = Page::Device;
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
    if shot.saved.load(Ordering::Acquire) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        return;
    }
    if !shot.requested && Instant::now() >= shot.at {
        shot.requested = true;
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

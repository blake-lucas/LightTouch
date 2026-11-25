//! Prepare from an IPSW: pick the device, then the iOS version, then where the IPSW and boot ROM come from, then
//! follow the preparation. Mirrors the Android prepare screen.
use super::{App, Page, icons, setup, theme, widgets};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, RichText, Sense, Ui, vec2};
use lighttouch_catalog::Bootrom;
use lighttouch_catalog::catalog::{Catalog, DeviceGroup, Firmware, RECOMMENDED, Support};
use lighttouch_model::prepare::{self, IpswCheck, IpswSource, PrepareRequest};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Step {
    #[default]
    Device,
    Version,
    Files,
    Progress,
}

#[derive(Default)]
pub struct PrepareState {
    pub step: Step,
    pub product: Option<String>,
    pub firmware: Option<String>,
    pub show_betas: bool,
    pub ipsw_file: Option<PathBuf>,
    /// [Self::ipsw_file] isn't the version's own IPSW; the user chose "Use anyway".
    pub ipsw_unchecked: bool,
    /// A chosen IPSW being hashed and identified.
    pub checking: Option<Checking>,
    /// A chosen IPSW that isn't the version's own: what it is, waiting for the user (or refused).
    pub pending: Option<Pending>,
    /// LIGHTTOUCH_UI_PICK (devshot.rs): a file chosen as if from the file dialog.
    pub dev_pick: Option<PathBuf>,
    pub bootrom: Option<PathBuf>,
    pub name: String,
    pub jailbreak: bool,
    pub error: Option<String>,
    /// When the running preparation started (for the elapsed time).
    pub started: Option<Instant>,
}

/// A chosen IPSW being checked on a thread: its hashing progress, then its SHA-1 and verdict.
pub struct Checking {
    pub file: PathBuf,
    firmware_id: String,
    progress: Arc<Mutex<(u64, u64)>>,
    result: Arc<Mutex<Option<(String, IpswCheck)>>>,
}

pub struct Pending {
    pub file: PathBuf,
    sha1: String,
    pub check: IpswCheck,
}

impl PrepareState {
    /// Forget a chosen IPSW and anything said about it (another version was chosen).
    pub fn clear_ipsw(&mut self) {
        self.ipsw_file = None;
        self.ipsw_unchecked = false;
        self.checking = None;
        self.pending = None;
    }

    /// Set up to run `request` again (one LightTouch closed during): its choices, at the IPSW step, saying why.
    pub fn resume(&mut self, request: PrepareRequest) -> bool {
        let Some(firmware) = Catalog::get().find(&request.firmware_id) else { return false };
        self.product = Some(firmware.product_type.clone());
        self.firmware = Some(firmware.id.clone());
        self.ipsw_file = match request.ipsw {
            IpswSource::File(path) if path.is_file() => Some(path),
            _ => None,
        };
        self.ipsw_unchecked = self.ipsw_file.is_some() && request.unchecked_ipsw;
        self.bootrom = request.bootrom.filter(|p| p.is_file());
        self.name = request.name.unwrap_or_default();
        self.jailbreak = request.jailbreak;
        let download = if self.ipsw_file.is_none() { "; its download picks up where it left off" } else { "" };
        self.error = Some(format!("LightTouch closed before this preparation finished. Prepare it again{download}."));
        self.step = Step::Files;
        true
    }
}

fn support_color(p: &theme::Palette, support: Support) -> Color32 {
    match support {
        Support::Tested => p.primary,
        Support::Experimental => theme::warning(p),
        Support::Untested => p.on_surface_variant,
        Support::DoesntStart => p.error,
    }
}

fn header(app: &mut App, ui: &mut Ui, title: &str, subtitle: &str, back: impl FnOnce(&mut App)) {
    let p = app.palette;
    ui.horizontal(|ui| {
        if widgets::icon_button(ui, &p, icons::BACK, "Back").clicked() {
            back(app);
        }
        ui.vertical(|ui| {
            ui.label(RichText::new(title).font(theme::headline()));
            if !subtitle.is_empty() {
                ui.label(widgets::muted(&p, subtitle));
            }
        });
    });
    ui.add_space(14.0);
}

fn page(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let w = (ui.available_width() - 48.0).clamp(200.0, 640.0);
        let margin = ((ui.available_width() - w) / 2.0).max(24.0);
        egui::Frame::new()
            .inner_margin(egui::Margin { left: margin as i8, right: margin as i8, top: 28, bottom: 24 })
            .show(ui, |ui| {
                ui.set_max_width(w);
                add(ui);
            });
    });
}

/// Open this page to add a device, starting afresh unless a preparation is still running.
pub fn open(app: &mut App) {
    if app.prepare.step == Step::Progress && app.model.busy().is_none() {
        app.prepare = Default::default();
    }
    app.page = Page::Prepare;
}

pub fn show(app: &mut App, ui: &mut Ui) {
    // A preparation in progress always shows its progress.
    if matches!(app.model.busy(), Some((lighttouch_model::Activity::Preparing(_), _))) {
        app.prepare.step = Step::Progress;
    }
    match app.prepare.step {
        Step::Device => page(ui, |ui| device_step(app, ui)),
        Step::Version => page(ui, |ui| version_step(app, ui)),
        Step::Files => page(ui, |ui| files_step(app, ui)),
        Step::Progress => page(ui, |ui| progress_step(app, ui)),
    }
}

fn device_step(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    header(app, ui, "Choose a device", "Pick the device to emulate. You'll choose its iOS version next.", |app| {
        app.page = if app.model.devices.is_empty() { Page::Setup(setup::Step::Welcome) } else { Page::Device };
    });
    // The sidebar's ⋮ has this too; here for whoever came to add a device they already have elsewhere.
    ui.horizontal(|ui| {
        ui.label(widgets::muted(&p, "Moving a device from another computer or phone?"));
        if widgets::text_button(ui, &p, "Import a device…").clicked() {
            super::import_device(app);
        }
    });
    let groups = Catalog::get().devices();
    let (tested, untested): (Vec<&DeviceGroup>, Vec<&DeviceGroup>) = groups.iter().partition(|g| g.works());
    for (title, list) in [("Tested on LightTouch", tested), ("Not tested on LightTouch yet", untested)] {
        if list.is_empty() {
            continue;
        }
        widgets::section(ui, &p, title);
        if title.starts_with("Not tested") {
            ui.label(widgets::muted(&p, "These may not start, or may not work fully. Try them and see.").size(12.5));
            ui.add_space(4.0);
        }
        widgets::card(ui, &p, |ui| {
            for group in list {
                let best = group.firmwares.iter().map(|f| f.support()).min().unwrap_or(Support::Untested);
                if choice_row(ui, &p, icons::PHONE, group.name(), &group.summary(), Some(best)).clicked() {
                    app.prepare.product = Some(group.product_type.clone());
                    app.prepare.step = Step::Version;
                }
            }
        });
    }
}

fn version_step(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(product) = app.prepare.product.clone() else {
        app.prepare.step = Step::Device;
        return;
    };
    let groups = Catalog::get().devices();
    let Some(group) = groups.iter().find(|g| g.product_type == product) else { return };
    let name = group.name();
    header(
        app,
        ui,
        &format!("Choose iOS for {name}"),
        "Tested versions are checked on LightTouch; the others are yours to try.",
        |app| app.prepare.step = Step::Device,
    );
    let betas = group.firmwares.iter().filter(|f| f.is_beta()).count();
    let mut sections: Vec<(Support, Vec<&Firmware>)> = Vec::new();
    for firmware in group.firmwares.iter().copied().filter(|f| app.prepare.show_betas || !f.is_beta()) {
        let support = firmware.support();
        match sections.iter_mut().find(|(s, _)| *s == support) {
            Some((_, list)) => list.push(firmware),
            None => sections.push((support, vec![firmware])),
        }
    }
    sections.sort_by_key(|(s, _)| *s);
    for (support, mut list) in sections {
        list.sort_by_key(|f| f.id != RECOMMENDED);
        widgets::section(ui, &p, support.label());
        ui.label(widgets::muted(&p, list[0].support_detail()).size(12.5));
        ui.add_space(4.0);
        widgets::card(ui, &p, |ui| {
            for firmware in list {
                let title = if firmware.id == RECOMMENDED {
                    format!("Recommended · iOS {}", firmware.label())
                } else {
                    format!("iOS {}", firmware.label())
                };
                let subtitle = format!("Released {} · build {}", firmware.released_label(), firmware.build);
                if choice_row(ui, &p, icons::PACKAGE, &title, &subtitle, None).clicked() {
                    app.prepare.firmware = Some(firmware.id.clone());
                    app.prepare.clear_ipsw();
                    app.prepare.error = None;
                    app.prepare.step = Step::Files;
                }
            }
        });
    }
    if betas > 0 {
        ui.add_space(8.0);
        let label = if app.prepare.show_betas {
            "Hide beta versions".to_string()
        } else {
            format!("Show {betas} beta versions")
        };
        if widgets::text_button(ui, &p, &label).clicked() {
            app.prepare.show_betas = !app.prepare.show_betas;
        }
    }
}

/// What the preparation will use and what it does, laid out like Android's: the IPSW, the boot file, then what
/// preparing involves, and one button.
fn files_step(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(firmware) = app.prepare.firmware.as_deref().and_then(|id| Catalog::get().find(id)).cloned() else {
        app.prepare.step = Step::Version;
        return;
    };
    let rom = Bootrom::for_board(&firmware.board);
    let name = firmware.device_name();
    header(
        app,
        ui,
        &format!("{name}, iOS {}", firmware.label()),
        "Downloads what it needs, then sets it up on this computer.",
        |app| app.prepare.step = Step::Version,
    );
    if let Err(reason) = prepare::availability(&app.model.resources) {
        widgets::card(ui, &p, |ui| {
            ui.label(RichText::new(reason).color(theme::warning(&p)));
        });
        return;
    }
    let support = firmware.support();
    if support != Support::Tested {
        let color = match support {
            Support::Untested => p.on_surface_variant,
            Support::DoesntStart => p.error,
            _ => theme::warning(&p),
        };
        ui.label(RichText::new(format!("{}: {}", support.label(), firmware.support_detail())).size(12.5).color(color));
        ui.add_space(8.0);
    }
    let busy = app.model.busy().is_some();

    // The IPSW: downloaded as preparing starts, or a file chosen here (checked as it is chosen).
    poll_check(app);
    let ipsw = app.prepare.ipsw_file.clone();
    let checking = app.prepare.checking.is_some();
    let detail = match &ipsw {
        Some(file) if app.prepare.ipsw_unchecked => format!(
            "{} · not Apple's file for this version; it may not work",
            file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ),
        Some(file) => {
            format!("{} · verified", file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        }
        None => format!("{:.0} MB · downloaded for you", firmware.ipsw_bytes() as f64 / 1e6),
    };
    let status = ipsw_status(app);
    let mut action = IpswAction::None;
    prep_card(
        ui,
        &p,
        icons::PACKAGE,
        "iOS firmware",
        &detail,
        ipsw.is_some(),
        Some(&mut |ui: &mut Ui| {
            // Right to left: the file button sits at the edge.
            let label = if ipsw.is_some() { "Change" } else { "Choose a file" };
            if widgets::tonal_enabled(ui, &p, label, !busy && !checking).clicked() {
                if let Some(file) =
                    rfd::FileDialog::new().set_title("Choose the IPSW").add_filter("IPSW", &["ipsw"]).pick_file()
                {
                    choose_ipsw(app, file, ui.ctx());
                }
            }
            if ipsw.is_some() && widgets::text_button(ui, &p, "Download instead").clicked() {
                app.prepare.clear_ipsw();
            }
        }),
        status.as_ref().map(|status| {
            Box::new(|ui: &mut Ui| action = show_ipsw_status(ui, &p, status)) as Box<dyn FnOnce(&mut Ui) + '_>
        }),
    );
    apply_ipsw_action(app, action);
    if let Some(file) = app.prepare.dev_pick.take() {
        choose_ipsw(app, file, ui.ctx());
    }

    // The chip's boot ROM, on the devices that start from one (the same for every iOS version).
    if let Some(rom) = rom {
        let known = prepare::known_bootrom(&app.model.dirs, &app.model.devices, rom);
        let (ready, detail) = match (&app.prepare.bootrom, &known) {
            (Some(_), _) => (true, format!("{} · your file", rom.label())),
            (None, Some(_)) => (true, format!("{} · reused", rom.label())),
            (None, None) => (false, "64 KB · downloaded for you".to_string()),
        };
        prep_card(
            ui,
            &p,
            icons::CPU,
            "Boot ROM",
            &detail,
            ready,
            Some(&mut |ui: &mut Ui| {
                let label = if ready { "Change" } else { "Choose a file" };
                if widgets::tonal_enabled(ui, &p, label, !busy).clicked() {
                    if let Some(file) = rfd::FileDialog::new().set_title("Choose the boot ROM").pick_file() {
                        if rom.matches_file(&file) {
                            app.prepare.bootrom = Some(file);
                            app.prepare.error = None;
                        } else {
                            app.prepare.error = Some(format!("That file isn't the {} boot ROM.", rom.label()));
                        }
                    }
                }
            }),
            None,
        );
    }

    prep_card(
        ui,
        &p,
        icons::PENCIL,
        "Name",
        "Optional; shown in the device list",
        false,
        Some(&mut |ui: &mut Ui| {
            ui.add(
                egui::TextEdit::singleline(&mut app.prepare.name)
                    .hint_text(lighttouch_catalog::catalog::default_device_name(
                        &firmware.board,
                        &firmware.build,
                        app.model.devices.iter().map(|d| d.name.as_str()),
                    ))
                    .desired_width(240.0)
                    .margin(vec2(10.0, 8.0)),
            );
        }),
        None,
    );

    ui.add_space(8.0);
    // The same row Android's prepare screen shows: icon, what it adds, and the switch (off and locked, saying why,
    // where the firmware can't be jailbroken).
    let unavailable = firmware.jailbreak_unavailable();
    if unavailable.is_some() {
        app.prepare.jailbreak = false;
    }
    widgets::card(ui, &p, |ui| {
        widgets::setting_row(ui, &p, icons::BAG, "Jailbreak", unavailable.unwrap_or("Adds Cydia and afc2"), |ui| {
            ui.add_enabled_ui(unavailable.is_none(), |ui| widgets::switch(ui, &p, &mut app.prepare.jailbreak));
        });
    });

    if let Some(error) = &app.prepare.error {
        ui.add_space(8.0);
        ui.label(RichText::new(error).color(p.error));
    }
    ui.add_space(12.0);
    let (time, space) = firmware.cost_hint();
    ui.label(
        widgets::muted(
            &p,
            format!("Takes {time} and needs about {space} free. You can use the rest of LightTouch while it works."),
        )
        .size(12.5),
    );
    ui.add_space(20.0);
    let mut start = false;
    ui.vertical_centered(|ui| {
        // Not while a chosen file is being checked or waits for an answer: it would download instead.
        let deciding =
            checking || app.prepare.pending.as_ref().is_some_and(|d| d.check.bypass || d.check.firmware.is_some());
        start = widgets::primary_enabled(ui, &p, "Prepare device", !busy && !deciding).clicked();
    });
    if start {
        app.prepare.error = None;
        app.model.prepare(PrepareRequest {
            firmware_id: firmware.id.clone(),
            ipsw: app.prepare.ipsw_file.clone().map(IpswSource::File).unwrap_or(IpswSource::Download),
            bootrom: app.prepare.bootrom.clone(),
            name: Some(app.prepare.name.clone()).filter(|n| !n.trim().is_empty()),
            jailbreak: app.prepare.jailbreak,
            ipas: Vec::new(),
            no_game_center: false,
            unchecked_ipsw: app.prepare.ipsw_file.is_some() && app.prepare.ipsw_unchecked,
        });
        app.prepare.started = Some(Instant::now());
        app.prepare.step = Step::Progress;
    }
}

/// A file chosen for the IPSW: hashed and identified on a thread (a few seconds for a large IPSW), the card showing
/// progress meanwhile. The dev hook LIGHTTOUCH_UI_PICK comes in here too.
pub fn choose_ipsw(app: &mut App, file: PathBuf, ctx: &egui::Context) {
    let Some(firmware_id) = app.prepare.firmware.clone() else { return };
    app.prepare.clear_ipsw();
    let progress = Arc::new(Mutex::new((0u64, 0u64)));
    let result = Arc::new(Mutex::new(None));
    let (resources, path, id) = (app.model.resources.clone(), file.clone(), firmware_id.clone());
    let (shared_progress, shared_result, ctx) = (progress.clone(), result.clone(), ctx.clone());
    std::thread::spawn(move || {
        let mut report = |_: &str, done: u64, total: u64| {
            if let Ok(mut p) = shared_progress.lock() {
                *p = (done, total);
            }
            ctx.request_repaint();
            true
        };
        let unreadable = || IpswCheck {
            verdict: "not_ipsw".into(),
            bypass: false,
            message: "LightTouch can't read that file.".into(),
            firmware: None,
            switch_label: None,
        };
        // Always an answer, even if hashing panics: the card must not stay at "Checking" forever.
        let answer =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match prepare::ipsw_sha1(&path, &mut report) {
                Ok(sha1) => {
                    let check = prepare::check_ipsw(&resources, &id, &path, &sha1);
                    (sha1, check)
                }
                Err(_) => (String::new(), unreadable()),
            }))
            .unwrap_or_else(|_| (String::new(), unreadable()));
        if let Ok(mut r) = shared_result.lock() {
            *r = Some(answer);
        }
        ctx.request_repaint();
    });
    app.prepare.checking = Some(Checking { file, firmware_id, progress, result });
}

/// Take a finished check: Apple's file is used, anything else waits for the user (or is refused).
fn poll_check(app: &mut App) {
    let Some(checking) = &app.prepare.checking else { return };
    let Some((sha1, check)) = checking.result.lock().ok().and_then(|mut r| r.take()) else { return };
    let checking = app.prepare.checking.take().unwrap();
    if app.prepare.firmware.as_deref() != Some(checking.firmware_id.as_str()) {
        return; // another version was chosen meanwhile
    }
    settle(app, checking.file, sha1, check);
}

fn settle(app: &mut App, file: PathBuf, sha1: String, check: IpswCheck) {
    if check.matches() {
        app.prepare.ipsw_file = Some(file);
        app.prepare.ipsw_unchecked = false;
        app.prepare.pending = None;
    } else {
        app.prepare.pending = Some(Pending { file, sha1, check });
    }
}

/// What the IPSW card shows under its row: a chosen file being checked, or what it turned out to be.
enum IpswStatus {
    Checking { name: String, fraction: Option<f32> },
    Verdict(IpswCheck),
}

/// The answer to a chosen file that isn't the version's own.
enum IpswAction {
    None,
    UseAnyway,
    Switch(String),
    Cancel,
}

fn ipsw_status(app: &App) -> Option<IpswStatus> {
    if let Some(checking) = &app.prepare.checking {
        let (done, total) = checking.progress.lock().map(|g| *g).unwrap_or((0, 0));
        let name = checking.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Some(IpswStatus::Checking { name, fraction: (total > 0).then(|| done as f32 / total as f32) });
    }
    app.prepare.pending.as_ref().map(|pending| IpswStatus::Verdict(pending.check.clone()))
}

/// Inside the IPSW card, under its row: the check's progress, or what the chosen file is and what can be done.
fn show_ipsw_status(ui: &mut Ui, p: &theme::Palette, status: &IpswStatus) -> IpswAction {
    let mut action = IpswAction::None;
    ui.add_space(6.0);
    match status {
        IpswStatus::Checking { name, fraction } => {
            ui.label(widgets::muted(p, format!("Checking {name}…")).size(12.5));
            ui.add_space(4.0);
            widgets::progress(ui, p, *fraction, ui.available_width());
        }
        IpswStatus::Verdict(check) => {
            let decision = check.bypass || check.firmware.is_some();
            let warning = theme::warning(p);
            ui.label(RichText::new(&check.message).size(13.0).color(if decision { warning } else { p.error }));
            if decision {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    match (&check.firmware, &check.switch_label) {
                        // Another version the catalog lists: switch to it, and check the file against that one.
                        (Some(id), Some(label)) => {
                            if widgets::tonal(ui, p, label).clicked() {
                                action = IpswAction::Switch(id.clone());
                            }
                        }
                        _ => {
                            if widgets::tonal(ui, p, "Use anyway").clicked() {
                                action = IpswAction::UseAnyway;
                            }
                        }
                    }
                    if widgets::text_button(ui, p, "Cancel").clicked() {
                        action = IpswAction::Cancel;
                    }
                });
            }
        }
    }
    action
}

fn apply_ipsw_action(app: &mut App, action: IpswAction) {
    let Some(pending) = &app.prepare.pending else { return };
    let (file, sha1) = (pending.file.clone(), pending.sha1.clone());
    match action {
        IpswAction::None => {}
        IpswAction::UseAnyway => {
            app.prepare.ipsw_file = Some(file);
            app.prepare.ipsw_unchecked = true;
            app.prepare.pending = None;
        }
        IpswAction::Switch(id) => {
            if let Some(other) = Catalog::get().find(&id) {
                app.prepare.product = Some(other.product_type.clone());
                app.prepare.firmware = Some(other.id.clone());
                let again = prepare::check_ipsw(&app.model.resources, &other.id, &file, &sha1);
                settle(app, file, sha1, again);
            }
        }
        IpswAction::Cancel => app.prepare.pending = None,
    }
}

/// One thing the preparation needs, as a card row: its icon (coloured once it's settled), the title and what it means
/// on the left, its control on the right (a check when it needs none), and anything `below` under the row.
fn prep_card<'a>(
    ui: &mut Ui,
    p: &theme::Palette,
    icon: &str,
    title: &str,
    detail: &str,
    done: bool,
    trailing: Option<&mut dyn FnMut(&mut Ui)>,
    below: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
) {
    ui.add_space(6.0);
    widgets::card(ui, p, |ui| {
        // The control's width as laid out last frame, so the text wraps before it instead of running under it.
        let id = ui.scope_id().with(("prep-card", title));
        let reserved = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(140.0);
        ui.horizontal(|ui| {
            ui.set_min_height(44.0);
            let color = if done { p.primary } else { p.on_surface_variant };
            ui.label(widgets::icon_text(icon, 22.0).color(color));
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.set_max_width((ui.available_width() - reserved - 20.0).max(160.0));
                ui.label(RichText::new(title).size(14.5).family(theme::medium()));
                ui.label(widgets::muted(p, detail).size(12.5));
            });
            let control = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match trailing {
                Some(add) => add(ui),
                None if done => {
                    ui.label(widgets::icon_text(icons::CHECK_CIRCLE, 22.0).color(p.primary));
                }
                None => {}
            });
            let width = control.response.rect.width();
            if (width - reserved).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(id, width));
                ui.ctx().request_discard("a preparation card's control changed width");
            }
        });
        if let Some(below) = below {
            // Under the text, level with the title.
            egui::Frame::new().inner_margin(egui::Margin { left: 38, right: 0, top: 0, bottom: 2 }).show(ui, below);
        }
    });
}

fn progress_step(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let firmware = app.prepare.firmware.as_deref().and_then(|id| Catalog::get().find(id)).cloned();
    let title = firmware
        .as_ref()
        .map(|f| format!("Preparing {} · iOS {}", f.device_name(), f.label()))
        .unwrap_or_else(|| "Preparing".into());
    ui.add_space(30.0);
    ui.label(RichText::new(title).font(theme::headline()));
    ui.add_space(18.0);
    match app.model.busy() {
        Some((_, progress)) => {
            let text = progress.phase.clone();
            let secs = app.prepare.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
            let elapsed = format!("{}:{:02}", secs / 60, secs % 60);
            let percent = if text.contains("IPSW") && progress.total > 1 << 20 {
                format!("{:.0} of {:.0} MB", progress.done as f64 / 1e6, progress.total as f64 / 1e6)
            } else {
                progress.fraction().map(|f| format!("{:.0}%", f * 100.0)).unwrap_or_default()
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(if text.is_empty() { "Starting".into() } else { text }).size(15.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(widgets::muted(&p, format!("{elapsed}   {percent}")));
                });
            });
            ui.add_space(8.0);
            widgets::progress(ui, &p, progress.fraction(), ui.available_width());
            ui.add_space(14.0);
            // As on Android, how long it takes; unlike Android, the app has to stay open for it.
            let time = firmware.as_ref().map_or("a few minutes", |f| f.cost_hint().0);
            ui.label(widgets::muted(
                &p,
                format!(
                    "This takes {time}. You can use the rest of LightTouch meanwhile; keep it open until it finishes."
                ),
            ));
            ui.add_space(12.0);
            if widgets::text_button(ui, &p, "Cancel").clicked() {
                app.model.cancel_task();
            }
        }
        None => {
            // Finished: success goes to "ready" (App), a failure comes back here.
            let Some(error) = app.prepare.error.clone() else {
                app.prepare.step = Step::Files;
                return;
            };
            {
                ui.label(widgets::icon_text(icons::WARNING, 30.0).color(theme::warning(&p)));
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(error).size(14.0)).wrap());
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if widgets::primary(ui, &p, "Try again").clicked() {
                        app.prepare.error = None;
                        app.prepare.step = Step::Files;
                    }
                    if widgets::text_button(ui, &p, "Back").clicked() {
                        app.prepare.step = Step::Version;
                    }
                    let label = if app.model.exporting_logs() { "Saving logs…" } else { "Save logs" };
                    if widgets::text_button(ui, &p, label).clicked() && !app.model.exporting_logs() {
                        super::save_logs(app);
                    }
                });
            }
        }
    }
}

/// A tappable row in a card: icon tile, title, subtitle, optional support badge, chevron.
fn choice_row(
    ui: &mut Ui,
    p: &theme::Palette,
    icon: &str,
    title: &str,
    subtitle: &str,
    support: Option<Support>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.expand2(vec2(8.0, 0.0)),
            CornerRadius::same(theme::radius::ROW),
            p.container_high,
        );
    }
    let painter = ui.painter();
    let tile = egui::Rect::from_center_size(rect.left_center() + vec2(18.0, 0.0), vec2(36.0, 36.0));
    painter.rect_filled(tile, CornerRadius::same(10), p.container_highest);
    painter.text(tile.center(), Align2::CENTER_CENTER, icon, FontId::new(19.0, theme::icons()), p.on_surface_variant);
    painter.text(
        egui::pos2(rect.left() + 48.0, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        title,
        FontId::new(14.5, theme::medium()),
        p.on_surface,
    );
    painter.text(
        egui::pos2(rect.left() + 48.0, rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        subtitle,
        FontId::new(12.5, egui::FontFamily::Proportional),
        p.on_surface_variant,
    );
    painter.text(
        rect.right_center() - vec2(8.0, 0.0),
        Align2::RIGHT_CENTER,
        icons::CARET_RIGHT,
        FontId::new(16.0, theme::icons()),
        p.on_surface_variant,
    );
    if let Some(support) = support {
        let color = support_color(p, support);
        let galley = painter.layout_no_wrap(support.label().to_string(), FontId::new(11.5, theme::medium()), color);
        let size = galley.size() + vec2(16.0, 6.0);
        let badge = egui::Rect::from_center_size(rect.right_center() - vec2(36.0 + size.x / 2.0, 0.0), size);
        painter.rect_filled(badge, CornerRadius::same(8), color.gamma_multiply(0.16));
        painter.galley(badge.min + vec2(8.0, 3.0), galley, color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

//! Prepare from an IPSW: pick the device, then the iOS version, then where the IPSW and boot ROM come from, then
//! follow the preparation. Mirrors the Android prepare screen.
use super::{App, Page, icons, setup, theme, widgets};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, RichText, Sense, Ui, vec2};
use lighttouch_catalog::Bootrom;
use lighttouch_catalog::catalog::{Catalog, DeviceGroup, Firmware, RECOMMENDED, Support};
use lighttouch_model::prepare::{self, IpswSource, PrepareRequest};
use std::path::PathBuf;
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
    pub bootrom: Option<PathBuf>,
    pub name: String,
    pub error: Option<String>,
    /// When the running preparation started (for the elapsed time).
    pub started: Option<Instant>,
}

impl PrepareState {
    /// Set up to run `request` again (one LightTouch closed during): its choices, at the IPSW step, saying why.
    pub fn resume(&mut self, request: PrepareRequest) -> bool {
        let Some(firmware) = Catalog::get().find(&request.firmware_id) else { return false };
        self.product = Some(firmware.product_type.clone());
        self.firmware = Some(firmware.id.clone());
        self.ipsw_file = match request.ipsw {
            IpswSource::File(path) if path.is_file() => Some(path),
            _ => None,
        };
        self.bootrom = request.bootrom.filter(|p| p.is_file());
        self.name = request.name.unwrap_or_default();
        let download = if self.ipsw_file.is_none() { "; its download picks up where it left off" } else { "" };
        self.error = Some(format!("LightTouch closed before this preparation finished. Prepare it again{download}."));
        self.step = Step::Files;
        true
    }
}

fn support_color(p: &theme::Palette, support: Support) -> Color32 {
    match support {
        Support::Tested => p.primary,
        Support::TestedSlow => Color32::from_rgb(0x8F, 0xB8, 0xFF),
        Support::Experimental => theme::WARNING,
        Support::Untested => p.on_surface_variant,
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
    header(app, ui, "Prepare from an IPSW", "Choose the device to emulate", |app| {
        app.page = if app.model.devices.is_empty() { Page::Setup(setup::Step::Welcome) } else { Page::Device };
    });
    let groups = Catalog::get().devices();
    let (tested, untested): (Vec<&DeviceGroup>, Vec<&DeviceGroup>) =
        groups.iter().partition(|g| !g.tested().is_empty());
    for (title, list) in [("Tested on LightTouch", tested), ("Not tested on LightTouch yet", untested)] {
        if list.is_empty() {
            continue;
        }
        widgets::section(ui, &p, title);
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
    header(app, ui, name, "Choose the iOS version", |app| app.prepare.step = Step::Device);
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
        ui.label(widgets::muted(&p, support.detail()).size(12.5));
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
                    app.prepare.ipsw_file = None;
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
        &format!(
            "LightTouch needs Apple's stock IPSW for this version{}",
            if rom.is_some() { ", and the device's boot file." } else { "." }
        ),
        |app| app.prepare.step = Step::Version,
    );
    if let Err(reason) = prepare::availability(&app.model.resources) {
        widgets::card(ui, &p, |ui| {
            ui.label(RichText::new(reason).color(theme::WARNING));
        });
        return;
    }
    let support = firmware.support();
    if support != Support::Tested {
        let color = if support == Support::Untested { p.on_surface_variant } else { theme::WARNING };
        ui.label(RichText::new(format!("{}: {}", support.label(), support.detail())).size(12.5).color(color));
        ui.add_space(8.0);
    }
    let busy = app.model.busy().is_some();

    // The IPSW: downloaded as preparing starts, or a file chosen here.
    let ipsw = app.prepare.ipsw_file.clone();
    let detail = match &ipsw {
        Some(file) => format!(
            "{} · checked against the catalog before it is used",
            file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ),
        None => format!(
            "{} · {:.0} MB. Downloaded when preparing starts, from Apple's servers when ipsw.me lists it, otherwise \
             from the Internet Archive.",
            firmware.file_name(),
            firmware.ipsw_bytes() as f64 / 1e6
        ),
    };
    prep_card(
        ui,
        &p,
        icons::PACKAGE,
        "iOS firmware (IPSW)",
        &detail,
        ipsw.is_some(),
        Some(&mut |ui: &mut Ui| {
            // Right to left: the file button sits at the edge.
            let label = if ipsw.is_some() { "Change" } else { "Choose a file" };
            if widgets::tonal_enabled(ui, &p, label, !busy).clicked() {
                if let Some(file) =
                    rfd::FileDialog::new().set_title("Choose the IPSW").add_filter("IPSW", &["ipsw"]).pick_file()
                {
                    app.prepare.ipsw_file = Some(file);
                }
            }
            if ipsw.is_some() && widgets::text_button(ui, &p, "Download instead").clicked() {
                app.prepare.ipsw_file = None;
            }
        }),
    );

    // The boot file: which one the device starts from, always shown.
    match rom {
        None => prep_card(
            ui,
            &p,
            icons::CPU,
            "Boot file",
            &format!("None needed. The {name} starts from the files preparation makes."),
            true,
            None,
        ),
        Some(rom) => {
            let known = prepare::known_bootrom(&app.model.dirs, &app.model.devices, rom);
            let (ready, detail) = match (&app.prepare.bootrom, &known) {
                (Some(file), _) => (
                    true,
                    format!(
                        "{} ({}). It is the same for every iOS version.",
                        file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        rom.label()
                    ),
                ),
                (None, Some(path)) => {
                    let from = app.model.devices.iter().find(|d| d.record.bootrom_path.as_ref() == Some(path));
                    let whose = from.map(|d| format!("the one from {}", d.name)).unwrap_or("the saved one".into());
                    (true, format!("Reusing {whose} ({}). It is the same for every iOS version.", rom.label()))
                }
                (None, None) => (
                    false,
                    format!(
                        "64 KB, part of the chip rather than the IPSW. Downloaded from {} when preparing starts.",
                        rom.sources()
                    ),
                ),
            };
            let title = format!("Boot file · {} boot ROM", rom.label());
            prep_card(
                ui,
                &p,
                icons::CPU,
                &title,
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
            );
        }
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
    );

    if let Some(error) = &app.prepare.error {
        ui.add_space(8.0);
        ui.label(RichText::new(error).color(p.error));
    }
    ui.add_space(12.0);
    let (time, space) = firmware.cost_hint();
    ui.label(
        widgets::muted(
            &p,
            format!(
                "Preparing runs on this computer, takes {time} and needs about {space} free. It shows in the device \
                 list while it runs. LightTouch's guest tools are added for you."
            ),
        )
        .size(12.5),
    );
    ui.add_space(20.0);
    let mut start = false;
    ui.vertical_centered(|ui| {
        start = widgets::primary_enabled(ui, &p, "Prepare device", !busy).clicked();
    });
    if start {
        app.prepare.error = None;
        app.model.prepare(PrepareRequest {
            firmware_id: firmware.id.clone(),
            ipsw: app.prepare.ipsw_file.clone().map(IpswSource::File).unwrap_or(IpswSource::Download),
            bootrom: app.prepare.bootrom.clone(),
            name: Some(app.prepare.name.clone()).filter(|n| !n.trim().is_empty()),
        });
        app.prepare.started = Some(Instant::now());
        app.prepare.step = Step::Progress;
    }
}

/// One thing the preparation needs, as a card row: its icon (coloured once it's settled), the title and what it means
/// on the left, its control on the right (a check when it needs none).
fn prep_card(
    ui: &mut Ui,
    p: &theme::Palette,
    icon: &str,
    title: &str,
    detail: &str,
    done: bool,
    trailing: Option<&mut dyn FnMut(&mut Ui)>,
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
            ui.label(widgets::muted(
                &p,
                "You can use the rest of LightTouch while this runs. Keep it open until it finishes.",
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
                ui.label(widgets::icon_text(icons::WARNING, 30.0).color(theme::WARNING));
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

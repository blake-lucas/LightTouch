//! Adding a device: a welcome on first run that leads into preparing one from an IPSW (prepare.rs), and "ready".
use super::{App, Page, icons, theme, widgets};
use eframe::egui::{self, RichText, Ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Done,
}

#[derive(Default)]
pub struct SetupState {
    pub added: Option<String>,
    /// (stage, seconds) of the preparation that made it, for "Details".
    pub timings: Vec<(String, f64)>,
}

fn centered(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let w = (ui.available_width() - 48.0).clamp(200.0, width);
        let margin = ((ui.available_width() - w) / 2.0).max(24.0);
        egui::Frame::new()
            .inner_margin(egui::Margin { left: margin as i8, right: margin as i8, top: 36, bottom: 24 })
            .show(ui, |ui| {
                ui.set_max_width(w);
                add(ui);
            });
    });
}

pub fn show(app: &mut App, ui: &mut Ui, step: Step) {
    let p = app.palette;
    match step {
        Step::Welcome => centered(ui, 520.0, |ui| {
            ui.add_space(30.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, egui::CornerRadius::same(18), p.primary);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                icons::PHONE,
                egui::FontId::new(36.0, theme::icons_fill()),
                p.on_primary,
            );
            ui.add_space(16.0);
            ui.label(RichText::new("Welcome to LightTouch").font(theme::headline()));
            ui.add_space(6.0);
            ui.label(
                widgets::muted(&p, "Classic iPod touch and iPhone software, running on your computer.").size(15.0),
            );
            ui.add_space(22.0);
            ui.label(RichText::new("To start you need an iOS version for it to run. LightTouch downloads one from Apple's archive and prepares it for you, or uses an IPSW you already have."));
            ui.add_space(22.0);
            if widgets::primary(ui, &p, "Get started").clicked() {
                super::prepare::open(app);
            }
        }),
        Step::Done => centered(ui, 520.0, |ui| {
            ui.add_space(40.0);
            ui.label(widgets::icon_text(icons::CHECK_CIRCLE, 54.0).color(p.primary));
            ui.add_space(10.0);
            let name = app.setup.added.clone().unwrap_or_else(|| "Your device".into());
            ui.label(RichText::new(format!("{name} is ready")).font(theme::headline()));
            ui.add_space(6.0);
            ui.label(widgets::muted(&p, "The first start takes longer while iOS sets itself up."));
            if !app.setup.timings.is_empty() {
                ui.add_space(12.0);
                egui::CollapsingHeader::new(widgets::muted(&p, "Details")).id_salt("prepare-details").show(ui, |ui| {
                    ui.spacing_mut().interact_size.y = 20.0;
                    egui::Grid::new("timings").num_columns(2).spacing([28.0, 2.0]).show(ui, |ui| {
                        for (stage, secs) in &app.setup.timings {
                            ui.label(widgets::muted(&p, stage.as_str()));
                            ui.label(format!("{secs:.1} s"));
                            ui.end_row();
                        }
                    });
                });
            }
            ui.add_space(22.0);
            ui.horizontal(|ui| {
                let can_start = !app.model.session_active();
                if can_start && widgets::primary(ui, &p, "Start it now").clicked() {
                    app.model.start(false);
                    app.page = Page::Device;
                }
                if widgets::text_button(ui, &p, "Not now").clicked() {
                    app.page = Page::Device;
                }
            });
        }),
    }
}

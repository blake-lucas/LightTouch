//! The States button (a floppy disk in the device page's header, and in full screen's More menu) and its menu: save the
//! running device, and load, rename or delete the device's save states, each shown with its screen, name and time.
//! The behaviour is the model's (lighttouch-model states.rs).
use super::{App, Dialog, icons, theme, widgets};
use eframe::egui::{self, Color32, CornerRadius, RichText, Ui, vec2};
use lighttouch_core::library::SaveState;
use std::collections::HashMap;
use std::path::PathBuf;

/// The thumbnails decoded so far, by file.
#[derive(Default)]
pub struct StatesUi {
    thumbnails: HashMap<PathBuf, Option<egui::TextureHandle>>,
}

/// Whether the header shows the States button: the setting is on and a device is on screen.
pub fn wants_button(app: &App) -> bool {
    app.model.settings.states_button && app.model.current().is_some()
}

/// The header's States button and its menu.
pub fn header_button(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let busy = app.model.state_job().is_some() || app.model.state_load_pending();
    let r = widgets::icon_button_sized(ui, &p, icons::FLOPPY, "States", 40.0, true);
    if busy {
        // A ring around the button while a state is saved or loaded.
        let fraction = app.model.state_job().map_or(0.0, |j| j.fraction());
        ring(ui, r.rect, fraction, p.primary);
    }
    egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| menu(app, ui));
}

fn ring(ui: &Ui, rect: egui::Rect, fraction: f32, color: Color32) {
    let center = rect.center();
    let radius = rect.width() / 2.0 - 1.5;
    let steps = 48;
    let end = (fraction.clamp(0.02, 1.0) * steps as f32) as usize;
    let points: Vec<egui::Pos2> = (0..=end)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 / steps as f32 * std::f32::consts::TAU;
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    ui.painter().add(egui::Shape::line(points, egui::Stroke::new(2.5, color)));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
}

/// The menu: Save state (with the progress of a save or load), then the states, newest first.
pub fn menu(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    ui.set_width(340.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Save states").font(theme::label()).color(p.primary));
    });
    ui.add_space(4.0);
    if let Some(job) = app.model.state_job().cloned() {
        ui.label(widgets::muted(&p, format!("{}… {:.0}%", job.label(), job.fraction() * 100.0)).size(12.5));
        widgets::progress(ui, &p, (job.total > 0).then(|| job.fraction()), 320.0);
        ui.add_space(6.0);
    } else if app.model.state_load_pending() {
        ui.label(widgets::muted(&p, "Ending the device to load the state…").size(12.5));
        widgets::progress(ui, &p, None, 320.0);
        ui.add_space(6.0);
    }
    let can_save = app.model.can_save_state();
    let save = ui
        .add_enabled(can_save, egui::Button::new(widgets::icon_label(icons::FLOPPY, "Save state", None)))
        .on_disabled_hover_text("Start the device to save its state");
    if save.clicked() {
        app.model.save_state(None);
    }
    ui.separator();
    let states = app.model.states().to_vec();
    if states.is_empty() {
        ui.label(
            widgets::muted(
                &p,
                "No save states yet. Save one while the device runs, and load it later to carry on from that moment.",
            )
            .size(12.5),
        );
        return;
    }
    let can_load = app.model.can_load_state();
    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
        for state in &states {
            row(app, ui, state, can_load);
            ui.add_space(4.0);
        }
    });
}

fn row(app: &mut App, ui: &mut Ui, state: &SaveState, can_load: bool) {
    let p = app.palette;
    ui.horizontal(|ui| {
        let height = 64.0;
        let texture = state.thumbnail.as_ref().and_then(|path| thumbnail(app, ui.ctx(), path));
        let width = match (state.thumbnail_width, state.thumbnail_height) {
            (w, h) if w > 0 && h > 0 => height * w as f32 / h as f32,
            _ => height * 0.66,
        };
        let (rect, _) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
        match texture {
            Some(texture) => {
                egui::Image::new((texture.id(), rect.size())).corner_radius(CornerRadius::same(4)).paint_at(ui, rect)
            }
            None => {
                ui.painter().rect_filled(rect, CornerRadius::same(4), p.container_highest);
            }
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(&state.name).family(theme::medium()));
            ui.label(widgets::muted(&p, format!("{} · {}", when(state.created_ms), size(state.bytes))).size(12.0));
            ui.horizontal(|ui| {
                let load =
                    ui.add_enabled(can_load, egui::Button::new("Load")).on_hover_text("Carry on from this moment");
                if load.clicked() {
                    if app.model.session_active() {
                        app.dialog = Some(Dialog::LoadState { id: state.id.clone(), name: state.name.clone() });
                    } else {
                        app.model.load_state(&state.id);
                    }
                    ui.close();
                }
                if ui.button(widgets::icon_label(icons::PENCIL, "", None)).on_hover_text("Rename").clicked() {
                    app.dialog = Some(Dialog::RenameState { id: state.id.clone(), name: state.name.clone() });
                    ui.close();
                }
                if ui
                    .button(widgets::icon_label(icons::TRASH, "", Some(theme::DANGER)))
                    .on_hover_text("Delete")
                    .clicked()
                {
                    app.dialog = Some(Dialog::DeleteState { id: state.id.clone(), name: state.name.clone() });
                    ui.close();
                }
            });
        });
    });
}

fn thumbnail(app: &mut App, ctx: &egui::Context, path: &PathBuf) -> Option<egui::TextureHandle> {
    app.states_ui
        .thumbnails
        .entry(path.clone())
        .or_insert_with(|| {
            let bytes = std::fs::read(path).ok()?;
            let rgba = image::load_from_memory(&bytes).ok()?.to_rgba8();
            let image =
                egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], &rgba);
            Some(ctx.load_texture(path.to_string_lossy(), image, egui::TextureOptions::LINEAR))
        })
        .clone()
}

/// When a state was saved, in local time ("Today 14:05", "2026-11-20 09:12").
fn when(created_ms: i64) -> String {
    let t = lighttouch_core::local_time_of(created_ms / 1000);
    let now = lighttouch_core::local_time();
    if (t.year, t.month, t.day) == (now.year, now.month, now.day) {
        format!("Today {:02}:{:02}", t.hour, t.minute)
    } else {
        format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute)
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / (1u64 << 20) as f64)
    }
}

/// The confirmations the menu opens. Returns whether the dialog closes.
pub fn load_dialog(app: &mut App, ui: &mut Ui, id: &str, name: &str) -> bool {
    let p = app.palette;
    let device = app.model.current().map(|d| d.name.clone()).unwrap_or_default();
    ui.label(widgets::icon_text(icons::FLOPPY, 26.0).color(p.primary));
    ui.add_space(6.0);
    ui.label(RichText::new(format!("Load \u{201c}{name}\u{201d}?")).font(theme::title()));
    ui.add_space(4.0);
    ui.label(widgets::muted(
        &p,
        format!(
            "{device} goes back to that moment: what it has done since is replaced, as if it never happened. Save a state first to keep it."
        ),
    ));
    ui.add_space(16.0);
    let mut close = false;
    widgets::dialog_buttons(ui, |ui| {
        if widgets::primary(ui, &p, "Load").clicked() {
            app.model.load_state(id);
            close = true;
        }
        if widgets::text_button(ui, &p, "Cancel").clicked() {
            close = true;
        }
    });
    close
}

pub fn delete_dialog(app: &mut App, ui: &mut Ui, id: &str, name: &str) -> bool {
    let p = app.palette;
    ui.label(widgets::icon_text(icons::TRASH, 26.0).color(theme::DANGER));
    ui.add_space(6.0);
    ui.label(RichText::new(format!("Delete \u{201c}{name}\u{201d}?")).font(theme::title()));
    ui.add_space(4.0);
    ui.label(widgets::muted(&p, "The save state is removed from this computer. The device itself isn't changed."));
    ui.add_space(16.0);
    let mut close = false;
    widgets::dialog_buttons(ui, |ui| {
        if widgets::danger(ui, "Delete").clicked() {
            app.model.delete_state(id);
            close = true;
        }
        if widgets::text_button(ui, &p, "Cancel").clicked() {
            close = true;
        }
    });
    close
}

/// The rename dialog's body; `name` is edited in place. Returns whether it closes.
pub fn rename_dialog(app: &mut App, ui: &mut Ui, id: &str, name: &mut String) -> bool {
    let p = app.palette;
    ui.label(RichText::new("Rename save state").font(theme::title()));
    ui.add_space(10.0);
    let edit = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY).margin(vec2(10.0, 8.0)));
    if !edit.has_focus() && !edit.lost_focus() {
        edit.request_focus();
    }
    let fits = !name.trim().is_empty() && name.trim().len() < 128;
    let submit = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    ui.add_space(16.0);
    let mut close = false;
    let mut save = false;
    widgets::dialog_buttons(ui, |ui| {
        save = widgets::primary_enabled(ui, &p, "Save", fits).clicked();
        if widgets::text_button(ui, &p, "Cancel").clicked() {
            close = true;
        }
    });
    if (save || submit) && fits {
        app.model.rename_state(id, name);
        close = true;
    }
    close
}

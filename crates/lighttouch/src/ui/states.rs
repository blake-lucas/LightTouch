//! Save states on screen: the dock's States button (the stopped screen's, full screen's More › States…) opens a panel
//! with Save state and the device's states, each a card with its screen, its name, when and how big, delete
//! (confirmed), rename in place and Load; and the picture a state shows while it loads. Laid out as Android's States
//! sheet (StatesSheet.kt). The behaviour is the model's (lighttouch-model states.rs).
use super::{App, Dialog, icons, theme, widgets};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, RichText, Stroke, Ui, vec2,
};
use lighttouch_core::library::SaveState;
use std::collections::HashMap;
use std::path::PathBuf;

/// Width of the panel, and the height of a state's picture in it.
const PANEL_WIDTH: f32 = 520.0;
const THUMB_HEIGHT: f32 = 76.0;

/// The panel's state and the thumbnails decoded so far.
#[derive(Default)]
pub struct StatesUi {
    thumbnails: HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// The panel is up, its bottom centre here (above the button that opened it).
    anchor: Option<Pos2>,
    /// The button that opened it: a click there toggles rather than counting as outside.
    button: Option<Rect>,
    /// A state being renamed in place, and the name as typed.
    renaming: Option<(String, String)>,
    /// The furthest a load shown restoring has got (its state's id): between its steps the job reports no fraction
    /// for a moment, and the bar holds rather than going back to sliding.
    furthest: Option<(String, f32)>,
    /// Open the panel at the next frame (devshot's LIGHTTOUCH_UI_STATES).
    pub open: bool,
}

/// Whether the dock shows the States button: the setting is on and a device is on screen.
pub fn wants_button(app: &App) -> bool {
    app.model.settings.states_button && app.model.current().is_some()
}

/// The dock's States button: opens and closes the panel above it.
pub fn dock_button(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let busy = app.model.state_job().is_some() || app.model.state_load_pending();
    let open = app.states_ui.anchor.is_some();
    let r = widgets::dock_button(ui, &p, icons::FLOPPY, "States", true, (open || busy).then_some(p.primary));
    if r.clicked() || std::mem::take(&mut app.states_ui.open) {
        toggle(app, r.rect);
    }
}

/// Open the panel above `button` (or close it when it is open).
pub fn toggle(app: &mut App, button: Rect) {
    if app.states_ui.anchor.take().is_none() {
        app.states_ui.anchor = Some(egui::pos2(button.center().x, button.top() - 10.0));
        app.states_ui.button = Some(button);
        app.states_ui.renaming = None;
    }
}

/// Open the panel from the stopped screen's States button: as from the dock, at the bottom of the window.
pub fn open_from_screen(app: &mut App, ctx: &egui::Context, button: Rect) {
    let screen = ctx.content_rect();
    app.states_ui.anchor = Some(egui::pos2(button.center().x, screen.bottom() - 24.0));
    app.states_ui.button = Some(button);
    app.states_ui.renaming = None;
}

/// Open the panel in the middle of the window's bottom (full screen's More › States…, which has no dock).
pub fn open_centered(app: &mut App, ctx: &egui::Context) {
    let screen = ctx.content_rect();
    app.states_ui.anchor = Some(egui::pos2(screen.center().x, screen.bottom() - 24.0));
    app.states_ui.button = None;
    app.states_ui.renaming = None;
}

/// The panel, while open: drawn over the page, closed by Escape or a click outside it.
pub fn panel(app: &mut App, ctx: &egui::Context) {
    let Some(anchor) = app.states_ui.anchor else { return };
    if app.model.current().is_none() {
        app.states_ui.anchor = None;
        return;
    }
    let screen = ctx.content_rect();
    // Below the page's header, where there is room; never taller than a long list needs to stay readable.
    let height = (anchor.y - screen.top() - 72.0).clamp(220.0, 760.0);
    let width = PANEL_WIDTH.min(screen.width() - 32.0);
    let left = (anchor.x - width / 2.0).clamp(screen.left() + 16.0, (screen.right() - 16.0 - width).max(16.0));
    let p = app.palette;
    let mut close = false;
    // Escape while renaming only ends the rename.
    let renaming = app.states_ui.renaming.is_some();
    let area = egui::Area::new(egui::Id::unique("states-panel"))
        // Over the page, under the confirmations it opens (modals are Foreground).
        .order(egui::Order::Middle)
        .pivot(Align2::LEFT_BOTTOM)
        .fixed_pos(egui::pos2(left, anchor.y))
        .show(ctx, |ui| {
            Frame::new()
                .fill(p.container_low)
                .stroke(Stroke::new(1.0, p.outline.gamma_multiply(0.35)))
                .corner_radius(CornerRadius::same(theme::radius::DOCK))
                .inner_margin(Margin::same(18))
                .shadow(egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(70) })
                .show(ui, |ui| {
                    ui.set_width(width - 36.0);
                    ui.set_max_height(height - 36.0);
                    close = contents(app, ui);
                });
        });
    let rect = area.response.rect;
    let outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|pos| !rect.contains(pos) && !app.states_ui.button.is_some_and(|b| b.contains(pos)))
    });
    // A confirmation dialog over the panel takes the clicks; the panel stays for when it closes.
    let dialog = matches!(app.dialog, Some(Dialog::LoadState { .. } | Dialog::DeleteState { .. }));
    if close || (outside && !dialog) || (ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !dialog && !renaming) {
        app.states_ui.anchor = None;
        app.states_ui.renaming = None;
    }
}

/// The panel's contents, as Android's sheet: the title and the device with Save state beside them, then a card per
/// state. Whether it should close (a load started).
fn contents(app: &mut App, ui: &mut Ui) -> bool {
    let p = app.palette;
    let mut close = false;
    let subtitle = app.model.current().map(|d| d.subtitle.clone()).unwrap_or_default();
    let can_save = app.model.can_save_state();
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new("Save states").family(theme::semibold()).size(19.0));
            ui.label(widgets::muted(&p, &subtitle).size(13.0));
        });
        // A stopped device can't be saved: its panel only loads, renames and deletes.
        if app.model.session_active() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if pill(ui, icons::PLUS, "Save state", p.primary, p.on_primary, 40.0, can_save).clicked() {
                    app.model.save_state(None);
                }
            });
        }
    });
    if !can_save && app.model.state_job().is_none() && !app.model.session_active() {
        ui.add_space(6.0);
        ui.label(widgets::muted(&p, "Start the device to save its state.").size(12.5));
    }
    if let Some(job) = app.model.state_job().cloned() {
        ui.add_space(10.0);
        ui.label(widgets::muted(&p, format!("{}… {:.0}%", job.label(), job.fraction() * 100.0)).size(12.5));
        ui.add_space(4.0);
        widgets::progress(ui, &p, (job.total > 0).then(|| job.fraction()), ui.available_width());
    }
    ui.add_space(16.0);
    let states = app.model.states().to_vec();
    if states.is_empty() {
        ui.label(widgets::muted(
            &p,
            "No saved states yet. Save one while the device runs; loading it later takes the device back to that moment.",
        ));
        return close;
    }
    let can_load = app.model.can_load_state();
    ui.scope(|ui| {
        // The scroll bar takes room of its own beside the cards, never over them.
        ui.spacing_mut().scroll.floating = false;
        ui.spacing_mut().scroll.bar_inner_margin = 6.0;
        egui::ScrollArea::vertical().id_salt("states-list").auto_shrink([false, true]).show(ui, |ui| {
            for (i, state) in states.iter().enumerate() {
                if i > 0 {
                    ui.add_space(10.0);
                }
                if row(app, ui, state, can_load) {
                    close = true;
                }
            }
        });
    });
    close
}

/// One state's card: its screen at the device's shape, its name and when and how big, then delete, rename and Load
/// at the right edge. Whether a load started.
fn row(app: &mut App, ui: &mut Ui, state: &SaveState, can_load: bool) -> bool {
    let p = app.palette;
    let mut loaded = false;
    Frame::new()
        .fill(p.container_lowest)
        .stroke(Stroke::new(1.0, p.outline.gamma_multiply(0.3)))
        .corner_radius(CornerRadius::same(theme::radius::CARD))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let width = match (state.thumbnail_width, state.thumbnail_height) {
                    (w, h) if w > 0 && h > 0 => (THUMB_HEIGHT * w as f32 / h as f32).min(THUMB_HEIGHT * 1.5),
                    _ => THUMB_HEIGHT * 0.66,
                };
                let (rect, _) = ui.allocate_exact_size(vec2(width, THUMB_HEIGHT), egui::Sense::hover());
                let texture = state.thumbnail.as_ref().and_then(|path| thumbnail(app, ui.ctx(), path));
                ui.painter().rect_filled(rect, CornerRadius::same(8), p.container_highest);
                if let Some(texture) = texture {
                    egui::Image::new((texture.id(), rect.size()))
                        .corner_radius(CornerRadius::same(8))
                        .paint_at(ui, rect);
                }
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let tint = p.primary.gamma_multiply(0.2);
                    let load = pill(ui, icons::PLAY, "Load", tint, p.primary, 40.0, can_load)
                        .on_hover_text("Carry on from this moment");
                    if load.clicked() {
                        if app.model.session_active() {
                            app.dialog = Some(Dialog::LoadState { id: state.id.clone(), name: state.name.clone() });
                        } else {
                            app.model.load_state(&state.id);
                            loaded = true;
                        }
                    }
                    ui.add_space(4.0);
                    if icon_action(ui, &p, icons::PENCIL, "Rename", p.on_surface_variant).clicked() {
                        app.states_ui.renaming = Some((state.id.clone(), state.name.clone()));
                    }
                    if icon_action(ui, &p, icons::TRASH, "Delete", theme::DANGER).clicked() {
                        app.dialog = Some(Dialog::DeleteState { id: state.id.clone(), name: state.name.clone() });
                    }
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            name_line(app, ui, state);
                            let detail = format!("{} · {}", when(state.created_ms), size(state.bytes));
                            ui.add(egui::Label::new(widgets::muted(&p, detail).size(13.0)).truncate());
                        });
                    });
                });
            });
        });
    loaded
}

/// The state's name, two lines at most and cut with an ellipsis (the whole name on hover), or its rename field.
fn name_line(app: &mut App, ui: &mut Ui, state: &SaveState) {
    let editing = app.states_ui.renaming.as_ref().is_some_and(|(id, _)| *id == state.id);
    if !editing {
        let mut job = egui::text::LayoutJob::single_section(
            state.name.clone(),
            egui::TextFormat::simple(FontId::new(15.5, theme::semibold()), ui.visuals().strong_text_color()),
        );
        job.wrap = egui::text::TextWrapping {
            max_width: ui.available_width(),
            max_rows: 2,
            break_anywhere: false,
            overflow_character: Some('…'),
        };
        ui.add(egui::Label::new(job)).on_hover_text(&state.name);
        return;
    }
    let Some((_, text)) = app.states_ui.renaming.as_mut() else { return };
    let edit = ui.add(
        egui::TextEdit::singleline(text)
            .desired_width(ui.available_width())
            .margin(vec2(8.0, 4.0))
            .char_limit(120)
            .hint_text("Name"),
    );
    if !edit.has_focus() && !edit.lost_focus() {
        edit.request_focus();
    }
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if escape {
        app.states_ui.renaming = None;
    } else if edit.lost_focus() {
        // Enter, or a click elsewhere: keep the name when it is one.
        let name = text.trim().to_string();
        app.states_ui.renaming = None;
        if !name.is_empty() && name != state.name {
            app.model.rename_state(&state.id, &name);
        }
    }
}

/// A pill-shaped button with an icon before its label (Save state, Load).
fn pill(
    ui: &mut Ui,
    icon: &str,
    text: &str,
    fill: Color32,
    color: Color32,
    height: f32,
    enabled: bool,
) -> egui::Response {
    let (fill, color) = if enabled { (fill, color) } else { (fill.gamma_multiply(0.35), color.gamma_multiply(0.5)) };
    let mut job = egui::text::LayoutJob::default();
    let format =
        |font: FontId| egui::TextFormat { font_id: font, color, valign: egui::Align::Center, ..Default::default() };
    // Play is solid; plus has no solid form (its filled glyph is a box).
    let family = if icon == icons::PLAY { theme::icons_fill() } else { theme::icons() };
    job.append(icon, 0.0, format(FontId::new(18.0, family)));
    job.append(text, 6.0, format(FontId::new(14.5, theme::semibold())));
    let button = egui::Button::new(job)
        .fill(fill)
        .corner_radius(CornerRadius::same(theme::radius::PILL))
        .min_size(vec2(0.0, height));
    let r = ui
        .scope(|ui| {
            ui.spacing_mut().button_padding = vec2(16.0, 0.0);
            ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::NONE;
            ui.visuals_mut().widgets.hovered.bg_stroke = Stroke::NONE;
            ui.add_enabled(enabled, button)
        })
        .inner;
    if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

/// A round icon button in a given colour (the card's delete and rename).
fn icon_action(ui: &mut Ui, p: &theme::Palette, icon: &str, tooltip: &str, color: Color32) -> egui::Response {
    let size = 38.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), egui::Sense::click());
    let fill = if response.is_pointer_button_down_on() {
        p.outline.gamma_multiply(0.5)
    } else if response.hovered() {
        p.container_highest
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().circle_filled(rect.center(), size / 2.0, fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::new(19.0, theme::icons()), color);
    response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tooltip)
}

/// While a state loads into the device on screen (the model's `restoring`): its picture over the screen, dimmed, with
/// the load's progress and percentage in the middle, until the state runs. Whether it drew.
pub fn restoring_overlay(app: &mut App, ui: &mut Ui, screen: Rect) -> bool {
    let Some(restoring) = app.model.restoring().cloned() else { return false };
    let p = app.palette;
    let painter = ui.painter().with_clip_rect(screen);
    painter.rect_filled(screen, 0.0, Color32::BLACK);
    if let Some(texture) = restoring.thumbnail.as_ref().and_then(|path| thumbnail(app, ui.ctx(), path)) {
        // The picture is a frame as the engine scans it out: turned to the screen as the guest's frames are.
        let (rotation, turned) = (app.model.settings.rotation, app.guest.scan_turned());
        let mut mesh = egui::Mesh::with_texture(texture.id());
        for corner in [screen.left_top(), screen.right_top(), screen.right_bottom(), screen.left_bottom()] {
            let u = ((corner.x - screen.left()) / screen.width()) as f64;
            let v = ((corner.y - screen.top()) / screen.height()) as f64;
            let (x, y) = rotation.to_scan(u, v, turned);
            mesh.vertices.push(egui::epaint::Vertex {
                pos: corner,
                uv: egui::pos2(x as f32, y as f32),
                color: Color32::WHITE,
            });
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    }
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha(150));
    let now = app.model.state_job().filter(|j| j.total > 0).map(|j| j.fraction());
    let furthest = &mut app.states_ui.furthest;
    if furthest.as_ref().is_none_or(|(id, _)| *id != restoring.state) {
        *furthest = None;
    }
    if let Some(f) = now
        && furthest.as_ref().is_none_or(|(_, best)| f > *best)
    {
        *furthest = Some((restoring.state.clone(), f));
    }
    let fraction = furthest.as_ref().map(|(_, f)| *f);
    let center = screen.center();
    let label = match fraction {
        Some(f) => format!("Restoring… {:.0}%", f * 100.0),
        None => "Restoring…".to_string(),
    };
    let width = (screen.width() * 0.6).clamp(80.0, 220.0);
    // A dark plate under the words and the bar keeps them readable over any picture.
    let plate = Rect::from_center_size(center - vec2(0.0, 3.0), vec2(width + 40.0, 66.0));
    painter.rect_filled(plate, CornerRadius::same(theme::radius::ROW), Color32::from_black_alpha(170));
    painter.text(
        center - vec2(0.0, 16.0),
        Align2::CENTER_CENTER,
        label,
        FontId::new(14.0, theme::medium()),
        Color32::from_white_alpha(230),
    );
    let bar = Rect::from_center_size(center + vec2(0.0, 10.0), vec2(width, 6.0));
    painter.rect_filled(bar, CornerRadius::same(3), Color32::from_white_alpha(50));
    let fill = match fraction {
        Some(f) => Rect::from_min_size(bar.min, vec2((bar.width() * f.clamp(0.0, 1.0)).max(6.0), bar.height())),
        None => {
            let t = (ui.input(|i| i.time) * 0.8).fract() as f32;
            let seg = bar.width() * 0.3;
            let x0 = bar.left() + (bar.width() + seg) * t - seg;
            Rect::from_min_max(
                egui::pos2(x0.max(bar.left()), bar.top()),
                egui::pos2((x0 + seg).min(bar.right()), bar.bottom()),
            )
        }
    };
    if fill.width() > 0.0 {
        painter.rect_filled(fill, CornerRadius::same(3), p.primary);
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
    true
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

/// When a state was saved, in local time, as on Android: "Today 14:05", "Nov 20 09:12", "Dec 30, 2025 18:40".
fn when(created_ms: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let t = lighttouch_core::local_time_of(created_ms / 1000);
    let now = lighttouch_core::local_time();
    if (t.year, t.month, t.day) == (now.year, now.month, now.day) {
        format!("Today {:02}:{:02}", t.hour, t.minute)
    } else {
        let month = MONTHS[(t.month as usize).clamp(1, 12) - 1];
        let year = if t.year == now.year { String::new() } else { format!(", {}", t.year) };
        format!("{month} {}{year} {:02}:{:02}", t.day, t.hour, t.minute)
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / (1u64 << 20) as f64)
    }
}

/// The confirmations the panel opens. Returns whether the dialog closes.
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
            "{device} goes back to that moment: what it has done since is replaced, as if it never happened. \
             Save a state first to keep it."
        ),
    ));
    ui.add_space(16.0);
    let mut close = false;
    widgets::dialog_buttons(ui, |ui| {
        if widgets::primary(ui, &p, "Load").clicked() {
            app.model.load_state(id);
            app.states_ui.anchor = None;
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

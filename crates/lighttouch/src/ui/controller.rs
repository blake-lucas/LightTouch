//! Game controllers and keyboard bindings: the Controller window (presets, the controllers connected and what they are
//! doing, and the bindings), the spots the active preset puts on the device's screen (moved by dragging while the
//! layout is being edited), and the per-frame step that drives the device from them (lighttouch_model::gamepad).
use super::{App, icons, theme, widgets};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2,
};
use lighttouch_model::Model;
use lighttouch_model::gamepad::mapper::View;
use lighttouch_model::gamepad::preset::{
    Action, Binding, DeviceButton, Input, PadButton, Point, PointerStyle, Preset, Stick, TapMode,
};
use lighttouch_model::gamepad::sdl::{Backend, Pad, PadKind};
use std::collections::HashSet;
use std::time::{Duration, Instant};

pub const GAME_CONTROLLER: &str = "\u{E26E}";
const HAND_TAP: &str = "\u{EC90}";
const SWIPE: &str = "\u{EC92}";
const JOYSTICK: &str = "\u{EA5E}";
const CURSOR: &str = "\u{E1DC}";
const FLICK: &str = "\u{E0A4}";
const TILT: &str = "\u{E1E0}";
const MOTION: &str = "\u{E1C8}";
const HOUSE: &str = "\u{E2C2}";
const COPY: &str = "\u{E1CA}";
const EYE: &str = "\u{E220}";
const ARROWS_OUT: &str = "\u{E0A2}";

#[derive(Default)]
pub struct ControllerUi {
    pub open: bool,
    /// The binding whose settings are showing.
    expanded: Option<usize>,
    capture: Option<Capture>,
    rename: Option<String>,
    drag: Option<(usize, Handle)>,
    /// The Controller window had the keyboard last frame.
    window_focused: bool,
    asked_front: Option<Instant>,
}

/// Waiting for the input to bind: which binding and which of its inputs.
#[derive(Clone)]
struct Capture {
    binding: usize,
    slot: Slot,
    /// What was already held when it started (so the click that started it doesn't bind).
    held: (u32, HashSet<String>),
    sticks: [f32; 4],
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Slot {
    Main,
    With,
    Touch,
    Stick,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Handle {
    Main,
    End,
    Size,
}

/// Keys the active preset uses: they act only through it, not as H, P, the arrows and so on.
pub fn bound_keys(model: &Model) -> HashSet<String> {
    let mut keys = HashSet::new();
    let Some(p) = model.gamepads.active_preset() else { return keys };
    let add = |i: &Input, keys: &mut HashSet<String>| {
        if let Input::Key(k) = i {
            if !k.is_empty() {
                keys.insert(k.clone());
            }
        }
    };
    for b in &p.bindings {
        if let Some(w) = &b.with {
            add(w, &mut keys);
        }
        match &b.action {
            Action::Tap { input, .. } | Action::Swipe { input, .. } | Action::Device { input, .. } => {
                add(input, &mut keys)
            }
            Action::Pointer { touch, stick, .. } => {
                add(touch, &mut keys);
                stick_keys(stick, &mut keys);
            }
            Action::Joystick { stick, .. } | Action::Flick { stick, .. } | Action::Tilt { stick, .. } => {
                stick_keys(stick, &mut keys)
            }
            Action::Motion { .. } => {}
        }
    }
    keys
}

fn stick_keys(stick: &Stick, keys: &mut HashSet<String>) {
    if let Stick::Keys { up, down, left, right } = stick {
        keys.extend([up, down, left, right].into_iter().cloned());
    }
}

fn view(app: &App, screen: Rect) -> View {
    View {
        rotation: app.model.settings.rotation,
        turned: app.guest.scan_turned(),
        aspect: screen.width() / screen.height().max(1.0),
    }
}

/// Drive the device from the active preset for this frame (after the screen is drawn).
pub fn step(app: &mut App, ui: &Ui, screen: Rect) {
    let ctx = ui.ctx().clone();
    let view = view(app, screen);
    let bound = bound_keys(&app.model);
    let keys: HashSet<String> = if ctx.egui_wants_keyboard_input() || bound.is_empty() {
        HashSet::new()
    } else {
        ctx.input(|i| i.keys_down.iter().map(|k| k.name().to_string()).filter(|k| bound.contains(k)).collect())
    };
    let focused = ctx.input(|i| i.focused) || app.controller.window_focused;
    let listening = focused || app.model.settings.controller_background;
    let model = &mut app.model;
    if model.gamepads.step(&keys, view, listening, model.session.as_mut()) {
        ctx.request_repaint_after(Duration::from_millis(8));
    }
}

fn kind(app: &App) -> PadKind {
    app.model.gamepads.pads().pads.first().map(|p| p.kind).unwrap_or_default()
}

/// The spot style: a dark disc with a light edge, lit in the accent while its input acts.
fn spot(painter: &egui::Painter, at: Pos2, r: f32, label: &str, lit: bool, editing: bool) {
    let fill = if lit {
        theme::ACCENT.gamma_multiply(0.75)
    } else {
        Color32::from_black_alpha(if editing { 150 } else { 105 })
    };
    let edge = if lit { theme::ACCENT } else { Color32::from_white_alpha(if editing { 230 } else { 170 }) };
    painter.circle(at, r, fill, Stroke::new(1.5, edge));
    let size = if label.chars().count() > 3 { 9.5 } else { 11.5 };
    let text = if lit { Color32::from_rgb(0x00, 0x20, 0x1B) } else { Color32::WHITE };
    painter.text(at, Align2::CENTER_CENTER, label, FontId::new(size, theme::semibold()), text);
}

fn arrow(painter: &egui::Painter, from: Pos2, to: Pos2, color: Color32) {
    let stroke = Stroke::new(2.0, color);
    painter.line_segment([from, to], stroke);
    let d = (to - from).normalized();
    let n = vec2(-d.y, d.x);
    painter.line_segment([to, to - d * 9.0 + n * 5.0], stroke);
    painter.line_segment([to, to - d * 9.0 - n * 5.0], stroke);
}

/// The active preset's spots on the device's screen, its pointers' bubbles, and, while editing, dragging them.
pub fn overlay(app: &mut App, ui: &mut Ui, screen: Rect) {
    let Some(index) = app.model.gamepads.active else { return };
    if app.model.session.as_ref().is_some_and(|s| s.display_asleep()) {
        return;
    }
    let editing = app.model.gamepads.editing;
    let mut preset = app.model.gamepads.presets[index].clone();
    let view = view(app, screen);
    let kind = kind(app);
    let painter = ui.painter().with_clip_rect(screen);
    let px = |(u, v): (f32, f32)| pos2(screen.left() + u * screen.width(), screen.top() + v * screen.height());
    let short = screen.width().min(screen.height());
    let r = (short * 0.045).clamp(11.0, 19.0);
    let show = editing || preset.show_spots;
    let mut changed = false;
    let mut released = false;

    for (i, binding) in preset.bindings.iter_mut().enumerate() {
        let lit = app.model.gamepads.mapper.active(i);
        let finger = app.model.gamepads.mapper.finger(i);
        let with = binding.with.as_ref().map(|w| format!("{}+", w.label(kind))).unwrap_or_default();
        let mut handles: Vec<(Handle, Pos2)> = Vec::new();
        match &binding.action {
            Action::Tap { input, at, .. } => {
                let at = px(view.shown(*at));
                if show {
                    spot(&painter, at, r, &format!("{with}{}", short_label(input, kind)), lit, editing);
                }
                handles.push((Handle::Main, at));
            }
            Action::Swipe { input, from, to, .. } => {
                let (a, b) = (px(view.shown(*from)), px(view.shown(*to)));
                if show {
                    arrow(&painter, a, b, if lit { theme::ACCENT } else { Color32::from_white_alpha(200) });
                    spot(&painter, a, r, &format!("{with}{}", short_label(input, kind)), lit, editing);
                }
                handles.extend([(Handle::Main, a), (Handle::End, b)]);
            }
            Action::Joystick { stick, center, radius } => {
                let c = px(view.shown(*center));
                let ring = radius * short;
                if show {
                    painter.circle(
                        c,
                        ring,
                        Color32::from_black_alpha(40),
                        Stroke::new(1.5, Color32::from_white_alpha(150)),
                    );
                    spot(&painter, c, r * 0.8, &stick.short(), lit, editing);
                    if let Some(f) = finger {
                        painter.circle_filled(px(f), r * 0.55, theme::ACCENT.gamma_multiply(0.8));
                    }
                }
                handles.extend([(Handle::Main, c), (Handle::Size, c + vec2(ring, 0.0))]);
            }
            Action::Flick { stick, center, length } => {
                let c = px(view.shown(*center));
                let (w, h) = view.span(*length);
                let (dx, dy) = (w * screen.width(), h * screen.height());
                if show {
                    let color = if lit { theme::ACCENT } else { Color32::from_white_alpha(150) };
                    for d in [vec2(dx, 0.0), vec2(-dx, 0.0), vec2(0.0, dy), vec2(0.0, -dy)] {
                        arrow(&painter, c + d * 0.35, c + d, color);
                    }
                    spot(&painter, c, r * 0.8, &stick.short(), lit, editing);
                }
                handles.extend([(Handle::Main, c), (Handle::Size, c + vec2(dx, 0.0))]);
            }
            Action::Pointer { center, half, .. } => {
                let c = px(view.shown(*center));
                let corner = c + vec2(half[0] * screen.width(), half[1] * screen.height());
                if show {
                    let area = Rect::from_two_pos(c - (corner - c), corner);
                    painter.rect_stroke(
                        area,
                        CornerRadius::same(8),
                        Stroke::new(1.0, Color32::from_white_alpha(if editing { 170 } else { 70 })),
                        egui::StrokeKind::Inside,
                    );
                }
                if editing {
                    spot(&painter, c, r * 0.8, "⌖", false, true);
                } else {
                    // Always drawn: a centre-returning pointer is useless unseen.
                    let (at, touching) = app.model.gamepads.mapper.pointer(i).unwrap_or((view.shown(*center), false));
                    let p = px(at);
                    let (sx, sy) = view.rotation.to_scan(at.0 as f64, at.1 as f64, view.turned);
                    let under = app.guest.sample(sx, sy).unwrap_or(Color32::GRAY);
                    super::guest::bubble(&painter, p, (short * 0.035).clamp(9.0, 15.0), under, touching);
                }
                handles.extend([(Handle::Main, c), (Handle::Size, corner)]);
            }
            Action::Tilt { .. } | Action::Motion { .. } | Action::Device { .. } => {}
        }
        if !editing {
            continue;
        }
        for (handle, at) in handles {
            let at = screen.shrink(8.0).clamp(at);
            let size = if handle == Handle::Main { r * 2.2 } else { 18.0 };
            if handle != Handle::Main {
                painter.circle(at, 6.0, theme::ACCENT, Stroke::new(1.5, Color32::from_black_alpha(160)));
            }
            let response = ui.interact(
                Rect::from_center_size(at, vec2(size, size)),
                ui.scope_id().with(("ctl-handle", i, handle as u8)),
                Sense::drag(),
            );
            let response = response.on_hover_cursor(egui::CursorIcon::Grab);
            if response.drag_started() {
                app.controller.drag = Some((i, handle));
                app.controller.expanded = Some(i);
            }
            if response.dragged() && app.controller.drag == Some((i, handle)) {
                if let Some(p) = response.interact_pointer_pos() {
                    let p = screen.clamp(p);
                    let (u, v) = ((p.x - screen.left()) / screen.width(), (p.y - screen.top()) / screen.height());
                    drag_to(&mut binding.action, handle, u, v, &view, screen, short);
                    changed = true;
                }
            }
            if response.drag_stopped() {
                app.controller.drag = None;
                released = true;
            }
        }
    }
    if editing {
        edit_banner(app, ui, screen);
    }
    if changed || released {
        app.model.gamepads.presets[index] = preset;
        if released {
            app.model.gamepads.changed(app.model.session.as_mut());
        }
    }
}

fn short_label(input: &Input, kind: PadKind) -> String {
    match input {
        Input::Key(k) if k.is_empty() => "?".into(),
        Input::Pad(PadButton::DpadUp) => "↑".into(),
        Input::Pad(PadButton::DpadDown) => "↓".into(),
        Input::Pad(PadButton::DpadLeft) => "←".into(),
        Input::Pad(PadButton::DpadRight) => "→".into(),
        i => i.label(kind),
    }
}

/// Move a handle to (u, v) on the screen as shown.
fn drag_to(action: &mut Action, handle: Handle, u: f32, v: f32, view: &View, screen: Rect, short: f32) {
    let to = view.guest(u, v);
    let shown_px = |p: Point| {
        let (a, b) = view.shown(p);
        pos2(a * screen.width(), b * screen.height())
    };
    let here = pos2(u * screen.width(), v * screen.height());
    match (action, handle) {
        (Action::Tap { at, .. }, Handle::Main) => *at = to,
        (Action::Swipe { from, .. }, Handle::Main) => *from = to,
        (Action::Swipe { to: end, .. }, Handle::End) => *end = to,
        (
            Action::Joystick { center, .. } | Action::Flick { center, .. } | Action::Pointer { center, .. },
            Handle::Main,
        ) => *center = to,
        (Action::Joystick { center, radius, .. }, Handle::Size) => {
            *radius = (here.distance(shown_px(*center)) / short).clamp(0.04, 0.5)
        }
        (Action::Flick { center, length, .. }, Handle::Size) => {
            *length = (here.distance(shown_px(*center)) / short).clamp(0.05, 0.8)
        }
        (Action::Pointer { center, half, .. }, Handle::Size) => {
            let c = shown_px(*center);
            *half = [
                ((here.x - c.x).abs() / screen.width()).clamp(0.03, 1.0),
                ((here.y - c.y).abs() / screen.height()).clamp(0.03, 1.0),
            ];
        }
        _ => {}
    }
}

/// While the layout is being edited: what to do, and Done, along the top of the screen.
fn edit_banner(app: &mut App, ui: &mut Ui, screen: Rect) {
    let p = app.palette;
    let area = Rect::from_center_size(
        pos2(screen.center().x, screen.top() + 30.0),
        vec2(screen.width().min(300.0) - 16.0, 40.0),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
        Frame::new()
            .fill(p.container.gamma_multiply(0.94))
            .corner_radius(CornerRadius::same(20))
            .inner_margin(Margin { left: 14, right: 4, top: 4, bottom: 4 })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Drag spots to place them").size(12.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pill_button(ui, &p, "Done", true).clicked() {
                            app.model.gamepads.editing = false;
                        }
                    });
                });
            });
    });
}

/// A compact filled button for tight spots (the banner, the capture prompt).
fn pill_button(ui: &mut Ui, p: &theme::Palette, text: &str, primary: bool) -> egui::Response {
    let (fill, color) = if primary { (p.primary, p.on_primary) } else { (p.container_highest, p.on_surface) };
    ui.add(
        egui::Button::new(RichText::new(text).color(color).size(13.0).family(theme::semibold()))
            .fill(fill)
            .corner_radius(CornerRadius::same(15))
            .min_size(vec2(0.0, 30.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ------------------------------------------------------------------------------------------------ the window ----

/// The Controller window, while open: its own native window beside the main one.
pub fn window(app: &mut App, ctx: &egui::Context) {
    if !app.controller.open {
        app.controller.window_focused = false;
        return;
    }
    app.model.gamepads.start();
    let builder = egui::ViewportBuilder::default()
        .with_title("Controller")
        .with_app_id("org.lighttouch.LightTouch")
        .with_inner_size([1000.0, 700.0])
        .with_min_inner_size([860.0, 560.0]);
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("controller-window"), builder, |ui, _class| {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            app.controller.open = false;
            app.controller.capture = None;
            app.model.gamepads.editing = false;
        }
        app.controller.window_focused = ctx.input(|i| i.focused);
        let p = app.palette;
        egui::CentralPanel::default().frame(Frame::new().fill(p.background).inner_margin(Margin::same(0))).show(
            ui,
            |ui| {
                body(app, ui);
            },
        );
        // Keep the live view moving while a controller is connected.
        if !app.model.gamepads.pads().pads.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    });
}

fn body(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    header(app, ui);
    let gap = 20.0;
    let left_width = 340.0;
    let rest = ui.available_rect_before_wrap();
    let left = Rect::from_min_size(rest.min + vec2(24.0, 0.0), vec2(left_width, rest.height()));
    let right = Rect::from_min_max(pos2(left.right() + gap, rest.top()), rest.max - vec2(24.0, 0.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
        egui::ScrollArea::vertical().id_salt("ctl-left").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(left_width);
            controllers_card(app, ui);
            live_card(app, ui);
            options_card(app, ui);
            ui.add_space(20.0);
        });
    });
    ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
        egui::ScrollArea::vertical().id_salt("ctl-right").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(right.width());
            bindings(app, ui);
            ui.add_space(20.0);
        });
    });
    if app.controller.capture.is_some() {
        capture_prompt(app, ui);
    }
    let _ = p;
}

fn header(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    Frame::new().inner_margin(Margin { left: 24, right: 20, top: 16, bottom: 10 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            ui.label(widgets::icon_text(GAME_CONTROLLER, 26.0).color(p.primary));
            ui.add_space(2.0);
            ui.label(RichText::new("Controller").font(theme::headline()));
            ui.add_space(12.0);
            if let Some(name) = app.controller.rename.as_mut() {
                let edit = ui.add(
                    egui::TextEdit::singleline(name).desired_width(180.0).font(FontId::new(14.0, theme::medium())),
                );
                edit.request_focus();
                let done = edit.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
                if done {
                    let name = app.controller.rename.take().unwrap_or_default();
                    if let Some(i) = app.model.gamepads.active {
                        app.model.gamepads.rename(i, &name);
                        app.model.settings.controller_preset = app.model.gamepads.active_name();
                        app.model.save_settings();
                    }
                }
            } else {
                preset_menu(app, ui);
            }
            let has = app.model.gamepads.active.is_some();
            if widgets::icon_button_sized(ui, &p, icons::PLUS, "New preset", 34.0, true).clicked() {
                let i = app.model.gamepads.add(None, app.model.session.as_mut());
                app.controller.expanded = None;
                let _ = i;
                remember_active(app);
            }
            if widgets::icon_button_sized(ui, &p, COPY, "Duplicate", 34.0, has).clicked() {
                let copy = app.model.gamepads.active_preset().cloned();
                app.model.gamepads.add(copy, app.model.session.as_mut());
                remember_active(app);
            }
            if widgets::icon_button_sized(ui, &p, icons::PENCIL, "Rename", 34.0, has).clicked() {
                app.controller.rename = app.model.gamepads.active_name();
            }
            more_menu(app, ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let editing = app.model.gamepads.editing;
                let running = app.model.session.as_ref().is_some_and(|s| s.phase.active());
                let label = if editing { "Done placing" } else { "Place on screen" };
                let enabled = has && running;
                let r = if editing {
                    widgets::primary_enabled(ui, &p, label, enabled)
                } else {
                    widgets::tonal_enabled(ui, &p, label, enabled)
                };
                let r = r.on_disabled_hover_text("Start the device to place spots on its screen");
                if r.clicked() {
                    app.model.gamepads.editing = !editing;
                    app.model.gamepads.release(app.model.session.as_mut());
                }
            });
        });
    });
}

fn remember_active(app: &mut App) {
    app.model.settings.controller_preset = app.model.gamepads.active_name();
    app.model.save_settings();
}

fn preset_menu(app: &mut App, ui: &mut Ui) {
    let name = app.model.gamepads.active_name().unwrap_or_else(|| "Off".into());
    let r = widgets::chip_menu(ui, &app.palette, &name);
    egui::Popup::menu(&r).show(|ui| {
        ui.set_min_width(200.0);
        let mut pick = None;
        if ui.selectable_label(app.model.gamepads.active.is_none(), "Off").clicked() {
            pick = Some(None);
        }
        ui.separator();
        for (i, preset) in app.model.gamepads.presets.iter().enumerate() {
            if ui.selectable_label(app.model.gamepads.active == Some(i), &preset.name).clicked() {
                pick = Some(Some(i));
            }
        }
        if let Some(index) = pick {
            app.model.gamepads.set_active(index, app.model.session.as_mut());
            app.controller.expanded = None;
            remember_active(app);
        }
    });
}

fn more_menu(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let r = widgets::icon_button_sized(ui, &p, icons::MORE, "More", 34.0, true);
    egui::Popup::menu(&r).show(|ui| {
        ui.set_min_width(180.0);
        let has = app.model.gamepads.active.is_some();
        if ui.button(widgets::icon_label(icons::DOWNLOAD, "Import…", None)).clicked() {
            if let Some(file) = rfd::FileDialog::new().add_filter("Controller preset", &["json"]).pick_file() {
                match lighttouch_model::gamepad::preset::Store::import(&file) {
                    Ok(preset) => {
                        app.model.gamepads.add(Some(preset), app.model.session.as_mut());
                        remember_active(app);
                    }
                    Err(e) => app.model.error = Some(format!("Couldn't import: {e}")),
                }
            }
        }
        if ui.add_enabled(has, egui::Button::new(widgets::icon_label(icons::FILE, "Export…", None))).clicked() {
            if let Some(preset) = app.model.gamepads.active_preset().cloned() {
                if let Some(file) = rfd::FileDialog::new()
                    .add_filter("Controller preset", &["json"])
                    .set_file_name(format!("{}.json", preset.name))
                    .save_file()
                {
                    if let Err(e) = lighttouch_model::gamepad::preset::Store::export(&preset, &file) {
                        app.model.error = Some(format!("Couldn't export: {e}"));
                    }
                }
            }
        }
        ui.separator();
        let delete = egui::Button::new(widgets::icon_label(icons::TRASH, "Delete preset", Some(theme::DANGER)));
        if ui.add_enabled(has, delete).clicked() {
            if let Some(i) = app.model.gamepads.active {
                app.model.gamepads.remove(i, app.model.session.as_mut());
                app.controller.expanded = None;
                remember_active(app);
            }
        }
    });
}

fn controllers_card(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let pads = app.model.gamepads.pads();
    widgets::section(ui, &p, "Controllers");
    widgets::card(ui, &p, |ui| match &pads.backend {
        Backend::Unavailable(why) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::icon_text(icons::WARNING, 18.0).color(theme::WARNING));
                ui.label(RichText::new("Controllers need SDL2").size(14.5));
            });
            ui.label(widgets::muted(&p, sdl_hint()).size(12.5));
            ui.label(widgets::muted(&p, why.as_str()).size(11.5));
        }
        Backend::Starting => {
            ui.label(widgets::muted(&p, "Looking for controllers…"));
        }
        Backend::Ready if pads.pads.is_empty() => {
            ui.label(RichText::new("No controller connected").size(14.5));
            ui.label(
                widgets::muted(
                    &p,
                    "Connect one by USB or Bluetooth; it shows here. Keyboard bindings work without one.",
                )
                .size(12.5),
            );
        }
        Backend::Ready => {
            for pad in &pads.pads {
                ui.horizontal(|ui| {
                    ui.set_min_height(36.0);
                    ui.label(widgets::icon_text(GAME_CONTROLLER, 20.0).color(p.on_surface_variant));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add(egui::Label::new(RichText::new(&pad.name).size(14.0)).truncate());
                        let motion = if pad.gyro.is_some() { "Motion sensors" } else { "No motion sensors" };
                        ui.label(widgets::muted(&p, motion).size(12.0));
                    });
                });
            }
        }
    });
}

fn sdl_hint() -> &'static str {
    if cfg!(target_os = "linux") {
        "Install your distribution's SDL2 package (libsdl2) and reopen this window. Keyboard bindings work without it."
    } else {
        "LightTouch couldn't load SDL2. Keyboard bindings work without it."
    }
}

fn live_card(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let pads = app.model.gamepads.pads();
    widgets::section(ui, &p, "Live input");
    widgets::card(ui, &p, |ui| {
        let pad = pads.pads.first();
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 160.0), Sense::hover());
        draw_pad(ui.painter(), &p, rect, pad);
        if let Some(pad) = pad {
            ui.add_space(8.0);
            motion_bars(ui, &p, pad);
        }
        let held: Vec<String> = {
            let bound = bound_keys(&app.model);
            let mut v: Vec<String> = ui
                .ctx()
                .input(|i| i.keys_down.iter().map(|k| k.name().to_string()).filter(|k| bound.contains(k)).collect());
            v.sort();
            v
        };
        if !held.is_empty() {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::muted(&p, "Keys").size(12.0));
                for k in held {
                    widgets::key_cap(ui, &p, &lighttouch_model::gamepad::preset::key_label(&k));
                }
            });
        }
    });
}

/// A controller drawn from what it reports: sticks and their dots, triggers filling, buttons lit.
fn draw_pad(painter: &egui::Painter, p: &theme::Palette, rect: Rect, pad: Option<&Pad>) {
    let buttons = pad.map_or(0, |p| p.buttons);
    let axes = pad.map_or([0.0; 6], |p| p.axes);
    let kind = pad.map_or(PadKind::Xbox, |p| p.kind);
    let on = |b: PadButton| buttons & b.bit() != 0;
    let body = Rect::from_center_size(rect.center() + vec2(0.0, 12.0), vec2(rect.width().min(300.0), 120.0));
    let idle = p.container_highest;
    let lit = p.primary;
    painter.rect(
        body,
        CornerRadius::same(48),
        p.container_high,
        Stroke::new(1.0, p.outline_variant),
        egui::StrokeKind::Inside,
    );
    // Triggers and shoulders above the body.
    for (side, trigger, shoulder, value) in [
        (-1.0f32, PadButton::LeftTrigger, PadButton::LeftShoulder, axes[4]),
        (1.0, PadButton::RightTrigger, PadButton::RightShoulder, axes[5]),
    ] {
        let x = body.center().x + side * body.width() * 0.3;
        let t = Rect::from_center_size(pos2(x, rect.top() + 7.0), vec2(56.0, 10.0));
        painter.rect_filled(t, CornerRadius::same(5), idle);
        let mut fill = t;
        fill.set_width(t.width() * value.clamp(0.0, 1.0));
        painter.rect_filled(fill, CornerRadius::same(5), lit);
        painter.text(
            t.center(),
            Align2::CENTER_CENTER,
            trigger.label(kind),
            FontId::new(9.0, theme::semibold()),
            p.on_surface,
        );
        let s = Rect::from_center_size(pos2(x, rect.top() + 23.0), vec2(56.0, 11.0));
        painter.rect_filled(s, CornerRadius::same(5), if on(shoulder) { lit } else { idle });
        let text = if on(shoulder) { p.on_primary } else { p.on_surface_variant };
        painter.text(
            s.center(),
            Align2::CENTER_CENTER,
            shoulder.label(kind),
            FontId::new(8.5, theme::semibold()),
            text,
        );
    }
    let stick = |c: Pos2, x: f32, y: f32, click: bool| {
        painter.circle(c, 20.0, p.container, Stroke::new(1.0, p.outline));
        painter.circle_filled(c + vec2(x, y) * 13.0, 8.0, if click { lit } else { p.on_surface_variant });
    };
    let cy = body.center().y;
    let w = body.width();
    stick(pos2(body.left() + w * 0.2, cy - 14.0), axes[0], axes[1], on(PadButton::LeftStick));
    stick(pos2(body.left() + w * 0.62, cy + 26.0), axes[2], axes[3], on(PadButton::RightStick));
    // D-pad.
    let d = pos2(body.left() + w * 0.38, cy + 26.0);
    for (b, off) in [
        (PadButton::DpadUp, vec2(0.0, -11.0)),
        (PadButton::DpadDown, vec2(0.0, 11.0)),
        (PadButton::DpadLeft, vec2(-11.0, 0.0)),
        (PadButton::DpadRight, vec2(11.0, 0.0)),
    ] {
        painter.rect_filled(
            Rect::from_center_size(d + off, vec2(10.0, 10.0)),
            CornerRadius::same(2),
            if on(b) { lit } else { idle },
        );
    }
    // Face buttons.
    let f = pos2(body.left() + w * 0.8, cy - 14.0);
    for (b, off) in [
        (PadButton::North, vec2(0.0, -14.0)),
        (PadButton::South, vec2(0.0, 14.0)),
        (PadButton::West, vec2(-14.0, 0.0)),
        (PadButton::East, vec2(14.0, 0.0)),
    ] {
        let c = f + off;
        painter.circle_filled(c, 8.5, if on(b) { lit } else { idle });
        let label: String = b.label(kind).chars().take(1).collect();
        painter.text(
            c,
            Align2::CENTER_CENTER,
            label,
            FontId::new(9.0, theme::semibold()),
            if on(b) { p.on_primary } else { p.on_surface },
        );
    }
    // Back, guide, start.
    for (b, off) in [(PadButton::Back, -22.0), (PadButton::Guide, 0.0), (PadButton::Start, 22.0)] {
        let c = pos2(body.center().x + off, cy - 22.0);
        painter.circle_filled(c, if b == PadButton::Guide { 7.0 } else { 5.0 }, if on(b) { lit } else { idle });
    }
    if pad.is_none() {
        painter.rect_filled(body, CornerRadius::same(48), p.container.gamma_multiply(0.6));
        painter.text(
            body.center(),
            Align2::CENTER_CENTER,
            "No controller",
            FontId::new(13.0, theme::medium()),
            p.on_surface_variant,
        );
    }
}

/// The gyro's turn and the accelerometer's pull, one bar each per axis.
fn motion_bars(ui: &mut Ui, p: &theme::Palette, pad: &Pad) {
    let Some(gyro) = pad.gyro else {
        ui.label(widgets::muted(p, "This controller has no motion sensors").size(12.0));
        return;
    };
    let accel = pad.accel.unwrap_or_default();
    egui::Grid::new("motion-bars").num_columns(4).spacing(vec2(8.0, 4.0)).show(ui, |ui| {
        for (title, values, scale) in [("Gyro", gyro, 6.0f32), ("Accel", accel, 12.0)] {
            ui.label(widgets::muted(p, title).size(12.0));
            for (axis, v) in ["x", "y", "z"].iter().zip(values) {
                let (rect, _) = ui.allocate_exact_size(vec2(72.0, 14.0), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, CornerRadius::same(4), p.container_highest);
                let half = rect.width() / 2.0;
                let len = (v / scale).clamp(-1.0, 1.0) * half;
                let bar = Rect::from_min_max(
                    pos2(rect.center().x + len.min(0.0), rect.top()),
                    pos2(rect.center().x + len.max(0.0), rect.bottom()),
                );
                painter.rect_filled(bar, CornerRadius::same(4), p.primary);
                painter.text(
                    rect.left_center() + vec2(5.0, 0.0),
                    Align2::LEFT_CENTER,
                    *axis,
                    FontId::new(9.5, theme::medium()),
                    p.on_surface,
                );
            }
            ui.end_row();
        }
    });
}

fn options_card(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(index) = app.model.gamepads.active else { return };
    widgets::section(ui, &p, "Options");
    let mut preset = app.model.gamepads.presets[index].clone();
    let mut background = app.model.settings.controller_background;
    widgets::card(ui, &p, |ui| {
        widgets::setting_row(ui, &p, EYE, "Show spots", "Draw where each input touches while you play", |ui| {
            widgets::switch(ui, &p, &mut preset.show_spots);
        });
        widgets::setting_row(ui, &p, JOYSTICK, "Stick dead zone", "", |ui| {
            ui.add(egui::Slider::new(&mut preset.deadzone, 0.0..=0.5).show_value(false));
        });
        widgets::setting_row(
            ui,
            &p,
            icons::MONITOR,
            "In the background",
            "Keep acting when another window is in front",
            |ui| {
                widgets::switch(ui, &p, &mut background);
            },
        );
        front_app_row(app, ui, &mut preset);
    });
    if preset != app.model.gamepads.presets[index] {
        app.model.gamepads.presets[index] = preset;
        app.model.gamepads.changed(app.model.session.as_mut());
    }
    if background != app.model.settings.controller_background {
        app.model.settings.controller_background = background;
        app.model.save_settings();
    }
}

/// Link the preset to the app in front, so it switches on by itself for that game.
fn front_app_row(app: &mut App, ui: &mut Ui, preset: &mut Preset) {
    let p = app.palette;
    if let Some(session) = app.model.session.as_mut() {
        if app.controller.asked_front.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
            app.controller.asked_front = Some(Instant::now());
            session.watch_frontmost();
        }
    }
    let front = app.model.gamepads.front_app(app.model.session.as_ref()).filter(|(b, _)| b != "com.apple.springboard");
    let subtitle = if preset.apps.is_empty() {
        "Switch to this preset when its game is in front".to_string()
    } else {
        format!("On for {}", preset.apps.join(", "))
    };
    widgets::setting_row(ui, &p, icons::PHONE, "Games", &subtitle, |ui| {
        if let Some((bundle, name)) = front {
            let linked = preset.apps.contains(&bundle);
            let label = if linked { format!("Unlink {name}") } else { format!("Use for {name}") };
            if widgets::tonal(ui, &p, &label).clicked() {
                if linked {
                    preset.apps.retain(|a| a != &bundle);
                } else {
                    preset.apps.push(bundle);
                }
            }
        } else if !preset.apps.is_empty() && widgets::tonal(ui, &p, "Clear").clicked() {
            preset.apps.clear();
        }
    });
}

// ------------------------------------------------------------------------------------------------- bindings ----

fn bindings(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    ui.horizontal(|ui| {
        widgets::section(ui, &p, "Bindings");
    });
    let Some(index) = app.model.gamepads.active else {
        widgets::card(ui, &p, |ui| {
            ui.label(RichText::new("Bindings are off").size(14.5));
            ui.label(widgets::muted(&p, "Pick a preset above, or make a new one.").size(12.5));
        });
        return;
    };
    let kind = kind(app);
    let mut preset = app.model.gamepads.presets[index].clone();
    let mut remove = None;
    let mut capture = None;
    let lean = app.model.gamepads.mapper.lean;
    let mut set_level = false;
    widgets::card(ui, &p, |ui| {
        if preset.bindings.is_empty() {
            ui.label(RichText::new("No bindings yet").size(14.5));
            ui.label(
                widgets::muted(
                    &p,
                    "Add one below: a tap, a joystick, a pointer you steer with a stick, tilt and more.",
                )
                .size(12.5),
            );
        }
        let count = preset.bindings.len();
        for (i, binding) in preset.bindings.iter_mut().enumerate() {
            let expanded = app.controller.expanded == Some(i);
            let lit = app.model.gamepads.mapper.active(i);
            binding_row(
                ui,
                &p,
                binding,
                kind,
                expanded,
                lit,
                i,
                &mut app.controller.expanded,
                &mut remove,
                &mut capture,
            );
            if expanded {
                Frame::new().inner_margin(Margin { left: 34, right: 4, top: 2, bottom: 10 }).show(ui, |ui| {
                    binding_settings(ui, &p, binding, kind, i, &mut capture, lean, &mut set_level);
                });
            }
            if i + 1 < count {
                ui.add(egui::Separator::default().spacing(4.0));
            }
        }
    });
    ui.add_space(10.0);
    let add = widgets::chip_menu(ui, &p, "Add binding");
    let mut added = None;
    egui::Popup::menu(&add).show(|ui| {
        ui.set_min_width(230.0);
        for (icon, title, make) in templates() {
            if ui.button(widgets::icon_label(icon, title, None)).clicked() {
                added = Some(make());
            }
        }
    });
    if let Some(action) = added {
        preset.bindings.push(Binding { action, with: None });
        let i = preset.bindings.len() - 1;
        app.controller.expanded = Some(i);
        // Ask for the input straight away when the binding needs one.
        let slot = match &preset.bindings[i].action {
            Action::Tap { .. } | Action::Swipe { .. } | Action::Device { .. } => Some(Slot::Main),
            Action::Pointer { .. } => Some(Slot::Touch),
            _ => None,
        };
        capture = slot.map(|s| (i, s));
    }
    if let Some(i) = remove {
        preset.bindings.remove(i);
        app.controller.expanded = None;
    }
    if set_level {
        if let Some(lean) = lean {
            app.model.gamepads.mapper.level = lean;
        }
    }
    if preset != app.model.gamepads.presets[index] {
        app.model.gamepads.presets[index] = preset;
        app.model.gamepads.changed(app.model.session.as_mut());
    }
    if let Some((binding, slot)) = capture {
        let pads = app.model.gamepads.pads();
        let merged = lighttouch_model::gamepad::mapper::Merged::from(&pads.pads);
        let keys = ui.ctx().input(|i| i.keys_down.iter().map(|k| k.name().to_string()).collect());
        app.controller.capture = Some(Capture {
            binding,
            slot,
            held: (merged.buttons, keys),
            sticks: [merged.axes[0], merged.axes[1], merged.axes[2], merged.axes[3]],
        });
    }
}

fn action_icon(action: &Action) -> &'static str {
    match action {
        Action::Tap { .. } => HAND_TAP,
        Action::Swipe { .. } => SWIPE,
        Action::Joystick { .. } => JOYSTICK,
        Action::Pointer { .. } => CURSOR,
        Action::Flick { .. } => FLICK,
        Action::Tilt { .. } => TILT,
        Action::Motion { .. } => MOTION,
        Action::Device { .. } => HOUSE,
    }
}

fn templates() -> Vec<(&'static str, &'static str, fn() -> Action)> {
    vec![
        (HAND_TAP, "Tap a spot", || Action::Tap { input: unset(), at: Point::new(0.5, 0.75), mode: TapMode::Hold }),
        (SWIPE, "Swipe", || Action::Swipe {
            input: unset(),
            from: Point::new(0.5, 0.65),
            to: Point::new(0.5, 0.35),
            ms: 150,
        }),
        (JOYSTICK, "On-screen joystick", || Action::Joystick {
            stick: Stick::Left,
            center: Point::new(0.22, 0.78),
            radius: 0.15,
        }),
        (CURSOR, "Pointer (stick + touch)", || Action::Pointer {
            stick: Stick::Left,
            center: Point::CENTER,
            half: [0.5, 0.5],
            touch: unset(),
            style: PointerStyle::Centered,
            speed: 0.9,
        }),
        (FLICK, "Flick a stick to swipe", || Action::Flick { stick: Stick::Right, center: Point::CENTER, length: 0.3 }),
        (TILT, "Tilt the device with a stick", || Action::Tilt { stick: Stick::Left, strength: 1.0 }),
        (MOTION, "Tilt with controller motion", || Action::Motion { sensitivity: 1.0 }),
        (HOUSE, "Home or another device button", || Action::Device { input: unset(), button: DeviceButton::Home }),
    ]
}

fn unset() -> Input {
    Input::Key(String::new())
}

fn input_text(input: &Input, kind: PadKind) -> String {
    match input {
        Input::Key(k) if k.is_empty() => "Not set".into(),
        i => i.label(kind),
    }
}

fn summary(binding: &Binding, kind: PadKind) -> String {
    let with = binding.with.as_ref().map(|w| format!("Hold {} · ", w.label(kind))).unwrap_or_default();
    let s = match &binding.action {
        Action::Tap { mode, .. } => match mode {
            TapMode::Hold => "Touches while held".into(),
            TapMode::Toggle => "Press to touch, press again to lift".into(),
            TapMode::Repeat => "Taps over and over while held".into(),
        },
        Action::Swipe { ms, .. } => format!("Swipes in {ms} ms"),
        Action::Joystick { stick, .. } => format!("{} drags an on-screen stick", stick.label()),
        Action::Pointer { stick, touch, style, .. } => {
            let how = if *style == PointerStyle::Centered { "returns to centre" } else { "stays where it's left" };
            format!("{} points ({how}), {} touches", stick.label(), input_text(touch, kind))
        }
        Action::Flick { stick, .. } => format!("{} flicks swipe", stick.label()),
        Action::Tilt { stick, .. } => format!("{} leans the device", stick.label()),
        Action::Motion { .. } => "The controller's motion tilts the device".into(),
        Action::Device { button, .. } => format!("Presses {}", button.label()),
    };
    format!("{with}{s}")
}

#[allow(clippy::too_many_arguments)]
fn binding_row(
    ui: &mut Ui,
    p: &theme::Palette,
    binding: &mut Binding,
    kind: PadKind,
    expanded: bool,
    lit: bool,
    i: usize,
    open: &mut Option<usize>,
    remove: &mut Option<usize>,
    capture: &mut Option<(usize, Slot)>,
) {
    let id = ui.scope_id().with(("binding-row", i));
    let reserved = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(190.0);
    let row = ui.horizontal(|ui| {
        ui.set_min_height(52.0);
        let icon_color = if lit { p.primary } else { p.on_surface_variant };
        ui.label(widgets::icon_text(action_icon(&binding.action), 22.0).color(icon_color));
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - reserved - 16.0).max(140.0));
            ui.add_space(5.0);
            ui.label(RichText::new(binding.action.title()).size(14.5));
            ui.label(RichText::new(summary(binding, kind)).size(12.5).color(p.on_surface_variant));
        });
        let trailing = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button_sized(ui, p, icons::TRASH, "Remove", 32.0, true).clicked() {
                *remove = Some(i);
            }
            let caret = if expanded { icons::CARET_DOWN } else { icons::CARET_RIGHT };
            if widgets::icon_button_sized(ui, p, caret, if expanded { "Hide settings" } else { "Settings" }, 32.0, true)
                .clicked()
            {
                *open = if expanded { None } else { Some(i) };
            }
            match &binding.action {
                Action::Tap { input, .. } | Action::Swipe { input, .. } | Action::Device { input, .. } => {
                    if input_cap(ui, p, &input_text(input, kind)).clicked() {
                        *capture = Some((i, Slot::Main));
                    }
                }
                Action::Pointer { touch, .. } => {
                    if input_cap(ui, p, &input_text(touch, kind)).clicked() {
                        *capture = Some((i, Slot::Touch));
                    }
                }
                Action::Joystick { stick, .. } | Action::Flick { stick, .. } | Action::Tilt { stick, .. } => {
                    if input_cap(ui, p, &stick.label()).clicked() {
                        *capture = Some((i, Slot::Stick));
                    }
                }
                Action::Motion { .. } => {}
            }
        });
        trailing.response.rect.width()
    });
    let width = row.inner;
    if (width - reserved).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(id, width));
        ui.ctx().request_discard("a binding row's controls changed width");
    }
}

/// A clickable key cap: the input bound, and click to bind another.
fn input_cap(ui: &mut Ui, p: &theme::Palette, text: &str) -> egui::Response {
    let unset = text == "Not set";
    let color = if unset { theme::WARNING } else { p.on_surface };
    ui.add(
        egui::Button::new(RichText::new(text).font(FontId::new(13.0, theme::medium())).color(color))
            .fill(p.container_highest)
            .stroke(Stroke::new(
                1.0,
                if unset { theme::WARNING.gamma_multiply(0.6) } else { p.outline.gamma_multiply(0.6) },
            ))
            .corner_radius(CornerRadius::same(8))
            .min_size(vec2(44.0, 30.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .on_hover_text("Click, then press the input to use")
}

#[allow(clippy::too_many_arguments)]
fn binding_settings(
    ui: &mut Ui,
    p: &theme::Palette,
    binding: &mut Binding,
    kind: PadKind,
    i: usize,
    capture: &mut Option<(usize, Slot)>,
    lean: Option<(f64, f64)>,
    set_level: &mut bool,
) {
    let row = |ui: &mut Ui, title: &str, add: &mut dyn FnMut(&mut Ui)| {
        ui.horizontal(|ui| {
            ui.set_min_height(36.0);
            ui.allocate_ui_with_layout(vec2(136.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_width(136.0);
                ui.label(widgets::muted(p, title).size(13.0));
            });
            add(ui);
        });
    };
    match &mut binding.action {
        Action::Tap { mode, .. } => {
            row(ui, "Acts", &mut |ui| {
                widgets::segmented(
                    ui,
                    p,
                    mode,
                    &[(TapMode::Hold, "While held"), (TapMode::Toggle, "Toggle"), (TapMode::Repeat, "Repeat")],
                );
            });
        }
        Action::Swipe { ms, .. } => {
            row(ui, "Duration", &mut |ui| {
                ui.add(egui::Slider::new(ms, 40..=800).suffix(" ms"));
            });
        }
        Action::Joystick { stick, radius, .. } => {
            stick_row(ui, p, row, stick);
            row(ui, "Reach", &mut |ui| {
                ui.add(egui::Slider::new(radius, 0.04..=0.5).show_value(false));
            });
        }
        Action::Flick { stick, length, .. } => {
            stick_row(ui, p, row, stick);
            row(ui, "Swipe length", &mut |ui| {
                ui.add(egui::Slider::new(length, 0.05..=0.8).show_value(false));
            });
        }
        Action::Pointer { stick, style, speed, half, center, touch } => {
            stick_row(ui, p, row, stick);
            row(ui, "Touch with", &mut |ui| {
                if input_cap(ui, p, &input_text(touch, kind)).clicked() {
                    *capture = Some((i, Slot::Touch));
                }
            });
            row(ui, "Pointer", &mut |ui| {
                widgets::segmented(
                    ui,
                    p,
                    style,
                    &[(PointerStyle::Centered, "Returns to centre"), (PointerStyle::Cursor, "Stays put")],
                );
            });
            if *style == PointerStyle::Cursor {
                row(ui, "Speed", &mut |ui| {
                    ui.add(egui::Slider::new(speed, 0.2..=3.0).show_value(false));
                });
            }
            row(ui, "Area", &mut |ui| {
                if widgets::text_button(ui, p, "Whole screen").clicked() {
                    *half = [0.5, 0.5];
                    *center = Point::CENTER;
                }
                ui.label(widgets::muted(p, "or drag its corner on the screen").size(12.0));
            });
        }
        Action::Tilt { stick, strength } => {
            stick_row(ui, p, row, stick);
            row(ui, "Strength", &mut |ui| {
                ui.add(egui::Slider::new(strength, 0.2..=1.0).show_value(false));
            });
        }
        Action::Motion { sensitivity } => {
            row(ui, "Sensitivity", &mut |ui| {
                ui.add(egui::Slider::new(sensitivity, 0.3..=3.0).show_value(false));
            });
            row(ui, "Level", &mut |ui| {
                let can = lean.is_some();
                if widgets::tonal_enabled(ui, p, "Set level", can)
                    .on_hover_text("Hold the controller the way you play, then click: that's flat for the device")
                    .clicked()
                {
                    *set_level = true;
                }
                if !can {
                    ui.label(widgets::muted(p, "Needs a controller with motion sensors").size(12.0));
                }
            });
        }
        Action::Device { button, .. } => {
            row(ui, "Button", &mut |ui| {
                let options: Vec<(DeviceButton, &str)> = DeviceButton::ALL.iter().map(|b| (*b, b.label())).collect();
                widgets::segmented(ui, p, button, &options);
            });
        }
    }
    row(ui, "Only while holding", &mut |ui| {
        let text = binding.with.as_ref().map_or("Nothing".to_string(), |w| w.label(kind));
        if input_cap(ui, p, &text).clicked() {
            *capture = Some((i, Slot::With));
        }
        if binding.with.is_some() && widgets::text_button(ui, p, "Clear").clicked() {
            binding.with = None;
        }
    });
}

fn stick_row(ui: &mut Ui, p: &theme::Palette, row: impl Fn(&mut Ui, &str, &mut dyn FnMut(&mut Ui)), stick: &mut Stick) {
    row(ui, "Stick", &mut |ui| {
        let r = widgets::chip_menu(ui, p, &stick.label());
        egui::Popup::menu(&r).show(|ui| {
            for (s, label) in [
                (Stick::Left, "Left stick"),
                (Stick::Right, "Right stick"),
                (Stick::Dpad, "D-pad"),
                (Stick::wasd(), "W A S D"),
                (Stick::arrows(), "Arrow keys"),
            ] {
                if ui.selectable_label(*stick == s, label).clicked() {
                    *stick = s;
                }
            }
        });
    });
}

/// "Press a button or key": the next input pressed binds (Escape cancels).
fn capture_prompt(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(capture) = app.controller.capture.clone() else { return };
    let pads = app.model.gamepads.pads();
    let merged = lighttouch_model::gamepad::mapper::Merged::from(&pads.pads);
    let keys: HashSet<String> = ui.ctx().input(|i| i.keys_down.iter().map(|k| k.name().to_string()).collect());
    let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
    // A new press: a button not held when the prompt opened, or a key.
    let new_button =
        PadButton::ALL.iter().copied().find(|b| merged.buttons & b.bit() != 0 && capture.held.0 & b.bit() == 0);
    let new_key = keys.iter().find(|k| !capture.held.1.contains(*k) && k.as_str() != "Escape").cloned();
    let mut got: Option<Input> = None;
    let mut stick: Option<Stick> = None;
    if capture.slot == Slot::Stick {
        let moved = |x: f32, y: f32| (x * x + y * y).sqrt() > 0.6;
        if moved(merged.axes[0], merged.axes[1]) && !moved(capture.sticks[0], capture.sticks[1]) {
            stick = Some(Stick::Left);
        } else if moved(merged.axes[2], merged.axes[3]) && !moved(capture.sticks[2], capture.sticks[3]) {
            stick = Some(Stick::Right);
        } else if matches!(
            new_button,
            Some(PadButton::DpadUp | PadButton::DpadDown | PadButton::DpadLeft | PadButton::DpadRight)
        ) {
            stick = Some(Stick::Dpad);
        } else if let Some(k) = &new_key {
            if ["W", "A", "S", "D"].contains(&k.as_str()) {
                stick = Some(Stick::wasd());
            } else if k.starts_with("Arrow") {
                stick = Some(Stick::arrows());
            }
        }
    } else {
        got = new_button.map(Input::Pad).or(new_key.map(Input::Key));
    }

    let modal =
        egui::Modal::new(egui::Id::unique("controller-capture")).backdrop_color(Color32::from_black_alpha(140)).frame(
            Frame::new()
                .fill(p.container_high)
                .corner_radius(CornerRadius::same(theme::radius::DOCK))
                .inner_margin(Margin::same(24)),
        );
    let mut cancelled = false;
    modal.show(ui.ctx(), |ui| {
        ui.set_width(340.0);
        let icon = if capture.slot == Slot::Stick { JOYSTICK } else { GAME_CONTROLLER };
        ui.label(widgets::icon_text(icon, 26.0).color(p.primary));
        ui.add_space(6.0);
        let (title, detail) = if capture.slot == Slot::Stick {
            ("Move a stick", "Or press the D-pad, W A S D, or an arrow key")
        } else {
            ("Press a button or key", "On the controller or the keyboard")
        };
        ui.label(RichText::new(title).font(theme::title()));
        ui.add_space(2.0);
        ui.label(widgets::muted(&p, detail));
        ui.add_space(16.0);
        widgets::dialog_buttons(ui, |ui| {
            if widgets::tonal(ui, &p, "Cancel").clicked() {
                cancelled = true;
            }
        });
    });
    if cancelled {
        app.controller.capture = None;
        return;
    }
    if cancel {
        app.controller.capture = None;
        return;
    }
    if got.is_none() && stick.is_none() {
        ui.ctx().request_repaint_after(Duration::from_millis(16));
        return;
    }
    app.controller.capture = None;
    let Some(index) = app.model.gamepads.active else { return };
    let mut preset = app.model.gamepads.presets[index].clone();
    let Some(binding) = preset.bindings.get_mut(capture.binding) else { return };
    match (capture.slot, &mut binding.action) {
        (Slot::With, _) => binding.with = got,
        (
            Slot::Stick,
            Action::Joystick { stick: s, .. }
            | Action::Flick { stick: s, .. }
            | Action::Tilt { stick: s, .. }
            | Action::Pointer { stick: s, .. },
        ) => {
            if let Some(new) = stick {
                *s = new;
            }
        }
        (Slot::Touch, Action::Pointer { touch, .. }) => {
            if let Some(g) = got {
                *touch = g;
            }
        }
        (Slot::Main, Action::Tap { input, .. } | Action::Swipe { input, .. } | Action::Device { input, .. }) => {
            if let Some(g) = got {
                *input = g;
            }
        }
        _ => {}
    }
    app.model.gamepads.presets[index] = preset;
    app.model.gamepads.changed(app.model.session.as_mut());
}

/// The dock's Controller button: opens the window (and puts bindings on, with the first preset, if they were off).
pub fn dock_button(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let on = app.model.gamepads.active.is_some();
    let r = widgets::dock_button(ui, &p, GAME_CONTROLLER, "Controller", true, on.then_some(p.primary));
    if r.clicked() {
        app.controller.open = !app.controller.open;
        if app.controller.open {
            app.model.gamepads.start();
            if app.model.gamepads.active.is_none() && !app.model.gamepads.presets.is_empty() {
                app.model.gamepads.set_active(Some(0), app.model.session.as_mut());
                remember_active(app);
            }
        }
    }
    let _ = ARROWS_OUT;
}

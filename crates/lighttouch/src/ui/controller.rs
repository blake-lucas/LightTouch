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
    Action, Binding, DeviceButton, Input, PadButton, Point, PointerStyle, Preset, Press, Stick, TAP_LIFT_MS, TapMode,
};
use lighttouch_model::gamepad::sdl::{Backend, Pad, PadKind, Sensors};
use lighttouch_model::tilt::Rest;
use std::collections::HashSet;
use std::time::{Duration, Instant};

pub const GAME_CONTROLLER: &str = "\u{E26E}";
const HAND_TAP: &str = "\u{EC90}";
const SWIPE: &str = "\u{EC92}";
const JOYSTICK: &str = "\u{EA5E}";
const CROSSHAIR: &str = "\u{E1D6}";
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
    /// Frames drawn since the window opened, and whether its title bar has been given the theme since it was shown.
    frames: u32,
    title_themed: bool,
    /// Linux: the motion-access explanation is open; the rule's install under way, and how it went.
    access_help: bool,
    access_job: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    access_result: Option<Result<(), String>>,
    /// Mapping four keys to a stick binding, one direction at a time.
    key_map: Option<KeyMap>,
}

impl ControllerUi {
    /// Waiting for a key or button to bind, so keys mean nothing else.
    pub fn listening(&self) -> bool {
        self.capture.is_some() || self.key_map.is_some()
    }
}

/// The keys mapped so far (up, right, down, left: the order they're asked for), for which binding, and the last
/// key turned away (already used for another direction).
struct KeyMap {
    binding: usize,
    keys: Vec<String>,
    refused: Option<String>,
}

/// The directions keys are asked for, in that order.
const KEY_DIRECTIONS: [&str; 4] = ["Up", "Right", "Down", "Left"];

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
    /// A stick binding's press button.
    Press,
    /// A joystick's or look binding's tap button.
    Tap,
    /// Four keys for a stick, one direction at a time.
    Keys,
    /// A mouse-look binding's button that frees the mouse while held.
    Free,
    /// A tilt binding's Set level button.
    Level,
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
            Action::Joystick { stick, press, tap, .. } => {
                for i in press.input().into_iter().chain(tap.as_ref()) {
                    add(i, &mut keys);
                }
                stick_keys(stick, &mut keys);
            }
            Action::Look { stick, press, tap, free, .. } => {
                for i in press.input().into_iter().chain(tap.as_ref()).chain(free.as_ref()) {
                    add(i, &mut keys);
                }
                stick_keys(stick, &mut keys);
            }
            Action::Tilt { stick, set_level, .. } => {
                if let Some(i) = set_level {
                    add(i, &mut keys);
                }
                stick_keys(stick, &mut keys);
            }
            Action::Flick { stick, .. } => stick_keys(stick, &mut keys),
            Action::Motion { .. } => {}
        }
    }
    keys
}

/// The keys held, by name, with Ctrl, Shift and Alt as keys too (egui keeps those apart, as modifiers).
pub fn held_keys(i: &egui::InputState) -> HashSet<String> {
    use lighttouch_model::gamepad::preset::{KEY_ALT, KEY_CTRL, KEY_SHIFT};
    let mut keys: HashSet<String> = i.keys_down.iter().map(|k| k.name().to_string()).collect();
    for (on, name) in [(i.modifiers.ctrl, KEY_CTRL), (i.modifiers.shift, KEY_SHIFT), (i.modifiers.alt, KEY_ALT)] {
        if on {
            keys.insert(name.to_string());
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
    let mut keys: HashSet<String> = if ctx.egui_wants_keyboard_input() || bound.is_empty() {
        HashSet::new()
    } else {
        ctx.input(|i| held_keys(i).into_iter().filter(|k| bound.contains(k)).collect())
    };
    // The mouse, while it's locked to the game: its buttons bind like keys.
    let (mouse, buttons) = app.guest.take_mouse();
    keys.extend(buttons.into_iter().filter(|b| bound.contains(*b)).map(String::from));
    let focused = ctx.input(|i| i.focused) || app.controller.window_focused;
    let listening = focused || app.model.settings.controller_background;
    let model = &mut app.model;
    if model.gamepads.step(&keys, mouse, view, listening, model.session.as_mut()) {
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
        // Hidden while playing; still shown (to be placed) in Edit layout.
        if binding.hidden && !editing {
            continue;
        }
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
            Action::Joystick { stick, center, radius, .. } => {
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
                }
                // Always drawn while down: a finger toggled down shouldn't be a secret.
                if let Some(f) = finger {
                    painter.circle_filled(px(f), r * 0.55, theme::ACCENT.gamma_multiply(0.8));
                }
                handles.extend([(Handle::Main, c), (Handle::Size, c + vec2(ring, 0.0))]);
            }
            Action::Look { stick, center, half, .. } => {
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
                    spot(&painter, c, r * 0.8, &stick.short(), lit, editing);
                    // Crosshair ticks round it: a look area, not a joystick.
                    let tick = Stroke::new(1.5, if lit { theme::ACCENT } else { Color32::from_white_alpha(200) });
                    for d in [vec2(1.0, 0.0), vec2(-1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, -1.0)] {
                        painter.line_segment([c + d * r * 1.0, c + d * r * 1.45], tick);
                    }
                }
                if let Some(f) = finger {
                    painter.circle_filled(px(f), r * 0.55, theme::ACCENT.gamma_multiply(0.8));
                }
                handles.extend([(Handle::Main, c), (Handle::Size, corner)]);
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
            Action::Joystick { center, .. }
            | Action::Flick { center, .. }
            | Action::Pointer { center, .. }
            | Action::Look { center, .. },
            Handle::Main,
        ) => *center = to,
        (Action::Joystick { center, radius, .. }, Handle::Size) => {
            *radius = (here.distance(shown_px(*center)) / short).clamp(0.04, 0.5)
        }
        (Action::Flick { center, length, .. }, Handle::Size) => {
            *length = (here.distance(shown_px(*center)) / short).clamp(0.05, 0.8)
        }
        (Action::Pointer { center, half, .. } | Action::Look { center, half, .. }, Handle::Size) => {
            let c = shown_px(*center);
            *half = [
                ((here.x - c.x).abs() / screen.width()).clamp(0.03, 1.0),
                ((here.y - c.y).abs() / screen.height()).clamp(0.03, 1.0),
            ];
        }
        _ => {}
    }
}

/// While the layout is being edited: Done, at the top of the screen.
fn edit_banner(app: &mut App, ui: &mut Ui, screen: Rect) {
    let p = app.palette;
    // At the top of the part that shows: an iPad at its real size may be cropped.
    let screen = screen.intersect(ui.clip_rect());
    let area = Rect::from_center_size(pos2(screen.center().x, screen.top() + 28.0), vec2(screen.width(), 32.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(egui::Align::Center)), |ui| {
        if pill_button(ui, &p, "Done", true).clicked() {
            app.model.gamepads.editing = false;
        }
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
        app.controller.frames = 0;
        app.controller.title_themed = false;
        return;
    }
    app.model.gamepads.start();
    let builder = egui::ViewportBuilder::default()
        .with_title("Controls")
        .with_app_id("org.lighttouch.LightTouch")
        .with_inner_size([1240.0, 780.0])
        .with_min_inner_size([900.0, 600.0]);
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("controller-window"), builder, |ui, _class| {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            app.controller.open = false;
            app.controller.capture = None;
            app.model.gamepads.editing = false;
        }
        app.controller.window_focused = ctx.input(|i| i.focused);
        let p = app.palette;
        // egui gives a new window the theme on its first frame, before Windows shows it, and Windows then keeps the
        // system's title bar (dark buttons on a dark bar in light mode) until the window is deactivated and activated
        // again. Once it's up and focused, give it the theme again: that also repaints the title bar.
        app.controller.frames = app.controller.frames.saturating_add(1);
        if !app.controller.title_themed && app.controller.frames > 2 && app.controller.window_focused {
            app.controller.title_themed = true;
            let theme = if p.dark { egui::SystemTheme::Dark } else { egui::SystemTheme::Light };
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(theme));
        }
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

/// The controllers and what they're doing on the left, the bindings in the middle, and the preset's options on the
/// right; a narrower window moves the options under the live view. Each column scrolls on its own.
fn body(app: &mut App, ui: &mut Ui) {
    header(app, ui);
    let gap = 20.0;
    let side = 300.0;
    let rest = ui.available_rect_before_wrap().shrink2(vec2(24.0, 0.0));
    let wide = rest.width() >= side * 2.0 + 20.0 + gap * 2.0 + 500.0;
    let left = Rect::from_min_size(rest.min, vec2(side + 20.0, rest.height()));
    // The options column is the preset's: without one the bindings take its room.
    let right = (wide && app.model.gamepads.active.is_some())
        .then(|| Rect::from_min_max(pos2(rest.right() - side, rest.top()), rest.max));
    let middle = Rect::from_min_max(
        pos2(left.right() + gap, rest.top()),
        pos2(right.map_or(rest.right(), |r| r.left() - gap), rest.bottom()),
    );
    let column = |ui: &mut Ui, salt: &str, rect: Rect, app: &mut App, add: &dyn Fn(&mut App, &mut Ui)| {
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            // The scroll bar takes room of its own when it shows, beside the column's cards and buttons, not over
            // them.
            ui.spacing_mut().scroll.floating = false;
            egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(app, ui);
                ui.add_space(20.0);
            });
        });
    };
    column(ui, "ctl-left", left, app, &|app, ui| {
        controllers_card(app, ui);
        live_card(app, ui);
        if !wide {
            options_card(app, ui);
        }
    });
    column(ui, "ctl-middle", middle, app, &|app, ui| bindings(app, ui));
    if let Some(right) = right {
        column(ui, "ctl-right", right, app, &|app, ui| options_card(app, ui));
    }
    if app.controller.capture.is_some() {
        capture_prompt(app, ui);
    }
    if app.controller.key_map.is_some() {
        key_map_prompt(app, ui);
    }
    #[cfg(target_os = "linux")]
    if app.controller.access_help {
        access_help(app, ui);
    }
}

fn header(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    Frame::new().inner_margin(Margin { left: 24, right: 20, top: 16, bottom: 6 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            ui.label(widgets::icon_text(GAME_CONTROLLER, 26.0).color(p.primary));
            ui.add_space(8.0);
            if let Some(name) = app.controller.rename.as_mut() {
                let edit = ui.add(
                    egui::TextEdit::singleline(name)
                        .desired_width(PICKER_WIDTH - 16.0)
                        .font(FontId::new(14.0, theme::medium())),
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
            ui.add_space(4.0);
            let has = app.model.gamepads.active.is_some();
            if widgets::icon_button_sized(ui, &p, icons::PLUS, "New preset", 34.0, true).clicked() {
                new_preset(app);
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
        });
    });
}

fn new_preset(app: &mut App) {
    app.model.gamepads.add(None, app.model.session.as_mut());
    app.controller.expanded = None;
    remember_active(app);
}

const PICKER_WIDTH: f32 = 300.0;

/// The preset picker: a wide field with the preset's name and a caret, opening the list.
fn picker(ui: &mut Ui, p: &theme::Palette, text: &str, placeholder: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(PICKER_WIDTH, 36.0), Sense::click());
    let painter = ui.painter();
    let fill = if r.hovered() { p.container_high } else { p.container };
    painter.rect(rect, CornerRadius::same(10), fill, Stroke::new(1.0, p.outline), egui::StrokeKind::Inside);
    let color = if placeholder { p.on_surface_variant } else { p.on_surface };
    let caret = painter.text(
        rect.right_center() - vec2(12.0, 0.0),
        Align2::RIGHT_CENTER,
        icons::CARET_DOWN,
        FontId::new(14.0, theme::icons()),
        p.on_surface_variant,
    );
    let room = rect.width() - 14.0 - caret.width() - 22.0;
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), FontId::new(14.5, theme::medium()), color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
    let galley = painter.layout_job(job);
    let at = pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(at, galley, color);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn remember_active(app: &mut App) {
    app.model.settings.controller_preset = app.model.gamepads.active_name();
    app.model.save_settings();
}

fn preset_menu(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let none = app.model.gamepads.presets.is_empty();
    let name = app.model.gamepads.active_name();
    let label = name.clone().unwrap_or_else(|| if none { "No presets yet".into() } else { "Off".into() });
    let r = picker(ui, &p, &label, name.is_none()).on_hover_text("The preset in use");
    egui::Popup::menu(&r).width(PICKER_WIDTH).show(|ui| {
        ui.set_min_width(PICKER_WIDTH - 12.0);
        let mut pick = None;
        if ui.selectable_label(app.model.gamepads.active.is_none(), "Off").clicked() {
            pick = Some(None);
        }
        if !none {
            ui.separator();
        }
        for (i, preset) in app.model.gamepads.presets.iter().enumerate() {
            if ui.selectable_label(app.model.gamepads.active == Some(i), &preset.name).clicked() {
                pick = Some(Some(i));
            }
        }
        ui.separator();
        if ui.button(widgets::icon_label(icons::PLUS, "New preset", None)).clicked() {
            new_preset(app);
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
                        let motion = match pad.sensors {
                            Sensors::Live => "Motion sensors",
                            Sensors::Quiet => "Motion sensors aren't reporting; trying again",
                            Sensors::None if pad.motion_note.is_some() => "Motion sensors not reachable",
                            Sensors::None => "No motion sensors",
                        };
                        let color = if pad.sensors == Sensors::Quiet { theme::WARNING } else { p.on_surface_variant };
                        ui.label(RichText::new(motion).size(12.0).color(color));
                    });
                });
            }
        }
    });
}

/// Linux: a button that installs LightTouch's udev rule for controllers' motion sensors (with the password prompt),
/// a ? that explains it, and how it went.
#[cfg(target_os = "linux")]
fn access_row(c: &mut ControllerUi, p: &theme::Palette, ui: &mut Ui) {
    use lighttouch_model::gamepad::linux_access;
    if let Some(rx) = &c.access_job {
        match rx.try_recv() {
            Ok(result) => {
                c.access_result = Some(result);
                c.access_job = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => ui.ctx().request_repaint_after(Duration::from_millis(200)),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => c.access_job = None,
        }
    }
    let running = c.access_job.is_some();
    let done = matches!(c.access_result, Some(Ok(())));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if !done {
            let label = if running { "Waiting for your password…" } else { "Allow access" };
            if widgets::tonal_enabled(ui, p, label, !running)
                .on_hover_text("Adds one udev rule so LightTouch can read the motion sensors; asks for your password")
                .clicked()
            {
                let (tx, rx) = std::sync::mpsc::channel();
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let _ = tx.send(linux_access::install());
                    ctx.request_repaint();
                });
                c.access_job = Some(rx);
                c.access_result = None;
            }
        }
        if widgets::icon_button_sized(ui, p, icons::QUESTION, "Why this needs your password", 30.0, true).clicked() {
            c.access_help = true;
        }
    });
    match &c.access_result {
        Some(Ok(())) => {
            ui.label(
                widgets::muted(
                    p,
                    "Done. Turn the controller off and on again, or reconnect it, for motion to come through",
                )
                .size(12.0),
            );
        }
        Some(Err(e)) => {
            ui.add(egui::Label::new(RichText::new(e).size(12.0).color(theme::warning(&p))).wrap());
        }
        None => {}
    }
}

/// Linux: why motion needs the person's password, what is changed, and the commands to do it by hand or undo it.
#[cfg(target_os = "linux")]
fn access_help(app: &mut App, ui: &mut Ui) {
    use lighttouch_model::gamepad::linux_access;
    let p = app.palette;
    let modal = egui::Modal::new(egui::Id::unique("controller-motion-access"))
        .backdrop_color(Color32::from_black_alpha(140))
        .frame(
            Frame::new()
                .fill(p.container_high)
                .corner_radius(CornerRadius::same(theme::radius::DOCK))
                .inner_margin(Margin::same(24)),
        );
    let mut close = false;
    let response = modal.show(ui.ctx(), |ui| {
        ui.set_width(560.0);
        ui.label(widgets::icon_text(MOTION, 26.0).color(p.primary));
        ui.add_space(6.0);
        ui.label(RichText::new("Motion sensors on Linux").font(theme::title()));
        ui.add_space(8.0);
        let para = |ui: &mut Ui, text: &str| {
            ui.add(egui::Label::new(RichText::new(text).size(13.0)).wrap());
            ui.add_space(6.0);
        };
        para(
            ui,
            "LightTouch reads controllers through SDL, which gets a Switch or PlayStation controller's motion sensors \
             from device files that Linux lets only the administrator open unless a rule allows more. Steam installs \
             such a rule; this computer has none that covers this controller.",
        );
        para(
            ui,
            &format!(
                "Allow access adds one file, {}, which lets whoever is signed in at this computer open Nintendo and \
                 Sony controllers, as Steam's rule does. No other file is changed. Writing it needs your password, \
                 which your system asks for. /etc is kept across updates, including on Fedora Silverblue, Bluefin, \
                 Bazzite and other atomic systems. Delete the file to undo it.",
                linux_access::RULES_PATH
            ),
        );
        let code = |ui: &mut Ui, title: &str, text: String| {
            ui.horizontal(|ui| {
                ui.label(widgets::muted(&p, title).size(12.5));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::text_button(ui, &p, "Copy").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                });
            });
            // Unwrapped, as it would be typed: a long line scrolls sideways.
            Frame::new()
                .fill(p.container_highest)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    egui::ScrollArea::horizontal().id_salt(title).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(text.trim_end()).monospace().size(12.0)).extend());
                    });
                });
            ui.add_space(8.0);
        };
        egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
            code(ui, "To do it yourself, in a terminal:", linux_access::install_commands());
            code(ui, "To undo it:", linux_access::remove_commands());
        });
        para(ui, "Then turn the controller off and on again, or reconnect it.");
        widgets::dialog_buttons(ui, |ui| {
            if widgets::tonal(ui, &p, "Close").clicked() {
                close = true;
            }
        });
    });
    if close || response.should_close() {
        app.controller.access_help = false;
    }
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
            #[cfg(target_os = "linux")]
            if pad.motion_fix || app.controller.access_result.is_some() || app.controller.access_job.is_some() {
                access_row(&mut app.controller, &p, ui);
            }
        }
        let held: Vec<String> = {
            let bound = bound_keys(&app.model);
            let mut v: Vec<String> =
                ui.ctx().input(|i| held_keys(i).into_iter().filter(|k| bound.contains(k)).collect());
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

/// How the controller is held and turning: its lean from level (lying flat, or held flat in the hands), which is
/// what a motion binding gives the device, and the gyroscope's turn about each axis.
fn motion_bars(ui: &mut Ui, p: &theme::Palette, pad: &Pad) {
    let (Some(accel), Some(gyro)) = (pad.accel, pad.gyro.or(Some([0.0; 3]))) else {
        let text = match (pad.sensors, &pad.motion_note) {
            (Sensors::None, Some(note)) => note.as_str(),
            (Sensors::None, None) => "This controller has no motion sensors",
            _ => "Waiting for the motion sensors",
        };
        ui.add(egui::Label::new(widgets::muted(p, text).size(12.0)).wrap());
        return;
    };
    let (roll, pitch) = lighttouch_model::gamepad::mapper::lean_of(accel);
    let bar = |ui: &mut Ui, label: &str, value: f32, scale: f32, width: f32| {
        let (rect, _) = ui.allocate_exact_size(vec2(width, 16.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), p.container_highest);
        painter.line_segment(
            [pos2(rect.center().x, rect.top() + 3.0), pos2(rect.center().x, rect.bottom() - 3.0)],
            Stroke::new(1.0, p.outline_variant),
        );
        let half = rect.width() / 2.0;
        let len = (value / scale).clamp(-1.0, 1.0) * half;
        let fill = Rect::from_min_max(
            pos2(rect.center().x + len.min(0.0), rect.top()),
            pos2(rect.center().x + len.max(0.0), rect.bottom()),
        );
        painter.rect_filled(fill, CornerRadius::same(4), p.primary);
        painter.text(
            rect.left_center() + vec2(5.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            FontId::new(9.5, theme::medium()),
            p.on_surface,
        );
    };
    // Rows of a fixed label column and bars sharing the rest: the bars' width comes from the card, not a grid cell.
    let label_w = 40.0;
    let width = (ui.available_width() - label_w - 10.0).max(60.0);
    let row = |ui: &mut Ui, name: &str, hint: &str, bars: &[(String, f32, f32)]| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (rect, _) = ui.allocate_exact_size(vec2(label_w, 16.0), Sense::hover());
            ui.painter().text(
                rect.left_center(),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(12.0),
                p.on_surface_variant,
            );
            ui.interact(rect, ui.scope_id().with(name), Sense::hover()).on_hover_text(hint);
            ui.add_space(4.0);
            let w = (width - 6.0 * (bars.len() as f32 - 1.0)) / bars.len() as f32;
            for (label, value, scale) in bars {
                bar(ui, label, *value, *scale, w);
            }
        });
    };
    row(
        ui,
        "Lean",
        "How far the controller leans from level: left or right, and its far edge up or down. Level reads in the \
         middle.",
        &[
            (format!("side {:+.0}°", roll.to_degrees()), roll.to_degrees() as f32, 90.0),
            (format!("tip {:+.0}°", pitch.to_degrees()), pitch.to_degrees() as f32, 90.0),
        ],
    );
    ui.add_space(6.0);
    row(
        ui,
        "Turn",
        "The gyroscope: how fast it turns about each axis",
        &[("x".into(), gyro[0], 6.0), ("y".into(), gyro[1], 6.0), ("z".into(), gyro[2], 6.0)],
    );
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
        // The options column is narrow: the slider goes under its title.
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(widgets::icon_text(JOYSTICK, 20.0).color(p.on_surface_variant));
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.add_space(6.0);
                ui.label(RichText::new("Stick dead zone").size(14.5));
                number_slider(ui, 170.0, percent(egui::Slider::new(&mut preset.deadzone, 0.0..=0.5)))
                    .on_hover_text("How far a stick must be pushed before it counts");
                ui.add_space(6.0);
            });
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
    // The button goes under the text: a game's name is long for this column.
    widgets::setting_row(ui, &p, icons::PHONE, "Games", &subtitle, |_| {});
    let indent = 2.0 + 20.0 + 6.0 + ui.spacing().item_spacing.x;
    let button = |ui: &mut Ui, label: &str| {
        ui.horizontal(|ui| {
            ui.add_space(indent);
            widgets::tonal(ui, &p, label).clicked()
        })
        .inner
    };
    if let Some((bundle, name)) = front {
        let linked = preset.apps.contains(&bundle);
        let label = if linked { format!("Unlink {name}") } else { format!("Use for {name}") };
        if button(ui, &label) {
            if linked {
                preset.apps.retain(|a| a != &bundle);
            } else {
                preset.apps.push(bundle);
            }
        }
    } else if !preset.apps.is_empty() && button(ui, "Clear") {
        preset.apps.clear();
    }
}

// ------------------------------------------------------------------------------------------------- bindings ----

fn bindings(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let added = bindings_header(app, ui);
    let Some(index) = app.model.gamepads.active else {
        let none = app.model.gamepads.presets.is_empty();
        widgets::card(ui, &p, |ui| {
            if none {
                ui.label(RichText::new("No presets yet").size(14.5));
                ui.label(
                    widgets::muted(
                        &p,
                        "A preset says what each button, stick and key does on the device: tap a spot, steer a \
                         pointer, tilt and more. Make one for each game.",
                    )
                    .size(12.5),
                );
                ui.add_space(8.0);
                if widgets::tonal(ui, &p, "New preset").clicked() {
                    new_preset(app);
                }
            } else {
                ui.label(RichText::new("Bindings are off").size(14.5));
                ui.label(widgets::muted(&p, "Pick a preset above to use it, or make a new one.").size(12.5));
            }
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
                    "Use Add binding: a tap, a joystick, a pointer you steer with a stick, tilt and more.",
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
    if let Some(action) = added {
        preset.bindings.push(Binding::new(action));
        let i = preset.bindings.len() - 1;
        app.controller.expanded = Some(i);
        // Ask for the input straight away when the binding needs one.
        let slot = match &preset.bindings[i].action {
            Action::Tap { .. } | Action::Swipe { .. } | Action::Device { .. } => Some(Slot::Main),
            Action::Pointer { .. } => Some(Slot::Touch),
            Action::Joystick { press, .. } | Action::Look { press, .. }
                if press.input().is_some_and(|i| matches!(i, Input::Key(k) if k.is_empty())) =>
            {
                Some(Slot::Press)
            }
            _ => None,
        };
        capture = slot.map(|s| (i, s));
    }
    if let Some(i) = remove {
        // Each More options fold stays with its binding, not its place in the list.
        let open: Vec<bool> =
            (0..preset.bindings.len()).map(|j| ui.data(|d| d.get_temp::<bool>(more_id(j))).unwrap_or(false)).collect();
        preset.bindings.remove(i);
        for (j, open) in open.iter().enumerate().filter(|&(j, _)| j != i) {
            let to = if j > i { j - 1 } else { j };
            ui.data_mut(|d| d.insert_temp(more_id(to), *open));
        }
        ui.data_mut(|d| d.remove_temp::<bool>(more_id(open.len() - 1)));
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
    if let Some((binding, Slot::Keys)) = capture {
        app.controller.key_map = Some(KeyMap { binding, keys: Vec::new(), refused: None });
    } else if let Some((binding, slot)) = capture {
        let pads = app.model.gamepads.pads();
        let merged = lighttouch_model::gamepad::mapper::Merged::from(&pads.pads);
        let keys = ui.ctx().input(held_keys);
        app.controller.capture = Some(Capture {
            binding,
            slot,
            held: (merged.buttons, keys),
            sticks: [merged.axes[0], merged.axes[1], merged.axes[2], merged.axes[3]],
        });
    }
}

/// "Bindings", with editing the layout (moving the spots on the screen) and adding a binding beside it. Returns the binding picked to add.
fn bindings_header(app: &mut App, ui: &mut Ui) -> Option<Action> {
    let p = app.palette;
    let has = app.model.gamepads.active.is_some();
    let mut added = None;
    // Lined up with the other columns' section titles, which have no buttons beside them.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.set_min_height(30.0);
        ui.label(RichText::new("Bindings").font(theme::label()).color(p.primary));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !has {
                return;
            }
            let add = widgets::chip_menu(ui, &p, "Add binding");
            egui::Popup::menu(&add).show(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for make in templates() {
                    let action = make();
                    if menu_item(ui, &p, action_icon(&action), action.title(), about(&action)).clicked() {
                        added = Some(action);
                    }
                }
            });
            let editing = app.model.gamepads.editing;
            let running = app.model.session.as_ref().is_some_and(|s| s.phase.active());
            let label = if editing { "Done" } else { "Edit layout" };
            let r = ui
                .add_enabled_ui(running || editing, |ui| pill_button(ui, &p, label, editing))
                .inner
                .on_hover_text("Move each binding's spot on the device's screen")
                .on_disabled_hover_text("Start the device to edit the layout");
            if r.clicked() {
                app.model.gamepads.editing = !editing;
                app.model.gamepads.release(app.model.session.as_mut());
            }
        });
    });
    added
}

fn action_icon(action: &Action) -> &'static str {
    match action {
        Action::Tap { .. } => HAND_TAP,
        Action::Swipe { .. } => SWIPE,
        Action::Joystick { .. } => JOYSTICK,
        Action::Look { .. } => CROSSHAIR,
        Action::Pointer { .. } => CURSOR,
        Action::Flick { .. } => FLICK,
        Action::Tilt { .. } => TILT,
        Action::Motion { .. } => MOTION,
        Action::Device { .. } => HOUSE,
    }
}

/// What a kind of binding is for, in a line: under its name in Add binding, and atop its settings.
fn about(action: &Action) -> &'static str {
    match action {
        Action::Tap { .. } => "Puts a finger on a spot of the screen (drag the spot into place in Edit layout)",
        Action::Swipe { .. } => "Swipes across the screen with each press (drag its ends in Edit layout)",
        Action::Joystick { .. } => "Works a game's on-screen joystick: a finger rests on it and follows your stick",
        Action::Look { .. } => "Turns a game's camera, the way a finger dragged across the screen does",
        Action::Pointer { .. } => "Steers a pointer around the screen with a stick, and touches where it points",
        Action::Flick { .. } => "Flicking the stick swipes the screen that way",
        Action::Tilt { .. } => "Leans the device as the stick is pushed, for games you steer by tilting",
        Action::Motion { .. } => "Tilts the device as you tilt the controller (one with motion sensors)",
        Action::Device { .. } => "Presses one of the device's own buttons: Home, Sleep/Wake or the volume",
    }
}

/// One choice in Add binding: its icon, name, and what it's for underneath.
fn menu_item(ui: &mut Ui, p: &theme::Palette, icon: &str, title: &str, about: &str) -> egui::Response {
    let width = 340.0;
    let text = ui.painter().layout(about.to_owned(), FontId::proportional(12.0), p.on_surface_variant, width - 54.0);
    let (rect, r) = ui.allocate_exact_size(vec2(width, 34.0 + text.size().y), Sense::click());
    let painter = ui.painter();
    if r.hovered() {
        painter.rect_filled(rect, CornerRadius::same(8), p.container_highest);
    }
    let left = rect.left_top();
    painter.text(left + vec2(14.0, 11.0), Align2::LEFT_TOP, icon, FontId::new(20.0, theme::icons()), p.primary);
    painter.text(left + vec2(46.0, 8.0), Align2::LEFT_TOP, title, FontId::new(14.0, theme::medium()), p.on_surface);
    painter.galley(left + vec2(46.0, 27.0), text, p.on_surface_variant);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn templates() -> Vec<fn() -> Action> {
    vec![
        || Action::Tap { input: unset(), at: Point::new(0.5, 0.75), mode: TapMode::Hold },
        || Action::Swipe { input: unset(), from: Point::new(0.5, 0.65), to: Point::new(0.5, 0.35), ms: 150 },
        || Action::Joystick {
            stick: Stick::Left,
            center: Point::new(0.22, 0.78),
            radius: 0.15,
            press: Press::toggle_for(&Stick::Left),
            tap: None,
            tap_lift: TAP_LIFT_MS,
            speed: 1.0,
        },
        || Action::Look {
            stick: Stick::Right,
            center: Point::new(0.7, 0.45),
            half: [0.25, 0.3],
            speed: LOOK_SPEED,
            invert_y: false,
            press: Press::Pushed,
            tap: None,
            tap_lift: TAP_LIFT_MS,
            mouse: false,
            mouse_speed: 1.0,
            free: None,
        },
        || Action::Pointer {
            stick: Stick::Left,
            center: Point::CENTER,
            half: [0.5, 0.5],
            touch: unset(),
            toggle: false,
            style: PointerStyle::Centered,
            speed: 0.9,
            follow: Some(4.0),
        },
        || Action::Flick { stick: Stick::Right, center: Point::CENTER, length: 0.3 },
        || Action::Tilt { stick: Stick::Left, strength: 1.0, rest: Rest::Upright, set_level: None },
        || Action::Motion { sensitivity: 1.0 },
        || Action::Device { input: unset(), button: DeviceButton::Home },
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

/// When a stick binding's finger is down, for its summary.
fn press_text(press: &Press, kind: PadKind) -> String {
    match press {
        Press::Pushed => "touching while pushed".into(),
        Press::Hold { input } => format!("touching while {} is held", input_text(input, kind)),
        Press::Toggle { input } => format!("{} toggles the finger", input_text(input, kind)),
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
        Action::Joystick { stick, press, tap, .. } => {
            let tap = tap.as_ref().map(|t| format!(", {} taps it", input_text(t, kind))).unwrap_or_default();
            format!("{} drags an on-screen stick, {}{tap}", stick.label(), press_text(press, kind))
        }
        Action::Look { stick, press, tap, mouse, .. } => {
            let tap = tap.as_ref().map(|t| format!(", {} taps", input_text(t, kind))).unwrap_or_default();
            let press = if *press == Press::Pushed { String::new() } else { format!(", {}", press_text(press, kind)) };
            let mouse = if *mouse { " (and the mouse)" } else { "" };
            format!("{}{mouse} looks around{press}{tap}", stick.label())
        }
        Action::Pointer { stick, touch, toggle, style, .. } => {
            let how = if *style == PointerStyle::Centered { "returns to center" } else { "stays where it's left" };
            let touches = if *toggle { "toggles the touch" } else { "touches" };
            format!("{} points ({how}), {} {touches}", stick.label(), input_text(touch, kind))
        }
        Action::Flick { stick, .. } => format!("{} flicks swipe", stick.label()),
        Action::Tilt { stick, rest, set_level, .. } => {
            let rest =
                if *rest == Rest::Upright { String::new() } else { format!(" from {}", rest.label().to_lowercase()) };
            let set = set_level.as_ref().map(|i| format!(", {} sets level", input_text(i, kind))).unwrap_or_default();
            format!("{} leans the device{rest}{set}", stick.label())
        }
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
    // The whole row opens and closes its settings; its buttons still take their own clicks.
    let background = ui.painter().add(egui::Shape::Noop);
    let row = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        ui.horizontal(|ui| {
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
                if widgets::icon_button_sized(
                    ui,
                    p,
                    caret,
                    if expanded { "Hide settings" } else { "Settings" },
                    32.0,
                    true,
                )
                .clicked()
                {
                    *open = if expanded { None } else { Some(i) };
                }
                match &binding.action {
                    Action::Tap { input, .. } | Action::Swipe { input, .. } | Action::Device { input, .. } => {
                        if input_cap(ui, p, &input_text(input, kind), CAP_HINT).clicked() {
                            *capture = Some((i, Slot::Main));
                        }
                    }
                    Action::Pointer { touch, .. } => {
                        if input_cap(ui, p, &input_text(touch, kind), &cap_hover("Touches where the pointer is"))
                            .clicked()
                        {
                            *capture = Some((i, Slot::Touch));
                        }
                    }
                    Action::Joystick { stick, .. }
                    | Action::Look { stick, .. }
                    | Action::Flick { stick, .. }
                    | Action::Tilt { stick, .. } => {
                        if input_cap(ui, p, &stick.label(), "Click, then move a stick or press a direction key")
                            .clicked()
                        {
                            *capture = Some((i, Slot::Stick));
                        }
                    }
                    Action::Motion { .. } => {}
                }
            });
            trailing.response.rect.width()
        })
        .inner
    });
    let clicked = row.response.clicked();
    let hovered = row.response.hovered();
    let row_rect = row.response.rect.expand2(vec2(8.0, 2.0));
    if hovered {
        ui.painter().set(background, egui::Shape::rect_filled(row_rect, CornerRadius::same(10), p.container_high));
    }
    row.response.on_hover_cursor(egui::CursorIcon::PointingHand);
    if clicked {
        *open = if expanded { None } else { Some(i) };
    }
    let width = row.inner;
    if (width - reserved).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(id, width));
        ui.ctx().request_discard("a binding row's controls changed width");
    }
}

/// What clicking an input's cap does.
const CAP_HINT: &str = "Click, then press the button or key to use";

/// A clickable key cap: the input bound, and click to bind another. `hover` says what it's for and how to change it.
fn input_cap(ui: &mut Ui, p: &theme::Palette, text: &str, hover: &str) -> egui::Response {
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
    .on_hover_text(hover)
}

/// A cap's tooltip: what the input is for, then how to change it (one tooltip: egui shows each one added separately).
fn cap_hover(what: &str) -> String {
    format!("{what}\n{CAP_HINT}")
}

/// A new look binding's speed, in screen widths a second at full tilt: shown as 1, up to 2 (twice it).
const LOOK_SPEED: f32 = 1.5;

/// A binding's slider bar: twice egui's.
const SLIDER: f32 = 200.0;

/// A slider with its value beside it, which can be clicked and typed into for an exact number.
fn number_slider(ui: &mut Ui, width: f32, slider: egui::Slider) -> egui::Response {
    ui.spacing_mut().slider_width = width;
    ui.add(slider.show_value(true))
}

/// A slider over a fraction shown and typed as a percentage.
fn percent(slider: egui::Slider) -> egui::Slider {
    slider
        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
        .custom_parser(|t| t.trim().trim_end_matches('%').trim().parse::<f64>().ok().map(|v| v / 100.0))
}

/// A setting's row: its name (hover it for what it does) and its controls.
type Row<'a> = dyn Fn(&mut Ui, &str, &str, &mut dyn FnMut(&mut Ui)) + 'a;

/// An optional input (a binding's tap, Free the mouse, Only while holding): its cap, "Nothing" when there's none, and
/// Clear.
#[allow(clippy::too_many_arguments)]
fn optional_input(
    ui: &mut Ui,
    p: &theme::Palette,
    row: &Row,
    title: &str,
    tip: &str,
    input: &mut Option<Input>,
    kind: PadKind,
    capture: &mut dyn FnMut(),
) {
    row(ui, title, tip, &mut |ui| {
        let text = input.as_ref().map_or("Nothing".to_string(), |t| input_text(t, kind));
        if input_cap(ui, p, &text, &cap_hover(tip)).clicked() {
            capture();
        }
        if input.is_some() && widgets::text_button(ui, p, "Clear").on_hover_text("Bind nothing").clicked() {
            *input = None;
        }
    });
}

const WITH_TIP: &str = "The binding works only while this is held too, so one button can do two things";
const SHOWN_TIP: &str = "Draw its spot, area and finger on the screen while playing (Edit layout always shows them)";

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
    // Names in a column of their own, narrower when the window is.
    let label = (ui.available_width() * 0.3).clamp(104.0, 136.0);
    let row = |ui: &mut Ui, title: &str, tip: &str, add: &mut dyn FnMut(&mut Ui)| {
        ui.horizontal(|ui| {
            ui.set_min_height(36.0);
            ui.allocate_ui_with_layout(vec2(label, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_width(label);
                let r = ui.label(widgets::muted(p, title).size(13.0));
                if !tip.is_empty() {
                    r.on_hover_text(tip);
                }
            });
            add(ui);
        });
    };
    let gap = |ui: &mut Ui| ui.add_space(8.0);
    ui.label(widgets::muted(p, about(&binding.action)).size(12.5));
    ui.add_space(4.0);
    // Rarely changed settings fold away under More options, which lists any that are set while it's closed.
    let mut more = Vec::new();
    match &mut binding.action {
        Action::Tap { mode, .. } => {
            row(ui, "Touches", "How the finger touches the spot while its button is pressed", &mut |ui| {
                widgets::segmented_tips(
                    ui,
                    p,
                    mode,
                    &[
                        (TapMode::Hold, "While held", "Touches while the button is held"),
                        (TapMode::Toggle, "Toggle", "A press touches, the next lifts the finger"),
                        (TapMode::Repeat, "Repeatedly", "Taps over and over while the button is held"),
                    ],
                );
            });
        }
        Action::Swipe { ms, .. } => {
            let tip = "How long each swipe takes: quicker is a flick, slower a drag";
            row(ui, "Duration", tip, &mut |ui| {
                number_slider(ui, SLIDER, egui::Slider::new(ms, 40..=800).suffix(" ms")).on_hover_text(tip);
            });
        }
        Action::Joystick { stick, radius, press, tap, tap_lift, speed, .. } => {
            let was = stick.clone();
            if stick_row(ui, p, &row, stick) {
                *capture = Some((i, Slot::Keys));
            }
            follow_stick(&was, stick, press);
            let tip = "How far the finger moves from the stick's center at a full push, as a share of the screen's \
                       shorter side";
            row(ui, "Reach", tip, &mut |ui| {
                number_slider(ui, SLIDER, percent(egui::Slider::new(radius, 0.04..=0.5))).on_hover_text(tip);
            });
            let tip = "How fast the finger follows the stick: at 1, across the whole reach in 50 ms. Slow it for a game \
                       whose stick loses a finger that moves too fast (a D-pad, keys or a quick flick)";
            row(ui, "Speed", tip, &mut |ui| {
                number_slider(ui, SLIDER, egui::Slider::new(speed, 0.1..=2.0).logarithmic(true).max_decimals(2))
                    .on_hover_text(tip);
            });
            gap(ui);
            press_rows(ui, p, &row, press, stick, kind, i, capture);
            optional_input(
                ui,
                p,
                &row,
                "Tap with",
                "For games that fire with a tap on the stick: lifts the finger and touches the stick's center while \
                 held, then lands again",
                tap,
                kind,
                &mut || *capture = Some((i, Slot::Tap)),
            );
            if tap.is_some() {
                more.push(More::TapLift(tap_lift));
            }
            more.push(More::Shown);
        }
        Action::Look { stick, half, center, speed, invert_y, press, tap, tap_lift, mouse, mouse_speed, free } => {
            let was = stick.clone();
            if stick_row(ui, p, &row, stick) {
                *capture = Some((i, Slot::Keys));
            }
            follow_stick(&was, stick, press);
            let tip = "How fast the view turns at a full push (pushed a little, it turns gently); 1 is the usual";
            row(ui, "Speed", tip, &mut |ui| {
                // Shown so a new binding's speed reads 1 (presets keep screen widths a second).
                let was = *speed / LOOK_SPEED;
                let mut shown = was;
                number_slider(ui, SLIDER, egui::Slider::new(&mut shown, 0.1..=2.0).max_decimals(2)).on_hover_text(tip);
                if shown != was {
                    *speed = shown * LOOK_SPEED;
                }
            });
            let tip = "Pushing up looks down, as in a plane";
            row(ui, "Invert up and down", tip, &mut |ui| {
                widgets::switch(ui, p, invert_y).on_hover_text(tip);
            });
            gap(ui);
            press_rows(ui, p, &row, press, stick, kind, i, capture);
            optional_input(
                ui,
                p,
                &row,
                "Tap with",
                "For games that fire with a tap: lifts the look finger and touches the area while held",
                tap,
                kind,
                &mut || *capture = Some((i, Slot::Tap)),
            );
            gap(ui);
            let tip = "Click the game's screen to lock the mouse to it: moving it looks around, and its buttons bind \
                       like keys. Escape lets go";
            row(ui, "Mouse looks", tip, &mut |ui| {
                let was = *mouse;
                widgets::switch(ui, p, mouse).on_hover_text(tip);
                // Turned on with nothing to fire: the left button taps, as in a shooter; Ctrl frees the mouse for
                // the game's buttons (Alt is the pinch's).
                if *mouse && !was && tap.is_none() {
                    *tap = Some(Input::Key(lighttouch_model::gamepad::preset::MOUSE_LEFT.into()));
                }
                if *mouse && !was && free.is_none() {
                    *free = Some(Input::Key(lighttouch_model::gamepad::preset::KEY_CTRL.into()));
                }
            });
            if *mouse {
                let tip = "How far the view turns as the mouse moves; 1 is the usual";
                row(ui, "Mouse speed", tip, &mut |ui| {
                    number_slider(
                        ui,
                        SLIDER,
                        egui::Slider::new(mouse_speed, 0.2..=5.0).logarithmic(true).max_decimals(2),
                    )
                    .on_hover_text(tip);
                });
                optional_input(
                    ui,
                    p,
                    &row,
                    "Free the mouse",
                    "Hold it to use the mouse as a finger instead, to tap and swipe the game's buttons and menus",
                    free,
                    kind,
                    &mut || *capture = Some((i, Slot::Free)),
                );
            }
            if tap.is_some() {
                more.push(More::TapLift(tap_lift));
            }
            more.push(More::Area { half, center, whole: false });
            more.push(More::Shown);
        }
        Action::Flick { stick, length, .. } => {
            if stick_row(ui, p, &row, stick) {
                *capture = Some((i, Slot::Keys));
            }
            let tip = "How far each swipe goes, as a share of the screen's shorter side";
            row(ui, "Swipe length", tip, &mut |ui| {
                number_slider(ui, SLIDER, percent(egui::Slider::new(length, 0.05..=0.8))).on_hover_text(tip);
            });
            more.push(More::Shown);
        }
        Action::Pointer { stick, style, speed, follow, half, center, touch, toggle } => {
            if stick_row(ui, p, &row, stick) {
                *capture = Some((i, Slot::Keys));
            }
            row(ui, "When let go", "Where the pointer goes when the stick is let go", &mut |ui| {
                widgets::segmented_tips(
                    ui,
                    p,
                    style,
                    &[
                        (
                            PointerStyle::Centered,
                            "Returns to center",
                            "The stick points from the area's center, and the pointer goes back there when it's let \
                             go: like aiming a remote at the screen",
                        ),
                        (
                            PointerStyle::Cursor,
                            "Stays put",
                            "Like a mouse: the stick moves the pointer, and it stays where it's left",
                        ),
                    ],
                );
            });
            if *style == PointerStyle::Cursor {
                let tip = "How fast the pointer moves at a full push, in screen widths a second";
                row(ui, "Speed", tip, &mut |ui| {
                    number_slider(ui, SLIDER, egui::Slider::new(speed, 0.2..=3.0).max_decimals(2)).on_hover_text(tip);
                });
            } else {
                // All the way right: no glide, the pointer jumps to where the stick points.
                const INSTANT: f32 = 12.0;
                let tip = "How fast the pointer glides to where the stick points and back, in screen widths a second \
                           (all the way right: at once)";
                row(ui, "Speed", tip, &mut |ui| {
                    let mut v = follow.unwrap_or(INSTANT);
                    let slider = egui::Slider::new(&mut v, 0.5..=INSTANT)
                        .logarithmic(true)
                        .custom_formatter(|v, _| {
                            if v >= INSTANT as f64 {
                                "At once".into()
                            } else {
                                format!("{}", (v * 100.0).round() / 100.0)
                            }
                        })
                        .custom_parser(|t| match t.trim().to_lowercase().as_str() {
                            "at once" | "instant" => Some(INSTANT as f64),
                            t => t.parse().ok(),
                        });
                    number_slider(ui, SLIDER, slider).on_hover_text(tip);
                    *follow = (v < INSTANT).then_some(v);
                });
            }
            gap(ui);
            let tip = "Touches the screen where the pointer is";
            row(ui, "Touch with", tip, &mut |ui| {
                if input_cap(ui, p, &input_text(touch, kind), &cap_hover(tip)).clicked() {
                    *capture = Some((i, Slot::Touch));
                }
                widgets::segmented_tips(
                    ui,
                    p,
                    toggle,
                    &[
                        (false, "While held", "Touches while the button is held"),
                        (true, "Toggle", "A press touches, the next lifts the finger"),
                    ],
                );
            });
            more.push(More::Area { half, center, whole: true });
            more.push(More::Shown);
        }
        Action::Tilt { stick, strength, rest, set_level } => {
            if stick_row(ui, p, &row, stick) {
                *capture = Some((i, Slot::Keys));
            }
            let tip = "How far the device leans at a full push";
            row(ui, "Strength", tip, &mut |ui| {
                number_slider(ui, SLIDER, percent(egui::Slider::new(strength, 0.2..=1.0))).on_hover_text(tip);
            });
            let tip = "How the device rests when the stick is let go, as the screen is shown; the stick leans it from \
                       there";
            row(ui, "Rests", tip, &mut |ui| {
                egui::ComboBox::from_id_salt(("tilt-rest", i))
                    .selected_text(rest.label())
                    .width(170.0)
                    .show_ui(ui, |ui| {
                        for r in Rest::ALL {
                            ui.selectable_value(rest, r, r.label()).on_hover_text(r.about());
                        }
                    })
                    .response
                    .on_hover_text(tip);
            });
            optional_input(
                ui,
                p,
                &row,
                "Set level with",
                "Lean the device with the stick to where the game looks level, then press it: the device rests there \
                 when the stick is let go. Pressed with the stick let go, it rests as chosen above again",
                set_level,
                kind,
                &mut || *capture = Some((i, Slot::Level)),
            );
        }
        Action::Motion { sensitivity } => {
            let tip = "How far the device tilts as the controller tilts; 1× is the same amount";
            row(ui, "Sensitivity", tip, &mut |ui| {
                number_slider(ui, SLIDER, egui::Slider::new(sensitivity, 0.3..=3.0).max_decimals(2).suffix("×"))
                    .on_hover_text(tip);
            });
            let tip = "Hold the controller the way you play, then click Set level: held that way, the device lies flat on a table. Without it, the controller lying flat is level";
            row(ui, "Level", tip, &mut |ui| {
                let can = lean.is_some();
                if widgets::tonal_enabled(ui, p, "Set level", can).on_hover_text(tip).clicked() {
                    *set_level = true;
                }
                if !can {
                    ui.label(widgets::muted(p, "Needs a controller with motion sensors").size(12.0));
                }
            });
        }
        Action::Device { button, .. } => {
            row(ui, "Presses", "Which of the device's buttons it presses", &mut |ui| {
                let r = widgets::chip_menu(ui, p, button.label()).on_hover_text("Pick the device's button");
                egui::Popup::menu(&r).show(|ui| {
                    ui.set_min_width(160.0);
                    for b in DeviceButton::ALL {
                        if ui.selectable_label(*button == b, b.label()).clicked() {
                            *button = b;
                        }
                    }
                });
            });
        }
    }
    let mut with_capture = || *capture = Some((i, Slot::With));
    if more.is_empty() {
        // Nothing else to fold away: no fold for one row.
        optional_input(ui, p, &row, "Only while holding", WITH_TIP, &mut binding.with, kind, &mut with_capture);
        return;
    }
    // More options, open or closed, and what's set in it.
    let id = more_id(i);
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let mut set = Vec::new();
    if let Some(w) = &binding.with {
        set.push(format!("only while holding {}", w.label(kind)));
    }
    for item in &more {
        if let More::TapLift(ms) = item
            && **ms != TAP_LIFT_MS
        {
            set.push(format!("tap lift {ms} ms"));
        }
    }
    if binding.hidden {
        set.push("not shown on screen".to_string());
    }
    gap(ui);
    ui.horizontal(|ui| {
        ui.set_min_height(32.0);
        let mut job = egui::text::LayoutJob::default();
        let format = |font: FontId| egui::TextFormat {
            font_id: font,
            color: p.primary,
            valign: egui::Align::Center,
            ..Default::default()
        };
        job.append(
            if open { icons::CARET_DOWN } else { icons::CARET_RIGHT },
            0.0,
            format(FontId::new(13.0, theme::icons())),
        );
        job.append("More options", 6.0, format(FontId::new(13.0, theme::medium())));
        let r = ui
            .add(egui::Button::new(job).frame(false).min_size(vec2(0.0, 28.0)))
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Where it shows on the screen, and making it work only while another button is held");
        if r.clicked() {
            open = !open;
            ui.data_mut(|d| d.insert_temp(id, open));
        }
        if !open && !set.is_empty() {
            let mut text = set.join(" · ");
            text[..1].make_ascii_uppercase();
            ui.label(widgets::muted(p, &text).size(12.0));
        }
    });
    if !open {
        return;
    }
    for item in more {
        match item {
            More::Area { half, center, whole } => {
                let tip = "Where the finger moves. Drag its corners in Edit layout to change it";
                row(ui, "Area", tip, &mut |ui| {
                    let (text, to_half, to_center) = if whole {
                        ("Whole screen", [0.5, 0.5], Point::CENTER)
                    } else {
                        ("Right half", [0.25, 0.5], Point::new(0.75, 0.5))
                    };
                    if widgets::text_button(ui, p, text)
                        .on_hover_text(format!("Make the area the {}", text.to_lowercase()))
                        .clicked()
                    {
                        *half = to_half;
                        *center = to_center;
                    }
                    ui.label(widgets::muted(p, "or drag its corners in Edit layout").size(12.0));
                });
            }
            More::TapLift(ms) => {
                let tip = "How long the finger stays up before and after a tap, so the game sees it lift and land \
                           again. Raise it if fast taps go missing (a game at 30 frames a second needs more than 33 \
                           ms); lower it to tap faster";
                row(ui, "Tap lift", tip, &mut |ui| {
                    number_slider(ui, SLIDER, egui::Slider::new(ms, 20..=200).suffix(" ms")).on_hover_text(tip);
                    if *ms != TAP_LIFT_MS
                        && widgets::text_button(ui, p, "Reset")
                            .on_hover_text(format!("Back to {TAP_LIFT_MS} ms"))
                            .clicked()
                    {
                        *ms = TAP_LIFT_MS;
                    }
                });
            }
            More::Shown => {
                row(ui, "Show on screen", SHOWN_TIP, &mut |ui| {
                    let mut shown = !binding.hidden;
                    widgets::switch(ui, p, &mut shown).on_hover_text(SHOWN_TIP);
                    binding.hidden = !shown;
                });
            }
        }
    }
    optional_input(ui, p, &row, "Only while holding", WITH_TIP, &mut binding.with, kind, &mut with_capture);
}

/// Whether binding `i`'s More options are open.
fn more_id(i: usize) -> egui::Id {
    egui::Id::unique(("binding-more", i))
}

/// A setting folded under More options.
enum More<'a> {
    /// The area's size and place, with a button for the usual one: the whole screen or its right half.
    Area { half: &'a mut [f32; 2], center: &'a mut Point, whole: bool },
    /// Drawing the spot while playing.
    Shown,
    /// How long a stick binding's finger stays up around its tap.
    TapLift(&'a mut u32),
}

/// A stick binding's press setting: while pushed, while a button is held, or toggled by one (the stick's click unless
/// it has none, when the player is asked for one).
#[allow(clippy::too_many_arguments)]
fn press_rows(
    ui: &mut Ui,
    p: &theme::Palette,
    row: &Row,
    press: &mut Press,
    stick: &Stick,
    kind: PadKind,
    i: usize,
    capture: &mut Option<(usize, Slot)>,
) {
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Pushed,
        Hold,
        Toggle,
    }
    let was = match press {
        Press::Pushed => Mode::Pushed,
        Press::Hold { .. } => Mode::Hold,
        Press::Toggle { .. } => Mode::Toggle,
    };
    let mut mode = was;
    row(ui, "Finger down", "When the finger is on the screen", &mut |ui| {
        widgets::segmented_tips(
            ui,
            p,
            &mut mode,
            &[
                (Mode::Toggle, "Toggle", "A button puts the finger down, and the next press lifts it"),
                (Mode::Hold, "While held", "The finger is down while a button is held"),
                (
                    Mode::Pushed,
                    "While pushed",
                    "The finger is down while the stick is pushed, and lifts when it's let go",
                ),
            ],
        );
    });
    if mode != was {
        let input = press.input().cloned().filter(|i| !matches!(i, Input::Key(k) if k.is_empty()));
        let input = input.or_else(|| stick.click());
        let asked = input.is_none();
        let input = input.unwrap_or_else(unset);
        *press = match mode {
            Mode::Pushed => Press::Pushed,
            Mode::Hold => Press::Hold { input },
            Mode::Toggle => Press::Toggle { input },
        };
        if asked && mode != Mode::Pushed {
            *capture = Some((i, Slot::Press));
        }
    }
    // The button, under the choice that needs it, saying what it does.
    if let Some(input) = press.input() {
        let what = if matches!(press, Press::Toggle { .. }) {
            "puts the finger down, and lifts it"
        } else {
            "holds the finger down"
        };
        row(ui, "", "", &mut |ui| {
            if input_cap(ui, p, &input_text(input, kind), &cap_hover(&format!("The button that {what}"))).clicked() {
                *capture = Some((i, Slot::Press));
            }
            ui.label(widgets::muted(p, what).size(12.5));
        });
    }
}

/// The stick changed: a press button that was the old stick's click becomes the new one's (or unset: the D-pad and
/// keys have none).
fn follow_stick(was: &Stick, now: &Stick, press: &mut Press) {
    if was == now {
        return;
    }
    if let Some(input) = press.input_mut() {
        if was.click().as_ref() == Some(input) {
            *input = now.click().unwrap_or_else(unset);
        }
    }
}

/// The stick a binding follows, and Map keys, which asks for four keys one direction at a time (returns true when
/// clicked).
fn stick_row(ui: &mut Ui, p: &theme::Palette, row: &Row, stick: &mut Stick) -> bool {
    let mut map = false;
    row(ui, "Moved by", "A stick, the D-pad or four keys", &mut |ui| {
        let r = widgets::chip_menu(ui, p, &stick.label()).on_hover_text("Pick a stick, the D-pad or keys");
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
        map = widgets::text_button(ui, p, "Map keys")
            .on_hover_text("Pick a key for each direction (up, right, down, left), to play on the keyboard")
            .clicked();
    });
    map
}

/// Map keys: asks for the key for each direction in turn over a drawing of the stick, then gives the binding those
/// four keys. Backspace goes back a step, Escape cancels; a key already used for another direction is turned away.
fn key_map_prompt(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(map) = app.controller.key_map.as_mut() else { return };
    let pressed: Vec<egui::Key> = ui.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key { key, pressed: true, repeat: false, .. } => Some(*key),
                _ => None,
            })
            .collect()
    });
    let mut cancel = false;
    for key in pressed {
        match key {
            egui::Key::Escape => cancel = true,
            // Reserved: it goes in and out of full screen.
            egui::Key::F11 => {}
            egui::Key::Backspace => {
                map.keys.pop();
                map.refused = None;
            }
            _ if map.keys.len() < 4 => {
                let name = key.name().to_string();
                if let Some(i) = map.keys.iter().position(|k| *k == name) {
                    map.refused = Some(format!(
                        "{} is already {}",
                        lighttouch_model::gamepad::preset::key_label(&name),
                        KEY_DIRECTIONS[i]
                    ));
                } else {
                    map.keys.push(name);
                    map.refused = None;
                }
            }
            _ => {}
        }
    }
    let step = map.keys.len();
    let modal =
        egui::Modal::new(egui::Id::unique("controller-key-map")).backdrop_color(Color32::from_black_alpha(140)).frame(
            Frame::new()
                .fill(p.container_high)
                .corner_radius(CornerRadius::same(theme::radius::DOCK))
                .inner_margin(Margin::same(24)),
        );
    let mut back = false;
    modal.show(ui.ctx(), |ui| {
        ui.set_width(340.0);
        if step < 4 {
            ui.label(RichText::new(format!("Press the key for {}", KEY_DIRECTIONS[step])).font(theme::title()));
        }
        ui.add_space(2.0);
        ui.label(widgets::muted(&p, format!("Step {} of 4 · Backspace goes back · Escape cancels", (step + 1).min(4))));
        ui.add_space(8.0);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 220.0), Sense::hover());
        draw_key_map(ui.painter(), &p, rect, &map.keys);
        let note = map.refused.clone().unwrap_or_default();
        ui.label(RichText::new(note).size(12.5).color(theme::warning(&p)));
        ui.add_space(8.0);
        widgets::dialog_buttons(ui, |ui| {
            if widgets::tonal(ui, &p, "Cancel").clicked() {
                cancel = true;
            }
            if step > 0 && widgets::text_button(ui, &p, "Back").clicked() {
                back = true;
            }
        });
    });
    if back {
        map.keys.pop();
        map.refused = None;
    }
    if cancel {
        app.controller.key_map = None;
        return;
    }
    if map.keys.len() < 4 {
        return;
    }
    let (binding, keys) = (map.binding, map.keys.clone());
    app.controller.key_map = None;
    let new = Stick::Keys { up: keys[0].clone(), right: keys[1].clone(), down: keys[2].clone(), left: keys[3].clone() };
    let Some(index) = app.model.gamepads.active else { return };
    let mut preset = app.model.gamepads.presets[index].clone();
    let Some(binding) = preset.bindings.get_mut(binding) else { return };
    match &mut binding.action {
        Action::Joystick { stick, press, .. } | Action::Look { stick, press, .. } => {
            let was = std::mem::replace(stick, new);
            follow_stick(&was, stick, press);
        }
        Action::Flick { stick, .. } | Action::Tilt { stick, .. } | Action::Pointer { stick, .. } => *stick = new,
        _ => return,
    }
    app.model.gamepads.presets[index] = preset;
    app.model.gamepads.changed(app.model.session.as_mut());
}

/// The stick with a key cap for each direction: the ones mapped show their key, the one being asked for is lit with
/// the stick pushed toward it, the rest wait.
fn draw_key_map(painter: &egui::Painter, p: &theme::Palette, rect: Rect, keys: &[String]) {
    let c = rect.center();
    let dirs = [vec2(0.0, -1.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(-1.0, 0.0)];
    let step = keys.len();
    // The stick: a ring, and its knob pushed toward the direction asked for.
    painter.circle_stroke(c, 34.0, Stroke::new(2.0, p.outline));
    let push = dirs.get(step).copied().unwrap_or(vec2(0.0, 0.0));
    if step < 4 {
        // Short of the nearest cap (above and below, 64 px out).
        let tip = c + push * 50.0;
        painter.line_segment([c + push * 20.0, tip], Stroke::new(3.0, p.primary));
        let side = vec2(-push.y, push.x);
        painter.add(egui::Shape::convex_polygon(
            vec![tip + push * 8.0, tip - push * 4.0 + side * 7.0, tip - push * 4.0 - side * 7.0],
            p.primary,
            Stroke::NONE,
        ));
    }
    painter.circle_filled(c + push * 14.0, 16.0, if step < 4 { p.primary } else { p.on_surface_variant });
    for (i, d) in dirs.iter().enumerate() {
        let at = c + *d * vec2(118.0, 84.0);
        let cap = Rect::from_center_size(at, vec2(72.0, 40.0));
        let (fill, stroke, text, color) = if let Some(k) = keys.get(i) {
            (
                p.container_highest,
                Stroke::new(1.0, p.outline),
                lighttouch_model::gamepad::preset::key_label(k),
                p.on_surface,
            )
        } else if i == step {
            (p.primary.gamma_multiply(0.18), Stroke::new(2.0, p.primary), "?".to_string(), p.primary)
        } else {
            (
                Color32::TRANSPARENT,
                Stroke::new(1.0, p.outline.gamma_multiply(0.5)),
                KEY_DIRECTIONS[i].to_string(),
                p.on_surface_variant,
            )
        };
        painter.rect(cap, CornerRadius::same(8), fill, stroke, egui::StrokeKind::Inside);
        painter.text(at, Align2::CENTER_CENTER, text, FontId::new(14.0, theme::medium()), color);
    }
}

/// "Press a button or key": the next input pressed binds (Escape cancels).
fn capture_prompt(app: &mut App, ui: &mut Ui) {
    let p = app.palette;
    let Some(capture) = app.controller.capture.clone() else { return };
    let pads = app.model.gamepads.pads();
    let merged = lighttouch_model::gamepad::mapper::Merged::from(&pads.pads);
    let keys: HashSet<String> = ui.ctx().input(held_keys);
    let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
    // A new press: a button not held when the prompt opened, or a key.
    let new_button =
        PadButton::ALL.iter().copied().find(|b| merged.buttons & b.bit() != 0 && capture.held.0 & b.bit() == 0);
    let new_key =
        keys.iter().find(|k| !capture.held.1.contains(*k) && k.as_str() != "Escape" && k.as_str() != "F11").cloned();
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
    let mut mouse_got = None;
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
        if capture.slot != Slot::Stick {
            // Mouse buttons bind like keys (they act while the mouse is locked to the game, for a mouse-look binding).
            ui.add_space(12.0);
            let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
            let lit = r.hovered();
            ui.painter().rect(
                rect,
                CornerRadius::same(8),
                if lit { p.container_highest } else { Color32::TRANSPARENT },
                Stroke::new(1.0, p.outline.gamma_multiply(0.7)),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Or click here with a mouse button",
                FontId::new(13.0, theme::medium()),
                p.on_surface_variant,
            );
            use lighttouch_model::gamepad::preset::{MOUSE_LEFT, MOUSE_MIDDLE, MOUSE_RIGHT};
            for (button, name) in [
                (egui::PointerButton::Primary, MOUSE_LEFT),
                (egui::PointerButton::Secondary, MOUSE_RIGHT),
                (egui::PointerButton::Middle, MOUSE_MIDDLE),
            ] {
                if r.clicked_by(button) {
                    mouse_got = Some(Input::Key(name.into()));
                }
            }
        }
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
    if mouse_got.is_some() {
        got = mouse_got;
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
        (Slot::Stick, Action::Joystick { stick: s, press, .. } | Action::Look { stick: s, press, .. }) => {
            if let Some(new) = stick {
                let was = std::mem::replace(s, new);
                follow_stick(&was, s, press);
            }
        }
        (
            Slot::Stick,
            Action::Flick { stick: s, .. } | Action::Tilt { stick: s, .. } | Action::Pointer { stick: s, .. },
        ) => {
            if let Some(new) = stick {
                *s = new;
            }
        }
        (Slot::Press, Action::Joystick { press, .. } | Action::Look { press, .. }) => {
            if let (Some(g), Some(input)) = (got, press.input_mut()) {
                *input = g;
            }
        }
        (Slot::Free, Action::Look { free, .. }) => {
            if got.is_some() {
                *free = got;
            }
        }
        (Slot::Level, Action::Tilt { set_level, .. }) => {
            if got.is_some() {
                *set_level = got;
            }
        }
        (Slot::Tap, Action::Joystick { tap, .. } | Action::Look { tap, .. }) => {
            if got.is_some() {
                *tap = got;
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
    let r = widgets::dock_button(ui, &p, GAME_CONTROLLER, "Controls", true, on.then_some(p.primary));
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

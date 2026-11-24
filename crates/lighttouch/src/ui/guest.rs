//! The guest screen: the newest frame as a texture, and input. Left mouse is finger 1, right mouse finger 2, a
//! touchscreen's fingers map to their own contacts. As on Android, a finger dragged past the screen's edge holds at
//! the nearest edge rather than lifting. Alt-dragging adds a second finger opposite the first through the screen's centre, to pinch
//! and rotate; Alt+Shift moves the two together (as in Xcode's Simulator). Keys: H Home, P Sleep/Wake, +/- volume
//! (held while the key is held). With a mouse-look binding in the controller preset, a click on the screen locks the
//! mouse to the game instead: its movement and buttons go to the preset until Escape (or another window) lets go.
use super::device_frame;
use super::scaling::{self, Scaled};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Ui};
use lighttouch_core::runtime::{BoardInfo, Contact};
use lighttouch_model::Model;
use lighttouch_model::gamepad::preset::{MOUSE_LEFT, MOUSE_MIDDLE, MOUSE_RIGHT};
use lighttouch_model::tilt::TiltKeys;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const DOWN: u32 = 0;
const MOVE: u32 = 1;
const UP: u32 = 2;
const CANCEL: u32 = 3;

/// Where an Alt-drag's second finger goes, in normalized screen coordinates.
#[derive(Clone, Copy)]
struct Pair {
    start: (f64, f64),
    /// Alt+Shift: alongside the first finger rather than opposite it.
    parallel: bool,
}

impl Pair {
    fn partner(&self, x: f64, y: f64) -> (f64, f64) {
        let (sx, sy) = self.start;
        let (px, py) = if self.parallel { (1.0 - sx + (x - sx), 1.0 - sy + (y - sy)) } else { (1.0 - x, 1.0 - y) };
        (px.clamp(0.0, 1.0), py.clamp(0.0, 1.0))
    }
}

#[derive(Default)]
pub struct GuestView {
    texture: Scaled,
    rgba: Vec<u8>,
    size: [usize; 2],
    /// The longer side of the screen as last drawn, in physical pixels: Crisp's prescale follows it.
    shown_px: std::cell::Cell<f32>,
    /// The screen's rect as last logged for LIGHTTOUCH_UI_LOG_SCREEN (devshot.rs), in physical pixels.
    logged: Option<[i32; 4]>,
    record: Option<String>,
    /// Mouse buttons holding a contact: (contact id, last position).
    mouse: HashMap<u32, Pos2>,
    /// Touchscreen finger id → contact id.
    touches: HashMap<u64, u32>,
    /// The second finger following the left button while Alt-dragging.
    pair: Option<Pair>,
    /// Hardware buttons as the guest has them: down while any control holds them.
    held: Vec<u32>,
    /// Which control holds which button. Each keeps its own state (the H key is up while the pointer holds the
    /// drawn Home), or they would undo each other every frame.
    held_by: Vec<(u32, ButtonControl)>,
    focused: bool,
    /// The mouse is locked to the game (since when, for the hint), its movement not yet taken, and its buttons
    /// held; `armed` once the click that locked it has been let go (that click isn't a press).
    locked: Option<Instant>,
    mouse_moved: (f32, f32),
    mouse_buttons: Vec<&'static str>,
    armed: bool,
    /// The window had the focus last frame while locked (letting go when it's lost, not when a system never says).
    lock_focused: bool,
    /// Escape let go: the mouse is a finger again until Escape locks it again (a click doesn't), and since when
    /// (for the hint).
    released: Option<Instant>,
    /// Locked, but the binding's free input is held: the mouse moves a pointer of our own over the screen (drawn as
    /// a bubble, and stopping at its edges: the real cursor stays hidden and locked) whose left button is finger 1.
    /// Where it is (0..1 across and down the screen as drawn), and whether its finger is down (at that scan point).
    free: bool,
    free_at: (f32, f32),
    free_down: Option<(f64, f64)>,
}

/// Whether a binding's input is held: a key (Ctrl, Shift and Alt among them), a mouse button or a controller's.
fn held(ui: &Ui, model: &Model, input: &lighttouch_model::gamepad::preset::Input) -> bool {
    use lighttouch_model::gamepad::preset::Input;
    match input {
        Input::Key(k) => {
            let button = match k.as_str() {
                MOUSE_LEFT => Some(egui::PointerButton::Primary),
                MOUSE_RIGHT => Some(egui::PointerButton::Secondary),
                MOUSE_MIDDLE => Some(egui::PointerButton::Middle),
                _ => None,
            };
            match button {
                Some(b) => ui.input(|i| i.pointer.button_down(b)),
                None => ui.input(|i| super::controller::held_keys(i).contains(k)),
            }
        }
        Input::Pad(b) => {
            lighttouch_model::gamepad::mapper::Merged::from(&model.gamepads.pads().pads).buttons & b.bit() != 0
        }
    }
}

/// A note across the top of the screen for a couple of seconds after `since`.
fn hint(ui: &Ui, rect: Rect, since: Instant, text: &str) {
    hint_at(ui, egui::pos2(rect.center().x, rect.top() + 22.0), since, text);
}

/// A note centred on `at` for a couple of seconds after `since`.
pub(super) fn hint_at(ui: &Ui, at: egui::Pos2, since: Instant, text: &str) {
    if since.elapsed() >= Duration::from_millis(2500) {
        return;
    }
    let font = egui::FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(text.into(), font.clone(), Color32::WHITE);
    let back = Rect::from_center_size(at, galley.size() + egui::vec2(20.0, 10.0));
    ui.painter().rect_filled(back, 8.0, Color32::from_black_alpha(170));
    ui.painter().text(at, egui::Align2::CENTER_CENTER, text, font, Color32::WHITE);
    ui.ctx().request_repaint_after(Duration::from_millis(100));
}

/// What presses a hardware button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonControl {
    Keys,
    /// The drawn device's buttons.
    Device,
    /// The full-screen bar's.
    Bar,
    /// A short Home press that wakes the screen.
    Wake,
}

impl GuestView {
    /// Pull a new frame into the texture. Returns true when one arrived.
    pub fn update(&mut self, ctx: &egui::Context, model: &mut Model) -> bool {
        let scaling = model.settings.scaling;
        let Some(session) = &mut model.session else {
            return false;
        };
        if self.record.as_deref() != Some(session.record_id.as_str()) {
            // A new session: drop the last device's picture.
            self.record = Some(session.record_id.clone());
            self.texture.clear();
        }
        let fresh = match session.read_frame(&mut self.rgba) {
            Some(meta) => {
                self.size = [meta.width as usize, meta.height as usize];
                true
            }
            None => false,
        };
        if !fresh && !self.texture.ready() {
            return false;
        }
        // A changed method or size uploads the last frame again, so it applies at once, even to a still screen.
        let (size, rgba) = (self.size, &self.rgba);
        self.texture.update(ctx, "guest", fresh, scaling, size, self.shown_px.get(), || {
            egui::ColorImage::from_rgba_unmultiplied(size, rgba)
        });
        fresh
    }

    /// Forget the picture (the device stopped).
    pub fn clear(&mut self) {
        self.texture.clear();
        self.record = None;
    }

    pub fn has_frame(&self) -> bool {
        self.texture.ready()
    }

    /// The screen's size in pixels as it stands upright (the iPad's landscape scan turned), once a frame came.
    pub fn shown_pixels(&self) -> Option<[u32; 2]> {
        let [w, h] = self.size;
        if w == 0 || h == 0 {
            return None;
        }
        Some(if self.scan_turned() { [h as u32, w as u32] } else { [w as u32, h as u32] })
    }

    /// The guest laid its portrait UI a quarter clockwise into a landscape scan (the iPad's panel), so the scan
    /// stands a quarter-turn from upright: the frame is wider than tall while the cutout it fills is portrait.
    pub fn scan_turned(&self) -> bool {
        self.size[0] > self.size[1]
    }

    /// The frame's average colour around a point of its scan (0..1), for drawing over it in its inverse.
    pub fn sample(&self, x: f64, y: f64) -> Option<Color32> {
        let [w, h] = self.size;
        if w == 0 || h == 0 || self.rgba.len() < w * h * 4 {
            return None;
        }
        let (cx, cy) = ((x.clamp(0.0, 1.0) * (w - 1) as f64) as i64, (y.clamp(0.0, 1.0) * (h - 1) as f64) as i64);
        let reach = (w.min(h) as i64 / 40).max(2);
        let (mut sum, mut n) = ([0u64; 3], 0u64);
        for dy in (-reach..=reach).step_by(2) {
            for dx in (-reach..=reach).step_by(2) {
                let (px, py) = ((cx + dx).clamp(0, w as i64 - 1) as usize, (cy + dy).clamp(0, h as i64 - 1) as usize);
                let i = (py * w + px) * 4;
                for c in 0..3 {
                    sum[c] += self.rgba[i + c] as u64;
                }
                n += 1;
            }
        }
        Some(Color32::from_rgb((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8))
    }

    /// Draw the newest frame into `rect`, turned to `rotation`. Returns false when there is none.
    pub fn paint(&self, painter: &egui::Painter, rect: Rect, rotation: lighttouch_model::rotation::Rotation) -> bool {
        self.shown_px.set(scaling::shown_px(rect, painter.ctx().pixels_per_point()));
        self.texture.paint(painter, rect, rotation, self.scan_turned())
    }

    /// Draw the screen into `rect` and route input to the guest when `interactive`. An asleep display is black,
    /// as on the device: the engine sends no frames then, so the last one would otherwise stay up.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, model: &mut Model, interactive: bool) -> egui::Response {
        if super::devshot::log_screen() {
            let ppp = ui.ctx().pixels_per_point();
            let px = [rect.left(), rect.top(), rect.width(), rect.height()].map(|v| (v * ppp).round() as i32);
            if self.logged != Some(px) {
                self.logged = Some(px);
                lighttouch_core::app_log!("guest screen {}x{} px at {},{}", px[2], px[3], px[0], px[1]);
            }
        }
        let response = ui.interact(rect, ui.scope_id().with("guest-screen"), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
        let asleep = model.session.as_ref().is_some_and(|s| s.display_asleep());
        if !asleep {
            self.paint(ui.painter(), rect, model.settings.rotation);
        }
        let mouse_look = interactive && model.gamepads.mouse_look();
        if !mouse_look {
            self.unlock(ui.ctx());
            self.released = None;
        }
        // The Escape that locks the mouse again isn't also the one that lets it go.
        let mut locked_now = false;
        if mouse_look && self.locked.is_none() {
            // A click on the screen locks the mouse; after Escape let it go, Escape locks it again instead (clicks
            // are touches meanwhile, for the game's menus).
            let (click, escape) = ui
                .input(|i| (i.pointer.button_pressed(egui::PointerButton::Primary), i.key_pressed(egui::Key::Escape)));
            let lock = if self.released.is_some() { escape } else { click && response.hovered() };
            if lock {
                self.released = None;
                self.lock(ui.ctx(), model);
                locked_now = true;
            } else if let Some(since) = self.released {
                hint(ui, rect, since, "The mouse is a finger · Esc to look with it again");
            }
        }
        let was_locked = self.locked.is_some();
        if was_locked {
            let free_input = model.gamepads.mouse_free();
            let free = free_input.as_ref().is_some_and(|f| held(ui, model, f));
            if free != self.free {
                self.set_free(free);
            }
            let free_label = free_input.map(|f| f.label(lighttouch_model::gamepad::sdl::PadKind::default()));
            self.locked_input(ui, rect, locked_now, free_label);
            if self.locked.is_some() && self.free {
                self.free_pointer(ui, rect, model);
            }
            self.keyboard_input(ui, model);
        }
        // The free pointer's finger lifts when it stops being one (Ctrl let go, Escape, another window).
        if !(self.locked.is_some() && self.free) {
            if let Some((x, y)) = self.free_down.take() {
                if let Some(session) = &mut model.session {
                    session.touch(vec![Contact { id: 1, phase: UP, x, y }]);
                }
            }
        }
        if was_locked {
            // Done above.
        } else if interactive {
            self.pointer_input(ui, rect, &response, model);
            self.keyboard_input(ui, model);
        } else {
            // Fingers only: the drawn device's Home and Sleep/Wake keep working (they wake an asleep screen), and
            // so do their keys.
            self.release_touches(model);
            if asleep {
                self.button_keys(ui, model);
            }
        }
        response
    }

    fn pointer_input(&mut self, ui: &Ui, rect: Rect, response: &egui::Response, model: &mut Model) {
        let rotation = model.settings.rotation;
        let turned = self.scan_turned();
        let Some(session) = &mut model.session else { return };
        // Where in the guest's scan a point lands, however the device is turned and however its panel scans.
        let normalized = |p: Pos2| {
            let u = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
            let v = ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0) as f64;
            let (x, y) = rotation.to_scan(u, v, turned);
            Contact { id: 0, phase: MOVE, x, y }
        };
        let mut batch: Vec<Contact> = Vec::new();
        let (events, pointer) = ui.input(|i| (i.events.clone(), i.pointer.clone()));

        // Touchscreens: each finger is its own contact (ids 3..8); the mouse is ignored while fingers are down.
        for event in &events {
            if let egui::Event::Touch { id, phase, pos, .. } = event {
                let key = id.0;
                match phase {
                    egui::TouchPhase::Start if rect.contains(*pos) && self.touches.len() < 6 => {
                        let contact = (3..=8).find(|c| !self.touches.values().any(|v| v == c)).unwrap_or(8);
                        self.touches.insert(key, contact);
                        batch.push(Contact { id: contact, phase: DOWN, ..normalized(*pos) });
                    }
                    egui::TouchPhase::Move => {
                        if let Some(&contact) = self.touches.get(&key) {
                            batch.push(Contact { id: contact, phase: MOVE, ..normalized(*pos) });
                        }
                    }
                    egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                        if let Some(contact) = self.touches.remove(&key) {
                            let phase = if *phase == egui::TouchPhase::End { UP } else { CANCEL };
                            batch.push(Contact { id: contact, phase, ..normalized(*pos) });
                        }
                    }
                    _ => {}
                }
            }
        }

        let multitouch = session.board.as_ref().is_some_and(|b| b.has(BoardInfo::MULTITOUCH));
        let modifiers = ui.input(|i| i.modifiers);
        if self.touches.is_empty() {
            let position = pointer.interact_pos().or(pointer.latest_pos());
            let mouse_from = batch.len();
            for (button, contact) in [(egui::PointerButton::Primary, 1u32), (egui::PointerButton::Secondary, 2u32)] {
                if contact == 2 && self.pair.is_some() {
                    continue;
                }
                let down = pointer.button_down(button);
                let held = self.mouse.get(&contact).copied();
                match (held, down, position) {
                    (None, true, Some(p))
                        if pointer.button_pressed(button) && response.hovered() && rect.contains(p) =>
                    {
                        self.mouse.insert(contact, p);
                        batch.push(Contact { id: contact, phase: DOWN, ..normalized(p) });
                    }
                    (Some(last), true, Some(p)) if p != last => {
                        self.mouse.insert(contact, p);
                        batch.push(Contact { id: contact, phase: MOVE, ..normalized(p) });
                    }
                    (Some(last), false, _) => {
                        self.mouse.remove(&contact);
                        batch.push(Contact { id: contact, phase: UP, ..normalized(position.unwrap_or(last)) });
                    }
                    _ => {}
                }
            }
            // The Alt-drag's second finger mirrors every step of the first.
            let mut twins = Vec::new();
            for c in &batch[mouse_from..] {
                if c.id != 1 {
                    continue;
                }
                if c.phase == DOWN && modifiers.alt && multitouch && !self.mouse.contains_key(&2) {
                    self.pair = Some(Pair { start: (c.x, c.y), parallel: modifiers.shift });
                }
                if let Some(pair) = self.pair {
                    let (x, y) = pair.partner(c.x, c.y);
                    twins.push(Contact { id: 2, phase: c.phase, x, y });
                    if c.phase == UP || c.phase == CANCEL {
                        self.pair = None;
                    }
                }
            }
            batch.extend(twins);
            // Show where the two fingers are (or would land) while Alt is held over the screen.
            let hover = position.filter(|p| rect.contains(*p) && (response.hovered() || self.pair.is_some()));
            if multitouch && (modifiers.alt || self.pair.is_some()) {
                if let Some(p) = hover {
                    let n = normalized(p);
                    let pair = self.pair.unwrap_or(Pair { start: (n.x, n.y), parallel: modifiers.shift });
                    let (x, y) = pair.partner(n.x, n.y);
                    let (u, v) = rotation.from_scan(x, y, turned);
                    let other = rect.min + egui::vec2(u as f32 * rect.width(), v as f32 * rect.height());
                    for at in [p, other] {
                        let (u, v) = (
                            ((at.x - rect.left()) / rect.width()) as f64,
                            ((at.y - rect.top()) / rect.height()) as f64,
                        );
                        let (sx, sy) = rotation.to_scan(u, v, turned);
                        let under = self.sample(sx, sy).unwrap_or(Color32::GRAY);
                        bubble(&ui.painter().with_clip_rect(rect), at, 11.0, under, self.pair.is_some());
                    }
                    ui.ctx().request_repaint();
                }
            }
        }
        if !batch.is_empty() {
            session.touch(batch);
        }
        // Losing focus releases everything (only on the change: some setups never report focus at all).
        let focused = ui.input(|i| i.focused);
        if self.focused && !focused {
            self.release_all(model);
        }
        self.focused = focused;
    }

    /// Escape switches the mouse between looking and touching (it's locked, or Escape let it go).
    pub fn mouse_toggles(&self) -> bool {
        self.locked.is_some() || self.released.is_some()
    }

    /// The locked mouse's movement since this was last asked, and its buttons held (as key names: MouseLeft...).
    pub fn take_mouse(&mut self) -> ((f32, f32), Vec<&'static str>) {
        (std::mem::take(&mut self.mouse_moved), self.mouse_buttons.clone())
    }

    /// Hide the cursor and keep it on the game: locked where the system can (macOS, Wayland), else confined to the
    /// window (Windows, X11) and put back over the screen each frame. The look follows the mouse's raw movement,
    /// which neither stops at the window's edge nor sees the cursor put back.
    fn lock(&mut self, ctx: &egui::Context, model: &mut Model) {
        self.release_touches(model);
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Confined));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Locked));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
        (self.locked, self.mouse_moved, self.armed) = (Some(Instant::now()), (0.0, 0.0), false);
        self.free_at = (0.5, 0.5);
        self.lock_focused = ctx.input(|i| i.focused);
        self.mouse_buttons.clear();
    }

    /// Held free: the mouse moves our pointer instead of looking (the cursor stays hidden and locked); let go: it
    /// looks again (a click still held doesn't fire).
    fn set_free(&mut self, free: bool) {
        self.free = free;
        (self.mouse_moved, self.armed) = ((0.0, 0.0), false);
        self.mouse_buttons.clear();
    }

    /// The free pointer: its left button is finger 1 where it is, drawn as a bubble (smaller and fuller while down).
    fn free_pointer(&mut self, ui: &Ui, rect: Rect, model: &mut Model) {
        let (u, v) = self.free_at;
        let at = rect.min + egui::vec2(u * rect.width(), v * rect.height());
        let (x, y) = model.settings.rotation.to_scan(u as f64, v as f64, self.scan_turned());
        let (down, pressed) = ui.input(|i| {
            (
                i.pointer.button_down(egui::PointerButton::Primary),
                i.pointer.button_pressed(egui::PointerButton::Primary),
            )
        });
        let mut batch = Vec::new();
        match (self.free_down, down) {
            (None, true) if pressed => {
                batch.push(Contact { id: 1, phase: DOWN, x, y });
                self.free_down = Some((x, y));
            }
            (Some(last), true) if last != (x, y) => {
                batch.push(Contact { id: 1, phase: MOVE, x, y });
                self.free_down = Some((x, y));
            }
            (Some(_), false) => {
                batch.push(Contact { id: 1, phase: UP, x, y });
                self.free_down = None;
            }
            _ => {}
        }
        if !batch.is_empty() {
            if let Some(session) = &mut model.session {
                session.touch(batch);
            }
        }
        let under = self.sample(x, y).unwrap_or(Color32::GRAY);
        bubble(&ui.painter().with_clip_rect(rect), at, 11.0, under, self.free_down.is_some());
        ui.ctx().request_repaint();
    }

    fn unlock(&mut self, ctx: &egui::Context) {
        self.free = false;
        if self.locked.take().is_none() {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
        self.mouse_moved = (0.0, 0.0);
        self.mouse_buttons.clear();
    }

    /// While locked: the mouse's movement and buttons for the preset, Escape or another window lets go.
    fn locked_input(&mut self, ui: &Ui, rect: Rect, locked_now: bool, free_label: Option<String>) {
        let ctx = ui.ctx();
        let (escape, focused, events, pointer) =
            ui.input(|i| (i.key_pressed(egui::Key::Escape), i.focused, i.events.clone(), i.pointer.clone()));
        let escape = escape && !locked_now;
        // Escape lets go and leaves the mouse a finger until Escape again; losing the focus lets go too (a click
        // locks it again).
        if escape {
            self.released = Some(Instant::now());
        }
        let lost = self.lock_focused && !focused;
        self.lock_focused = focused;
        if escape || lost {
            self.unlock(ctx);
            return;
        }
        let per_point = ctx.pixels_per_point().max(0.1);
        for e in &events {
            if let egui::Event::MouseMoved(d) = e {
                if self.free {
                    // Our pointer, as far as the screen's edges: each axis stops on its own, so sliding along an
                    // edge keeps moving along it.
                    // A hair inside the edge, as a touch on the last pixel's far edge is off the panel.
                    const EDGE: f32 = 0.002;
                    let (u, v) = self.free_at;
                    self.free_at = (
                        (u + d.x / per_point / rect.width()).clamp(EDGE, 1.0 - EDGE),
                        (v + d.y / per_point / rect.height()).clamp(EDGE, 1.0 - EDGE),
                    );
                } else {
                    self.mouse_moved.0 += d.x;
                    self.mouse_moved.1 += d.y;
                }
            }
        }
        let buttons = [
            (egui::PointerButton::Primary, MOUSE_LEFT),
            (egui::PointerButton::Secondary, MOUSE_RIGHT),
            (egui::PointerButton::Middle, MOUSE_MIDDLE),
        ];
        let down: Vec<&'static str> =
            buttons.iter().filter(|(b, _)| pointer.button_down(*b)).map(|(_, n)| *n).collect();
        // A click pressed and let go since the last frame counts as held for this one, or it would never be seen
        // (the click that locked it, this frame, aside).
        let quick: Vec<&'static str> = buttons
            .iter()
            .filter(|(b, _)| self.armed && !locked_now && pointer.button_pressed(*b) && !pointer.button_down(*b))
            .map(|(_, n)| *n)
            .collect();
        // The click that locked it isn't a press: buttons count once it's let go. Freed, they're the pointer's.
        self.armed |= down.is_empty();
        self.mouse_buttons =
            if self.armed && !self.free { down.into_iter().chain(quick).collect() } else { Vec::new() };
        // Confined, the hidden cursor still moves: back over the screen before it reaches anything else.
        if pointer.latest_pos().is_some_and(|p| p.distance(rect.center()) > rect.width().min(rect.height()) * 0.25) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorPosition(rect.center()));
        }
        if let Some(since) = self.locked {
            let text = match free_label {
                Some(key) => format!("The mouse looks around · hold {key} to tap and swipe · Esc lets go"),
                None => "The mouse looks around · Esc to use it as a finger".to_string(),
            };
            hint(ui, rect, since, &text);
        }
    }

    fn keyboard_input(&mut self, ui: &Ui, model: &mut Model) {
        let typing = ui.ctx().egui_wants_keyboard_input();
        // Keys a controller preset binds act only through it.
        let bound = super::controller::bound_keys(model);
        let key = |key: egui::Key| {
            !typing && !bound.contains(key.name()) && ui.input(|i| i.key_down(key) && i.modifiers.is_none())
        };
        // Tilt turned off while leaning still settles back to rest: the engine holds the last sample it was sent.
        // A binding moving the device, or holding it where the controller's sensors left it, comes first.
        let settling = model.session.as_ref().is_some_and(|s| s.tilting());
        if (model.settings.tilt || settling) && !model.gamepads.holds_motion() {
            let keys = if model.settings.tilt {
                TiltKeys {
                    left: key(egui::Key::ArrowLeft),
                    right: key(egui::Key::ArrowRight),
                    up: key(egui::Key::ArrowUp),
                    down: key(egui::Key::ArrowDown),
                }
            } else {
                TiltKeys::default()
            };
            let rotation = model.settings.rotation;
            if let Some(session) = &mut model.session {
                session.tilt(keys, rotation);
                // Leaning and settling back need steps between guest frames too.
                if keys != TiltKeys::default() || session.tilting() {
                    ui.ctx().request_repaint();
                }
            }
        }
        self.button_keys(ui, model);
    }

    /// H, P, + and -: the hardware buttons, held while the key is.
    fn button_keys(&mut self, ui: &Ui, model: &mut Model) {
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let bound = super::controller::bound_keys(model);
        let key =
            |key: egui::Key| !bound.contains(key.name()) && ui.input(|i| i.key_down(key) && i.modifiers.is_none());
        // Pressed and let go between two frames: still a press.
        let tapped =
            |key: egui::Key| !bound.contains(key.name()) && ui.input(|i| i.key_pressed(key) && i.modifiers.is_none());
        let keys: [(&[egui::Key], u32); 4] = [
            (&[egui::Key::H], device_frame::HOME),
            (&[egui::Key::P], device_frame::POWER),
            (&[egui::Key::Plus, egui::Key::Equals], device_frame::VOLUME_UP),
            (&[egui::Key::Minus], device_frame::VOLUME_DOWN),
        ];
        for (ks, button) in keys {
            let (down, tap) = (ks.iter().any(|k| key(*k)), ks.iter().any(|k| tapped(*k)));
            self.follow_button(model, ButtonControl::Keys, button, down, tap);
        }
    }

    /// Follow one control of a hardware button: `down` while it holds the button, and `tapped` when it was pressed
    /// and let go since the last frame, which is then a whole press (the engine holds it long enough for iOS to see,
    /// and queues quick double and triple clicks).
    pub fn follow_button(&mut self, model: &mut Model, control: ButtonControl, button: u32, down: bool, tapped: bool) {
        let mine = |s: &Self| s.held_by.contains(&(button, control));
        if tapped && !down && !mine(self) {
            self.set_button(model, control, button, true);
        }
        if down != mine(self) {
            self.set_button(model, control, button, down);
        }
    }

    /// One control presses or lets go of a hardware button; the guest sees it go down when the first control holds
    /// it and up when the last lets go.
    pub fn set_button(&mut self, model: &mut Model, control: ButtonControl, button: u32, down: bool) {
        self.held_by.retain(|h| *h != (button, control));
        if down {
            self.held_by.push((button, control));
        }
        let any = self.held_by.iter().any(|(b, _)| *b == button);
        if any == self.held.contains(&button) {
            return;
        }
        if any {
            self.held.push(button);
        } else {
            self.held.retain(|b| *b != button);
        }
        if let Some(session) = &mut model.session {
            session.button(button, any);
        }
    }

    pub fn is_held(&self, button: u32) -> bool {
        self.held.contains(&button)
    }

    /// Lift every finger, leaving hardware buttons as they are.
    fn release_touches(&mut self, model: &mut Model) {
        let mut ids: Vec<u32> = self.mouse.keys().chain(self.touches.values()).copied().collect();
        if self.pair.take().is_some() {
            ids.push(2);
        }
        if ids.is_empty() {
            return;
        }
        self.mouse.clear();
        self.touches.clear();
        ids.sort_unstable();
        ids.dedup();
        if let Some(session) = &mut model.session {
            session.touch(ids.into_iter().map(|id| Contact { id, phase: CANCEL, x: 0.5, y: 0.5 }).collect());
        }
    }

    /// Release every contact and button (focus loss, the screen going away).
    pub fn release_all(&mut self, model: &mut Model) {
        if self.mouse.is_empty() && self.touches.is_empty() && self.held.is_empty() {
            return;
        }
        self.mouse.clear();
        self.touches.clear();
        self.pair = None;
        self.held.clear();
        self.held_by.clear();
        if let Some(session) = &mut model.session {
            session.cancel_input();
        }
    }
}

/// A finger's place drawn over the guest (the Alt-drag pinch, a controller's pointer): a ring and a light fill in the
/// inverse of the colour under it, so it shows on any game, with a thin dark and light edge for flat mid-greys.
/// `pressed` draws it smaller and fuller, as a finger down.
pub fn bubble(painter: &egui::Painter, at: Pos2, radius: f32, under: Color32, pressed: bool) {
    let inverse = Color32::from_rgb(255 - under.r(), 255 - under.g(), 255 - under.b());
    let r = if pressed { radius * 0.82 } else { radius };
    painter.circle_filled(at, r, inverse.gamma_multiply(if pressed { 0.55 } else { 0.28 }));
    painter.circle_stroke(at, r + 1.5, egui::Stroke::new(1.0, Color32::from_black_alpha(140)));
    painter.circle_stroke(at, r, egui::Stroke::new(2.0, inverse));
    painter.circle_stroke(at, r - 1.5, egui::Stroke::new(1.0, Color32::from_white_alpha(110)));
    painter.circle_filled(at, 2.0, inverse);
}

#[cfg(test)]
mod tests {
    use super::Pair;

    #[test]
    fn second_finger_mirrors_or_follows() {
        let pinch = Pair { start: (0.6, 0.7), parallel: false };
        let (x, y) = pinch.partner(0.8, 0.9);
        assert!((x - 0.2).abs() < 1e-9 && (y - 0.1).abs() < 1e-9);
        let pan = Pair { start: (0.6, 0.7), parallel: true };
        let (x, y) = pan.partner(0.7, 0.6);
        assert!((x - 0.5).abs() < 1e-9 && (y - 0.2).abs() < 1e-9);
        assert_eq!(pan.partner(1.0, 0.0), (0.8, 0.0));
    }
}

//! The guest screen: the newest frame as a texture, and input. Left mouse is finger 1, right mouse finger 2, a
//! touchscreen's fingers map to their own contacts. As on Android, a finger dragged past the screen's edge holds at
//! the nearest edge rather than lifting. Alt-dragging adds a second finger opposite the first through the screen's centre, to pinch
//! and rotate; Alt+Shift moves the two together (as in Xcode's Simulator). Keys: H Home, P Sleep/Wake, +/- volume
//! (held while the key is held).
use super::device_frame;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, TextureHandle, TextureOptions, Ui};
use lighttouch_core::runtime::{BoardInfo, Contact};
use lighttouch_model::Model;
use lighttouch_model::settings::Scaling;
use lighttouch_model::tilt::TiltKeys;
use std::collections::HashMap;

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
    texture: Option<TextureHandle>,
    rgba: Vec<u8>,
    size: [usize; 2],
    scaling: Scaling,
    record: Option<String>,
    /// Mouse buttons holding a contact: (contact id, last position).
    mouse: HashMap<u32, Pos2>,
    /// Touchscreen finger id → contact id.
    touches: HashMap<u64, u32>,
    /// The second finger following the left button while Alt-dragging.
    pair: Option<Pair>,
    /// Hardware buttons held by the pointer on the drawn device or by keys.
    held: Vec<u32>,
    focused: bool,
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
            self.texture = None;
        }
        let Some(meta) = session.read_frame(&mut self.rgba) else {
            if scaling != self.scaling {
                self.scaling = scaling;
                if let Some(texture) = &mut self.texture {
                    let image = egui::ColorImage::from_rgba_unmultiplied(self.size, &self.rgba);
                    texture.set(image, options(scaling));
                }
            }
            return false;
        };
        self.size = [meta.width as usize, meta.height as usize];
        self.scaling = scaling;
        let image = egui::ColorImage::from_rgba_unmultiplied(self.size, &self.rgba);
        match &mut self.texture {
            Some(texture) => texture.set(image, options(scaling)),
            None => self.texture = Some(ctx.load_texture("guest", image, options(scaling))),
        }
        true
    }

    /// Forget the picture (the device stopped).
    pub fn clear(&mut self) {
        self.texture = None;
        self.record = None;
    }

    pub fn has_frame(&self) -> bool {
        self.texture.is_some()
    }

    /// The guest laid its portrait UI a quarter clockwise into a landscape scan (the iPad's panel), so the scan
    /// stands a quarter-turn from upright: the frame is wider than tall while the cutout it fills is portrait.
    fn scan_turned(&self) -> bool {
        self.size[0] > self.size[1]
    }

    /// Draw the screen into `rect` and route input to the guest when `interactive`. An asleep display is black,
    /// as on the device: the engine sends no frames then, so the last one would otherwise stay up.
    pub fn show(&mut self, ui: &mut Ui, rect: Rect, model: &mut Model, interactive: bool) -> egui::Response {
        let response = ui.interact(rect, ui.scope_id().with("guest-screen"), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
        let asleep = model.session.as_ref().is_some_and(|s| s.display_asleep());
        if let Some(texture) = self.texture.as_ref().filter(|_| !asleep) {
            // Each corner shows the guest pixel the rotation puts there.
            let rotation = model.settings.rotation;
            let turned = self.scan_turned();
            let mut mesh = egui::Mesh::with_texture(texture.id());
            for corner in [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()] {
                let u = ((corner.x - rect.left()) / rect.width()) as f64;
                let v = ((corner.y - rect.top()) / rect.height()) as f64;
                let (x, y) = rotation.to_scan(u, v, turned);
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: corner,
                    uv: egui::pos2(x as f32, y as f32),
                    color: Color32::WHITE,
                });
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            ui.painter().add(egui::Shape::mesh(mesh));
        }
        if interactive {
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
                    let stroke = egui::Stroke::new(1.5, Color32::from_white_alpha(200));
                    for at in [p, other] {
                        ui.painter().circle(at, 11.0, Color32::from_white_alpha(50), stroke);
                    }
                    ui.ctx().request_repaint();
                }
            }
        }
        if !batch.is_empty() {
            batch.truncate(8);
            session.touch(batch);
        }
        // Losing focus releases everything (only on the change: some setups never report focus at all).
        let focused = ui.input(|i| i.focused);
        if self.focused && !focused {
            self.release_all(model);
        }
        self.focused = focused;
    }

    fn keyboard_input(&mut self, ui: &Ui, model: &mut Model) {
        let typing = ui.ctx().egui_wants_keyboard_input();
        let key = |key| !typing && ui.input(|i| i.key_down(key) && i.modifiers.is_none());
        if model.settings.tilt {
            let keys = TiltKeys {
                left: key(egui::Key::ArrowLeft),
                right: key(egui::Key::ArrowRight),
                up: key(egui::Key::ArrowUp),
                down: key(egui::Key::ArrowDown),
            };
            let keys = model.settings.rotation.keys(keys);
            if let Some(session) = &mut model.session {
                session.tilt(keys);
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
        let key = |key| ui.input(|i| i.key_down(key) && i.modifiers.is_none());
        let keys = [
            (egui::Key::H, device_frame::HOME),
            (egui::Key::P, device_frame::POWER),
            (egui::Key::Plus, device_frame::VOLUME_UP),
            (egui::Key::Equals, device_frame::VOLUME_UP),
            (egui::Key::Minus, device_frame::VOLUME_DOWN),
        ];
        for (k, button) in keys {
            let down = key(k);
            let held = self.held.contains(&button);
            if down != held {
                self.set_button(model, button, down);
            }
        }
    }

    /// Press or release a hardware button (drawn device, fullscreen bar or keys).
    pub fn set_button(&mut self, model: &mut Model, button: u32, down: bool) {
        let held = self.held.contains(&button);
        if down == held {
            return;
        }
        if down {
            self.held.push(button);
        } else {
            self.held.retain(|b| *b != button);
        }
        if let Some(session) = &mut model.session {
            session.button(button, down);
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
        if let Some(session) = &mut model.session {
            session.cancel_input();
        }
    }
}

fn options(scaling: Scaling) -> TextureOptions {
    match scaling {
        Scaling::Smooth => TextureOptions::LINEAR,
        Scaling::Sharp => TextureOptions::NEAREST,
    }
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

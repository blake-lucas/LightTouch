//! Turns what the controllers and keys are doing into touches, device buttons and motion, one step per UI frame.
//!
//! Geometry happens on the screen as shown (`u` right, `v` down, 0..1), where a stick's up is the screen's up and a
//! radius is round, then goes to the guest: places are kept in its portrait coordinates and touches go out in its
//! panel's scan ([`Rotation::to_scan`]). Each binding owns one contact id, so the engine sees every finger the bindings
//! hold as its own; the engine takes five fingers at once, so a sixth waits.
use super::preset::{Action, Binding, Input, Point, PointerStyle, Preset, Press, Stick, TapMode};
use super::sdl::Pad;
use crate::rotation::Rotation;
use crate::tilt::Tilt;
use lighttouch_core::runtime::Contact;
use std::collections::HashSet;
use std::time::{Duration, Instant};

const DOWN: u32 = 0;
const MOVE: u32 = 1;
const UP: u32 = 2;
/// Contact ids 20 and up: the mouse uses 1-2 and a touchscreen 3-8.
const FIRST_CONTACT: u32 = 20;
const MAX_FINGERS: usize = 5;
const REPEAT: Duration = Duration::from_millis(70);
const FLICK_MS: u64 = 140;
/// A stick pushed this far puts its finger down at once; less, once it has stayed pushed for `ENGAGE` (a nudge or a
/// drifting stick then doesn't tap the screen).
const ENGAGE_AT: f32 = 0.5;
const ENGAGE: Duration = Duration::from_millis(50);
/// Looking: how long the finger rests where it is once the stick lets go, before lifting (lifting mid-drag could
/// fling the camera), and how long it stays up before landing again at an edge.
const LOOK_REST: Duration = Duration::from_millis(300);
const REGRIP: Duration = Duration::from_millis(20);
/// A look binding's tap: how long the finger stays down.
const LOOK_TAP: Duration = Duration::from_millis(60);
const GRAVITY: f64 = 9.80665;

/// How the screen is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub rotation: Rotation,
    /// The guest lays its portrait UI into a landscape scan (the iPad).
    pub turned: bool,
    /// The shown screen's width over its height.
    pub aspect: f32,
}

impl View {
    /// A guest point on the screen as shown.
    pub fn shown(&self, p: Point) -> (f32, f32) {
        let (u, v) = self.rotation.from_guest(p.x as f64, p.y as f64);
        (u as f32, v as f32)
    }

    /// A point on the screen as shown, in the guest's portrait coordinates.
    pub fn guest(&self, u: f32, v: f32) -> Point {
        let (x, y) = self.rotation.to_guest(u.clamp(0.0, 1.0) as f64, v.clamp(0.0, 1.0) as f64);
        Point::new(x as f32, y as f32)
    }

    /// A length in fractions of the shorter side, as (width, height) fractions of the screen.
    pub fn span(&self, length: f32) -> (f32, f32) {
        let a = self.aspect.max(0.01);
        (length * (1.0 / a).min(1.0), length * a.min(1.0))
    }

    fn contact(&self, id: u32, phase: u32, u: f32, v: f32) -> Contact {
        let (x, y) = self.rotation.to_scan(u.clamp(0.0, 1.0) as f64, v.clamp(0.0, 1.0) as f64, self.turned);
        Contact { id, phase, x, y }
    }
}

/// Every controller merged: any of them can press a button, the stick pushed furthest wins, and motion comes from the
/// first one with sensors.
#[derive(Debug, Clone, Default)]
pub struct Merged {
    pub buttons: u32,
    pub axes: [f32; 6],
    pub gyro: Option<[f32; 3]>,
    pub accel: Option<[f32; 3]>,
}

impl Merged {
    pub fn from(pads: &[Pad]) -> Merged {
        let mut m = Merged::default();
        for p in pads {
            m.buttons |= p.buttons;
            for (a, &b) in m.axes.iter_mut().zip(&p.axes) {
                if b.abs() > a.abs() {
                    *a = b;
                }
            }
            // The first controller with an accelerometer moves the device (with its gyroscope, if it has one).
            if m.accel.is_none() && p.accel.is_some() {
                m.gyro = p.gyro;
                m.accel = p.accel;
            }
        }
        m
    }
}

/// What the inputs are this frame.
pub struct Inputs<'a> {
    pub pad: &'a Merged,
    /// Keys held (egui names), only while the device's screen has the keyboard.
    pub keys: &'a HashSet<String>,
}

impl Inputs<'_> {
    pub fn held(&self, input: &Input) -> bool {
        match input {
            Input::Pad(b) => self.pad.buttons & b.bit() != 0,
            Input::Key(k) => self.keys.contains(k),
        }
    }

    /// A stick's direction on the screen as shown, length up to 1, with `deadzone` taken off and the rest rescaled.
    pub fn stick(&self, stick: &Stick, deadzone: f32) -> (f32, f32) {
        let key = |k: &String| if self.keys.contains(k) { 1.0 } else { 0.0 };
        let bit = |b: super::preset::PadButton| if self.pad.buttons & b.bit() != 0 { 1.0 } else { 0.0 };
        use super::preset::PadButton::*;
        let (x, y, analog) = match stick {
            Stick::Left => (self.pad.axes[0], self.pad.axes[1], true),
            Stick::Right => (self.pad.axes[2], self.pad.axes[3], true),
            Stick::Dpad => (bit(DpadRight) - bit(DpadLeft), bit(DpadDown) - bit(DpadUp), false),
            Stick::Keys { up, down, left, right } => (key(right) - key(left), key(down) - key(up), false),
        };
        let m = (x * x + y * y).sqrt();
        if m < 1e-6 {
            return (0.0, 0.0);
        }
        if !analog {
            return (x / m, y / m);
        }
        let dz = deadzone.clamp(0.0, 0.9);
        if m <= dz {
            return (0.0, 0.0);
        }
        let scaled = ((m - dz) / (1.0 - dz)).min(1.0);
        (x / m * scaled, y / m * scaled)
    }
}

#[derive(Debug, Clone, Default)]
struct State {
    /// The binding's finger is down, at (u, v) on the screen as shown.
    finger: Option<(f32, f32)>,
    pressed: bool,
    toggled: bool,
    /// Repeat taps: when the finger last went down or up.
    repeat_at: Option<Instant>,
    /// A swipe or flick under way: from, to, start, length.
    stroke: Option<((f32, f32), (f32, f32), Instant, Duration)>,
    /// A pointer's place on the screen as shown.
    pointer: Option<(f32, f32)>,
    /// A flick's stick was back near the middle (armed for the next flick).
    flick_armed: bool,
    /// A stick binding's press input was held last step (for toggling).
    press_held: bool,
    /// Since when the stick has been pushed (for engaging).
    pushed_since: Option<Instant>,
    /// Looking: when the stick let go with the finger down, and when the finger may land again after an edge.
    rest_since: Option<Instant>,
    regrip_at: Option<Instant>,
}

impl State {
    /// Whether a stick binding's finger belongs on the screen, from its press setting; `m` is the stick's push.
    fn pressing(&mut self, press: &Press, inputs: &Inputs, enabled: bool, m: f32, now: Instant) -> bool {
        self.pushed_since = if m > 0.0 { self.pushed_since.or(Some(now)) } else { None };
        match press {
            Press::Pushed => {
                let pushed_for = self.pushed_since.map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
                m > 0.0 && (self.finger.is_some() || m >= ENGAGE_AT || pushed_for >= ENGAGE)
            }
            Press::Hold { input } => enabled && inputs.held(input),
            Press::Toggle { input } => {
                let held = enabled && inputs.held(input);
                if held && !self.press_held {
                    self.toggled = !self.toggled;
                }
                self.press_held = held;
                self.toggled && enabled
            }
        }
    }
}

/// What a step asks of the device.
#[derive(Debug, Default)]
pub struct Output {
    pub contacts: Vec<Contact>,
    pub buttons: Vec<(u32, bool)>,
    /// Acceleration and, where the board has one, the gyroscope's turn.
    pub motion: Option<([f64; 3], [f64; 3])>,
    /// Something moves between frames (a swipe, a repeat, a leaning device): step again soon.
    pub busy: bool,
}

#[derive(Default)]
pub struct Mapper {
    states: Vec<State>,
    tilt: Tilt,
    tilting: bool,
    /// The controller's lean when the player set "level" (roll, pitch, radians).
    pub level: (f64, f64),
    /// The last motion sample's lean, for setting level.
    pub lean: Option<(f64, f64)>,
    /// The controller's sensors moved the device last step (it goes back to level when they stop).
    sensing: bool,
    /// Until when level goes on being sent after the sensors stop (one sample could fall between the session's).
    leveling: Option<Instant>,
    last: Option<Instant>,
}

impl Mapper {
    /// Where pointer `index` is, and whether it is touching, for drawing its bubble.
    pub fn pointer(&self, index: usize) -> Option<((f32, f32), bool)> {
        let s = self.states.get(index)?;
        Some((s.pointer?, s.finger.is_some()))
    }

    /// Where binding `index`'s finger is on the screen as shown, while it is down.
    pub fn finger(&self, index: usize) -> Option<(f32, f32)> {
        self.states.get(index)?.finger
    }

    /// Whether binding `index` is doing something now (for lighting its spot).
    pub fn active(&self, index: usize) -> bool {
        self.states.get(index).is_some_and(|s| s.finger.is_some() || s.pressed || s.stroke.is_some())
    }

    /// Lift every finger and release every button the preset holds (it changed, or input stopped).
    pub fn release(&mut self, preset: Option<&Preset>, view: &View) -> Output {
        let mut out = Output::default();
        for (i, s) in self.states.iter_mut().enumerate() {
            if let Some((u, v)) = s.finger.take() {
                out.contacts.push(view.contact(FIRST_CONTACT + i as u32, UP, u, v));
            }
            if s.pressed {
                if let Some(Action::Device { button, .. }) = preset.and_then(|p| p.bindings.get(i)).map(|b| &b.action) {
                    out.buttons.push((button.number(), false));
                }
            }
        }
        self.states.clear();
        if self.tilting || self.sensing {
            self.tilting = false;
            self.sensing = false;
            self.tilt = Tilt::default();
            out.motion = Some(([0.0, 0.0, GRAVITY], [0.0; 3]));
        }
        out
    }

    pub fn step(&mut self, preset: &Preset, inputs: &Inputs, view: &View, now: Instant) -> Output {
        let dt = self.last.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32().min(0.1));
        self.last = Some(now);
        if self.states.len() != preset.bindings.len() {
            self.states.resize(preset.bindings.len(), State::default());
        }
        let mut out = Output::default();
        let mut fingers = self.states.iter().filter(|s| s.finger.is_some()).count();
        let mut tilt: Option<(f64, f64)> = None;
        for (i, binding) in preset.bindings.iter().enumerate() {
            let id = FIRST_CONTACT + i as u32;
            let state = &mut self.states[i];
            let enabled = binding.with.as_ref().is_none_or(|w| inputs.held(w));
            step_binding(
                binding,
                state,
                inputs,
                view,
                preset.deadzone,
                enabled,
                now,
                dt,
                id,
                &mut fingers,
                &mut out,
                &mut tilt,
            );
        }

        // Motion: the controller's sensors first (while its hold button is, if it has one), then a stick's tilt,
        // settling back to level when it lets go.
        let motion = preset.bindings.iter().find_map(|b| match b.action {
            Action::Motion { sensitivity } if b.with.as_ref().is_none_or(|w| inputs.held(w)) => Some(sensitivity),
            _ => None,
        });
        let was_sensing = std::mem::replace(&mut self.sensing, false);
        if let (Some(sensitivity), Some(accel)) = (motion, inputs.pad.accel) {
            out.motion = Some(self.motion_from_sensors(accel, inputs.pad.gyro, sensitivity as f64));
            self.sensing = true;
            self.leveling = None;
        } else if tilt.is_some() || self.tilting {
            let (roll, pitch) = tilt.unwrap_or((0.0, 0.0));
            let sample = self.tilt.step_toward(roll, pitch, now);
            self.tilting = !self.tilt.at_rest() || tilt.is_some_and(|t| t != (0.0, 0.0));
            out.motion = Some(sample);
            out.busy |= self.tilting;
        } else if was_sensing || self.leveling.is_some_and(|t| now < t) {
            self.leveling = self.leveling.filter(|_| !was_sensing).or(Some(now + Duration::from_millis(100)));
            out.motion = Some(([0.0, 0.0, GRAVITY], [0.0; 3]));
            out.busy = true;
        }
        out.busy |= self
            .states
            .iter()
            .any(|s| s.stroke.is_some() || s.repeat_at.is_some() || s.rest_since.is_some() || s.regrip_at.is_some());
        out
    }

    /// The device's motion from the controller's: as if the controller were the device lying face up on a table with
    /// its top edge away from the player, so tipping the controller tips the device. Its axes (x right, y up, z
    /// toward the player) become the device's (x right, y toward the top edge, z out of the screen).
    fn motion_from_sensors(
        &mut self,
        accel: [f32; 3],
        gyro: Option<[f32; 3]>,
        sensitivity: f64,
    ) -> ([f64; 3], [f64; 3]) {
        let g = gyro.map(|g| [g[0] as f64, -g[2] as f64, g[1] as f64]).unwrap_or([0.0; 3]);
        let (roll, pitch) = lean_of(accel);
        self.lean = Some((roll, pitch));
        let (r, p) = ((roll - self.level.0) * sensitivity, (pitch - self.level.1) * sensitivity);
        let acceleration = [-GRAVITY * r.sin(), GRAVITY * r.cos() * p.sin(), GRAVITY * r.cos() * p.cos()];
        (acceleration, g.map(|w| w * sensitivity))
    }
}

/// A controller's lean from its accelerometer (SDL's axes: x right, y up, z toward the player), as the device's
/// (roll, pitch) in radians: level when it lies flat or is held flat; positive roll lowers its right side, positive
/// pitch raises its far edge.
pub fn lean_of(accel: [f32; 3]) -> (f64, f64) {
    let a = [accel[0] as f64, -accel[2] as f64, accel[1] as f64];
    let norm = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-3);
    let roll = (-a[0] / norm).clamp(-1.0, 1.0).asin();
    let pitch = a[1].atan2(a[2]);
    (roll, pitch)
}

#[allow(clippy::too_many_arguments)]
fn step_binding(
    binding: &Binding,
    s: &mut State,
    inputs: &Inputs,
    view: &View,
    deadzone: f32,
    enabled: bool,
    now: Instant,
    dt: f32,
    id: u32,
    fingers: &mut usize,
    out: &mut Output,
    tilt: &mut Option<(f64, f64)>,
) {
    let mut put = |s: &mut State, at: Option<(f32, f32)>, out: &mut Output| match (s.finger, at) {
        (None, Some((u, v))) if *fingers < MAX_FINGERS => {
            *fingers += 1;
            s.finger = Some((u, v));
            out.contacts.push(view.contact(id, DOWN, u, v));
        }
        (Some(was), Some(now_at)) if was != now_at => {
            s.finger = Some(now_at);
            out.contacts.push(view.contact(id, MOVE, now_at.0, now_at.1));
        }
        (Some((u, v)), None) => {
            *fingers -= 1;
            s.finger = None;
            out.contacts.push(view.contact(id, UP, u, v));
        }
        _ => {}
    };
    match &binding.action {
        Action::Tap { input, at, mode } => {
            let held = enabled && inputs.held(input);
            let rising = held && !s.pressed;
            s.pressed = held;
            let spot = view.shown(*at);
            let down = match mode {
                TapMode::Hold => held,
                TapMode::Toggle => {
                    if rising {
                        s.toggled = !s.toggled;
                    }
                    s.toggled
                }
                TapMode::Repeat => {
                    if !held {
                        s.repeat_at = None;
                        false
                    } else {
                        let since = s.repeat_at.map_or(REPEAT, |t| now.saturating_duration_since(t));
                        if since >= REPEAT {
                            s.repeat_at = Some(now);
                            s.finger.is_none()
                        } else {
                            s.finger.is_some()
                        }
                    }
                }
            };
            put(s, down.then_some(spot), out);
        }
        Action::Swipe { input, from, to, ms } => {
            let held = enabled && inputs.held(input);
            if held && !s.pressed && s.stroke.is_none() {
                s.stroke = Some((view.shown(*from), view.shown(*to), now, Duration::from_millis((*ms).max(30) as u64)));
            }
            s.pressed = held;
            stroke(s, now, &mut put, out);
        }
        Action::Flick { stick, center, length } => {
            let (x, y) = if enabled { inputs.stick(stick, deadzone) } else { (0.0, 0.0) };
            let m = (x * x + y * y).sqrt();
            if m < 0.3 {
                s.flick_armed = true;
            }
            if m > 0.7 && s.flick_armed && s.stroke.is_none() {
                s.flick_armed = false;
                let (dx, dy) = if x.abs() > y.abs() { (x.signum(), 0.0) } else { (0.0, y.signum()) };
                let c = view.shown(*center);
                let (w, h) = view.span(*length);
                s.stroke = Some((c, (c.0 + dx * w, c.1 + dy * h), now, Duration::from_millis(FLICK_MS)));
            }
            stroke(s, now, &mut put, out);
        }
        Action::Joystick { stick, center, radius, press } => {
            let (x, y) = if enabled { inputs.stick(stick, deadzone) } else { (0.0, 0.0) };
            let c = view.shown(*center);
            if !s.pressing(press, inputs, enabled, x.hypot(y), now) {
                put(s, None, out);
            } else if s.finger.is_none() {
                // Down on the centre first, then pushed: an on-screen stick starts where the finger lands.
                put(s, Some(c), out);
            } else {
                let (w, h) = view.span(*radius);
                put(s, Some((c.0 + x * w, c.1 + y * h)), out);
            }
        }
        Action::Look { stick, center, half, speed, invert_y, press, tap } => {
            let (x, y) = if enabled { inputs.stick(stick, deadzone) } else { (0.0, 0.0) };
            let (x, y) = (x, if *invert_y { -y } else { y });
            let m = x.hypot(y);
            let c = view.shown(*center);
            let (hw, hh) = (half[0].max(0.01), half[1].max(0.01));
            let inside = |(u, v): (f32, f32)| {
                (u - c.0).abs() <= hw && (v - c.1).abs() <= hh && (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)
            };
            // The tap: lift the look finger, tap where it was (a second finger on a look area would confuse it), then
            // carry on looking.
            let tap_held = enabled && tap.as_ref().is_some_and(|t| inputs.held(t));
            if tap_held && !s.pressed && s.stroke.is_none() {
                let at = s.finger.unwrap_or(c);
                put(s, None, out);
                s.stroke = Some((at, at, now + REGRIP, LOOK_TAP));
            }
            s.pressed = tap_held;
            if let Some((_, _, start, _)) = s.stroke {
                if now >= start {
                    stroke(s, now, &mut put, out);
                }
                s.regrip_at = None;
                return;
            }
            let mut down = s.pressing(press, inputs, enabled, m, now);
            if *press == Press::Pushed && s.finger.is_some() {
                // Resting where it is for a moment once the stick lets go.
                s.rest_since = if m > 0.0 { None } else { s.rest_since.or(Some(now)) };
                down = s.rest_since.is_none_or(|t| now.saturating_duration_since(t) < LOOK_REST);
            }
            if !down {
                put(s, None, out);
                (s.rest_since, s.regrip_at) = (None, None);
                return;
            }
            match s.finger {
                None => {
                    if s.regrip_at.is_some_and(|t| now < t) {
                        return;
                    }
                    s.regrip_at = None;
                    s.rest_since = None;
                    // Set back from the way it's heading, so the drag has room before the edge.
                    let back = if m > 0.0 { (-x / m * hw * 0.5, -y / m * hh * 0.5) } else { (0.0, 0.0) };
                    put(s, Some((c.0 + back.0, c.1 + back.1)), out);
                }
                Some(f) if m > 0.0 => {
                    // Faster the further it's pushed, gently near the middle for aiming.
                    let pace = m.powf(1.5) / m;
                    let (w, h) = view.span(*speed * dt);
                    let next = (f.0 + x * pace * w, f.1 + y * pace * h);
                    if inside(next) {
                        put(s, Some(next), out);
                    } else {
                        put(s, None, out);
                        s.regrip_at = Some(now + REGRIP);
                    }
                }
                Some(_) => {}
            }
        }
        Action::Pointer { stick, center, half, touch, toggle, style, speed } => {
            let (x, y) = if enabled { inputs.stick(stick, deadzone) } else { (0.0, 0.0) };
            let c = view.shown(*center);
            let (hw, hh) = (half[0].max(0.01), half[1].max(0.01));
            let at = match style {
                PointerStyle::Centered => (c.0 + x * hw, c.1 + y * hh),
                PointerStyle::Cursor => {
                    let (u, v) = s.pointer.unwrap_or(c);
                    let step = speed * dt;
                    ((u + x * step).clamp(c.0 - hw, c.0 + hw), (v + y * step * view.aspect).clamp(c.1 - hh, c.1 + hh))
                }
            };
            s.pointer = Some(at);
            let press =
                if *toggle { Press::Toggle { input: touch.clone() } } else { Press::Hold { input: touch.clone() } };
            let touching = s.pressing(&press, inputs, enabled, x.hypot(y), now);
            put(s, touching.then_some(at), out);
        }
        Action::Tilt { stick, strength } => {
            let (x, y) = if enabled { inputs.stick(stick, deadzone) } else { (0.0, 0.0) };
            if (x, y) != (0.0, 0.0) || tilt.is_none() {
                // The stick's direction on the screen as shown, turned into the guest's axes.
                let (gx, gy) = view.rotation.to_guest(0.5 + x as f64, 0.5 + y as f64);
                let s = *strength as f64;
                *tilt = Some(((gx - 0.5) * s, (gy - 0.5) * s));
            }
        }
        Action::Motion { .. } => {}
        Action::Device { input, button } => {
            let held = enabled && inputs.held(input);
            if held != s.pressed {
                s.pressed = held;
                out.buttons.push((button.number(), held));
            }
        }
    }
}

/// Advance a swipe or flick under way: down at its start, along the line, up at its end.
fn stroke(
    s: &mut State,
    now: Instant,
    put: &mut impl FnMut(&mut State, Option<(f32, f32)>, &mut Output),
    out: &mut Output,
) {
    let Some((from, to, start, length)) = s.stroke else { return };
    let t = (now.saturating_duration_since(start).as_secs_f32() / length.as_secs_f32()).min(1.0);
    if s.finger.is_none() && t < 1.0 {
        put(s, Some(from), out);
        return;
    }
    let at = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
    if t >= 1.0 {
        put(s, Some(at), out);
        put(s, None, out);
        s.stroke = None;
    } else {
        put(s, Some(at), out);
    }
}

#[cfg(test)]
mod tests {
    use super::super::preset::{Binding, DeviceButton, PadButton};
    use super::*;

    fn view() -> View {
        View { rotation: Rotation::Portrait, turned: false, aspect: 320.0 / 480.0 }
    }

    fn preset(actions: Vec<Action>) -> Preset {
        let mut p = Preset::new("t");
        p.bindings = actions.into_iter().map(Binding::new).collect();
        p
    }

    #[test]
    fn lean_is_level_when_the_controller_lies_flat() {
        // SDL: lying flat, gravity's reaction is along the controller's y (up).
        let (roll, pitch) = lean_of([0.0, GRAVITY as f32, 0.0]);
        assert!(roll.abs() < 1e-6 && pitch.abs() < 1e-6);
        // Right side down: the reaction leans toward -x.
        let (roll, _) = lean_of([-(GRAVITY as f32) * 0.5, GRAVITY as f32 * 0.866, 0.0]);
        assert!((roll.to_degrees() - 30.0).abs() < 0.5);
        // Far edge raised: the reaction tips toward -z, away from the player.
        let (_, pitch) = lean_of([0.0, GRAVITY as f32 * 0.866, -(GRAVITY as f32) * 0.5]);
        assert!((pitch.to_degrees() - 30.0).abs() < 0.5);
    }

    #[test]
    fn motion_without_sensor_readings_leaves_the_stick_tilt_working() {
        // Sensors quiet (the snapshot leaves them out): the stick's tilt still leans the device.
        let p = preset(vec![Action::Motion { sensitivity: 1.0 }, Action::Tilt { stick: Stick::Left, strength: 1.0 }]);
        let mut m = Mapper::default();
        let keys = HashSet::new();
        let start = Instant::now();
        let right = Merged { axes: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() };
        let mut a = [0.0; 3];
        for i in 0..20 {
            let out = m.step(&p, &Inputs { pad: &right, keys: &keys }, &view(), start + Duration::from_millis(50 * i));
            a = out.motion.expect("motion each step").0;
        }
        assert!(a[0] < -5.0, "leans right: {a:?}");
        // With readings, the controller's own lean wins: lying flat is flat.
        let flat = Merged { accel: Some([0.0, GRAVITY as f32, 0.0]), gyro: Some([0.0; 3]), ..right };
        let out = m.step(&p, &Inputs { pad: &flat, keys: &keys }, &view(), start + Duration::from_secs(2));
        let a = out.motion.unwrap().0;
        assert!(a[0].abs() < 1e-6 && a[1].abs() < 1e-6 && (a[2] - GRAVITY).abs() < 1e-6, "{a:?}");
    }

    #[test]
    fn tap_holds_while_pressed() {
        let p = preset(vec![Action::Tap {
            input: Input::Pad(PadButton::South),
            at: Point::new(0.25, 0.75),
            mode: TapMode::Hold,
        }]);
        let mut m = Mapper::default();
        let keys = HashSet::new();
        let now = Instant::now();
        let pressed = Merged { buttons: PadButton::South.bit(), ..Default::default() };
        let out = m.step(&p, &Inputs { pad: &pressed, keys: &keys }, &view(), now);
        assert_eq!(out.contacts.len(), 1);
        assert_eq!((out.contacts[0].phase, out.contacts[0].x, out.contacts[0].y), (DOWN, 0.25, 0.75));
        assert!(m.step(&p, &Inputs { pad: &pressed, keys: &keys }, &view(), now).contacts.is_empty());
        let out = m.step(&p, &Inputs { pad: &Merged::default(), keys: &keys }, &view(), now);
        assert_eq!(out.contacts[0].phase, UP);
    }

    #[test]
    fn centered_pointer_returns_and_touches_where_it_points() {
        let p = preset(vec![Action::Pointer {
            stick: Stick::Left,
            center: Point::CENTER,
            half: [0.5, 0.5],
            touch: Input::Pad(PadButton::RightTrigger),
            toggle: false,
            style: PointerStyle::Centered,
            speed: 1.0,
        }]);
        let mut m = Mapper::default();
        let keys = HashSet::new();
        let now = Instant::now();
        let mut pad = Merged { axes: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() };
        m.step(&p, &Inputs { pad: &pad, keys: &keys }, &view(), now);
        assert_eq!(m.pointer(0), Some(((1.0, 0.5), false)));
        pad.buttons = PadButton::RightTrigger.bit();
        let out = m.step(&p, &Inputs { pad: &pad, keys: &keys }, &view(), now);
        assert_eq!((out.contacts[0].phase, out.contacts[0].x), (DOWN, 1.0));
        let out = m.step(&p, &Inputs { pad: &Merged::default(), keys: &keys }, &view(), now);
        assert_eq!(out.contacts[0].phase, UP);
        assert_eq!(m.pointer(0), Some(((0.5, 0.5), false)));
    }

    #[test]
    fn joystick_lands_on_its_centre_then_pushes_round() {
        let p = preset(vec![Action::Joystick {
            stick: Stick::Dpad,
            center: Point::new(0.2, 0.8),
            radius: 0.1,
            press: Press::Pushed,
        }]);
        let mut m = Mapper::default();
        let keys = HashSet::new();
        let now = Instant::now();
        let right = Merged { buttons: PadButton::DpadRight.bit(), ..Default::default() };
        let down = m.step(&p, &Inputs { pad: &right, keys: &keys }, &view(), now).contacts;
        assert_eq!((down[0].phase, down[0].x, down[0].y), (DOWN, 0.2f32 as f64, 0.8f32 as f64));
        let moved = m.step(&p, &Inputs { pad: &right, keys: &keys }, &view(), now).contacts;
        // 0.1 of the shorter side (the width here) to the right.
        assert!((moved[0].x - 0.3).abs() < 1e-6 && moved[0].phase == MOVE);
    }

    #[test]
    fn swipe_runs_to_its_end_and_lifts() {
        let p = preset(vec![Action::Swipe {
            input: Input::Key("Space".into()),
            from: Point::new(0.5, 0.7),
            to: Point::new(0.5, 0.3),
            ms: 100,
        }]);
        let mut m = Mapper::default();
        let keys: HashSet<String> = ["Space".to_string()].into();
        let t0 = Instant::now();
        let first = m.step(&p, &Inputs { pad: &Merged::default(), keys: &keys }, &view(), t0);
        assert_eq!(first.contacts[0].phase, DOWN);
        assert!(first.busy);
        let last =
            m.step(&p, &Inputs { pad: &Merged::default(), keys: &keys }, &view(), t0 + Duration::from_millis(150));
        assert_eq!(last.contacts.iter().map(|c| c.phase).collect::<Vec<_>>(), vec![MOVE, UP]);
        assert!((last.contacts[1].y - 0.3).abs() < 1e-6);
    }

    #[test]
    fn device_buttons_and_shift() {
        let mut p = preset(vec![Action::Device { input: Input::Pad(PadButton::Start), button: DeviceButton::Home }]);
        p.bindings[0].with = Some(Input::Pad(PadButton::LeftShoulder));
        let mut m = Mapper::default();
        let keys = HashSet::new();
        let now = Instant::now();
        let start = Merged { buttons: PadButton::Start.bit(), ..Default::default() };
        assert!(m.step(&p, &Inputs { pad: &start, keys: &keys }, &view(), now).buttons.is_empty());
        let both = Merged { buttons: PadButton::Start.bit() | PadButton::LeftShoulder.bit(), ..Default::default() };
        assert_eq!(m.step(&p, &Inputs { pad: &both, keys: &keys }, &view(), now).buttons, vec![(0, true)]);
    }

    #[test]
    fn landscape_turns_the_stick() {
        // Home on the right: the screen's right is the guest's bottom, so pushing right lowers the bottom edge.
        let p = preset(vec![Action::Tilt { stick: Stick::Dpad, strength: 1.0 }]);
        let mut m = Mapper::default();
        let keys = HashSet::new();
        let v = View { rotation: Rotation::HomeRight, turned: false, aspect: 1.5 };
        let right = Merged { buttons: PadButton::DpadRight.bit(), ..Default::default() };
        let t0 = Instant::now();
        m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, t0);
        let out = m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, t0 + Duration::from_millis(100));
        let (a, _) = out.motion.unwrap();
        assert!(a[1] > 0.0 && a[0].abs() < 1e-9, "{a:?}");
    }

    #[test]
    fn motion_with_a_hold_button_acts_only_while_held_and_levels_after() {
        let mut p = preset(vec![Action::Motion { sensitivity: 1.0 }]);
        p.bindings[0].with = Some(Input::Pad(PadButton::LeftShoulder));
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        // Leaning right, about 30°.
        let lean = [-(GRAVITY as f32) * 0.5, GRAVITY as f32 * 0.866, 0.0];
        let free = Merged { accel: Some(lean), ..Default::default() };
        let held = Merged { buttons: PadButton::LeftShoulder.bit(), ..free };
        let at = |ms| t0 + Duration::from_millis(ms);
        assert!(m.step(&p, &Inputs { pad: &free, keys: &keys }, &v, at(0)).motion.is_none());
        let (a, _) = m.step(&p, &Inputs { pad: &held, keys: &keys }, &v, at(10)).motion.unwrap();
        assert!(a[0] < -4.0, "{a:?}");
        // Let go: level, for a moment, then nothing more.
        for ms in [20, 60, 100] {
            let (a, _) = m.step(&p, &Inputs { pad: &free, keys: &keys }, &v, at(ms)).motion.unwrap();
            assert_eq!(a, [0.0, 0.0, GRAVITY]);
        }
        assert!(m.step(&p, &Inputs { pad: &free, keys: &keys }, &v, at(200)).motion.is_none());
    }

    fn stick_at(x: f32, y: f32, buttons: u32) -> Merged {
        Merged { axes: [0.0, 0.0, x, y, 0.0, 0.0], buttons, ..Default::default() }
    }

    fn phases(out: &Output) -> Vec<u32> {
        out.contacts.iter().map(|c| c.phase).collect()
    }

    #[test]
    fn a_pushed_joystick_ignores_a_nudge_but_not_a_push() {
        let p = preset(vec![Action::Joystick {
            stick: Stick::Right,
            center: Point::new(0.8, 0.8),
            radius: 0.1,
            press: Press::Pushed,
        }]);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let at = |ms| t0 + Duration::from_millis(ms);
        let nudge = stick_at(0.3, 0.0, 0);
        assert!(m.step(&p, &Inputs { pad: &nudge, keys: &keys }, &v, at(0)).contacts.is_empty());
        assert!(m.step(&p, &Inputs { pad: &Merged::default(), keys: &keys }, &v, at(20)).contacts.is_empty());
        // Held a little longer, it lands; pushed hard, at once.
        m.step(&p, &Inputs { pad: &nudge, keys: &keys }, &v, at(40));
        assert_eq!(phases(&m.step(&p, &Inputs { pad: &nudge, keys: &keys }, &v, at(100))), [DOWN]);
        let mut m = Mapper::default();
        assert_eq!(phases(&m.step(&p, &Inputs { pad: &stick_at(0.9, 0.0, 0), keys: &keys }, &v, at(0))), [DOWN]);
    }

    #[test]
    fn a_toggled_joystick_stays_down_at_rest_until_clicked_again() {
        let p = preset(vec![Action::Joystick {
            stick: Stick::Right,
            center: Point::new(0.8, 0.8),
            radius: 0.1,
            press: Press::toggle_for(&Stick::Right),
        }]);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let r3 = PadButton::RightStick.bit();
        let mut step = |pad: Merged| phases(&m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0));
        // Pushing alone does nothing; R3 lands it on the centre, the stick pushes it, and it stays at rest.
        assert!(step(stick_at(1.0, 0.0, 0)).is_empty());
        assert_eq!(step(stick_at(0.0, 0.0, r3)), [DOWN]);
        assert_eq!(step(stick_at(1.0, 0.0, 0)), [MOVE]);
        assert_eq!(step(stick_at(0.0, 0.0, 0)), [MOVE]);
        assert!(step(stick_at(0.0, 0.0, 0)).is_empty());
        assert_eq!(step(stick_at(0.0, 0.0, r3)), [UP]);
    }

    fn look(press: Press, tap: Option<Input>) -> Preset {
        preset(vec![Action::Look {
            stick: Stick::Right,
            center: Point::new(0.5, 0.5),
            half: [0.2, 0.2],
            speed: 1.0,
            invert_y: false,
            press,
            tap,
        }])
    }

    #[test]
    fn looking_drags_at_the_sticks_pace_and_lands_again_before_the_edge() {
        let p = look(Press::Pushed, None);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let right = stick_at(1.0, 0.0, 0);
        let mut log = Vec::new();
        let mut x = Vec::new();
        for i in 0..60 {
            let out = m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, t0 + Duration::from_millis(16 * i));
            for c in &out.contacts {
                log.push(c.phase);
                x.push(c.x);
            }
            // Never lifted and landed in the same step.
            assert!(out.contacts.len() <= 1, "{:?}", phases(&out));
        }
        // Lands set back from the way it's heading (left of centre), moves right, lifts before 0.7 and lands again.
        assert_eq!(log[0], DOWN);
        assert!((x[0] - 0.4).abs() < 1e-6, "{}", x[0]);
        assert!(x.iter().all(|&u| u <= 0.7 + 1e-6));
        assert!(log.iter().filter(|&&ph| ph == UP).count() >= 2, "{log:?}");
        assert!(log.windows(2).all(|w| w != [UP, MOVE]));
        // 16 ms at full tilt moves 1.0 × 16 ms of the shorter side (the width here).
        let i = log.iter().position(|&ph| ph == MOVE).unwrap();
        assert!((x[i] - x[i - 1] - 0.016).abs() < 1e-3, "{}", x[i] - x[i - 1]);
    }

    #[test]
    fn looking_rests_a_moment_when_the_stick_lets_go_then_lifts() {
        let p = look(Press::Pushed, None);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let at = |ms| t0 + Duration::from_millis(ms);
        let right = stick_at(1.0, 0.0, 0);
        m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, at(0));
        m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, at(16));
        let rest = Merged::default();
        let out = m.step(&p, &Inputs { pad: &rest, keys: &keys }, &v, at(32));
        assert!(out.contacts.is_empty() && out.busy);
        assert!(m.step(&p, &Inputs { pad: &rest, keys: &keys }, &v, at(200)).contacts.is_empty());
        assert_eq!(phases(&m.step(&p, &Inputs { pad: &rest, keys: &keys }, &v, at(400))), [UP]);
    }

    #[test]
    fn a_look_tap_lifts_the_finger_taps_where_it_was_and_looks_on() {
        let p = look(Press::Pushed, Some(Input::Pad(PadButton::RightShoulder)));
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let r1 = PadButton::RightShoulder.bit();
        let mut all = Vec::new();
        for i in 0..20u64 {
            let pad = stick_at(1.0, 0.0, if (3..5).contains(&i) { r1 } else { 0 });
            let out = m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(16 * i));
            all.push(phases(&out));
        }
        // Looking (down, moves), the tap's lift, its down and up, then the look lands again.
        let flat: Vec<u32> = all.concat();
        let ups = flat.iter().filter(|&&ph| ph == UP).count();
        let downs = flat.iter().filter(|&&ph| ph == DOWN).count();
        assert!(ups >= 2 && downs >= 3, "{all:?}");
        assert_eq!(all[3], [UP]);
    }

    #[test]
    fn presets_from_before_press_settings_load_as_they_were() {
        let old = r#"{"name":"Old","bindings":[
            {"action":{"kind":"pointer","stick":"left","center":{"x":0.5,"y":0.5},"half":[0.5,0.5],
              "touch":{"pad":"right_trigger"},"style":"centered","speed":0.9}},
            {"action":{"kind":"joystick","stick":"left","center":{"x":0.2,"y":0.8},"radius":0.15}}],
            "show_spots":true,"deadzone":0.15,"apps":[]}"#;
        let p: Preset = serde_json::from_str(old).unwrap();
        assert!(matches!(&p.bindings[0].action, Action::Pointer { toggle: false, .. }));
        assert!(matches!(&p.bindings[1].action, Action::Joystick { press: Press::Pushed, .. }));
    }
}

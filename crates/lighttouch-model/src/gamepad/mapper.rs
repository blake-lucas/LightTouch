//! Turns what the controllers and keys are doing into touches, device buttons and motion, one step per UI frame.
//!
//! Geometry happens on the screen as shown (`u` right, `v` down, 0..1), where a stick's up is the screen's up and a
//! radius is round, then goes to the guest: places are kept in its portrait coordinates and touches go out in its
//! panel's scan ([`Rotation::to_scan`]). Each binding owns one contact id, so the engine sees every finger the bindings
//! hold as its own; the engine takes five fingers at once, so a sixth waits.
use super::preset::{Action, Binding, Input, Point, PointerStyle, Preset, Press, Stick, TapMode};
use super::sdl::{Pad, Sensors};
use crate::rotation::Rotation;
use crate::tilt::{Rest, Tilt};
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
/// A D-pad or keys, all or nothing, push a speed-set binding (looking, a cursor) from this share of full speed up to
/// all of it over `KEY_RAMP`, so a tap nudges and holding turns fully.
const KEY_START: f32 = 0.3;
const KEY_RAMP: Duration = Duration::from_millis(300);
/// A centered pointer set to jump at once glides at this speed when keys steer it (screen widths a second).
const KEY_FOLLOW: f32 = 4.0;
/// A stick binding's tap: how long the finger stays down at least, so a quick click still registers (how long it stays
/// up around the tap is the binding's `tap_lift`).
const STICK_TAP: Duration = Duration::from_millis(60);
/// A joystick's finger glides to where the stick points at one reach in this long (a stick let go in a frame would
/// otherwise jump the finger back to the centre, which some games' sticks don't settle from), at speed 1.
const JOYSTICK_GLIDE: f32 = 0.05;
/// A joystick's finger stays where it landed, on the stick's centre, this long before following the stick: a game
/// that reads touches once a frame (30 a second, say) would otherwise first see it already pushed away, and centre its
/// stick there.
const JOYSTICK_LAND: Duration = Duration::from_millis(50);

/// How the screen is shown.
/// How far inside the screen's edge a finger stays, as a fraction of its side (about a pixel): a touch on the last
/// pixel's far edge is off the panel.
const EDGE: f32 = 0.002;

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

    /// `to`, or where the line from `from` toward it leaves the screen (just inside its edge): a stick pushed
    /// toward an edge its spot sits near keeps its direction and goes as far as the screen allows, rather than
    /// sliding along the edge.
    pub fn reach(&self, from: (f32, f32), to: (f32, f32)) -> (f32, f32) {
        let from = (from.0.clamp(EDGE, 1.0 - EDGE), from.1.clamp(EDGE, 1.0 - EDGE));
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let room = |p: f32, d: f32| {
            if d > 0.0 {
                (1.0 - EDGE - p) / d
            } else if d < 0.0 {
                (EDGE - p) / d
            } else {
                f32::INFINITY
            }
        };
        let t = room(from.0, dx).min(room(from.1, dy)).clamp(0.0, 1.0);
        (from.0 + dx * t, from.1 + dy * t)
    }

    fn contact(&self, id: u32, phase: u32, u: f32, v: f32) -> Contact {
        let (u, v) = (u.clamp(EDGE, 1.0 - EDGE), v.clamp(EDGE, 1.0 - EDGE));
        let (x, y) = self.rotation.to_scan(u as f64, v as f64, self.turned);
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
    /// No controller's sensors report, but one's are on and paused (sdl.rs turns them on again).
    pub sensors_quiet: bool,
    /// The mouse's movement since the last step (raw counts), while it's locked to the game.
    pub mouse: (f32, f32),
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
            m.sensors_quiet |= p.sensors == Sensors::Quiet;
        }
        m.sensors_quiet &= m.accel.is_none();
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
    /// Since when a D-pad or keys stick has been pushed (its speed builds up).
    keyed_since: Option<Instant>,
    /// Mouse movement not yet given to the look finger (it was lifted to land again).
    mouse_carry: (f32, f32),
    /// When a joystick's finger landed on its centre.
    landed_at: Option<Instant>,
    /// A stick binding's tap under way, where it taps, and whether a click came while it was down (one more tap).
    tap: Option<TapPhase>,
    tap_at: (f32, f32),
    tap_again: bool,
}

/// A stick binding's tap: the binding's finger is up and the tap lands then; it's down (since then); or it's lifted
/// and the binding waits until then before its own finger lands again.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TapPhase {
    Landing(Instant),
    Down(Instant),
    Resting(Instant),
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
    /// A Motion binding has a controller with motion sensors: level is then the controller's own, the device lying
    /// flat (FLAT), not the app's upright rest, while it is held or not.
    motion_present: bool,
    /// A Tilt binding's Set level: the lean it rests at (roll, pitch, radians) from the rest it was set against, and
    /// whether the stick is ignored until it's let go (so setting it doesn't lean it on by the stick's push again).
    tilt_level: (f64, f64),
    tilt_level_rest: Rest,
    tilt_hold: bool,
    last: Option<Instant>,
}

/// What a Tilt binding asks this step: the stick's lean (fractions of the full lean, as shown), the rest it leans from,
/// and whether its Set level was just pressed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct TiltAsk {
    lean: (f64, f64),
    rest: Rest,
    set: bool,
}

impl Mapper {
    /// A binding moves the device, or holds it where the controller's sensors left it: nothing else should level it.
    pub fn holds_motion(&self) -> bool {
        self.tilting || self.sensing || self.motion_present
    }

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
        self.states.get(index).is_some_and(|s| s.finger.is_some() || s.pressed || s.stroke.is_some() || s.tap.is_some())
    }

    /// Lift every finger and release every button the preset holds (it changed, or input stopped), and level the
    /// device if its motion was leaning it.
    pub fn release(&mut self, preset: Option<&Preset>, view: &View) -> Output {
        let mut out = self.release_fingers(preset, view);
        if self.tilting || self.sensing || self.motion_present {
            self.tilting = false;
            self.sensing = false;
            self.motion_present = false;
            self.tilt = Tilt::default();
            out.motion = Some((crate::tilt::rest(view.rotation), [0.0; 3]));
        }
        out
    }

    /// Forget a Tilt binding's Set level (another preset).
    pub fn forget_tilt_level(&mut self) {
        (self.tilt_level, self.tilt_hold) = ((0.0, 0.0), false);
    }

    /// Lift every finger and release every button the preset holds, leaving the device's motion as it is (an edit
    /// to the preset: a Motion binding removed levels on the next step).
    pub fn release_fingers(&mut self, preset: Option<&Preset>, view: &View) -> Output {
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
        out
    }

    /// While the layout is edited: no fingers or buttons (the mouse places the bindings), but the controller's
    /// motion still tilts the device, so a running game isn't jolted level.
    pub fn step_motion(&mut self, preset: &Preset, inputs: &Inputs, view: &View, now: Instant) -> Output {
        self.last = Some(now);
        let mut out = self.release_fingers(Some(preset), view);
        self.motion(preset, inputs, view, None, now, &mut out);
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
        let mut tilt: Option<TiltAsk> = None;
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

        self.motion(preset, inputs, view, tilt, now, &mut out);
        out.busy |= self.states.iter().any(|s| {
            s.stroke.is_some()
                || s.repeat_at.is_some()
                || s.rest_since.is_some()
                || s.regrip_at.is_some()
                || s.tap.is_some()
        });
        out
    }

    fn motion(
        &mut self,
        preset: &Preset,
        inputs: &Inputs,
        view: &View,
        tilt: Option<TiltAsk>,
        now: Instant,
        out: &mut Output,
    ) {
        // Motion: the controller's sensors first (while its hold button is, if it has one), then a stick's tilt,
        // settling back to level when it lets go. Sensors that pause while the binding is engaged leave the device
        // where they had it (the engine holds the last sample): levelling then was a recentre mid-turn.
        let motion = preset.bindings.iter().find_map(|b| match b.action {
            Action::Motion { sensitivity } if b.with.as_ref().is_none_or(|w| inputs.held(w)) => Some(sensitivity),
            _ => None,
        });
        // No stick tilt this step (the layout is being edited): a Tilt binding still holds its rest and level.
        let tilt = tilt.or_else(|| {
            preset.bindings.iter().find_map(|b| match b.action {
                Action::Tilt { rest, .. } => Some(TiltAsk { lean: (0.0, 0.0), rest, set: false }),
                _ => None,
            })
        });
        let was_sensing = std::mem::replace(&mut self.sensing, false);
        let present = preset.bindings.iter().any(|b| matches!(b.action, Action::Motion { .. }))
            && (inputs.pad.accel.is_some() || inputs.pad.sensors_quiet);
        let was_present = std::mem::replace(&mut self.motion_present, present);
        let level = if present { FLAT } else { crate::tilt::rest(view.rotation) };
        if let (Some(sensitivity), Some(accel)) = (motion, inputs.pad.accel) {
            out.motion = Some(self.motion_from_sensors(accel, inputs.pad.gyro, sensitivity as f64, view.rotation));
            self.sensing = true;
            self.leveling = None;
        } else if was_sensing && motion.is_some() && inputs.pad.sensors_quiet {
            self.sensing = true;
        } else if tilt.is_some() || self.tilting {
            let ask = tilt.unwrap_or(TiltAsk { lean: (0.0, 0.0), rest: Rest::Upright, set: false });
            if ask.rest != self.tilt_level_rest {
                (self.tilt_level, self.tilt_level_rest, self.tilt_hold) = ((0.0, 0.0), ask.rest, false);
            }
            let mut lean = ask.lean;
            if ask.set {
                // Where it leans now is where it rests; with the stick let go, the chosen rest again.
                let pushed = lean != (0.0, 0.0);
                self.tilt_level = if pushed { self.tilt.leaning() } else { (0.0, 0.0) };
                self.tilt_hold = pushed;
            }
            if self.tilt_hold {
                // The stick that set it leans no further until it's let go, so the device stays put.
                self.tilt_hold = lean != (0.0, 0.0);
                lean = (0.0, 0.0);
            }
            let max = crate::tilt::MAX_DEGREES.to_radians();
            let to =
                (self.tilt_level.0 + lean.0.clamp(-1.0, 1.0) * max, self.tilt_level.1 + lean.1.clamp(-1.0, 1.0) * max);
            let sample = self.tilt.step_to(to.0, to.1, ask.rest, view.rotation, now);
            let moving = self.tilt.leaning() != to || lean != (0.0, 0.0);
            // Resting anywhere but upright, the binding holds the device there (the app's own rest is upright).
            let away = tilt.is_some() && (ask.rest != Rest::Upright || self.tilt_level != (0.0, 0.0));
            self.tilting = moving || away || !self.tilt.at_rest();
            out.motion = Some(sample);
            out.busy |= moving;
        } else if was_sensing || was_present != present || self.leveling.is_some_and(|t| now < t) {
            let fresh = was_sensing || was_present != present;
            self.leveling = self.leveling.filter(|_| !fresh).or(Some(now + Duration::from_millis(100)));
            out.motion = Some((level, [0.0; 3]));
            out.busy = true;
        }
    }

    /// The device's motion from the controller's: the controller held level (flat, or as when "level" was set) is the
    /// device lying flat on a table, face up, whichever way the screen is shown; rolling the controller lowers the
    /// device's side as shown, and raising its far edge raises the device's top, as a board held flat would (a
    /// Labyrinth ball rolls toward the bottom). SDL's axes for the controller (x right, y up out of its face, z toward
    /// the player) are the flat device's x, z and -y as shown: the same turn for the accelerometer and the gyroscope,
    /// so the two agree (d(a)/dt = -w x a), then turned into the guest's axes.
    fn motion_from_sensors(
        &mut self,
        accel: [f32; 3],
        gyro: Option<[f32; 3]>,
        sensitivity: f64,
        rotation: Rotation,
    ) -> ([f64; 3], [f64; 3]) {
        let g = gyro.map(|g| g.map(|w| w as f64)).unwrap_or([0.0; 3]);
        let (roll, pitch) = lean_of(accel);
        self.lean = Some((roll, pitch));
        let (r, p) = ((roll - self.level.0) * sensitivity, (pitch - self.level.1) * sensitivity);
        // tilt::lean leans from upright; the same lean from lying flat is it tipped back a quarter turn.
        let (up, _) = crate::tilt::lean(r, p, 0.0, 0.0);
        let acceleration = flat_axes(up);
        let w = flat_axes(g).map(|w| w * sensitivity);
        (rotation.to_guest_axes(acceleration), rotation.to_guest_axes(w))
    }
}

/// The device lying flat, face up: gravity's reaction out of its screen, whichever way the screen is shown.
pub const FLAT: [f64; 3] = [0.0, 0.0, crate::tilt::GRAVITY];

/// A vector in the controller's axes (or tilt::lean's upright device's), in the flat device's: (x, -z, y), a quarter
/// turn about x.
fn flat_axes(v: [f64; 3]) -> [f64; 3] {
    [v[0], -v[2], v[1]]
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
    tilt: &mut Option<TiltAsk>,
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
                let to = view.reach(c, (c.0 + dx * w, c.1 + dy * h));
                s.stroke = Some((c, to, now, Duration::from_millis(FLICK_MS)));
            }
            stroke(s, now, &mut put, out);
        }
        Action::Joystick { stick, center, radius, press, tap, tap_lift, speed } => {
            let (x, y) = if enabled { inputs.stick(stick, deadzone) } else { (0.0, 0.0) };
            let c = view.shown(*center);
            if tapping(s, tap, lift(*tap_lift), inputs, enabled, c, now, &mut put, out) {
                return;
            }
            if !s.pressing(press, inputs, enabled, x.hypot(y), now) {
                s.landed_at = None;
                put(s, None, out);
            } else if s.finger.is_some()
                && s.landed_at.is_some_and(|t| now.saturating_duration_since(t) < JOYSTICK_LAND)
            {
                // Just landed: still on the centre.
                out.busy = true;
            } else if let Some((u, v)) = s.finger {
                // Gliding toward the stick's spot, in reaches so it goes as fast up as across.
                let (w, h) = view.span(*radius);
                let to = view.reach(c, (c.0 + x * w, c.1 + y * h));
                let (dx, dy) = ((to.0 - u) / w, (to.1 - v) / h);
                let (d, step) = (dx.hypot(dy), dt * speed.max(0.01) / JOYSTICK_GLIDE);
                if d <= step {
                    put(s, Some(to), out);
                } else {
                    out.busy = true;
                    put(s, Some((u + dx * w * step / d, v + dy * h * step / d)), out);
                }
            } else {
                // Down on the centre first, then pushed: an on-screen stick starts where the finger lands.
                put(s, Some(c), out);
                s.landed_at = s.finger.map(|_| now);
                out.busy = true;
            }
        }
        Action::Look { stick, center, half, speed, invert_y, press, tap, tap_lift, mouse, mouse_speed, .. } => {
            let (x, y) = if enabled { inputs.stick(stick, deadzone) } else { (0.0, 0.0) };
            // Whether the finger is down goes by the push itself; the pace, by keys' built-up push.
            let mut pushed = x.hypot(y);
            let (x, y) = key_ramp(s, stick, (x, y), now);
            let (x, y) = (x, if *invert_y { -y } else { y });
            let m = x.hypot(y);
            // The mouse, while it's locked to the game: its movement is the finger's.
            if *mouse && enabled {
                let (mx, my) = inputs.pad.mouse;
                s.mouse_carry.0 += mx;
                s.mouse_carry.1 += if *invert_y { -my } else { my };
            }
            let carry = s.mouse_carry;
            let mousing = carry != (0.0, 0.0);
            if mousing {
                pushed = 1.0;
            }
            let c = view.shown(*center);
            let (hw, hh) = (half[0].max(0.01), half[1].max(0.01));
            let inside = |(u, v): (f32, f32)| {
                (u - c.0).abs() <= hw && (v - c.1).abs() <= hh && (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)
            };
            // The tap, where the look finger was.
            let at = s.finger.unwrap_or(c);
            if tapping(s, tap, lift(*tap_lift), inputs, enabled, at, now, &mut put, out) {
                s.regrip_at = None;
                return;
            }
            let mut down = s.pressing(press, inputs, enabled, pushed, now);
            if *press == Press::Pushed && s.finger.is_some() {
                // Resting where it is for a moment once the stick lets go.
                s.rest_since = if m > 0.0 || mousing { None } else { s.rest_since.or(Some(now)) };
                down = s.rest_since.is_none_or(|t| now.saturating_duration_since(t) < LOOK_REST);
            }
            if !down {
                put(s, None, out);
                (s.rest_since, s.regrip_at, s.mouse_carry) = (None, None, (0.0, 0.0));
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
                    let (dx, dy, d) = if m > 0.0 { (x, y, m) } else { (carry.0, carry.1, carry.0.hypot(carry.1)) };
                    let back = if d > 0.0 { (-dx / d * hw * 0.5, -dy / d * hh * 0.5) } else { (0.0, 0.0) };
                    put(s, Some((c.0 + back.0, c.1 + back.1)), out);
                }
                Some(f) if m > 0.0 || mousing => {
                    let mut next = f;
                    if m > 0.0 {
                        // Faster the further it's pushed, gently near the middle for aiming.
                        let pace = m.powf(1.5) / m;
                        let (w, h) = view.span(*speed * dt);
                        next = (next.0 + x * pace * w, next.1 + y * pace * h);
                    }
                    // A thousand counts across the shorter side at speed 1.
                    let (w, h) = view.span(mouse_speed.max(0.01) / 1000.0);
                    let to = (next.0 + carry.0 * w, next.1 + carry.1 * h);
                    if inside(to) {
                        put(s, Some(to), out);
                        s.mouse_carry = (0.0, 0.0);
                    } else if !mousing {
                        put(s, None, out);
                        s.regrip_at = Some(now + REGRIP);
                    } else {
                        // As far as the area goes, then lift to land again; the rest of the mouse's movement
                        // goes on from there (a flick can be wider than the area).
                        let d = (to.0 - f.0, to.1 - f.1);
                        let room = |p: f32, d: f32, lo: f32, hi: f32| {
                            if d > 0.0 {
                                (hi - p) / d
                            } else if d < 0.0 {
                                (lo - p) / d
                            } else {
                                f32::INFINITY
                            }
                        };
                        let (lo_u, hi_u) = ((c.0 - hw).max(0.0), (c.0 + hw).min(1.0));
                        let (lo_v, hi_v) = ((c.1 - hh).max(0.0), (c.1 + hh).min(1.0));
                        let t = (room(f.0, d.0, lo_u, hi_u).min(room(f.1, d.1, lo_v, hi_v)) * 0.98).clamp(0.0, 1.0);
                        put(s, Some((f.0 + d.0 * t, f.1 + d.1 * t)), out);
                        // The mouse's movement past where the finger stopped (the stick's isn't carried).
                        s.mouse_carry = (carry.0 * (1.0 - t), carry.1 * (1.0 - t));
                        put(s, None, out);
                        s.regrip_at = Some(now + REGRIP);
                    }
                }
                Some(_) => {}
            }
        }
        Action::Pointer { stick, center, half, touch, toggle, style, speed, follow } => {
            let (x, y) = if enabled { inputs.stick(stick, deadzone) } else { (0.0, 0.0) };
            let digital = matches!(stick, Stick::Dpad | Stick::Keys { .. });
            let follow = if digital { follow.or(Some(KEY_FOLLOW)) } else { *follow };
            let (x, y) = if *style == PointerStyle::Cursor { key_ramp(s, stick, (x, y), now) } else { (x, y) };
            let c = view.shown(*center);
            let (hw, hh) = (half[0].max(0.01), half[1].max(0.01));
            let at = match style {
                PointerStyle::Centered => {
                    let to = (c.0 + x * hw, c.1 + y * hh);
                    match (follow, s.pointer) {
                        // Gliding toward the stick's spot at `follow` screen widths a second (and back to the center
                        // when let go), in widths both ways so it moves as fast up as across.
                        (Some(follow), Some((u, v))) => {
                            let (dx, dy) = (to.0 - u, (to.1 - v) / view.aspect);
                            let (d, step) = (dx.hypot(dy), follow * dt);
                            if d <= step {
                                to
                            } else {
                                out.busy = true;
                                (u + dx * step / d, v + dy * step / d * view.aspect)
                            }
                        }
                        _ => to,
                    }
                }
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
        Action::Tilt { stick, strength, rest, set_level } => {
            let (x, y) = if enabled { inputs.stick(stick, deadzone) } else { (0.0, 0.0) };
            let held = enabled && set_level.as_ref().is_some_and(|i| inputs.held(i));
            let set = held && !s.pressed;
            s.pressed = held;
            if (x, y) != (0.0, 0.0) || tilt.is_none() {
                // As the screen shows it: sideways rocks the device that way, down tips its top toward the player.
                let k = *strength as f64;
                let was = tilt.is_some_and(|t| t.set);
                *tilt = Some(TiltAsk { lean: (x as f64 * k, y as f64 * k), rest: *rest, set: set || was });
            } else if let Some(t) = tilt {
                t.set |= set;
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
/// A D-pad or keys stick's push, built up from `KEY_START` over `KEY_RAMP` while held; an analog stick's as it is.
fn key_ramp(s: &mut State, stick: &Stick, (x, y): (f32, f32), now: Instant) -> (f32, f32) {
    if !matches!(stick, Stick::Dpad | Stick::Keys { .. }) {
        return (x, y);
    }
    if x == 0.0 && y == 0.0 {
        s.keyed_since = None;
        return (x, y);
    }
    let since = *s.keyed_since.get_or_insert(now);
    let t = (now.saturating_duration_since(since).as_secs_f32() / KEY_RAMP.as_secs_f32()).min(1.0);
    let k = KEY_START + (1.0 - KEY_START) * t;
    (x * k, y * k)
}

/// A stick binding's tap lift, as set (within reason).
fn lift(ms: u32) -> Duration {
    Duration::from_millis(ms.clamp(10, 500) as u64)
}

/// A stick binding's tap button, as a finger: its press lifts the binding's finger and, `gap` later, puts one down on
/// `at` (a second finger on a game's stick or look area would confuse it); letting go lifts it, though not before
/// `STICK_TAP`, so a quick click still registers; `gap` after that, the binding's own finger may land again.
/// A click while the tap is down taps once more after it, however many came meanwhile, so fast clicks never queue up
/// taps that go on after the clicking stops. True while the tap is under way: the binding does nothing else meanwhile.
#[allow(clippy::too_many_arguments)]
fn tapping(
    s: &mut State,
    tap: &Option<Input>,
    gap: Duration,
    inputs: &Inputs,
    enabled: bool,
    at: (f32, f32),
    now: Instant,
    put: &mut impl FnMut(&mut State, Option<(f32, f32)>, &mut Output),
    out: &mut Output,
) -> bool {
    let held = enabled && tap.as_ref().is_some_and(|t| inputs.held(t));
    let rising = held && !s.pressed;
    s.pressed = held;
    if rising {
        match s.tap {
            None => {
                put(s, None, out);
                s.tap_at = at;
                s.tap = Some(TapPhase::Landing(now + gap));
            }
            // Lifted a moment ago: down again once the lift has registered, where it was.
            Some(TapPhase::Resting(until)) => s.tap = Some(TapPhase::Landing(until)),
            Some(TapPhase::Down(_)) => s.tap_again = true,
            Some(TapPhase::Landing(_)) => {}
        }
    }
    match s.tap {
        None => return false,
        Some(TapPhase::Landing(when)) if now >= when => {
            let spot = s.tap_at;
            put(s, Some(spot), out);
            // Every finger busy: tries again next step.
            if s.finger.is_some() {
                s.tap = Some(TapPhase::Down(now));
            }
        }
        Some(TapPhase::Down(since)) if now.saturating_duration_since(since) >= STICK_TAP && (!held || s.tap_again) => {
            put(s, None, out);
            // A click that came while it was down taps once more (if it's still held, that tap lasts while it is).
            s.tap = Some(if std::mem::take(&mut s.tap_again) {
                TapPhase::Landing(now + gap)
            } else {
                TapPhase::Resting(now + gap)
            });
        }
        Some(TapPhase::Resting(until)) if now >= until => {
            s.tap = None;
            return false;
        }
        Some(_) => {}
    }
    true
}

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
    use super::super::preset::{Binding, DeviceButton, PadButton, TAP_LIFT_MS};
    use super::*;
    use crate::tilt::{GRAVITY, rest};

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
    fn a_joystick_near_the_edge_pushed_off_screen_stops_inside_it_on_its_line() {
        let p = preset(vec![Action::Joystick {
            stick: Stick::Right,
            center: Point::new(0.04, 0.5),
            radius: 0.15,
            press: Press::Pushed,
            tap: None,
            tap_lift: TAP_LIFT_MS,
            speed: 1.0,
        }]);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        // Pushed all the way at once, left and a little up: the finger lands on the spot, then goes as far as it can.
        let (x, y) = (-0.9, -0.43);
        let pushed = stick_at(x, y, 0);
        m.step(&p, &Inputs { pad: &pushed, keys: &keys }, &v, t0);
        let out = m.step(&p, &Inputs { pad: &pushed, keys: &keys }, &v, t0 + Duration::from_millis(60));
        let c = &out.contacts[0];
        assert!(c.x > 0.0 && c.x < 0.01, "{c:?}");
        // Same direction as the stick: the finger moved left and up in the stick's proportion (in screen units).
        let (u, vv) = m.states[0].finger.unwrap();
        let (w, h) = v.span(0.15);
        let (du, dv) = ((u - 0.04) / w, (vv - 0.5) / h);
        assert!(du < 0.0 && dv < 0.0 && (dv / du - y / x).abs() < 1e-3, "{du} {dv}");
        // Away from the edge it reaches the full push.
        let right = stick_at(1.0, 0.0, 0);
        m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, t0 + Duration::from_millis(200));
        assert!((m.states[0].finger.unwrap().0 - (0.04 + w)).abs() < 1e-5);
    }

    #[test]
    fn a_centered_pointer_with_a_speed_glides_to_the_stick_and_back() {
        let p = preset(vec![Action::Pointer {
            stick: Stick::Right,
            center: Point::CENTER,
            half: [0.5, 0.5],
            touch: Input::Pad(PadButton::RightTrigger),
            toggle: false,
            style: PointerStyle::Centered,
            speed: 0.9,
            follow: Some(2.0),
        }]);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let at = |ms| t0 + Duration::from_millis(ms);
        let rest = stick_at(0.0, 0.0, 0);
        let right = stick_at(1.0, 0.0, 0);
        m.step(&p, &Inputs { pad: &rest, keys: &keys }, &v, at(0));
        // Two widths a second: a tenth of a second in, a fifth of the way from the center to the right edge.
        let out = m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, at(100));
        let (u, _) = m.states[0].pointer.unwrap();
        assert!((u - 0.7).abs() < 1e-4 && out.busy, "{u}");
        for ms in (110..=400).step_by(10) {
            m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, at(ms));
        }
        assert!((m.states[0].pointer.unwrap().0 - 1.0).abs() < 1e-4);
        // Let go: it glides back, still busy until it's there.
        assert!(m.step(&p, &Inputs { pad: &rest, keys: &keys }, &v, at(410)).busy);
        for ms in (420..=700).step_by(10) {
            m.step(&p, &Inputs { pad: &rest, keys: &keys }, &v, at(ms));
        }
        assert!((m.states[0].pointer.unwrap().0 - 0.5).abs() < 1e-4);
        assert!(!m.step(&p, &Inputs { pad: &rest, keys: &keys }, &v, at(710)).busy);
    }

    #[test]
    fn editing_the_layout_keeps_the_controllers_motion_but_lifts_fingers() {
        let p = preset(vec![
            Action::Motion { sensitivity: 1.0 },
            Action::Tap { input: Input::Pad(PadButton::South), at: Point::CENTER, mode: TapMode::Hold },
        ]);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let lean = [-(GRAVITY as f32) * 0.5, GRAVITY as f32 * 0.866, 0.0];
        let pad = Merged { buttons: PadButton::South.bit(), accel: Some(lean), ..Default::default() };
        let playing = m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0);
        assert_eq!(playing.contacts.len(), 1);
        let editing = m.step_motion(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(10));
        assert_eq!(editing.contacts.len(), 1, "the tap's finger lifts");
        assert_eq!(editing.motion, playing.motion, "the lean carries on");
        let again = m.step_motion(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(20));
        assert!(again.contacts.is_empty() && again.motion == playing.motion);
        // An edit to the preset lifts fingers without levelling.
        m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(30));
        assert!(m.release_fingers(Some(&p), &v).motion.is_none());
    }

    #[test]
    fn motion_follows_the_screen_as_shown_when_the_device_is_turned() {
        let p = preset(vec![Action::Motion { sensitivity: 1.0 }]);
        let keys = HashSet::new();
        // The controller's right side down 30°, and turning that way.
        let right_down = Merged {
            accel: Some([-(GRAVITY as f32) * 0.5, GRAVITY as f32 * 0.866, 0.0]),
            gyro: Some([0.0, 0.0, -1.0]),
            ..Default::default()
        };
        let lean = |rotation| {
            let v = View { rotation, turned: false, aspect: 1.5 };
            Mapper::default().step(&p, &Inputs { pad: &right_down, keys: &keys }, &v, Instant::now()).motion.unwrap()
        };
        // Flat in portrait, its right side lowered 30°: gravity's reaction toward -x, the rest out of the screen,
        // turning about the device's long axis (+y).
        let (a, w) = lean(Rotation::Portrait);
        let near = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-2);
        let shown = [-GRAVITY * 0.5, 0.0, GRAVITY * 0.866];
        assert!(near(a, shown), "{a:?}");
        assert_eq!(w, [0.0, 1.0, 0.0]);
        // Home on the right: up as shown is the device's right edge, right as shown its bottom; as the arrow keys do.
        let (a, w2) = lean(Rotation::HomeRight);
        assert!(near(a, [shown[1], -shown[0], shown[2]]) && a[1] > 4.0, "{a:?}");
        assert_eq!(w2, [w[1], -w[0], w[2]]);
        // Home on the left: right as shown is the device's top.
        let (a, _) = lean(Rotation::HomeLeft);
        assert!(near(a, [-shown[1], shown[0], shown[2]]) && a[1] < -4.0, "{a:?}");
        // Same as a stick Tilt to the right in that turn.
        let tilt =
            preset(vec![Action::Tilt { stick: Stick::Right, strength: 1.0, rest: Rest::Upright, set_level: None }]);
        let v = View { rotation: Rotation::HomeRight, turned: false, aspect: 1.5 };
        let t0 = Instant::now();
        let mut m = Mapper::default();
        let mut last = [0.0; 3];
        for i in 0..40 {
            let at = t0 + Duration::from_millis(10 * i);
            if let Some((a, _)) = m.step(&tilt, &Inputs { pad: &stick_at(1.0, 0.0, 0), keys: &keys }, &v, at).motion {
                last = a;
            }
        }
        let full = crate::tilt::lean(35f64.to_radians(), 0.0, 0.0, 0.0).0;
        assert!(near(last, Rotation::HomeRight.to_guest_axes(full)) && last[1] > 4.0, "{last:?}");
    }

    #[test]
    fn a_stick_tilt_rests_as_chosen_and_holds_it() {
        let keys = HashSet::new();
        let p = preset(vec![Action::Tilt { stick: Stick::Right, strength: 1.0, rest: Rest::Flat, set_level: None }]);
        let v = View { rotation: Rotation::HomeRight, turned: false, aspect: 1.5 };
        let (mut m, t0) = (Mapper::default(), Instant::now());
        let out = m.step(&p, &Inputs { pad: &stick_at(0.0, 0.0, 0), keys: &keys }, &v, t0);
        // Flat on a table in any turn, and held there (the app's upright rest stays away), without busying the UI.
        let (a, w) = out.motion.expect("the rest");
        assert!(near(a, [0.0, 0.0, GRAVITY]) && w == [0.0; 3], "{a:?}");
        assert!(m.holds_motion() && !out.busy);
        // Editing the layout keeps it (and a level set) where it rests.
        let lean =
            |x| Merged { axes: [0.0, 0.0, x, 0.0, 0.0, 0.0], buttons: PadButton::South.bit(), ..Default::default() };
        let mut p = p.clone();
        if let Action::Tilt { set_level, .. } = &mut p.bindings[0].action {
            *set_level = Some(Input::Pad(PadButton::South));
        }
        let mut leaned = [0.0; 3];
        for i in 1..40 {
            let pad = if i < 30 { stick_at(0.5, 0.0, 0) } else { lean(0.5) };
            leaned = m
                .step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(20 * i))
                .motion
                .unwrap()
                .0;
        }
        let at = t0 + Duration::from_millis(900);
        let (a, _) = m.step_motion(&p, &Inputs { pad: &stick_at(0.0, 0.0, 0), keys: &keys }, &v, at).motion.unwrap();
        assert!(near(a, leaned) && m.holds_motion(), "{a:?} {leaned:?}");
        let out = m.step(&p, &Inputs { pad: &stick_at(0.0, 0.0, 0), keys: &keys }, &v, at + Duration::from_millis(20));
        assert!(near(out.motion.unwrap().0, leaned));
        // Let go of the binding (another preset): the app's rest again.
        let out = m.release(Some(&p), &v);
        assert!(!m.holds_motion() && out.motion.is_some());
    }

    #[test]
    fn set_level_keeps_the_lean_as_the_rest_and_the_stick_let_go_resets_it() {
        let keys = HashSet::new();
        let set = Input::Pad(PadButton::South);
        let p =
            preset(vec![Action::Tilt { stick: Stick::Right, strength: 1.0, rest: Rest::Flat, set_level: Some(set) }]);
        let (mut m, t0, v) = (Mapper::default(), Instant::now(), view());
        let mut t = 0;
        let mut run = |m: &mut Mapper, x: f32, button: bool, steps: u64| {
            let pad = stick_at(x, 0.0, if button { PadButton::South.bit() } else { 0 });
            let mut a = [0.0; 3];
            for _ in 0..steps {
                t += 20;
                if let Some((s, _)) =
                    m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(t)).motion
                {
                    a = s;
                }
            }
            a
        };
        let flat = run(&mut m, 0.0, false, 2);
        let half = run(&mut m, 0.5, false, 30);
        assert!(half[0] < -1.0, "{half:?}");
        // Set while the stick still leans it: it stays put, through the stick being let go.
        assert!(near(run(&mut m, 0.5, true, 1), half));
        assert!(near(run(&mut m, 0.5, false, 10), half));
        assert!(near(run(&mut m, 0.0, false, 30), half));
        // From there, the stick leans it further.
        assert!(run(&mut m, 0.5, false, 30)[0] < half[0] - 1.0);
        // Set with the stick let go: back to flat.
        run(&mut m, 0.0, false, 30);
        run(&mut m, 0.0, true, 1);
        assert!(near(run(&mut m, 0.0, false, 30), flat), "{flat:?}");
    }

    /// The controller turning, read through its sensors, turns the device the same way in both: the device's
    /// gyroscope agrees with its accelerometer (d(a)/dt = -ω × a), however the device is turned on the desktop.
    #[test]
    fn controller_motion_keeps_gyro_and_accelerometer_agreeing() {
        let cross = |a: [f64; 3], b: [f64; 3]| {
            [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
        };
        let dt = 1e-4;
        for r in [Rotation::Portrait, Rotation::HomeRight, Rotation::HomeLeft] {
            for (a0, w) in [
                ([0.0, GRAVITY, 0.0], [0.0, 0.0, -1.0]),
                ([-4.9, 8.5, 0.0], [0.8, 0.0, 0.0]),
                ([2.0, 8.0, -5.0], [0.3, -0.7, 0.5]),
            ] {
                // The controller's own reading a moment later: gravity's reaction turns against it.
                let c = cross(w, a0);
                let a1 = [a0[0] - c[0] * dt, a0[1] - c[1] * dt, a0[2] - c[2] * dt];
                let mut m = Mapper::default();
                let to32 = |v: [f64; 3]| v.map(|x| x as f32);
                let (d0, wd) = m.motion_from_sensors(to32(a0), Some(to32(w)), 1.0, r);
                let (d1, _) = m.motion_from_sensors(to32(a1), Some(to32(w)), 1.0, r);
                let da = [(d1[0] - d0[0]) / dt, (d1[1] - d0[1]) / dt, (d1[2] - d0[2]) / dt];
                let want = cross(wd, d0).map(|x| -x);
                assert!(da.iter().zip(want).all(|(x, y)| (x - y).abs() < 0.05 * GRAVITY), "{r:?}: {da:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn the_controller_level_is_the_device_flat_and_its_far_edge_its_top() {
        let mut m = Mapper::default();
        let (a, _) = m.motion_from_sensors([0.0, GRAVITY as f32, 0.0], None, 1.0, Rotation::HomeRight);
        assert!(a.iter().zip(FLAT).all(|(x, y)| (x - y).abs() < 1e-6), "flat in any rotation: {a:?}");
        // Its far edge (the triggers) raised 20 degrees: gravity's reaction leans toward its near edge (+z).
        let t = 20f64.to_radians();
        let raised = [0.0, (GRAVITY * t.cos()) as f32, (-GRAVITY * t.sin()) as f32];
        let (a, _) = m.motion_from_sensors(raised, None, 1.0, Rotation::Portrait);
        // The device's top raised, as a board's: a ball on it rolls toward the bottom.
        assert!((a[1] - GRAVITY * t.sin()).abs() < 1e-3 && (a[2] - GRAVITY * t.cos()).abs() < 1e-3, "{a:?}");
    }

    #[test]
    fn motion_without_sensor_readings_leaves_the_stick_tilt_working() {
        // Sensors quiet (the snapshot leaves them out): the stick's tilt still leans the device.
        let p = preset(vec![
            Action::Motion { sensitivity: 1.0 },
            Action::Tilt { stick: Stick::Left, strength: 1.0, rest: Rest::Upright, set_level: None },
        ]);
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
        // With readings, the controller's own lean wins: lying flat (level) is the device lying flat.
        let flat = Merged { accel: Some([0.0, GRAVITY as f32, 0.0]), gyro: Some([0.0; 3]), ..right };
        let out = m.step(&p, &Inputs { pad: &flat, keys: &keys }, &view(), start + Duration::from_secs(2));
        let a = out.motion.unwrap().0;
        assert!(a.iter().zip(FLAT).all(|(x, y)| (x - y).abs() < 1e-6), "{a:?}");
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
            follow: None,
        }]);
        let mut m = Mapper::default();
        let keys = HashSet::new();
        let now = Instant::now();
        let mut pad = Merged { axes: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() };
        m.step(&p, &Inputs { pad: &pad, keys: &keys }, &view(), now);
        assert_eq!(m.pointer(0), Some(((1.0, 0.5), false)));
        pad.buttons = PadButton::RightTrigger.bit();
        let out = m.step(&p, &Inputs { pad: &pad, keys: &keys }, &view(), now);
        assert_eq!((out.contacts[0].phase, out.contacts[0].x), (DOWN, (1.0 - EDGE) as f64));
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
            tap: None,
            tap_lift: TAP_LIFT_MS,
            speed: 1.0,
        }]);
        let mut m = Mapper::default();
        let keys = HashSet::new();
        let now = Instant::now();
        let right = Merged { buttons: PadButton::DpadRight.bit(), ..Default::default() };
        let down = m.step(&p, &Inputs { pad: &right, keys: &keys }, &view(), now).contacts;
        assert_eq!((down[0].phase, down[0].x, down[0].y), (DOWN, 0.2f32 as f64, 0.8f32 as f64));
        let moved = m.step(&p, &Inputs { pad: &right, keys: &keys }, &view(), now + Duration::from_millis(60)).contacts;
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
        // Home on the right: the screen's right is the guest's bottom, so pushing right lowers the bottom edge, from
        // upright (the right edge up, +x) toward it lying on that edge.
        let p = preset(vec![Action::Tilt { stick: Stick::Dpad, strength: 1.0, rest: Rest::Upright, set_level: None }]);
        let mut m = Mapper::default();
        let keys = HashSet::new();
        let v = View { rotation: Rotation::HomeRight, turned: false, aspect: 1.5 };
        let right = Merged { buttons: PadButton::DpadRight.bit(), ..Default::default() };
        let t0 = Instant::now();
        m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, t0);
        let out = m.step(&p, &Inputs { pad: &right, keys: &keys }, &v, t0 + Duration::from_millis(100));
        let (a, _) = out.motion.unwrap();
        assert!(a[1] > 0.0 && a[0] > 0.0 && a[0] < GRAVITY && a[2].abs() < 1e-9, "{a:?}");
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
        // Not held: the device lies flat, the controller's level, while a controller with sensors is there.
        assert_eq!(m.step(&p, &Inputs { pad: &free, keys: &keys }, &v, at(0)).motion.unwrap().0, FLAT);
        assert!(m.holds_motion());
        let (a, _) = m.step(&p, &Inputs { pad: &held, keys: &keys }, &v, at(10)).motion.unwrap();
        assert!(a[0] < -4.0, "{a:?}");
        // Let go: level, for a moment, then nothing more.
        for ms in [20, 60, 100] {
            let (a, _) = m.step(&p, &Inputs { pad: &free, keys: &keys }, &v, at(ms)).motion.unwrap();
            assert_eq!(a, FLAT);
        }
        assert!(m.step(&p, &Inputs { pad: &free, keys: &keys }, &v, at(200)).motion.is_none());
    }

    #[test]
    fn motion_holds_its_lean_while_the_sensors_pause_and_levels_when_let_go() {
        let mut p = preset(vec![Action::Motion { sensitivity: 1.0 }]);
        p.bindings[0].with = Some(Input::Pad(PadButton::LeftShoulder));
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let lean = [-(GRAVITY as f32) * 0.5, GRAVITY as f32 * 0.866, 0.0];
        let shoulder = PadButton::LeftShoulder.bit();
        let live = Merged { buttons: shoulder, accel: Some(lean), ..Default::default() };
        let quiet = Merged { buttons: shoulder, sensors_quiet: true, ..Default::default() };
        let at = |ms| t0 + Duration::from_millis(ms);
        let (leaning, _) = m.step(&p, &Inputs { pad: &live, keys: &keys }, &v, at(0)).motion.unwrap();
        // Quiet for a second (Steam Input took the sensors, say): nothing sent, so the device stays leaning.
        for ms in (10..1000).step_by(50) {
            assert!(m.step(&p, &Inputs { pad: &quiet, keys: &keys }, &v, at(ms)).motion.is_none(), "{ms} ms");
        }
        let (back, _) = m.step(&p, &Inputs { pad: &live, keys: &keys }, &v, at(1000)).motion.unwrap();
        assert_eq!(back, leaning);
        // Quiet again, and the hold button let go: level, as when let go while reporting.
        m.step(&p, &Inputs { pad: &quiet, keys: &keys }, &v, at(1010));
        let released = Merged { sensors_quiet: true, ..Default::default() };
        let (a, _) = m.step(&p, &Inputs { pad: &released, keys: &keys }, &v, at(1020)).motion.unwrap();
        assert_eq!(a, FLAT);
        // The controller gone (no sensors at all) while engaged: the app's own rest, upright as shown.
        let mut m = Mapper::default();
        m.step(&p, &Inputs { pad: &live, keys: &keys }, &v, at(0));
        let gone = Merged { buttons: shoulder, ..Default::default() };
        let (a, _) = m.step(&p, &Inputs { pad: &gone, keys: &keys }, &v, at(10)).motion.unwrap();
        assert_eq!(a, rest(Rotation::Portrait));
    }

    fn stick_at(x: f32, y: f32, buttons: u32) -> Merged {
        Merged { axes: [0.0, 0.0, x, y, 0.0, 0.0], buttons, ..Default::default() }
    }

    fn near(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
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
            tap: None,
            tap_lift: TAP_LIFT_MS,
            speed: 1.0,
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
            tap: None,
            tap_lift: TAP_LIFT_MS,
            speed: 1.0,
        }]);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let r3 = PadButton::RightStick.bit();
        let mut ms = 0;
        let mut step = |pad: Merged| {
            ms += 100;
            phases(&m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(ms)))
        };
        // Pushing alone does nothing; R3 lands it on the centre, the stick pushes it, and it stays at rest.
        assert!(step(stick_at(1.0, 0.0, 0)).is_empty());
        assert_eq!(step(stick_at(0.0, 0.0, r3)), [DOWN]);
        assert_eq!(step(stick_at(1.0, 0.0, 0)), [MOVE]);
        assert_eq!(step(stick_at(0.0, 0.0, 0)), [MOVE]);
        assert!(step(stick_at(0.0, 0.0, 0)).is_empty());
        assert_eq!(step(stick_at(0.0, 0.0, r3)), [UP]);
    }

    fn joystick(press: Press, tap: Option<Input>) -> Preset {
        preset(vec![Action::Joystick {
            stick: Stick::Right,
            center: Point::new(0.5, 0.5),
            radius: 0.1,
            press,
            tap,
            tap_lift: TAP_LIFT_MS,
            speed: 1.0,
        }])
    }

    #[test]
    fn a_joystick_let_go_at_once_glides_back_to_its_centre() {
        let p = joystick(Press::toggle_for(&Stick::Right), None);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let at = |ms| t0 + Duration::from_millis(ms);
        let r3 = PadButton::RightStick.bit();
        let mut step = |pad: Merged, ms| m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, at(ms));
        step(stick_at(0.0, 0.0, r3), 0);
        // Pushed all the way and held: it gets there in a few frames, not one.
        let mut xs = Vec::new();
        for ms in (17..=170).step_by(17) {
            let out = step(stick_at(1.0, 0.0, 0), ms);
            xs.extend(out.contacts.iter().map(|c| c.x));
        }
        assert!(xs.len() >= 3 && (xs.last().unwrap() - 0.6).abs() < 1e-5, "{xs:?}");
        // Let go in a frame: it slides back through the middle, busy until it's on the centre, and stays down.
        let mut back = Vec::new();
        let mut busy = true;
        for ms in (187..=340).step_by(17) {
            let out = step(stick_at(0.0, 0.0, 0), ms);
            assert!(out.contacts.iter().all(|c| c.phase == MOVE));
            back.extend(out.contacts.iter().map(|c| c.x));
            busy = out.busy;
        }
        assert!(back.len() >= 3 && back.windows(2).all(|w| w[1] < w[0]), "{back:?}");
        assert!(back.iter().any(|x| *x > 0.52 && *x < 0.58), "{back:?}");
        assert!((back.last().unwrap() - 0.5).abs() < 1e-6 && !busy);
    }

    /// Pushed all at once (a D-pad, keys, a flick): down on the centre, held there a moment, then out. A game reading
    /// touches once a frame first sees the finger on the centre.
    #[test]
    fn a_joystick_pushed_at_once_lands_on_its_centre_and_stays_a_moment() {
        let p = joystick(Press::Pushed, None);
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut step = |ms| {
            let out = m.step(&p, &Inputs { pad: &stick_at(1.0, 0.0, 0), keys: &keys }, &v, at(ms));
            (out.contacts.iter().map(|c| (c.phase, c.x)).collect::<Vec<_>>(), out.busy)
        };
        assert_eq!(step(0), (vec![(DOWN, 0.5)], true));
        for ms in [8, 16, 33, 49] {
            assert_eq!(step(ms), (vec![], true), "{ms} ms: still on the centre");
        }
        let (moved, _) = step(58);
        assert!(moved.len() == 1 && moved[0].0 == MOVE && moved[0].1 > 0.5, "{moved:?}");
    }

    /// Speed sets how fast the finger follows: at a half, it's halfway when it would have arrived.
    #[test]
    fn a_joystick_at_half_speed_follows_half_as_fast() {
        let reach_after = |speed: f32| {
            let p = preset(vec![Action::Joystick {
                stick: Stick::Right,
                center: Point::new(0.5, 0.5),
                radius: 0.1,
                press: Press::Pushed,
                tap: None,
                tap_lift: TAP_LIFT_MS,
                speed,
            }]);
            let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
            let mut x = 0.5;
            for ms in (0..=100).step_by(10) {
                let pad = stick_at(1.0, 0.0, 0);
                let out = m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(ms));
                x = out.contacts.last().map_or(x, |c| c.x);
            }
            x - 0.5
        };
        // Landed at 0, on the centre until 50 ms, then following from the 50 ms step on (six 10 ms steps): all the way
        // at 1 (it would take 50 ms), 60% of the way at 0.5.
        let (full, half) = (reach_after(1.0), reach_after(0.5));
        assert!((full - 0.1).abs() < 1e-4 && (half - 0.06).abs() < 1e-3, "{full} {half}");
    }

    #[test]
    fn a_joystick_tap_lifts_the_finger_taps_the_centre_and_lands_again() {
        let rt = Input::Pad(PadButton::RightTrigger);
        let p = joystick(Press::Pushed, Some(rt));
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let at = |ms| t0 + Duration::from_millis(ms);
        let fire = PadButton::RightTrigger.bit();
        let mut step = |pad: Merged, ms| {
            let out = m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, at(ms));
            out.contacts.iter().map(|c| (c.phase, c.x)).collect::<Vec<_>>()
        };
        step(stick_at(1.0, 0.0, 0), 0);
        step(stick_at(1.0, 0.0, 0), 100);
        // Fire while aiming right: the finger lifts where it was, then taps the centre while the button is held.
        assert_eq!(step(stick_at(1.0, 0.0, fire), 110), [(UP, 0.6f32 as f64)]);
        assert_eq!(step(stick_at(1.0, 0.0, fire), 170), []);
        assert_eq!(step(stick_at(1.0, 0.0, fire), 180), [(DOWN, 0.5)]);
        assert_eq!(step(stick_at(1.0, 0.0, fire), 300), []);
        assert_eq!(step(stick_at(1.0, 0.0, 0), 320), [(UP, 0.5)]);
        // Still aiming: a moment later it lands on the centre and glides back out.
        assert_eq!(step(stick_at(1.0, 0.0, 0), 380), []);
        assert_eq!(step(stick_at(1.0, 0.0, 0), 390), [(DOWN, 0.5)]);
        let out = step(stick_at(1.0, 0.0, 0), 460);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, MOVE);
        assert!((out[0].1 - 0.6).abs() < 1e-5);
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
            tap_lift: TAP_LIFT_MS,
            mouse: false,
            mouse_speed: 1.0,
            free: None,
        }])
    }

    #[test]
    fn the_mouse_drags_the_look_finger_and_carries_on_past_a_landing() {
        let mut p = look(Press::Pushed, None);
        if let Action::Look { mouse, .. } = &mut p.bindings[0].action {
            *mouse = true;
        }
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let at = |ms| t0 + Duration::from_millis(ms);
        let moved = |dx: f32, dy: f32| Merged { mouse: (dx, dy), ..Default::default() };
        // The first movement lands the finger (set back from the way it's going), the next drags it.
        assert_eq!(phases(&m.step(&p, &Inputs { pad: &moved(10.0, 0.0), keys: &keys }, &v, at(0))), [DOWN]);
        let landed = m.states[0].finger.unwrap();
        m.step(&p, &Inputs { pad: &moved(0.0, 0.0), keys: &keys }, &v, at(16));
        // That first movement was kept: 10 counts, then 50 more: 60 thousandths of the width.
        m.step(&p, &Inputs { pad: &moved(50.0, 0.0), keys: &keys }, &v, at(32));
        let (w, _) = v.span(1.0);
        assert!((m.states[0].finger.unwrap().0 - (landed.0 + 0.06 * w)).abs() < 1e-4);
        // A flick wider than the whole area: as far as it goes, lift, land again and carry on, until all of it is
        // given; nothing is lost on the way.
        let mut travelled = 0.0;
        let mut last = m.states[0].finger;
        let mut ms = 48;
        let mut pad = moved(450.0, 0.0);
        while ms < 400 {
            let out = m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, at(ms));
            pad = moved(0.0, 0.0);
            for c in &out.contacts {
                if c.phase == MOVE {
                    travelled += c.x as f32 - last.unwrap().0;
                }
                last = if c.phase == UP { None } else { Some((c.x as f32, c.y as f32)) };
            }
            ms += 16;
        }
        assert!((travelled - 0.45 * w).abs() < 1e-3, "{travelled}");
        // Still: it rests, then lifts.
        assert_eq!(phases(&m.step(&p, &Inputs { pad: &moved(0.0, 0.0), keys: &keys }, &v, at(800))), [UP]);
        // Turned off, the mouse does nothing.
        if let Action::Look { mouse, .. } = &mut p.bindings[0].action {
            *mouse = false;
        }
        assert!(m.step(&p, &Inputs { pad: &moved(30.0, 0.0), keys: &keys }, &v, at(600)).contacts.is_empty());
    }

    #[test]
    fn keys_looking_start_slow_and_build_to_full_speed() {
        let mut p = look(Press::Pushed, None);
        if let Action::Look { stick, half, speed, .. } = &mut p.bindings[0].action {
            *stick = Stick::wasd();
            *half = [0.5, 0.5];
            // Slow enough not to reach the edge (and land again) in the time.
            *speed = 0.2;
        }
        let (mut m, v, t0) = (Mapper::default(), view(), Instant::now());
        let d: HashSet<String> = ["D".to_string()].into();
        let at = |ms| t0 + Duration::from_millis(ms);
        let x = |m: &Mapper| m.states[0].finger.map(|f| f.0);
        m.step(&p, &Inputs { pad: &Merged::default(), keys: &d }, &v, at(0));
        let start = x(&m).unwrap();
        // The first 50 ms against 50 ms once it's built up: a tap moves the camera a little.
        let mut last = start;
        let mut early = 0.0;
        for ms in (10..=400).step_by(10) {
            m.step(&p, &Inputs { pad: &Merged::default(), keys: &d }, &v, at(ms));
            let now = x(&m).unwrap();
            if ms == 50 {
                early = now - start;
            }
            if ms == 350 {
                last = now;
            }
        }
        let late = x(&m).unwrap() - last;
        assert!(early > 0.0 && late > early * 1.8, "{early} {late}");
    }

    #[test]
    fn keys_glide_a_centered_pointer_set_to_jump() {
        let p = preset(vec![Action::Pointer {
            stick: Stick::arrows(),
            center: Point::CENTER,
            half: [0.5, 0.5],
            touch: Input::Key("Space".into()),
            toggle: false,
            style: PointerStyle::Centered,
            speed: 0.9,
            follow: None,
        }]);
        let (mut m, v, t0) = (Mapper::default(), view(), Instant::now());
        let none = HashSet::new();
        let right: HashSet<String> = ["ArrowRight".to_string()].into();
        m.step(&p, &Inputs { pad: &Merged::default(), keys: &none }, &v, t0);
        let out = m.step(&p, &Inputs { pad: &Merged::default(), keys: &right }, &v, t0 + Duration::from_millis(16));
        let (u, _) = m.states[0].pointer.unwrap();
        assert!(u > 0.5 && u < 0.6 && out.busy, "{u}");
        for ms in (32..=400).step_by(16) {
            m.step(&p, &Inputs { pad: &Merged::default(), keys: &right }, &v, t0 + Duration::from_millis(ms));
        }
        assert!((m.states[0].pointer.unwrap().0 - 1.0).abs() < 1e-4);
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

    /// Steps a look binding with the stick pushed right, its tap (RightShoulder) held on the given 8 ms steps, and
    /// returns each step's contacts.
    fn look_taps(held: impl Fn(u64) -> bool, steps: u64) -> Vec<Vec<u32>> {
        let p = look(Press::Pushed, Some(Input::Pad(PadButton::RightShoulder)));
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        (0..steps)
            .map(|i| {
                let pad = stick_at(1.0, 0.0, if held(i) { PadButton::RightShoulder.bit() } else { 0 });
                phases(&m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(8 * i)))
            })
            .collect()
    }

    /// The steps where the binding's finger goes down and up.
    fn downs_ups(all: &[Vec<u32>]) -> (Vec<usize>, Vec<usize>) {
        let at = |ph| all.iter().enumerate().filter(|(_, c)| c.contains(&ph)).map(|(i, _)| i).collect();
        (at(DOWN), at(UP))
    }

    /// How many 8 ms steps `d` takes.
    fn steps(d: Duration) -> usize {
        (d.as_millis() as usize).div_ceil(8)
    }

    #[test]
    fn a_look_tap_lifts_the_finger_taps_where_it_was_and_looks_on() {
        let (gap, least) = (steps(lift(TAP_LIFT_MS)), steps(STICK_TAP));
        // Looking from step 0; a click on steps 10 and 11.
        let all = look_taps(|i| (10..12).contains(&i), 100);
        let (downs, ups) = downs_ups(&all);
        // The look's finger lifts on the click, the tap lands a gap later and stays down its least, lifts, and the
        // look lands again a gap after that.
        assert_eq!(ups[0], 10, "{all:?}");
        assert_eq!(downs[1], 10 + gap, "{all:?}");
        assert_eq!(ups[1], 10 + gap + least, "{all:?}");
        assert_eq!(downs[2], 10 + gap + least + gap, "{all:?}");
        // Nothing else until the look nears its area's edge.
        assert!(downs.len() == 3 || downs[3] > 50, "{all:?}");
        assert!(ups.len() == 2 || ups[2] > 50, "{all:?}");
    }

    #[test]
    fn the_tap_lift_is_the_bindings_own() {
        let mut p = look(Press::Pushed, Some(Input::Pad(PadButton::RightShoulder)));
        if let Action::Look { tap_lift, .. } = &mut p.bindings[0].action {
            *tap_lift = 120;
        }
        let (mut m, keys, v, t0) = (Mapper::default(), HashSet::new(), view(), Instant::now());
        let mut down_at = None;
        for i in 0..40u64 {
            let pad = stick_at(1.0, 0.0, if i == 10 { PadButton::RightShoulder.bit() } else { 0 });
            let out = m.step(&p, &Inputs { pad: &pad, keys: &keys }, &v, t0 + Duration::from_millis(8 * i));
            if i > 10 && phases(&out).contains(&DOWN) {
                down_at.get_or_insert(i as usize);
            }
        }
        assert_eq!(down_at, Some(10 + steps(Duration::from_millis(120))));
        // Older presets have none, and get the usual one.
        let json = serde_json::to_string(&look(Press::Pushed, None)).unwrap();
        assert!(!json.contains("tap_lift"));
    }

    #[test]
    fn a_held_tap_stays_down_until_let_go() {
        let all = look_taps(|i| (10..60).contains(&i), 100);
        let (downs, ups) = downs_ups(&all);
        assert_eq!((downs[1], ups[1]), (10 + steps(lift(TAP_LIFT_MS)), 60), "{all:?}");
    }

    #[test]
    fn fast_clicks_tap_once_more_at_most_and_none_is_lost_between_taps() {
        let (gap, least) = (steps(lift(TAP_LIFT_MS)), steps(STICK_TAP));
        let first = 10 + gap;
        // Three clicks while the first tap is down make one more tap, not three.
        let clicks = [10, first + 1, first + 3, first + 5];
        let all = look_taps(|i| clicks.contains(&(i as usize)), 140);
        let (downs, ups) = downs_ups(&all);
        let second = first + least + gap;
        assert_eq!(downs[1..3], [first, second], "{all:?}");
        // Then the look lands again, and nothing else.
        let before = |v: &[usize]| v.iter().filter(|&&i| i < second + least + gap + 20).count();
        assert_eq!((before(&downs), before(&ups)), (4, 3), "{all:?}");
        // A click just after a tap lifted (while it waits to look again) still taps, once the lift has shown.
        let all = look_taps(|i| i == 10 || i as usize == first + least + 2, 140);
        let (downs, _) = downs_ups(&all);
        assert_eq!(downs[1..3], [first, second], "{all:?}");
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

//! Controller presets: what each controller or keyboard input does to the device. Saved one per file as JSON in the
//! data folder's `controllers/`. Places on the screen are in the guest's portrait coordinates (0..1, x right, y down:
//! the device held upright), so a spot stays on the same part of the game however the window turns the device; sizes
//! are fractions of the screen's shorter side.
use serde::{Deserialize, Serialize};

/// A controller button, a trigger pulled past halfway, or a keyboard key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Input {
    Pad(PadButton),
    /// egui's key name ("Space", "A", "ArrowLeft").
    Key(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadButton {
    South,
    East,
    West,
    North,
    Back,
    Guide,
    Start,
    LeftStick,
    RightStick,
    LeftShoulder,
    RightShoulder,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    Misc,
    Paddle1,
    Paddle2,
    Paddle3,
    Paddle4,
    Touchpad,
    LeftTrigger,
    RightTrigger,
}

impl PadButton {
    pub const ALL: [PadButton; 23] = [
        Self::South,
        Self::East,
        Self::West,
        Self::North,
        Self::Back,
        Self::Guide,
        Self::Start,
        Self::LeftStick,
        Self::RightStick,
        Self::LeftShoulder,
        Self::RightShoulder,
        Self::DpadUp,
        Self::DpadDown,
        Self::DpadLeft,
        Self::DpadRight,
        Self::Misc,
        Self::Paddle1,
        Self::Paddle2,
        Self::Paddle3,
        Self::Paddle4,
        Self::Touchpad,
        Self::LeftTrigger,
        Self::RightTrigger,
    ];

    /// Its bit in [`super::sdl::Pad::buttons`].
    pub fn bit(self) -> u32 {
        match self {
            Self::LeftTrigger => super::sdl::TRIGGER_LEFT_BIT,
            Self::RightTrigger => super::sdl::TRIGGER_RIGHT_BIT,
            b => 1 << (b as u32),
        }
    }

    /// What the controller prints on it.
    pub fn label(self, kind: super::sdl::PadKind) -> &'static str {
        use super::sdl::PadKind::*;
        match (self, kind) {
            (Self::South, PlayStation) => "Cross",
            (Self::East, PlayStation) => "Circle",
            (Self::West, PlayStation) => "Square",
            (Self::North, PlayStation) => "Triangle",
            (Self::South, Nintendo) => "B",
            (Self::East, Nintendo) => "A",
            (Self::West, Nintendo) => "Y",
            (Self::North, Nintendo) => "X",
            (Self::South, Xbox) => "A",
            (Self::East, Xbox) => "B",
            (Self::West, Xbox) => "X",
            (Self::North, Xbox) => "Y",
            (Self::Back, PlayStation) => "Share",
            (Self::Back, Nintendo) => "−",
            (Self::Back, Xbox) => "View",
            (Self::Start, PlayStation) => "Options",
            (Self::Start, Nintendo) => "+",
            (Self::Start, Xbox) => "Menu",
            (Self::Guide, PlayStation) => "PS",
            (Self::Guide, Nintendo) => "Home",
            (Self::Guide, Xbox) => "Guide",
            (Self::LeftStick, PlayStation) => "L3",
            (Self::RightStick, PlayStation) => "R3",
            (Self::LeftStick, _) => "LS",
            (Self::RightStick, _) => "RS",
            (Self::LeftShoulder, PlayStation) => "L1",
            (Self::RightShoulder, PlayStation) => "R1",
            (Self::LeftShoulder, Nintendo) => "L",
            (Self::RightShoulder, Nintendo) => "R",
            (Self::LeftShoulder, Xbox) => "LB",
            (Self::RightShoulder, Xbox) => "RB",
            (Self::LeftTrigger, PlayStation) => "L2",
            (Self::RightTrigger, PlayStation) => "R2",
            (Self::LeftTrigger, Nintendo) => "ZL",
            (Self::RightTrigger, Nintendo) => "ZR",
            (Self::LeftTrigger, Xbox) => "LT",
            (Self::RightTrigger, Xbox) => "RT",
            (Self::DpadUp, _) => "D-pad up",
            (Self::DpadDown, _) => "D-pad down",
            (Self::DpadLeft, _) => "D-pad left",
            (Self::DpadRight, _) => "D-pad right",
            (Self::Misc, PlayStation) => "Mic",
            (Self::Misc, Nintendo) => "Capture",
            (Self::Misc, Xbox) => "Share",
            (Self::Paddle1, _) => "P1",
            (Self::Paddle2, _) => "P2",
            (Self::Paddle3, _) => "P3",
            (Self::Paddle4, _) => "P4",
            (Self::Touchpad, _) => "Touchpad",
        }
    }
}

impl Input {
    pub fn label(&self, kind: super::sdl::PadKind) -> String {
        match self {
            Input::Pad(b) => b.label(kind).to_string(),
            Input::Key(k) => key_label(k),
        }
    }
}

/// The mouse's buttons, bound like keys: they act while the mouse is locked to the game (a mouse-look binding).
pub const MOUSE_LEFT: &str = "MouseLeft";
pub const MOUSE_RIGHT: &str = "MouseRight";
pub const MOUSE_MIDDLE: &str = "MouseMiddle";

/// Modifier keys, bound like keys (egui's keys don't include them).
pub const KEY_CTRL: &str = "Ctrl";
pub const KEY_SHIFT: &str = "Shift";
pub const KEY_ALT: &str = "Alt";

/// A key's name as people say it.
pub fn key_label(key: &str) -> String {
    match key {
        "ArrowUp" => "↑".into(),
        "ArrowDown" => "↓".into(),
        "ArrowLeft" => "←".into(),
        "ArrowRight" => "→".into(),
        "Space" => "Space".into(),
        "Enter" => "Enter".into(),
        MOUSE_LEFT => "Left click".into(),
        MOUSE_RIGHT => "Right click".into(),
        MOUSE_MIDDLE => "Middle click".into(),
        k => k.to_string(),
    }
}

/// Something that gives a direction: a stick, the D-pad, or four keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stick {
    Left,
    Right,
    Dpad,
    Keys { up: String, down: String, left: String, right: String },
}

impl Stick {
    pub fn wasd() -> Stick {
        Stick::Keys { up: "W".into(), down: "S".into(), left: "A".into(), right: "D".into() }
    }

    pub fn arrows() -> Stick {
        Stick::Keys {
            up: "ArrowUp".into(),
            down: "ArrowDown".into(),
            left: "ArrowLeft".into(),
            right: "ArrowRight".into(),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Stick::Left => "Left stick".into(),
            Stick::Right => "Right stick".into(),
            Stick::Dpad => "D-pad".into(),
            Stick::Keys { up, left, down, right } => {
                if [up, left, down, right] == [&"W".to_string(), &"A".into(), &"S".into(), &"D".into()] {
                    "W A S D".into()
                } else if up == "ArrowUp" && down == "ArrowDown" && left == "ArrowLeft" && right == "ArrowRight" {
                    "Arrow keys".into()
                } else {
                    format!("{} {} {} {}", key_label(up), key_label(left), key_label(down), key_label(right))
                }
            }
        }
    }

    /// Pressing the stick in (L3, R3): the natural button for a stick's own finger. None for the D-pad and keys.
    pub fn click(&self) -> Option<Input> {
        match self {
            Stick::Left => Some(Input::Pad(PadButton::LeftStick)),
            Stick::Right => Some(Input::Pad(PadButton::RightStick)),
            Stick::Dpad | Stick::Keys { .. } => None,
        }
    }

    /// The short form drawn on the screen.
    pub fn short(&self) -> String {
        match self {
            Stick::Left => "L".into(),
            Stick::Right => "R".into(),
            Stick::Dpad => "+".into(),
            Stick::Keys { .. } => "⌨".into(),
        }
    }
}

/// A point in the guest's portrait coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const CENTER: Point = Point { x: 0.5, y: 0.5 };
    pub fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TapMode {
    /// The finger stays down while the input is held.
    #[default]
    Hold,
    /// A press puts the finger down, the next lifts it.
    Toggle,
    /// Taps over and over while held.
    Repeat,
}

/// When a stick binding's finger is on the screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Press {
    /// While the stick is pushed well enough (a nudge or drift doesn't touch).
    #[default]
    Pushed,
    /// While `input` is held.
    Hold { input: Input },
    /// A press of `input` puts the finger down, the next lifts it.
    Toggle { input: Input },
}

impl Press {
    pub fn input(&self) -> Option<&Input> {
        match self {
            Press::Pushed => None,
            Press::Hold { input } | Press::Toggle { input } => Some(input),
        }
    }

    pub fn input_mut(&mut self) -> Option<&mut Input> {
        match self {
            Press::Pushed => None,
            Press::Hold { input } | Press::Toggle { input } => Some(input),
        }
    }

    /// Pressed with the stick's click, or with nothing yet where the stick has none (the D-pad, keys).
    pub fn toggle_for(stick: &Stick) -> Press {
        Press::Toggle { input: stick.click().unwrap_or(Input::Key(String::new())) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PointerStyle {
    /// Dolphin's pointer: the stick's position is the offset from the area's centre, and letting go returns there.
    #[default]
    Centered,
    /// Like a mouse: the stick moves the pointer, and it stays where it was left.
    Cursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceButton {
    Home,
    SleepWake,
    VolumeUp,
    VolumeDown,
}

impl DeviceButton {
    pub const ALL: [DeviceButton; 4] = [Self::Home, Self::SleepWake, Self::VolumeUp, Self::VolumeDown];
    /// The session's button number (0 Home, 1 Power, 2 Volume up, 3 Volume down).
    pub fn number(self) -> u32 {
        self as u32
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::SleepWake => "Sleep/Wake",
            Self::VolumeUp => "Volume up",
            Self::VolumeDown => "Volume down",
        }
    }
}

/// What a binding does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Touch a spot.
    Tap {
        input: Input,
        at: Point,
        #[serde(default)]
        mode: TapMode,
    },
    /// Drag a finger from one spot to another when pressed (Temple Run's turns, a menu's page flip).
    Swipe { input: Input, from: Point, to: Point, ms: u32 },
    /// An on-screen joystick: a finger held on its centre and pushed toward the stick's direction, up to `radius`.
    /// The finger glides to where the stick points rather than jumping there, so a stick let go fast slides back to
    /// the centre the way a thumb does.
    Joystick {
        stick: Stick,
        center: Point,
        radius: f32,
        /// Presets from before this was asked: while pushed.
        #[serde(default)]
        press: Press,
        /// A button that lifts the finger and taps the stick's centre (games that fire with a tap on the stick).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tap: Option<Input>,
        /// How long (ms) the finger stays up before and after a tap, so the game sees it lift.
        #[serde(default = "tap_lift", skip_serializing_if = "is_tap_lift")]
        tap_lift: u32,
        /// How fast the finger follows the stick: at 1 it crosses a full reach in 50 ms; lower for games whose stick
        /// loses a finger dragged fast.
        #[serde(default = "one", skip_serializing_if = "is_one")]
        speed: f32,
    },
    /// Looking around (a game's camera): the stick drags a finger across an area at a speed set by how far it's
    /// pushed, lifting and landing again before it runs off the area's edge.
    Look {
        stick: Stick,
        center: Point,
        /// Half the area's width and height, as fractions of the screen's (as shown).
        half: [f32; 2],
        /// Screen widths (of the shorter side) a second at full tilt.
        speed: f32,
        #[serde(default)]
        invert_y: bool,
        #[serde(default)]
        press: Press,
        /// A button that taps where the finger is (games that fire with a tap on the look area).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tap: Option<Input>,
        /// How long (ms) the finger stays up before and after a tap, so the game sees it lift.
        #[serde(default = "tap_lift", skip_serializing_if = "is_tap_lift")]
        tap_lift: u32,
        /// The mouse looks too, while it's locked to the game (a click on the screen locks it, Escape lets go).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mouse: bool,
        /// The mouse's speed: at 1, a thousand counts of movement drag the finger across the screen's shorter side.
        #[serde(default = "one")]
        mouse_speed: f32,
        /// Held, the locked mouse is a finger on the screen (cursor shown, kept on the screen) instead of looking:
        /// for a game's buttons and menus.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        free: Option<Input>,
    },
    /// A pointer over an area of the screen, and an input that touches where it is. The bubble is always drawn.
    Pointer {
        stick: Stick,
        center: Point,
        /// Half the area's width and height, as fractions of the screen's (as shown).
        half: [f32; 2],
        touch: Input,
        /// `touch` presses to touch and again to lift, rather than touching while held.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        toggle: bool,
        #[serde(default)]
        style: PointerStyle,
        /// Cursor style: screen widths a second at full tilt.
        #[serde(default = "default_speed")]
        speed: f32,
        /// Centered style: how fast the pointer glides to the stick's spot and back, in screen widths a second;
        /// none jumps straight there (as presets from before this setting do).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        follow: Option<f32>,
    },
    /// Flicking the stick swipes from `center` toward it (four directions), `length` long.
    Flick { stick: Stick, center: Point, length: f32 },
    /// Lean the device toward the stick (games steered by tilting).
    Tilt {
        stick: Stick,
        #[serde(default = "one")]
        strength: f32,
        /// How the device rests when the stick is let go.
        #[serde(default, skip_serializing_if = "is_default")]
        rest: crate::tilt::Rest,
        /// A button that keeps the device's lean now as where it rests (pressed with the stick let go: `rest` again).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        set_level: Option<Input>,
    },
    /// The controller's own motion sensors move the device.
    Motion {
        #[serde(default = "one")]
        sensitivity: f32,
    },
    /// The device's Home, Sleep/Wake or volume buttons.
    Device { input: Input, button: DeviceButton },
}

fn default_speed() -> f32 {
    0.9
}

fn one() -> f32 {
    1.0
}

/// A stick binding's tap lift, unless set: a game held to 30 frames a second that drops to 20 still sees each lift
/// (Battle Bears Zombies missed 35 ms).
pub const TAP_LIFT_MS: u32 = 70;

fn tap_lift() -> u32 {
    TAP_LIFT_MS
}

fn is_tap_lift(ms: &u32) -> bool {
    *ms == TAP_LIFT_MS
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

impl Action {
    pub fn title(&self) -> &'static str {
        match self {
            Action::Tap { .. } => "Tap",
            Action::Swipe { .. } => "Swipe",
            Action::Joystick { .. } => "Joystick",
            Action::Look { .. } => "Look around",
            Action::Pointer { .. } => "Pointer",
            Action::Flick { .. } => "Flick to swipe",
            Action::Tilt { .. } => "Tilt",
            Action::Motion { .. } => "Motion",
            Action::Device { .. } => "Device button",
        }
    }

    /// Whether it puts a spot on the screen.
    pub fn placed(&self) -> bool {
        !matches!(self, Action::Tilt { .. } | Action::Motion { .. } | Action::Device { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub action: Action,
    /// Only while this is held too (a shoulder button as a shift).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<Input>,
    /// Nothing of it drawn on the screen while playing (its spot, area, finger or pointer), even with spots on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

impl Binding {
    pub fn new(action: Action) -> Binding {
        Binding { action, with: None, hidden: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub bindings: Vec<Binding>,
    /// Draw the spots on the screen while playing (a pointer's bubble is always drawn). Off unless turned on: spots
    /// over a game puzzle someone who hasn't set up a controller.
    #[serde(default)]
    pub show_spots: bool,
    /// How far a stick moves before it counts, 0..1.
    #[serde(default = "default_deadzone")]
    pub deadzone: f32,
    /// Apps (bundle IDs) this preset switches on for when they come to the front.
    #[serde(default)]
    pub apps: Vec<String>,
}

fn default_deadzone() -> f32 {
    0.15
}

impl Preset {
    pub fn new(name: &str) -> Preset {
        Preset {
            name: name.into(),
            bindings: Vec::new(),
            show_spots: false,
            deadzone: default_deadzone(),
            apps: Vec::new(),
        }
    }

    /// A typical preset, for tests: a pointer on the left stick with the right trigger as the finger, a tap on the
    /// south button and Home on the guide button. (Nothing is set up out of the box.)
    #[cfg(test)]
    pub fn starter() -> Preset {
        let mut p = Preset::new("Default");
        p.bindings = vec![
            Binding::new(Action::Pointer {
                stick: Stick::Left,
                center: Point::CENTER,
                half: [0.5, 0.5],
                touch: Input::Pad(PadButton::RightTrigger),
                toggle: false,
                style: PointerStyle::Centered,
                speed: default_speed(),
                follow: None,
            }),
            Binding::new(Action::Tap {
                input: Input::Pad(PadButton::South),
                at: Point::new(0.5, 0.8),
                mode: TapMode::Hold,
            }),
            Binding::new(Action::Device { input: Input::Pad(PadButton::Guide), button: DeviceButton::Home }),
        ];
        p
    }
}

/// The presets on disk.
pub struct Store {
    dir: std::path::PathBuf,
}

impl Store {
    pub fn new(dir: std::path::PathBuf) -> Store {
        Store { dir }
    }

    pub fn load_all(&self) -> Vec<Preset> {
        let mut out: Vec<Preset> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
            .collect();
        out.sort_by_key(|p: &Preset| p.name.to_lowercase());
        out
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        let slug: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_lowercase();
        self.dir.join(format!("{}.json", if slug.is_empty() { "preset" } else { &slug }))
    }

    pub fn save(&self, preset: &Preset) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let path = self.path(&preset.name);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(preset).map_err(|e| e.to_string())?)
            .and_then(|_| std::fs::rename(&tmp, &path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn remove(&self, name: &str) {
        let _ = std::fs::remove_file(self.path(name));
    }

    /// Write a preset somewhere else (export) or read one in (import).
    pub fn export(preset: &Preset, to: &std::path::Path) -> Result<(), String> {
        std::fs::write(to, serde_json::to_vec_pretty(preset).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    }

    pub fn import(from: &std::path::Path) -> Result<Preset, String> {
        let bytes = std::fs::read(from).map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| format!("not a controller preset: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let p = Preset::starter();
        let text = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Preset>(&text).unwrap(), p);
        // Older files without the optional fields still load.
        let bare: Preset =
            serde_json::from_str(r#"{"name":"x","bindings":[{"action":{"kind":"tilt","stick":"left"}}]}"#).unwrap();
        assert!(!bare.show_spots && bare.deadzone > 0.0);
        assert_eq!(
            bare.bindings[0].action,
            Action::Tilt { stick: Stick::Left, strength: 1.0, rest: Default::default(), set_level: None }
        );
        // A joystick from before its speed follows at 1, and one at 1 is saved without it.
        let old = r#"{"name":"x","bindings":[{"action":{"kind":"joystick","stick":"left","center":{"x":0.2,"y":0.8},"radius":0.15}}]}"#;
        let old: Preset = serde_json::from_str(old).unwrap();
        assert!(matches!(old.bindings[0].action, Action::Joystick { speed: 1.0, .. }));
        assert!(!serde_json::to_string(&old).unwrap().contains("speed"));
    }
}

//! Game controllers through SDL2, loaded at run time: the Windows build ships it beside the engine (which links it),
//! the Linux and macOS packages bundle it, and a development machine has the system's. Without it the controller
//! window says so and keyboard bindings still work.
//!
//! One thread owns SDL: it initializes the game controller and sensor subsystems, opens every controller SDL knows,
//! and polls them about 250 times a second (SDL's event queue is turned off, so `SDL_GameControllerUpdate` is what
//! advances them). Each poll publishes a [`Pads`] snapshot and wakes the UI when anything moved.
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One controller as last read. Axes are -1..1 (sticks; y down) and 0..1 (triggers).
#[derive(Debug, Clone, PartialEq)]
pub struct Pad {
    pub id: i32,
    pub name: String,
    pub kind: PadKind,
    /// SDL_GameControllerButton bits, plus [`TRIGGER_LEFT_BIT`] and [`TRIGGER_RIGHT_BIT`] past the trigger threshold.
    pub buttons: u32,
    /// Left x, left y, right x, right y, left trigger, right trigger.
    pub axes: [f32; 6],
    /// rad/s and m/s² in the controller's axes (x right, y up, z toward the player), when it has the sensors.
    pub gyro: Option<[f32; 3]>,
    pub accel: Option<[f32; 3]>,
}

pub const TRIGGER_LEFT_BIT: u32 = 1 << 30;
pub const TRIGGER_RIGHT_BIT: u32 = 1 << 31;
const TRIGGER_THRESHOLD: f32 = 0.35;

/// Which face-button names the controller prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PadKind {
    #[default]
    Xbox,
    PlayStation,
    Nintendo,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Backend {
    /// Not started yet.
    #[default]
    Starting,
    Ready,
    /// SDL2 isn't on this computer, or wouldn't start: why.
    Unavailable(String),
}

#[derive(Debug, Clone, Default)]
pub struct Pads {
    pub backend: Backend,
    pub pads: Vec<Pad>,
}

type Fn0<R> = unsafe extern "C" fn() -> R;

#[allow(non_snake_case)]
struct Sdl {
    _lib: libloading::Library,
    SDL_SetHint: unsafe extern "C" fn(*const c_char, *const c_char) -> c_int,
    SDL_Init: unsafe extern "C" fn(u32) -> c_int,
    SDL_GetError: Fn0<*const c_char>,
    SDL_NumJoysticks: Fn0<c_int>,
    SDL_IsGameController: unsafe extern "C" fn(c_int) -> c_int,
    SDL_JoystickGetDeviceInstanceID: unsafe extern "C" fn(c_int) -> i32,
    SDL_GameControllerOpen: unsafe extern "C" fn(c_int) -> *mut c_void,
    SDL_GameControllerClose: unsafe extern "C" fn(*mut c_void),
    SDL_GameControllerGetAttached: unsafe extern "C" fn(*mut c_void) -> c_int,
    SDL_GameControllerName: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    SDL_GameControllerGetAxis: unsafe extern "C" fn(*mut c_void, c_int) -> i16,
    SDL_GameControllerGetButton: unsafe extern "C" fn(*mut c_void, c_int) -> u8,
    SDL_GameControllerUpdate: Fn0<()>,
    SDL_GameControllerEventState: unsafe extern "C" fn(c_int) -> c_int,
    SDL_JoystickEventState: unsafe extern "C" fn(c_int) -> c_int,
    // 2.0.12 and 2.0.14: absent from older SDL2s, which then report no type and no sensors.
    SDL_GameControllerGetType: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    SDL_GameControllerHasSensor: Option<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>,
    SDL_GameControllerSetSensorEnabled: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int>,
    SDL_GameControllerGetSensorData: Option<unsafe extern "C" fn(*mut c_void, c_int, *mut f32, c_int) -> c_int>,
    // 2.0.14: the development hook's virtual controller (LIGHTTOUCH_VIRTUAL_PAD).
    SDL_JoystickAttachVirtual: Option<unsafe extern "C" fn(c_int, c_int, c_int, c_int) -> c_int>,
    SDL_JoystickOpen: Option<unsafe extern "C" fn(c_int) -> *mut c_void>,
    SDL_JoystickSetVirtualAxis: Option<unsafe extern "C" fn(*mut c_void, c_int, i16) -> c_int>,
    SDL_JoystickSetVirtualButton: Option<unsafe extern "C" fn(*mut c_void, c_int, u8) -> c_int>,
}

const SDL_INIT_GAMECONTROLLER: u32 = 0x2000;
const SDL_INIT_SENSOR: u32 = 0x8000;
const SDL_SENSOR_ACCEL: c_int = 1;
const SDL_SENSOR_GYRO: c_int = 2;
const BUTTONS: c_int = 21;

/// Where to look for SDL2: the resources folder first (the Windows build's SDL2.dll), then the system's.
fn candidates(resources: &std::path::Path) -> Vec<std::path::PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["SDL2.dll"]
    } else if cfg!(target_os = "macos") {
        &["libSDL2-2.0.0.dylib", "libSDL2.dylib"]
    } else {
        &["libSDL2-2.0.so.0", "libSDL2.so"]
    };
    let mut out = Vec::new();
    // Linux: the system's own SDL2 first, by path (the AppImage's library path would hand back its bundled copy for a
    // bare name), as it matches the system's X11, Wayland and audio libraries; the bundled one when there is none.
    if cfg!(target_os = "linux") {
        for dir in ["/usr/lib/x86_64-linux-gnu", "/usr/lib64", "/usr/lib", "/usr/local/lib"] {
            out.push(std::path::Path::new(dir).join("libSDL2-2.0.so.0"));
        }
    }
    for name in names {
        out.push(resources.join(name));
        out.push(resources.join("../Frameworks").join(name));
        out.push(resources.join("../lib").join(name));
        if cfg!(target_os = "macos") {
            out.push(std::path::Path::new("/opt/homebrew/lib").join(name));
            out.push(std::path::Path::new("/usr/local/lib").join(name));
        }
        out.push(std::path::PathBuf::from(name));
    }
    out
}

impl Sdl {
    fn load(resources: &std::path::Path) -> Result<Sdl, String> {
        let mut last = String::from("not found");
        for path in candidates(resources) {
            if path.components().count() > 1 && !path.exists() {
                continue;
            }
            // SAFETY: SDL2's initializers have no side effects we depend on; symbols are checked below.
            match unsafe { libloading::Library::new(&path) } {
                Ok(lib) => return Self::bind(lib).map_err(|e| format!("{}: {e}", path.display())),
                Err(e) => last = e.to_string(),
            }
        }
        Err(format!("SDL2 isn't installed ({last})"))
    }

    fn bind(lib: libloading::Library) -> Result<Sdl, String> {
        macro_rules! need {
            ($name:literal) => {
                // SAFETY: the signature is SDL2's for this name.
                *unsafe { lib.get(concat!($name, "\0").as_bytes()) }.map_err(|e| format!("{}: {e}", $name))?
            };
        }
        macro_rules! want {
            ($name:literal) => {
                // SAFETY: as above; None on an SDL2 too old to have it.
                unsafe { lib.get(concat!($name, "\0").as_bytes()) }.ok().map(|s| *s)
            };
        }
        Ok(Sdl {
            SDL_SetHint: need!("SDL_SetHint"),
            SDL_Init: need!("SDL_Init"),
            SDL_GetError: need!("SDL_GetError"),
            SDL_NumJoysticks: need!("SDL_NumJoysticks"),
            SDL_IsGameController: need!("SDL_IsGameController"),
            SDL_JoystickGetDeviceInstanceID: need!("SDL_JoystickGetDeviceInstanceID"),
            SDL_GameControllerOpen: need!("SDL_GameControllerOpen"),
            SDL_GameControllerClose: need!("SDL_GameControllerClose"),
            SDL_GameControllerGetAttached: need!("SDL_GameControllerGetAttached"),
            SDL_GameControllerName: need!("SDL_GameControllerName"),
            SDL_GameControllerGetAxis: need!("SDL_GameControllerGetAxis"),
            SDL_GameControllerGetButton: need!("SDL_GameControllerGetButton"),
            SDL_GameControllerUpdate: need!("SDL_GameControllerUpdate"),
            SDL_GameControllerEventState: need!("SDL_GameControllerEventState"),
            SDL_JoystickEventState: need!("SDL_JoystickEventState"),
            SDL_GameControllerGetType: want!("SDL_GameControllerGetType"),
            SDL_GameControllerHasSensor: want!("SDL_GameControllerHasSensor"),
            SDL_GameControllerSetSensorEnabled: want!("SDL_GameControllerSetSensorEnabled"),
            SDL_GameControllerGetSensorData: want!("SDL_GameControllerGetSensorData"),
            SDL_JoystickAttachVirtual: want!("SDL_JoystickAttachVirtual"),
            SDL_JoystickOpen: want!("SDL_JoystickOpen"),
            SDL_JoystickSetVirtualAxis: want!("SDL_JoystickSetVirtualAxis"),
            SDL_JoystickSetVirtualButton: want!("SDL_JoystickSetVirtualButton"),
            _lib: lib,
        })
    }

    fn error(&self) -> String {
        // SAFETY: SDL_GetError returns a valid C string owned by SDL.
        unsafe { CStr::from_ptr((self.SDL_GetError)()) }.to_string_lossy().into_owned()
    }
}

struct Open {
    id: i32,
    handle: *mut c_void,
    name: String,
    kind: PadKind,
    gyro: bool,
    accel: bool,
}

/// Start the polling thread. `woke` is called when the snapshot changes.
pub fn start(resources: std::path::PathBuf, shared: Arc<Mutex<Pads>>, woke: Arc<dyn Fn() + Send + Sync>) {
    let spawned = std::thread::Builder::new().name("gamepad".into()).spawn(move || {
        let sdl = match Sdl::load(&resources) {
            Ok(sdl) => sdl,
            Err(e) => {
                shared.lock().unwrap().backend = Backend::Unavailable(e);
                woke();
                return;
            }
        };
        let hint = |k: &str, v: &str| {
            let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
            // SAFETY: valid C strings; SDL copies them.
            unsafe { (sdl.SDL_SetHint)(k.as_ptr(), v.as_ptr()) };
        };
        // Controllers keep working while another window has the focus: LightTouch decides when to listen.
        hint("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
        // The PlayStation and Switch drivers that read the motion sensors (the default where SDL has HIDAPI).
        hint("SDL_JOYSTICK_HIDAPI_PS4_RUMBLE", "0");
        // SAFETY: SDL_Init from the thread that will make every other SDL call.
        let ok = unsafe { (sdl.SDL_Init)(SDL_INIT_GAMECONTROLLER | SDL_INIT_SENSOR) } == 0
            // SAFETY: as above.
            || unsafe { (sdl.SDL_Init)(SDL_INIT_GAMECONTROLLER) } == 0;
        if !ok {
            shared.lock().unwrap().backend = Backend::Unavailable(format!("SDL2 didn't start: {}", sdl.error()));
            woke();
            return;
        }
        // SAFETY: plain state switches; updates come from SDL_GameControllerUpdate instead of the event queue.
        unsafe {
            (sdl.SDL_GameControllerEventState)(0);
            (sdl.SDL_JoystickEventState)(0);
        }
        shared.lock().unwrap().backend = Backend::Ready;
        woke();
        poll(&sdl, &shared, &*woke);
    });
    if let Err(e) = spawned {
        eprintln!("gamepad thread: {e}");
    }
}

/// Development hook: `LIGHTTOUCH_VIRTUAL_PAD=FILE` attaches an SDL virtual game controller whose state follows FILE,
/// JSON `{"buttons": [SDL button numbers held], "axes": [six values, -1..1 sticks, 0..1 triggers]}`, re-read when it
/// changes. For checking the controller UI and bindings without a controller.
struct Virtual {
    file: std::path::PathBuf,
    joystick: *mut c_void,
    seen: Option<std::time::SystemTime>,
}

impl Virtual {
    fn attach(sdl: &Sdl) -> Option<Virtual> {
        let file = std::path::PathBuf::from(std::env::var_os("LIGHTTOUCH_VIRTUAL_PAD")?);
        let (attach, open) = (sdl.SDL_JoystickAttachVirtual?, sdl.SDL_JoystickOpen?);
        // SAFETY: SDL's virtual joystick API, from the SDL thread; 1 = SDL_JOYSTICK_TYPE_GAMECONTROLLER.
        let index = unsafe { attach(1, 6, BUTTONS, 0) };
        let joystick = if index >= 0 { unsafe { open(index) } } else { std::ptr::null_mut() };
        (!joystick.is_null()).then_some(Virtual { file, joystick, seen: None })
    }

    fn update(&mut self, sdl: &Sdl) {
        let Ok(modified) = std::fs::metadata(&self.file).and_then(|m| m.modified()) else { return };
        if self.seen == Some(modified) {
            return;
        }
        self.seen = Some(modified);
        let Ok(state) = std::fs::read(&self.file).map(|b| serde_json::from_slice::<serde_json::Value>(&b)) else {
            return;
        };
        let Ok(state) = state else { return };
        let held: Vec<i64> =
            state["buttons"].as_array().map(|a| a.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
        let axes: Vec<f64> =
            state["axes"].as_array().map(|a| a.iter().filter_map(|v| v.as_f64()).collect()).unwrap_or_default();
        let (Some(set_axis), Some(set_button)) = (sdl.SDL_JoystickSetVirtualAxis, sdl.SDL_JoystickSetVirtualButton)
        else {
            return;
        };
        // SAFETY: as in attach.
        unsafe {
            for b in 0..BUTTONS {
                set_button(self.joystick, b, held.contains(&(b as i64)) as u8);
            }
            for a in 0..6 {
                let v = axes.get(a).copied().unwrap_or(if a >= 4 { 0.0 } else { 0.0 });
                let raw = if a >= 4 {
                    (v.clamp(0.0, 1.0) * 65535.0 - 32768.0) as i16
                } else {
                    (v.clamp(-1.0, 1.0) * 32767.0) as i16
                };
                set_axis(self.joystick, a as c_int, raw);
            }
        }
    }
}

fn poll(sdl: &Sdl, shared: &Mutex<Pads>, woke: &dyn Fn()) {
    let mut open: Vec<Open> = Vec::new();
    let mut virtual_pad = Virtual::attach(sdl);
    let mut scanned = Instant::now() - Duration::from_secs(5);
    let mut last: Vec<Pad> = Vec::new();
    loop {
        if let Some(v) = virtual_pad.as_mut() {
            v.update(sdl);
        }
        // SAFETY: SDL calls from the thread that initialized it; handles are closed once detached.
        unsafe { (sdl.SDL_GameControllerUpdate)() };
        if scanned.elapsed() > Duration::from_millis(750) {
            scanned = Instant::now();
            rescan(sdl, &mut open);
        }
        let pads: Vec<Pad> = open.iter().map(|o| read(sdl, o)).collect();
        if pads != last {
            shared.lock().unwrap().pads = pads.clone();
            last = pads;
            woke();
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn rescan(sdl: &Sdl, open: &mut Vec<Open>) {
    // SAFETY: as in poll.
    unsafe {
        open.retain(|o| {
            let attached = (sdl.SDL_GameControllerGetAttached)(o.handle) != 0;
            if !attached {
                (sdl.SDL_GameControllerClose)(o.handle);
            }
            attached
        });
        for index in 0..(sdl.SDL_NumJoysticks)() {
            let id = (sdl.SDL_JoystickGetDeviceInstanceID)(index);
            if (sdl.SDL_IsGameController)(index) == 0 || open.iter().any(|o| o.id == id) {
                continue;
            }
            let handle = (sdl.SDL_GameControllerOpen)(index);
            if handle.is_null() {
                continue;
            }
            let name = (sdl.SDL_GameControllerName)(handle);
            let name = if name.is_null() {
                "Controller".to_string()
            } else {
                CStr::from_ptr(name).to_string_lossy().into_owned()
            };
            let kind = match sdl.SDL_GameControllerGetType.map(|f| f(handle)) {
                Some(3 | 4 | 7) => PadKind::PlayStation,
                Some(5 | 11 | 12 | 13) => PadKind::Nintendo,
                _ => PadKind::Xbox,
            };
            let sensor = |kind: c_int| {
                let has = sdl.SDL_GameControllerHasSensor.is_some_and(|f| f(handle, kind) != 0);
                has && sdl.SDL_GameControllerSetSensorEnabled.is_some_and(|f| f(handle, kind, 1) == 0)
            };
            let (gyro, accel) = (sensor(SDL_SENSOR_GYRO), sensor(SDL_SENSOR_ACCEL));
            open.push(Open { id, handle, name, kind, gyro, accel });
        }
    }
}

fn read(sdl: &Sdl, o: &Open) -> Pad {
    let mut buttons = 0u32;
    let mut axes = [0f32; 6];
    // SAFETY: as in poll; the sensor buffers hold the three floats asked for.
    unsafe {
        for b in 0..BUTTONS {
            if (sdl.SDL_GameControllerGetButton)(o.handle, b) != 0 {
                buttons |= 1 << b;
            }
        }
        for (i, a) in axes.iter_mut().enumerate() {
            let v = (sdl.SDL_GameControllerGetAxis)(o.handle, i as c_int) as f32;
            *a = if i < 4 { (v / 32767.0).clamp(-1.0, 1.0) } else { (v / 32767.0).clamp(0.0, 1.0) };
        }
        let sensor = |on: bool, kind: c_int| {
            let f = sdl.SDL_GameControllerGetSensorData.filter(|_| on)?;
            let mut v = [0f32; 3];
            (f(o.handle, kind, v.as_mut_ptr(), 3) == 0).then_some(v)
        };
        if axes[4] > TRIGGER_THRESHOLD {
            buttons |= TRIGGER_LEFT_BIT;
        }
        if axes[5] > TRIGGER_THRESHOLD {
            buttons |= TRIGGER_RIGHT_BIT;
        }
        // Sensor noise would wake the UI every poll: round to what matters.
        let round = |v: [f32; 3], step: f32| v.map(|x| (x / step).round() * step);
        Pad {
            id: o.id,
            name: o.name.clone(),
            kind: o.kind,
            buttons,
            axes: axes.map(|a| (a * 1000.0).round() / 1000.0),
            gyro: sensor(o.gyro, SDL_SENSOR_GYRO).map(|v| round(v, 0.01)),
            accel: sensor(o.accel, SDL_SENSOR_ACCEL).map(|v| round(v, 0.05)),
        }
    }
}

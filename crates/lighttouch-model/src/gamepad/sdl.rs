//! Game controllers through SDL2, loaded at run time: the Windows build ships it beside the engine (which links it),
//! the Linux and macOS packages bundle it, and a development machine has the system's. Without it the controller
//! window says so and keyboard bindings still work.
//!
//! One thread owns SDL: it initializes the game controller and sensor subsystems, opens every controller SDL knows,
//! and polls them about 250 times a second (SDL's event queue is turned off, so `SDL_GameControllerUpdate` is what
//! advances them). Each poll publishes a [`Pads`] snapshot and wakes the UI when anything moved.
//!
//! Motion sensors can go quiet after they're turned on: something else (Steam Input, another program) puts the
//! controller back into a mode without them, and SDL keeps handing back the last sample. A sensor whose readings
//! stop changing for a moment counts as quiet (the snapshot leaves it out, so bindings don't act on a stale lean)
//! and is turned off and on again. `logs/controllers.log` records what each controller reports.
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
    /// rad/s and m/s² in the controller's axes (x right, y up, z toward the player), while the sensors report.
    pub gyro: Option<[f32; 3]>,
    pub accel: Option<[f32; 3]>,
    pub sensors: Sensors,
    /// Why a controller that has motion sensors doesn't report them here (Linux: access to its devices), when known.
    pub motion_note: Option<String>,
    /// Linux: LightTouch's udev rule (linux_access) would let it open them.
    pub motion_fix: bool,
}

/// The controller's motion sensors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sensors {
    #[default]
    None,
    /// Reporting.
    Live,
    /// Turned on but not reporting (being turned on again).
    Quiet,
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
    SDL_GameControllerGetVendor: Option<unsafe extern "C" fn(*mut c_void) -> u16>,
    SDL_GameControllerHasSensor: Option<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>,
    SDL_GameControllerSetSensorEnabled: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int>,
    SDL_GameControllerGetSensorData: Option<unsafe extern "C" fn(*mut c_void, c_int, *mut f32, c_int) -> c_int>,
    // 2.26: when each sample was taken, which shows whether the sensor still reports.
    SDL_GameControllerGetSensorDataWithTimestamp:
        Option<unsafe extern "C" fn(*mut c_void, c_int, *mut u64, *mut f32, c_int) -> c_int>,
    SDL_GameControllerGetSensorDataRate: Option<unsafe extern "C" fn(*mut c_void, c_int) -> f32>,
    SDL_GetVersion: Option<unsafe extern "C" fn(*mut [u8; 3])>,
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
            SDL_GameControllerGetVendor: want!("SDL_GameControllerGetVendor"),
            SDL_GameControllerHasSensor: want!("SDL_GameControllerHasSensor"),
            SDL_GameControllerSetSensorEnabled: want!("SDL_GameControllerSetSensorEnabled"),
            SDL_GameControllerGetSensorData: want!("SDL_GameControllerGetSensorData"),
            SDL_GameControllerGetSensorDataWithTimestamp: want!("SDL_GameControllerGetSensorDataWithTimestamp"),
            SDL_GameControllerGetSensorDataRate: want!("SDL_GameControllerGetSensorDataRate"),
            SDL_GetVersion: want!("SDL_GetVersion"),
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
    motion_note: Option<String>,
    motion_fix: bool,
    /// The last accelerometer sample (its timestamp, or its values on SDLs without timestamps) and when it changed.
    last_sample: Option<(u64, [f32; 3])>,
    sampled_at: Instant,
    /// When the sensors were last turned on, and how often that has been done.
    enabled_at: Instant,
    restarts: u32,
    live: bool,
    /// New samples since the last note in the log, when that was, and how many notes so far.
    fresh: u32,
    noted_at: Instant,
    notes: u32,
}

/// A sensor quiet this long is turned off and on again, at most this often.
const QUIET: Duration = Duration::from_millis(600);
const RESTART_EVERY: Duration = Duration::from_secs(2);

/// `logs/controllers.log`: what SDL and each controller report, for when a controller misbehaves on someone's
/// computer. Kept short: started afresh each run.
struct Log(Option<std::fs::File>);

impl Log {
    fn open(path: Option<std::path::PathBuf>) -> Log {
        Log(path.and_then(|p| {
            let _ = std::fs::create_dir_all(p.parent()?);
            // Appending, so the app's own notes (see Gamepads) land after what's here.
            std::fs::File::create(&p).ok()?;
            std::fs::OpenOptions::new().append(true).open(p).ok()
        }))
    }

    fn line(&mut self, text: &str) {
        use std::io::Write;
        if let Some(f) = self.0.as_mut() {
            let _ = writeln!(f, "{text}");
        }
    }
}

/// Start the polling thread. `woke` is called when the snapshot changes.
pub fn start(
    resources: std::path::PathBuf,
    log: Option<std::path::PathBuf>,
    shared: Arc<Mutex<Pads>>,
    woke: Arc<dyn Fn() + Send + Sync>,
) {
    let spawned = std::thread::Builder::new().name("gamepad".into()).spawn(move || {
        let mut log = Log::open(log);
        let sdl = match Sdl::load(&resources) {
            Ok(sdl) => sdl,
            Err(e) => {
                log.line(&format!("SDL2 unavailable: {e}"));
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
        // No SDL_JOYSTICK_HIDAPI_PS4_RUMBLE: sdl2-compat (Fedora's and Homebrew's SDL2) maps it onto SDL 3's
        // SDL_JOYSTICK_ENHANCED_REPORTS, whose "0" keeps every Switch and PlayStation controller in its plain reports,
        // without motion. Left alone, SDL turns enhanced reports on when its sensors are asked for.
        // SAFETY: SDL_Init from the thread that will make every other SDL call.
        let ok = unsafe { (sdl.SDL_Init)(SDL_INIT_GAMECONTROLLER | SDL_INIT_SENSOR) } == 0
            // SAFETY: as above.
            || unsafe { (sdl.SDL_Init)(SDL_INIT_GAMECONTROLLER) } == 0;
        let mut version = [0u8; 3];
        if let Some(get) = sdl.SDL_GetVersion {
            // SAFETY: SDL_version is three bytes.
            unsafe { get(&mut version) };
        }
        log.line(&format!(
            "SDL {}.{}.{}, sensors {}, sensor timestamps {}",
            version[0],
            version[1],
            version[2],
            if sdl.SDL_GameControllerGetSensorData.is_some() { "yes" } else { "no" },
            if sdl.SDL_GameControllerGetSensorDataWithTimestamp.is_some() { "yes" } else { "no" },
        ));
        if !ok {
            log.line(&format!("SDL_Init failed: {}", sdl.error()));
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
        poll(&sdl, &shared, &*woke, &mut log);
    });
    if let Err(e) = spawned {
        eprintln!("gamepad thread: {e}");
    }
}

/// Development hook: `LIGHTTOUCH_VIRTUAL_PAD=FILE` attaches an SDL virtual game controller whose state follows FILE,
/// JSON `{"buttons": [SDL button numbers held], "axes": [six values, -1..1 sticks, 0..1 triggers]}`, re-read when it
/// changes, with optional `"accel"` (m/s²) and `"gyro"` (rad/s) readings standing in for motion sensors. For checking
/// the controller UI and bindings without a controller.
struct Virtual {
    file: std::path::PathBuf,
    joystick: *mut c_void,
    id: i32,
    seen: Option<std::time::SystemTime>,
    accel: Option<[f32; 3]>,
    gyro: Option<[f32; 3]>,
    /// `"blocked": true`: stands in for a controller whose motion Linux won't let LightTouch open.
    blocked: bool,
}

impl Virtual {
    fn attach(sdl: &Sdl) -> Option<Virtual> {
        let file = std::path::PathBuf::from(std::env::var_os("LIGHTTOUCH_VIRTUAL_PAD")?);
        let (attach, open) = (sdl.SDL_JoystickAttachVirtual?, sdl.SDL_JoystickOpen?);
        // SAFETY: SDL's virtual joystick API, from the SDL thread; 1 = SDL_JOYSTICK_TYPE_GAMECONTROLLER.
        let index = unsafe { attach(1, 6, BUTTONS, 0) };
        let joystick = if index >= 0 { unsafe { open(index) } } else { std::ptr::null_mut() };
        let id = unsafe { (sdl.SDL_JoystickGetDeviceInstanceID)(index) };
        (!joystick.is_null()).then_some(Virtual {
            file,
            joystick,
            id,
            seen: None,
            accel: None,
            gyro: None,
            blocked: false,
        })
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
        let three = |key: &str| {
            let v: Vec<f32> = state[key].as_array()?.iter().filter_map(|v| v.as_f64().map(|x| x as f32)).collect();
            (v.len() == 3).then(|| [v[0], v[1], v[2]])
        };
        (self.accel, self.gyro) = (three("accel"), three("gyro"));
        self.blocked = state["blocked"].as_bool() == Some(true);
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
                let v = axes.get(a).copied().unwrap_or(0.0);
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

fn poll(sdl: &Sdl, shared: &Mutex<Pads>, woke: &dyn Fn(), log: &mut Log) {
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
            rescan(sdl, &mut open, log);
        }
        let mut pads: Vec<Pad> = open.iter_mut().map(|o| read(sdl, o, log)).collect();
        if let Some(v) = virtual_pad.as_ref().filter(|v| v.accel.is_some()) {
            for pad in pads.iter_mut().filter(|p| p.id == v.id) {
                (pad.accel, pad.gyro, pad.sensors) = (v.accel, v.gyro, Sensors::Live);
            }
        }
        if let Some(v) = virtual_pad.as_ref().filter(|v| v.blocked) {
            for pad in pads.iter_mut().filter(|p| p.id == v.id) {
                pad.motion_note = Some("Linux doesn't let LightTouch open its motion sensors (/dev/hidraw7)".into());
                pad.motion_fix = true;
            }
        }
        if pads != last {
            shared.lock().unwrap().pads = pads.clone();
            last = pads;
            woke();
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Turn a controller's gyroscope and accelerometer on, where SDL has them: (gyro, accelerometer).
///
/// SAFETY: `handle` is an open controller, on SDL's thread.
unsafe fn enable_sensors(sdl: &Sdl, handle: *mut c_void) -> (bool, bool) {
    let sensor = |kind: c_int| {
        // SAFETY: as the function's.
        let has = sdl.SDL_GameControllerHasSensor.is_some_and(|f| unsafe { f(handle, kind) } != 0);
        // SAFETY: as above.
        has && sdl.SDL_GameControllerSetSensorEnabled.is_some_and(|f| unsafe { f(handle, kind, 1) } == 0)
    };
    (sensor(SDL_SENSOR_GYRO), sensor(SDL_SENSOR_ACCEL))
}

fn rescan(sdl: &Sdl, open: &mut Vec<Open>, log: &mut Log) {
    // A driver may add a controller's sensors after it opens (SDL 3's, once the controller sends its full reports):
    // look again at each one without them.
    for o in open.iter_mut().filter(|o| !o.accel) {
        // SAFETY: as in poll; the handle is still open.
        let (gyro, accel) = unsafe { enable_sensors(sdl, o.handle) };
        if accel {
            log.line(&format!("{}: motion sensors appeared (gyro {})", o.name, if gyro { "on" } else { "none" }));
            (o.gyro, o.accel, o.motion_note, o.motion_fix) = (gyro, accel, None, false);
            o.enabled_at = Instant::now();
        }
    }
    // SAFETY: as in poll.
    unsafe {
        open.retain(|o| {
            let attached = (sdl.SDL_GameControllerGetAttached)(o.handle) != 0;
            if !attached {
                log.line(&format!("{}: disconnected", o.name));
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
            let pad_type = sdl.SDL_GameControllerGetType.map(|f| f(handle));
            let kind = match pad_type {
                Some(3 | 4 | 7) => PadKind::PlayStation,
                Some(5 | 11 | 12 | 13) => PadKind::Nintendo,
                _ => PadKind::Xbox,
            };
            let (gyro, accel) = enable_sensors(sdl, handle);
            let rate = |kind: c_int| sdl.SDL_GameControllerGetSensorDataRate.map_or(0.0, |f| f(handle, kind));
            let vendor = sdl.SDL_GameControllerGetVendor.map_or(0, |f| f(handle));
            let (motion_note, motion_fix) = match if accel { None } else { motion_blocked(sdl, pad_type, vendor) } {
                Some((note, fix)) => (Some(note), fix),
                None => (None, false),
            };
            log.line(&format!(
                "{name}: type {}, gyro {} ({} Hz), accelerometer {} ({} Hz)",
                pad_type.map_or("unknown".to_string(), |t| t.to_string()),
                if gyro { "on" } else { "none" },
                rate(SDL_SENSOR_GYRO),
                if accel { "on" } else { "none" },
                rate(SDL_SENSOR_ACCEL),
            ));
            if let Some(note) = &motion_note {
                log.line(&format!("{name}: motion sensors not reachable: {note}"));
            }
            let now = Instant::now();
            open.push(Open {
                id,
                handle,
                name,
                kind,
                gyro,
                accel,
                motion_note,
                motion_fix,
                last_sample: None,
                sampled_at: now,
                enabled_at: now,
                restarts: 0,
                live: false,
                fresh: 0,
                noted_at: now,
                notes: 0,
            });
        }
    }
}

fn read(sdl: &Sdl, o: &mut Open, log: &mut Log) -> Pad {
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
        if axes[4] > TRIGGER_THRESHOLD {
            buttons |= TRIGGER_LEFT_BIT;
        }
        if axes[5] > TRIGGER_THRESHOLD {
            buttons |= TRIGGER_RIGHT_BIT;
        }
    }
    let (gyro, accel, sensors) = read_sensors(sdl, o, log);
    // Sensor noise would wake the UI every poll: round to what matters.
    let round = |v: [f32; 3], step: f32| v.map(|x| (x / step).round() * step);
    Pad {
        id: o.id,
        name: o.name.clone(),
        kind: o.kind,
        buttons,
        axes: axes.map(|a| (a * 1000.0).round() / 1000.0),
        gyro: gyro.map(|v| round(v, 0.01)),
        accel: accel.map(|v| round(v, 0.05)),
        sensors,
        motion_note: o.motion_note.clone(),
        motion_fix: o.motion_fix,
    }
}

/// Why SDL found no motion sensors on a controller that has them, on Linux. SDL reads a Switch or PlayStation
/// controller's sensors itself through its /dev/hidraw node, which most systems let only root open (Steam's udev rules,
/// the steam-devices package, open them up); failing that, SDL 2.28 and newer read them from the separate "IMU" or
/// "Motion Sensors" input device the kernel's driver makes, which needs read access too.
/// The reason, and whether LightTouch's udev rule would let it open them.
#[cfg(target_os = "linux")]
fn motion_blocked(sdl: &Sdl, pad_type: Option<c_int>, vendor: u16) -> Option<(String, bool)> {
    // Switch Pro and Joy-Cons; PS4, PS5 and PS3 (SDL_GameControllerType).
    if !matches!(pad_type, Some(5 | 11 | 12 | 13 | 3 | 4 | 7)) || vendor == 0 {
        return None;
    }
    let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap_or_default();
    let nodes = |class: &str| {
        let mut v: Vec<String> = std::fs::read_dir(format!("/sys/class/{class}"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    let can = |path: &str, mode: c_int| {
        let c = CString::new(path).unwrap();
        // SAFETY: a valid C string; access() only checks.
        unsafe { libc::access(c.as_ptr(), mode) == 0 }
    };
    // Its hidraw nodes: HID_ID=bus:vendor:product in the device's uevent.
    let id = format!(":{vendor:08X}:");
    let hidraw: Vec<String> = nodes("hidraw")
        .into_iter()
        .filter(|n| read(&std::path::Path::new("/sys/class/hidraw").join(n).join("device/uevent")).contains(&id))
        .map(|n| format!("/dev/{n}"))
        .collect();
    let mut version = [0u8; 3];
    if let Some(get) = sdl.SDL_GetVersion {
        // SAFETY: SDL_version is three bytes.
        unsafe { get(&mut version) };
    }
    if hidraw.iter().any(|p| can(p, libc::R_OK | libc::W_OK)) {
        let old = if (version[0], version[1]) < (2, 26) {
            format!(
                ", or SDL {}.{}.{} is too old for this controller and the kernel's driver",
                version[0], version[1], version[2]
            )
        } else {
            String::new()
        };
        return Some((
            format!(
                "SDL opens the controller but reports no motion sensors. If Steam is running, close it and reconnect the \
                 controller{old}"
            ),
            false,
        ));
    }
    // The driver's motion device, by name.
    let imu: Vec<String> = nodes("input")
        .into_iter()
        .filter(|n| n.starts_with("event"))
        .filter(|n| {
            let name = read(&std::path::Path::new("/sys/class/input").join(n).join("device/name"));
            name.contains("IMU") || name.contains("Motion Sensors")
        })
        .map(|n| format!("/dev/input/{n}"))
        .collect();
    let reads_imu = (version[0], version[1]) >= (2, 28);
    if reads_imu && imu.iter().any(|p| can(p, libc::R_OK)) {
        return None;
    }
    let blocked: Vec<&str> =
        hidraw.iter().chain(if reads_imu { imu.iter() } else { [].iter() }).map(|s| s.as_str()).collect();
    if blocked.is_empty() {
        return None;
    }
    if super::linux_access::installed() {
        return Some((
            "LightTouch's access rule is installed: turn the controller off and on again, or reconnect it, for it to \
             apply"
                .into(),
            false,
        ));
    }
    Some((format!("Linux doesn't let LightTouch open its motion sensors ({})", blocked.join(", ")), true))
}

#[cfg(not(target_os = "linux"))]
fn motion_blocked(_: &Sdl, _: Option<c_int>, _: u16) -> Option<(String, bool)> {
    None
}

/// The motion sensors' readings while they report; turns quiet ones off and on again.
fn read_sensors(sdl: &Sdl, o: &mut Open, log: &mut Log) -> (Option<[f32; 3]>, Option<[f32; 3]>, Sensors) {
    if !o.accel {
        return (None, None, Sensors::None);
    }
    let sample = |kind: c_int| -> Option<(u64, [f32; 3])> {
        let mut v = [0f32; 3];
        let mut stamp = 0u64;
        // SAFETY: as in poll; three floats and a u64 to write into.
        let ok = unsafe {
            match (sdl.SDL_GameControllerGetSensorDataWithTimestamp, sdl.SDL_GameControllerGetSensorData) {
                (Some(f), _) => f(o.handle, kind, &mut stamp, v.as_mut_ptr(), 3) == 0,
                (None, Some(f)) => f(o.handle, kind, v.as_mut_ptr(), 3) == 0,
                _ => false,
            }
        };
        ok.then_some((stamp, v))
    };
    let now = Instant::now();
    let accel = sample(SDL_SENSOR_ACCEL);
    let gyro = if o.gyro { sample(SDL_SENSOR_GYRO).map(|(_, v)| v) } else { None };
    // A new sample: a newer timestamp, or (without timestamps) readings that moved at all, as a real sensor's do.
    let fresh = match (accel, o.last_sample) {
        (Some((stamp, v)), Some((last_stamp, last_v))) => {
            if stamp != 0 || last_stamp != 0 {
                stamp != last_stamp
            } else {
                v != last_v
            }
        }
        (Some((stamp, v)), None) => stamp != 0 || v != [0.0; 3],
        (None, _) => false,
    };
    if fresh {
        o.last_sample = accel;
        o.sampled_at = now;
        o.fresh += 1;
    }
    // Every 5 s for the first minute, then every minute: how many new samples came and what they read, which tells
    // a controller at rest (new samples, steady readings) from one whose sensors stopped (no new samples).
    let every = if o.notes < 12 { Duration::from_secs(5) } else { Duration::from_secs(60) };
    if now.duration_since(o.noted_at) >= every {
        let secs = now.duration_since(o.noted_at).as_secs_f32();
        let accel_now = accel.map(|(_, v)| v.map(|x| (x * 100.0).round() / 100.0));
        let gyro_now = gyro.map(|v| v.map(|x| (x * 1000.0).round() / 1000.0));
        log.line(&format!(
            "{}: {:.0} new samples/s, accelerometer {:?} m/s², gyro {:?} rad/s",
            o.name,
            o.fresh as f32 / secs,
            accel_now.unwrap_or_default(),
            gyro_now.unwrap_or_default(),
        ));
        o.fresh = 0;
        o.noted_at = now;
        o.notes += 1;
    }
    let live = now.duration_since(o.sampled_at) < QUIET && o.last_sample.is_some();
    if live != o.live {
        o.live = live;
        log.line(&format!("{}: motion sensors {}", o.name, if live { "reporting" } else { "quiet" }));
    }
    if live {
        o.restarts = 0;
    }
    // Turning them on again talks to the controller (and can wait on it): after ten tries, only now and then.
    let every_restart = if o.restarts < 10 { RESTART_EVERY } else { RESTART_EVERY * 15 };
    if !live && now.duration_since(o.enabled_at) > every_restart {
        o.enabled_at = now;
        o.restarts += 1;
        if o.restarts <= 12 {
            log.line(&format!("{}: turning the motion sensors on again ({})", o.name, o.restarts));
        }
        if let Some(set) = sdl.SDL_GameControllerSetSensorEnabled {
            // SAFETY: as in poll.
            unsafe {
                for kind in [SDL_SENSOR_GYRO, SDL_SENSOR_ACCEL] {
                    set(o.handle, kind, 0);
                }
                for (kind, on) in [(SDL_SENSOR_GYRO, o.gyro), (SDL_SENSOR_ACCEL, o.accel)] {
                    if on {
                        set(o.handle, kind, 1);
                    }
                }
            }
        }
    }
    if live { (gyro, accel.map(|(_, v)| v), Sensors::Live) } else { (None, None, Sensors::Quiet) }
}

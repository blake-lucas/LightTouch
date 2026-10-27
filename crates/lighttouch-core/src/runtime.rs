//! One device session: the leased device and its engine runtime (`lt_device_*`, `lt_runtime_*`). Only the worker
//! process uses these; the engine may start once per process.
use crate::ffi::{Error, Result, chars, check, cpath, cstring, opt_cpath, ptr};
use crate::library::Library;
use lighttouch_sys as sys;
use serde::{Deserialize, Serialize};
use std::ffi::{c_int, c_void};
use std::path::{Path, PathBuf};

/// Engine-owned facts about a board.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardInfo {
    pub machine: String,
    pub board: String,
    pub width: u32,
    pub height: u32,
    pub scale: u32,
    pub orientation: u32,
    pub max_contacts: u32,
    pub capabilities: u64,
}

impl BoardInfo {
    pub const ACCELEROMETER: u64 = 1;
    pub const GYRO: u64 = 2;
    pub const MULTITOUCH: u64 = 4;
    pub const HEADPHONES: u64 = sys::LT_BOARD_HEADPHONES;

    pub fn has(&self, capability: u64) -> bool {
        self.capabilities & capability != 0
    }
}

/// Ask the engine library what it knows about `board` (loads the engine without starting it).
pub fn board_info(engine: &Path, board: &str) -> Result<BoardInfo> {
    let (engine, board) = (cpath(engine)?, cstring(board)?);
    let mut info = sys::lt_board_info::new();
    let mut error = sys::lt_error::new();
    check(unsafe { sys::lt_engine_board(engine.as_ptr(), board.as_ptr(), &mut info, &mut error) }, &error)?;
    Ok(BoardInfo {
        machine: chars(&info.machine),
        board: chars(&info.board),
        width: info.width,
        height: info.height,
        scale: info.scale,
        orientation: info.orientation,
        max_contacts: info.max_contacts,
        capabilities: info.capabilities,
    })
}

/// A device opened for writing: holds the state's exclusive lease until dropped.
pub struct Device {
    raw: *mut sys::lt_device,
}

unsafe impl Send for Device {}

impl Device {
    /// Open a library record, verifying its base (progress: bytes hashed, total). `recover` allows a cold boot of
    /// state that was not shut down cleanly; otherwise such state fails with `LT_DIRTY`.
    pub fn open(
        library: &Library,
        record_id: &str,
        recover: bool,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<Self> {
        let id = cstring(record_id)?;
        let mut raw = std::ptr::null_mut();
        let mut error = sys::lt_error::new();
        let mut observer: &mut dyn FnMut(u64, u64) -> bool = progress;
        let opaque = &mut observer as *mut &mut dyn FnMut(u64, u64) -> bool as *mut c_void;
        check(
            unsafe {
                sys::lt_library_device_open_observed(
                    library.raw(),
                    id.as_ptr(),
                    recover as u32,
                    Some(progress_trampoline),
                    opaque,
                    &mut raw,
                    &mut error,
                )
            },
            &error,
        )?;
        Ok(Self { raw })
    }

    pub fn board(&self) -> Result<String> {
        let mut info = sys::lt_device_info::new();
        let mut error = sys::lt_error::new();
        check(unsafe { sys::lt_device_describe(self.raw, &mut info, &mut error) }, &error)?;
        Ok(chars(&info.board))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sys::lt_device_close(self.raw) }
        }
    }
}

unsafe extern "C" fn progress_trampoline(opaque: *mut c_void, done: u64, total: u64) -> c_int {
    let observer = unsafe { &mut *(opaque as *mut &mut dyn FnMut(u64, u64) -> bool) };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(done, total))) {
        Ok(true) => 0,
        _ => 1,
    }
}

#[derive(Debug, Clone)]
pub struct BootOptions {
    pub engine: PathBuf,
    pub bootrom: Option<PathBuf>,
    pub guest_package: Option<PathBuf>,
    pub serial_log: Option<PathBuf>,
    pub audio: bool,
    pub headphones: bool,
    /// Guest networking; `None` boots with egress blocked and no way to turn it on until a restart.
    pub network: Option<NetworkBoot>,
}

/// How the guest reaches the network. The engine forwards the proxy address to `proxy_socket` from boot whatever the
/// mode, so the worker can listen there later and turn networking on live.
#[derive(Debug, Clone)]
pub struct NetworkBoot {
    pub mode: NetworkMode,
    pub local_network: bool,
    /// The private proxy's Unix socket, which the engine forwards `10.0.2.100:3128` to.
    pub proxy_socket: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    Off,
    Direct,
    Archive,
}

impl NetworkMode {
    pub(crate) fn raw(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Direct => 1,
            Self::Archive => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Stopped,
    Starting,
    Running,
    Paused,
    Stopping,
    Failed,
}

impl State {
    fn from_raw(value: u32) -> Self {
        match value {
            sys::LT_STARTING => Self::Starting,
            sys::LT_RUNNING => Self::Running,
            sys::LT_PAUSED => Self::Paused,
            sys::LT_STOPPING => Self::Stopping,
            sys::LT_FAILED => Self::Failed,
            _ => Self::Stopped,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub state: State,
    pub guest_shutdown_confirmed: bool,
    pub storage_failed: bool,
    pub dirty: bool,
    pub engine_exit: i32,
    pub frames: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Diagnostics {
    pub engine_ready: bool,
    pub display_sleeping: bool,
    pub backlight_level: i32,
    /// The guest agent: 0 absent, 1 alive, 2 stale (its lease ran out).
    pub agent_status: u32,
    pub shutdown_phase: u32,
    /// How the shutdown went to iOS: 0 not yet, 1 the agent's halt, 2 the power-button gesture.
    pub shutdown_route: u32,
}

/// The guest's NAND traffic since the engine loaded, and the host time spent serving it. Running totals: the
/// performance overlay turns two readings into rates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageCounters {
    pub read_ops: u64,
    pub read_bytes: u64,
    pub read_ns: u64,
    pub write_ops: u64,
    pub write_bytes: u64,
    pub write_ns: u64,
    pub erase_ops: u64,
    pub erase_ns: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub id: u32,
    /// 0 down, 1 move, 2 up, 3 cancel.
    pub phase: u32,
    /// Normalized guest screen coordinates, y down.
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone)]
pub struct GuestResult {
    /// 0 unknown, 1 queued, 2 complete, 3 rejected, 4 cancelled, 5 timed out.
    pub state: u32,
    pub guest_status: i32,
    pub output: String,
}

/// The running engine. Owns the device it boots; destroy only after a terminal state.
pub struct Runtime {
    raw: *mut sys::lt_runtime,
    // Closed after the runtime is destroyed (field order: dropped after `raw` is cleared).
    _device: Device,
}

unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

impl Runtime {
    pub fn create(device: Device, options: &BootOptions) -> Result<Self> {
        let engine = cpath(&options.engine)?;
        let rom = opt_cpath(options.bootrom.as_deref())?;
        let package = opt_cpath(options.guest_package.as_deref())?;
        let serial = opt_cpath(options.serial_log.as_deref())?;
        let socket = opt_cpath(options.network.as_ref().map(|n| n.proxy_socket.as_path()))?;
        let mut config = sys::lt_runtime_config::new();
        config.engine_library = engine.as_ptr();
        config.device = device.raw;
        config.boot = sys::lt_boot_config::new();
        config.boot.bootrom_path = ptr(&rom);
        config.boot.guest_package_path = ptr(&package);
        config.boot.serial_path = ptr(&serial);
        config.boot.audio_enabled = options.audio as u32;
        config.boot.headphones = options.headphones as u32;
        if let Some(network) = &options.network {
            config.boot.network_mode = network.mode.raw();
            config.boot.network_local = network.local_network as u32;
            config.boot.network_socket = ptr(&socket);
        }
        let mut raw = std::ptr::null_mut();
        let mut error = sys::lt_error::new();
        check(unsafe { sys::lt_runtime_create(&config, &mut raw, &mut error) }, &error)?;
        // The core copies every path it keeps, so the CStrings may drop here.
        Ok(Self { raw, _device: device })
    }

    fn call(&self, f: impl FnOnce(*mut sys::lt_runtime, &mut sys::lt_error) -> sys::lt_status) -> Result<()> {
        let mut error = sys::lt_error::new();
        let status = f(self.raw, &mut error);
        check(status, &error)
    }

    pub fn start(&self) -> Result<()> {
        self.call(|r, e| unsafe { sys::lt_runtime_start(r, e) })
    }

    pub fn poll(&self) -> Result<Status> {
        let mut status = sys::lt_runtime_status::new();
        self.call(|r, e| unsafe { sys::lt_runtime_poll(r, &mut status, e) })?;
        Ok(Status {
            state: State::from_raw(status.state),
            guest_shutdown_confirmed: status.guest_shutdown_confirmed != 0,
            storage_failed: status.storage_failed != 0,
            dirty: status.dirty != 0,
            engine_exit: status.engine_exit,
            frames: status.frames,
        })
    }

    /// Fails as unsupported on engines without the storage counters.
    pub fn storage(&self) -> Result<StorageCounters> {
        let mut c = sys::lt_runtime_storage_stats::new();
        self.call(|r, e| unsafe { sys::lt_runtime_storage_statistics(r, &mut c, e) })?;
        Ok(StorageCounters {
            read_ops: c.read_ops,
            read_bytes: c.read_bytes,
            read_ns: c.read_ns,
            write_ops: c.write_ops,
            write_bytes: c.write_bytes,
            write_ns: c.write_ns,
            erase_ops: c.erase_ops,
            erase_ns: c.erase_ns,
        })
    }

    pub fn diagnose(&self) -> Result<Diagnostics> {
        let mut d = sys::lt_runtime_diagnostics::new();
        self.call(|r, e| unsafe { sys::lt_runtime_diagnose(r, &mut d, e) })?;
        Ok(Diagnostics {
            engine_ready: d.engine_ready != 0,
            display_sleeping: d.display_sleeping != 0,
            backlight_level: d.backlight_level,
            agent_status: d.agent_status,
            shutdown_phase: d.shutdown_phase,
            shutdown_route: d.shutdown_route,
        })
    }

    /// Change the route live. Needs a proxy endpoint made at boot; `LT_AGAIN` until the engine is ready.
    pub fn network_policy(&self, mode: NetworkMode, local_network: bool) -> Result<()> {
        self.call(|r, e| unsafe { sys::lt_runtime_network_policy(r, mode.raw(), local_network as u32, e) })
    }

    pub fn pause(&self, paused: bool) -> Result<()> {
        self.call(|r, e| unsafe { sys::lt_runtime_pause(r, paused as u32, e) })
    }

    /// Ask iOS to shut down (like holding Power and sliding); poll for confirmation.
    pub fn request_shutdown(&self) -> Result<()> {
        self.call(|r, e| unsafe { sys::lt_runtime_request_shutdown(r, e) })
    }

    /// Copy the newest frame newer than `since` into `pixels` (BGRA, top-down). `Ok(None)` when there is none.
    pub fn copy_frame(&self, pixels: &mut [u8], since: &mut u64) -> Result<Option<FrameInfo>> {
        let mut info = sys::lt_frame_info::new();
        let mut error = sys::lt_error::new();
        let status = unsafe {
            sys::lt_runtime_copy_frame(
                self.raw,
                pixels.as_mut_ptr() as *mut c_void,
                pixels.len() as u64,
                since,
                &mut info,
                &mut error,
            )
        };
        if status == sys::LT_AGAIN {
            return Ok(None);
        }
        check(status, &error)?;
        Ok(Some(FrameInfo {
            width: info.width,
            height: info.height,
            stride: info.stride,
            sequence: info.sequence,
            timestamp_ns: info.timestamp_ns,
            bytes: info.bytes,
        }))
    }

    /// Call `listener(opaque, sequence)` on the runtime's publisher thread after each new frame. It must not block or
    /// call back into the runtime. Pass `None` to clear it (required before drop).
    ///
    /// # Safety
    /// `opaque` must stay valid until the listener is cleared.
    pub unsafe fn set_frame_listener(&self, listener: sys::lt_frame_listener, opaque: *mut c_void) -> Result<()> {
        self.call(|r, e| unsafe { sys::lt_runtime_set_frame_listener(r, listener, opaque, e) })
    }

    pub fn screenshot_bmp(&self, path: &Path) -> Result<()> {
        let path = cpath(path)?;
        let mut info = sys::lt_frame_info::new();
        self.call(|r, e| unsafe { sys::lt_runtime_write_screenshot(r, path.as_ptr(), &mut info, e) })
    }

    pub fn touch(&self, timestamp_ns: u64, contacts: &[Contact]) -> Result<()> {
        if contacts.is_empty() || contacts.len() > sys::LT_MAX_CONTACTS {
            return Err(Error::invalid("a touch batch holds 1 to 8 contacts"));
        }
        let mut batch = sys::lt_touch_batch::new();
        batch.timestamp_ns = timestamp_ns;
        batch.count = contacts.len() as u32;
        for (slot, c) in batch.contacts.iter_mut().zip(contacts) {
            *slot = sys::lt_contact::new(c.id, c.phase, c.x, c.y);
        }
        self.call(|r, e| unsafe { sys::lt_runtime_touch(r, &batch, e) })
    }

    /// The newest touch batch's fate: (request id, state 1 queued / 2 delivered / 3 rejected, engine result).
    pub fn input_status(&self) -> Result<(u64, u32, i32)> {
        let mut s = sys::lt_input_status::new();
        self.call(|r, e| unsafe { sys::lt_runtime_input_status(r, &mut s, e) })?;
        Ok((s.request_id, s.state, s.engine_result))
    }

    /// Release every held contact and hardware button.
    pub fn cancel_input(&self) -> Result<()> {
        self.call(|r, e| unsafe { sys::lt_runtime_cancel_input(r, e) })
    }

    /// Button ids: 0 Home, 1 Power, 2 Volume up, 3 Volume down.
    pub fn button(&self, button: u32, down: bool) -> Result<()> {
        self.call(|r, e| unsafe { sys::lt_runtime_button(r, button, down as u32, e) })
    }

    pub fn motion(&self, timestamp_ns: u64, acceleration: Option<[f64; 3]>, gyro: Option<[f64; 3]>) -> Result<()> {
        let mut m = sys::lt_motion::new();
        m.timestamp_ns = timestamp_ns;
        if let Some(a) = acceleration {
            m.kinds |= 1;
            m.acceleration = a;
        }
        if let Some(g) = gyro {
            m.kinds |= 2;
            m.angular_velocity = g;
        }
        self.call(|r, e| unsafe { sys::lt_runtime_motion(r, &m, e) })
    }

    /// Pull captured guest audio (44.1 kHz stereo S16) into `samples`; returns sample frames copied (0 when empty).
    pub fn pcm(&self, samples: &mut [i16]) -> Result<u32> {
        let mut info = sys::lt_pcm_info::new();
        let mut error = sys::lt_error::new();
        let capacity = (samples.len() / 2) as u32;
        let status = unsafe { sys::lt_runtime_pcm(self.raw, samples.as_mut_ptr(), capacity, &mut info, &mut error) };
        if status == sys::LT_AGAIN {
            return Ok(0);
        }
        check(status, &error)?;
        Ok(info.frames)
    }

    pub fn profile(&self, path: Option<&Path>) -> Result<()> {
        let path = match path {
            Some(p) => cpath(p)?,
            None => cstring("")?,
        };
        self.call(|r, e| unsafe { sys::lt_runtime_profile(r, path.as_ptr(), e) })
    }

    pub fn guest_request(&self, operation: u32, argument: &str, timeout_ms: u32) -> Result<u64> {
        let argument = cstring(argument)?;
        let mut request = sys::lt_guest_request::new();
        request.operation = operation;
        request.timeout_ms = timeout_ms;
        request.argument = argument.as_ptr();
        let mut id = 0u64;
        self.call(|r, e| unsafe { sys::lt_runtime_guest_request(r, &request, &mut id, e) })?;
        Ok(id)
    }

    pub fn guest_poll(&self, request_id: u64) -> Result<GuestResult> {
        let mut result = Box::new(sys::lt_guest_result::new());
        self.call(|r, e| unsafe { sys::lt_runtime_guest_poll(r, request_id, &mut *result, e) })?;
        let bytes = (result.bytes as usize).min(result.output.len());
        let output: Vec<u8> = result.output[..bytes].iter().map(|&c| c as u8).collect();
        Ok(GuestResult {
            state: result.state,
            guest_status: result.guest_status,
            output: String::from_utf8_lossy(&output).into_owned(),
        })
    }

    /// Withdraw a pending guest request (a mutation the guest already ran stays done).
    pub fn guest_cancel(&self, request_id: u64) -> Result<()> {
        self.call(|r, e| unsafe { sys::lt_runtime_guest_cancel(r, request_id, e) })
    }

    /// Destroy after a terminal state (`LT_BUSY` otherwise). On success the device lease is released too.
    pub fn destroy(mut self) -> std::result::Result<(), (Self, Error)> {
        let mut error = sys::lt_error::new();
        let status = unsafe { sys::lt_runtime_destroy(self.raw, &mut error) };
        match check(status, &error) {
            Ok(()) => {
                self.raw = std::ptr::null_mut();
                Ok(())
            }
            Err(e) => Err((self, e)),
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // A live runtime is only dropped when the worker gives up on it: the process is about to exit and the state
        // stays dirty. Destroying would block or fail, so only detach the listener and leave the rest to teardown.
        if !self.raw.is_null() {
            let mut error = sys::lt_error::new();
            unsafe { sys::lt_runtime_set_frame_listener(self.raw, None, std::ptr::null_mut(), &mut error) };
            std::mem::forget(std::mem::replace(&mut self._device, Device { raw: std::ptr::null_mut() }));
        }
    }
}

/// Map a point in a view (pixels) to normalized guest coordinates through the shared letterbox/rotation transform.
/// `None` when the point is outside the fitted image.
pub fn map_contact(
    px: f64,
    py: f64,
    left: f64,
    top: f64,
    width: f64,
    height: f64,
    rotation: u32,
) -> Option<(f64, f64)> {
    let (mut x, mut y) = (0.0, 0.0);
    let status = unsafe { sys::lt_map_contact(px, py, left, top, width, height, rotation, &mut x, &mut y) };
    (status == sys::LT_OK).then_some((x, y))
}

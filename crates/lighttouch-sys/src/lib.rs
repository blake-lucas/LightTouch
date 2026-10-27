//! Raw bindings to `Portable/include/lighttouch.h` (ABI 1). Layouts mirror the header field for field; every
//! struct starts with `size, version` and must be initialized through its `new()` (the `LT_INIT` macro).
#![allow(non_camel_case_types)]
use std::ffi::{c_char, c_int, c_void};

extern crate libz_sys;
// OpenSSL 3 for the web proxy (src/network.cpp); this crate links it, vendored for packaged builds.
extern crate openssl_sys;

pub const LT_ABI_VERSION: u32 = 1;
pub const LT_MAX_CONTACTS: usize = 8;
pub const LT_SHUTDOWN_CONFIRMATION_TIMEOUT_MS: u32 = 30000;
pub const LT_SHUTDOWN_GRACE_MS: u32 = 35000;
pub const LT_SHUTDOWN_AGENT_WAIT_MS: u32 = 20000;
pub const LT_BOARD_HEADPHONES: u64 = 1 << 4;

pub type lt_status = c_int;
pub const LT_OK: lt_status = 0;
pub const LT_INVALID: lt_status = 1;
pub const LT_IO: lt_status = 2;
pub const LT_BUSY: lt_status = 3;
pub const LT_UNSUPPORTED: lt_status = 4;
pub const LT_ENGINE: lt_status = 5;
pub const LT_DIRTY: lt_status = 6;
pub const LT_CANCELLED: lt_status = 7;
pub const LT_AGAIN: lt_status = 8;

pub const LT_STOPPED: u32 = 0;
pub const LT_STARTING: u32 = 1;
pub const LT_RUNNING: u32 = 2;
pub const LT_PAUSED: u32 = 3;
pub const LT_STOPPING: u32 = 4;
pub const LT_FAILED: u32 = 5;

pub const LT_GUEST_PING: u32 = 0;
pub const LT_GUEST_FRONTMOST: u32 = 1;
pub const LT_GUEST_LAUNCH_BUILTIN: u32 = 2;
pub const LT_GUEST_TYPE_TEXT: u32 = 3;
pub const LT_GUEST_SYNC: u32 = 4;
pub const LT_GUEST_LAUNCH_GYRO_PROBE: u32 = 5;
pub const LT_GUEST_NETWORK_CERTIFICATE: u32 = 6;
pub const LT_GUEST_NETWORK_TRUST: u32 = 7;
pub const LT_GUEST_NETWORK_ROUTE: u32 = 8;
pub const LT_GUEST_NETWORK_TRUST_CHUNK: u32 = 9;
pub const LT_GUEST_NETWORK_ROUTE_CHUNK: u32 = 10;

macro_rules! abi_struct {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct $name {
            pub size: u32,
            pub version: u32,
            $(pub $field: $ty,)*
        }
        impl $name {
            pub fn new() -> Self {
                // SAFETY: every field is plain data (integers, arrays, nullable pointers, Option<fn>).
                let mut value: Self = unsafe { std::mem::zeroed() };
                value.size = std::mem::size_of::<Self>() as u32;
                value.version = LT_ABI_VERSION;
                value
            }
        }
        impl Default for $name {
            fn default() -> Self { Self::new() }
        }
    };
}

abi_struct!(lt_error { code: i32, message: [c_char; 512] });

#[repr(C)]
pub struct lt_device {
    _private: [u8; 0],
}
#[repr(C)]
pub struct lt_runtime {
    _private: [u8; 0],
}
#[repr(C)]
pub struct lt_library {
    _private: [u8; 0],
}

pub type lt_progress_fn = Option<unsafe extern "C" fn(*mut c_void, u64, u64) -> c_int>;
pub type lt_import_phase_fn = Option<unsafe extern "C" fn(*mut c_void, *const c_char, u64, u64) -> c_int>;
pub type lt_frame_listener = Option<unsafe extern "C" fn(*mut c_void, u64)>;

abi_struct!(lt_library_config { root_path: *const c_char });
abi_struct!(lt_record_info {
    id: [c_char; 64],
    display_name: [c_char; 128],
    board: [c_char; 32],
    build: [c_char; 32],
    base_sha256: [c_char; 65],
    base_path: [c_char; 1024],
    state_path: [c_char; 1024],
    bootrom_path: [c_char; 1024],
    guest_package_path: [c_char; 1024],
    selected: u32,
    dirty: u32,
});
abi_struct!(lt_import_config {
    source: *const c_char,
    destination: *const c_char,
    expected_sha256: *const c_char,
    max_bytes: u64,
    progress: lt_progress_fn,
    opaque: *mut c_void,
});
abi_struct!(lt_library_import_config {
    source_path: *const c_char,
    expected_sha256: *const c_char,
    display_name: *const c_char,
    bootrom_path: *const c_char,
    guest_package_path: *const c_char,
    max_bytes: u64,
    progress: lt_progress_fn,
    opaque: *mut c_void,
});
abi_struct!(lt_finalize_import_config { stage: *const c_char, destination: *const c_char });
abi_struct!(lt_device_config { base_path: *const c_char, state_path: *const c_char, allow_dirty: u32 });
abi_struct!(lt_device_info {
    board: [c_char; 32],
    build: [c_char; 32],
    boot_strategy: [c_char; 32],
    base_sha256: [c_char; 65],
    dirty: u32,
});
abi_struct!(lt_board_info {
    machine: [c_char; 32],
    board: [c_char; 16],
    width: u32,
    height: u32,
    scale: u32,
    orientation: u32,
    max_contacts: u32,
    capabilities: u64,
});
abi_struct!(lt_boot_config {
    machine: *const c_char,
    bootrom_path: *const c_char,
    guest_package_path: *const c_char,
    serial_path: *const c_char,
    audio_enabled: u32,
    headphones: u32,
    network_mode: u32,
    network_local: u32,
    network_socket: *const c_char,
});
abi_struct!(lt_runtime_status {
    state: u32,
    guest_shutdown_confirmed: u32,
    storage_failed: u32,
    dirty: u32,
    engine_exit: i32,
    frames: u64,
});
abi_struct!(lt_runtime_config { engine_library: *const c_char, device: *mut lt_device, boot: lt_boot_config });
abi_struct!(lt_runtime_diagnostics {
    engine_ready: u32,
    display_sleeping: u32,
    backlight_level: i32,
    agent_status: u32,
    shutdown_phase: u32,
    shutdown_route: u32,
    shutdown_reply_status: i32,
    shutdown_reply_received: u32,
});
abi_struct!(lt_runtime_io_stats {
    frame_count: u64,
    frame_sequence: u64,
    frame_timestamp_ns: u64,
    frame_published_monotonic_ns: u64,
    pcm_captured_frames: u64,
    pcm_pulled_frames: u64,
    pcm_dropped_frames: u64,
    input_requests: u64,
    pcm_queued_packets: u32,
    reserved: u32,
});
abi_struct!(lt_frame_info {
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    sequence: u64,
    timestamp_ns: u64,
    bytes: u64,
});

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct lt_contact {
    pub size: u32,
    pub version: u32,
    pub id: u32,
    pub phase: u32,
    pub x: f64,
    pub y: f64,
}
impl lt_contact {
    pub fn new(id: u32, phase: u32, x: f64, y: f64) -> Self {
        Self { size: std::mem::size_of::<Self>() as u32, version: LT_ABI_VERSION, id, phase, x, y }
    }
}
abi_struct!(lt_touch_batch { timestamp_ns: u64, count: u32, reserved: u32, contacts: [lt_contact; LT_MAX_CONTACTS] });
abi_struct!(lt_input_status { request_id: u64, state: u32, engine_result: i32 });
abi_struct!(lt_motion {
    timestamp_ns: u64,
    kinds: u32,
    reserved: u32,
    acceleration: [f64; 3],
    angular_velocity: [f64; 3],
});
abi_struct!(lt_pcm_info {
    sample_rate: u32,
    channels: u32,
    frames: u32,
    reserved: u32,
    timestamp_ns: u64,
    dropped_frames: u64,
});
abi_struct!(lt_guest_request { operation: u32, timeout_ms: u32, argument: *const c_char });
abi_struct!(lt_guest_result {
    request_id: u64,
    state: u32,
    guest_status: i32,
    bytes: u32,
    reserved: u32,
    output: [c_char; 4096],
});

unsafe extern "C" {
    pub fn lt_abi_version() -> u32;

    pub fn lt_library_open(
        config: *const lt_library_config,
        out: *mut *mut lt_library,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_library_close(library: *mut lt_library);
    pub fn lt_library_count(library: *mut lt_library, count: *mut u32, error: *mut lt_error) -> lt_status;
    pub fn lt_library_record_at(
        library: *mut lt_library,
        index: u32,
        info: *mut lt_record_info,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_library_select(library: *mut lt_library, record_id: *const c_char, error: *mut lt_error) -> lt_status;
    pub fn lt_library_selected(library: *mut lt_library, info: *mut lt_record_info, error: *mut lt_error) -> lt_status;
    pub fn lt_library_rename(
        library: *mut lt_library,
        record_id: *const c_char,
        display_name: *const c_char,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_library_remove(library: *mut lt_library, record_id: *const c_char, error: *mut lt_error) -> lt_status;
    pub fn lt_library_adopt_prepared(
        library: *mut lt_library,
        config: *const lt_library_import_config,
        phase: lt_import_phase_fn,
        opaque: *mut c_void,
        info: *mut lt_record_info,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_library_set_guest_package(
        library: *mut lt_library,
        record_id: *const c_char,
        path: *const c_char,
        info: *mut lt_record_info,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_library_adopt_legacy(
        library: *mut lt_library,
        config: *const lt_library_import_config,
        legacy_state_path: *const c_char,
        info: *mut lt_record_info,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_library_device_open_observed(
        library: *mut lt_library,
        record_id: *const c_char,
        allow_dirty: u32,
        progress: lt_progress_fn,
        opaque: *mut c_void,
        device: *mut *mut lt_device,
        error: *mut lt_error,
    ) -> lt_status;

    pub fn lt_engine_board(
        engine_library: *const c_char,
        board: *const c_char,
        info: *mut lt_board_info,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_device_describe(device: *mut lt_device, info: *mut lt_device_info, error: *mut lt_error) -> lt_status;
    pub fn lt_device_close(device: *mut lt_device);

    pub fn lt_runtime_create(
        config: *const lt_runtime_config,
        out: *mut *mut lt_runtime,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_start(runtime: *mut lt_runtime, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_poll(runtime: *mut lt_runtime, status: *mut lt_runtime_status, error: *mut lt_error)
    -> lt_status;
    pub fn lt_runtime_diagnose(
        runtime: *mut lt_runtime,
        diagnostics: *mut lt_runtime_diagnostics,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_io_statistics(
        runtime: *mut lt_runtime,
        stats: *mut lt_runtime_io_stats,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_profile(runtime: *mut lt_runtime, path: *const c_char, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_profile_status(runtime: *mut lt_runtime, status: *mut i32, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_pause(runtime: *mut lt_runtime, paused: u32, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_network_policy(
        runtime: *mut lt_runtime,
        mode: u32,
        local_network: u32,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_request_shutdown(runtime: *mut lt_runtime, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_destroy(runtime: *mut lt_runtime, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_copy_frame(
        runtime: *mut lt_runtime,
        pixels: *mut c_void,
        capacity: u64,
        since_sequence: *mut u64,
        info: *mut lt_frame_info,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_frame_metadata(
        runtime: *mut lt_runtime,
        since_sequence: u64,
        info: *mut lt_frame_info,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_set_frame_listener(
        runtime: *mut lt_runtime,
        listener: lt_frame_listener,
        opaque: *mut c_void,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_write_screenshot(
        runtime: *mut lt_runtime,
        private_path: *const c_char,
        info: *mut lt_frame_info,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_touch(runtime: *mut lt_runtime, batch: *const lt_touch_batch, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_input_status(
        runtime: *mut lt_runtime,
        status: *mut lt_input_status,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_cancel_input(runtime: *mut lt_runtime, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_button(runtime: *mut lt_runtime, button: u32, down: u32, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_motion(runtime: *mut lt_runtime, motion: *const lt_motion, error: *mut lt_error) -> lt_status;
    pub fn lt_map_contact(
        px: f64,
        py: f64,
        left: f64,
        top: f64,
        width: f64,
        height: f64,
        rotation: u32,
        x: *mut f64,
        y: *mut f64,
    ) -> lt_status;
    pub fn lt_runtime_pcm(
        runtime: *mut lt_runtime,
        samples: *mut i16,
        capacity_frames: u32,
        info: *mut lt_pcm_info,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_guest_status(runtime: *mut lt_runtime, status: *mut u32, error: *mut lt_error) -> lt_status;
    pub fn lt_runtime_guest_request(
        runtime: *mut lt_runtime,
        request: *const lt_guest_request,
        request_id: *mut u64,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_guest_poll(
        runtime: *mut lt_runtime,
        request_id: u64,
        result: *mut lt_guest_result,
        error: *mut lt_error,
    ) -> lt_status;
    pub fn lt_runtime_guest_cancel(runtime: *mut lt_runtime, request_id: u64, error: *mut lt_error) -> lt_status;
}

/// The guest web proxy (`Portable/network`) through `src/network.cpp`. One per running device.
#[repr(C)]
pub struct lt_net_proxy {
    _private: [u8; 0],
}

#[repr(C)]
pub struct lt_net_activity {
    pub connections: u64,
    pub active: u64,
    pub completed: u64,
    pub failed: u64,
    pub last_result_error: u32,
    pub summary: [c_char; 160],
    pub last_result: [c_char; 512],
}

unsafe extern "C" {
    pub fn lt_net_valid_date(date: *const c_char) -> c_int;
    /// mode 1 direct, 2 archive. Null on failure, with the reason in `error`.
    pub fn lt_net_create(
        directory: *const c_char,
        mode: u32,
        date: *const c_char,
        local_network: u32,
        error: *mut c_char,
        capacity: usize,
    ) -> *mut lt_net_proxy;
    pub fn lt_net_configure(
        proxy: *mut lt_net_proxy,
        mode: u32,
        date: *const c_char,
        local_network: u32,
        error: *mut c_char,
        capacity: usize,
    ) -> c_int;
    pub fn lt_net_listen(proxy: *mut lt_net_proxy, path: *const c_char, error: *mut c_char, capacity: usize) -> c_int;
    pub fn lt_net_certificate(proxy: *const lt_net_proxy, out: *mut u8, capacity: usize) -> usize;
    pub fn lt_net_fingerprint(proxy: *const lt_net_proxy, out: *mut c_char, capacity: usize);
    pub fn lt_net_activity_read(proxy: *const lt_net_proxy, out: *mut lt_net_activity);
    pub fn lt_net_destroy(proxy: *mut lt_net_proxy);
    /// Windows only: the ROOT certificate store as a PEM file. How many were written, or -1.
    pub fn lt_net_export_system_roots(path: *const c_char, error: *mut c_char, capacity: usize) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_version_matches() {
        assert_eq!(unsafe { lt_abi_version() }, LT_ABI_VERSION);
    }

    unsafe extern "C" {
        fn lt_rust_layout(out: *mut u32, capacity: u32) -> u32;
    }

    #[test]
    fn layouts_match_the_header() {
        use std::mem::size_of;
        let rust = [
            size_of::<lt_error>(),
            size_of::<lt_library_config>(),
            size_of::<lt_record_info>(),
            size_of::<lt_import_config>(),
            size_of::<lt_library_import_config>(),
            size_of::<lt_finalize_import_config>(),
            size_of::<lt_device_config>(),
            size_of::<lt_device_info>(),
            size_of::<lt_board_info>(),
            size_of::<lt_boot_config>(),
            size_of::<lt_runtime_status>(),
            size_of::<lt_runtime_config>(),
            size_of::<lt_runtime_diagnostics>(),
            size_of::<lt_runtime_io_stats>(),
            size_of::<lt_frame_info>(),
            size_of::<lt_contact>(),
            size_of::<lt_touch_batch>(),
            size_of::<lt_input_status>(),
            size_of::<lt_motion>(),
            size_of::<lt_pcm_info>(),
            size_of::<lt_guest_request>(),
            size_of::<lt_guest_result>(),
        ];
        let mut c = [0u32; 32];
        let count = unsafe { lt_rust_layout(c.as_mut_ptr(), c.len() as u32) } as usize;
        assert_eq!(count, rust.len());
        for (index, (r, c)) in rust.iter().zip(c.iter()).enumerate() {
            assert_eq!(*r as u32, *c, "struct #{index} differs");
        }
    }
}

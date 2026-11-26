//! Safe Rust over the shared LightTouch core: the device library, the engine runtime (worker side), the worker
//! protocol, the shared-memory frame transport and app paths. No UI code lives here.
pub mod bundle;
pub mod carrier;
mod clock;
mod ffi;
pub mod files;
pub mod frames;
pub mod host;
pub mod job;
pub mod library;
pub mod logs;
pub mod network;
pub mod paths;
pub mod protocol;
pub mod runtime;

pub use clock::{LocalTime, local_date, local_time, monotonic_ns, process_cpu_ns, process_resident_bytes};
pub use ffi::{Error, Result, status_name};
pub use lighttouch_sys as sys;

/// The LightTouch commit this was built from or "unknown".
pub const COMMIT: &str = env!("LIGHTTOUCH_COMMIT");

//! Safe Rust over the shared LightTouch core: the device library, the engine runtime (worker side), the worker
//! protocol, the shared-memory frame transport and app paths. No UI code lives here.
mod clock;
mod ffi;
pub mod frames;
pub mod job;
pub mod library;
pub mod network;
pub mod paths;
pub mod protocol;
pub mod runtime;

pub use clock::{local_date, monotonic_ns, process_cpu_ns, process_resident_bytes};
pub use ffi::{Error, Result, status_name};
pub use lighttouch_sys as sys;

//! What LightTouch knows about iOS firmware: the built-in catalog, downloading IPSWs and boot ROMs, and choosing
//! guest tools for a device, the Legacy Store, and checking IPAs. No UI code.
pub mod catalog;
pub mod download;
pub mod guest_tools;
pub mod ipa;
pub mod store;
pub mod target;

pub use catalog::{Catalog, Firmware, Support};
pub use download::Bootrom;
pub use guest_tools::GuestTools;
pub use target::Target;

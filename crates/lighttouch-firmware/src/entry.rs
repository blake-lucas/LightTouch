//! One entry of the firmware catalog (FirmwareSchema's FirmwareWire.Entry), keys and recipe included.
//!
//! ```ignore
//! let entry = Entry::load_from_catalog(catalog, "n72ap-7E18")?;
//! let key = entry.key_for_path("Firmware/all_flash/all_flash.n72ap.production/iBoot.n72ap.RELEASE.img3")?;
//! ```
use crate::error::{Code, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub board: String,
    #[serde(rename = "product_type")]
    pub product_type: String,
    pub version: String,
    pub build: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prerelease: Option<String>,
    #[serde(rename = "prerelease_number", default, skip_serializing_if = "Option::is_none")]
    pub prerelease_number: Option<i64>,
    pub status: String,
    #[serde(rename = "status_note", default, skip_serializing_if = "Option::is_none")]
    pub status_note: Option<String>,
    /// The libraries the guest's media helpers are qualified on for this build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Vec<String>>,
    pub source: Source,
    pub keys: BTreeMap<String, Key>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<Recipe>,
    pub emulator: Emulator,
    pub estimates: Estimates,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirrors: Option<Vec<Mirror>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(rename = "archive_sha1", default, skip_serializing_if = "Option::is_none")]
    pub archive_sha1: Option<String>,
    #[serde(rename = "archive_bytes", default, skip_serializing_if = "Option::is_none")]
    pub archive_bytes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mirror {
    pub url: String,
    pub sha1: String,
    pub bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(rename = "archive_sha1", default, skip_serializing_if = "Option::is_none")]
    pub archive_sha1: Option<String>,
    #[serde(rename = "archive_bytes", default, skip_serializing_if = "Option::is_none")]
    pub archive_bytes: Option<i64>,
}

/// An img3's IV/key, or a root filesystem's VFDecrypt key (no IV). `file` is the name inside the IPSW.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iv: Option<String>,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub name: String,
    /// Raised when users' devices of the row must be prepared again.
    pub version: i64,
    pub storage: String,
    #[serde(rename = "system_mib")]
    pub system_mib: i64,
    #[serde(rename = "data_size")]
    pub data_size: String,
    pub options: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<Guest>,
    /// The k48 boot chain: "iboot" (default) or "kboot". Ignored by n72ap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<String>,
    /// A sibling entry whose restore ramdisk boots the keybag one-shot when this build has no ramdisk keys.
    #[serde(rename = "keybag_ramdisk_from", default, skip_serializing_if = "Option::is_none")]
    pub keybag_ramdisk_from: Option<String>,
    /// NANDDRIVERSIGN flags the build's FTL formats with, when not the store's default.
    #[serde(rename = "nand_sig_flags", default, skip_serializing_if = "Option::is_none")]
    pub nand_sig_flags: Option<i64>,
    /// The NAND vendor type the store's VFL context declares, when not the part's default.
    #[serde(rename = "nand_vendor_type", default, skip_serializing_if = "Option::is_none")]
    pub nand_vendor_type: Option<i64>,
    /// The PMU clock at power-on (Unix seconds) for every boot, when not the host's.
    #[serde(rename = "rtc_epoch", default, skip_serializing_if = "Option::is_none")]
    pub rtc_epoch: Option<i64>,
}

impl Recipe {
    pub fn option(&self, name: &str) -> Option<bool> {
        self.options.get(name).copied()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guest {
    pub arch: String,
    #[serde(rename = "gl_engine", default, skip_serializing_if = "Option::is_none")]
    pub gl_engine: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Emulator {
    #[serde(rename = "min_protocol")]
    pub min_protocol: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Estimates {
    #[serde(rename = "prepared_bytes")]
    pub prepared_bytes: i64,
    #[serde(rename = "peak_bytes")]
    pub peak_bytes: i64,
    pub seconds: i64,
}

#[derive(Deserialize)]
struct Catalog {
    format: i64,
    entries: Vec<Entry>,
}

impl Entry {
    pub fn load(path: &Path) -> Result<Entry> {
        lenient(path)
    }

    /// The one entry named `id` in the shared catalog; ambiguous or unknown ids are refused.
    pub fn load_from_catalog(path: &Path, id: &str) -> Result<Entry> {
        let catalog: Catalog = lenient(path)?;
        if catalog.format != 1 {
            return Err(crate::unsupported!("unknown catalog format {}", catalog.format));
        }
        let mut matches = catalog.entries.into_iter().filter(|e| e.id == id);
        match (matches.next(), matches.next()) {
            (Some(entry), None) => Ok(entry),
            _ => Err(crate::unsupported!("catalog must contain exactly one entry named {id}")),
        }
    }

    /// The key for an IPSW member, looked up by its file name (as qemu-ios' key pages are).
    pub fn key_for_path(&self, path: &str) -> Result<&Key> {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.keys
            .values()
            .find(|k| k.file == name)
            .ok_or_else(|| Error::new(Code::KeyMissing, format!("{}: no key for {name}", self.id)))
    }

    /// The major iOS version ("3.1.3" -> 3).
    pub fn major(&self) -> i64 {
        release(&self.version).0
    }
}

/// JSON through a `Value` first, so a key repeated in an object is the last one, as Swift's decoder takes it (the
/// catalog has rows that repeat status_note).
fn lenient<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let data = std::fs::read(path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_slice(&data).map_err(|e| crate::unsupported!("{}: {e}", path.display()))?;
    serde_json::from_value(value).map_err(|e| crate::unsupported!("{}: {e}", path.display()))
}

/// "3.1.3" -> (3, 1).
pub fn release(version: &str) -> (i64, i64) {
    let mut parts = version.split('.').map(|p| p.parse::<i64>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// 3.0 and 3.1's mount_hfs refuse a journaled HFSX data volume; 3.2 on mount it.
pub fn journaled_data(version: &str) -> bool {
    release(version) >= (3, 2)
}

/// 4.0 on: data protection (the keybag one-shot and the writable NOR that keeps it).
pub fn data_protection(version: &str) -> bool {
    release(version).0 >= 4
}

/// Bytes from a hex string (no separators); None if it isn't hex.
pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    hex::decode(text).ok()
}

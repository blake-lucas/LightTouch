//! What a device's Details page shows, as Android's `DeviceDetails.kt` does: the hardware its board stands for, facts
//! read from its record's files, and the space the record takes on disk (allocated blocks: the flash files are
//! sparse).
use lighttouch_core::files::stored_bytes;
use lighttouch_core::library::Record;
use serde_json::Value;
use std::path::Path;

/// The hardware a board stands for, as on the real device.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardSpecs {
    pub chip: &'static str,
    pub cpu: &'static str,
    pub gpu: &'static str,
    pub memory: &'static str,
    pub screen: &'static str,
    /// Every board has an accelerometer; these also have a gyroscope (the engine's QEMU_IOS_CAP_GYRO boards).
    pub gyroscope: bool,
}

pub fn board_specs(board: &str) -> Option<BoardSpecs> {
    let specs = |chip, cpu, gpu, memory, screen, gyroscope| BoardSpecs { chip, cpu, gpu, memory, screen, gyroscope };
    Some(match board {
        "n45ap" | "m68ap" | "n82ap" => {
            specs("Samsung S5L8900", "ARM1176JZF-S, 412 MHz", "PowerVR MBX Lite", "128 MB", "320 × 480", false)
        }
        "n72ap" => specs("Samsung S5L8720", "ARM1176JZF-S, 533 MHz", "PowerVR MBX Lite", "128 MB", "320 × 480", false),
        "n18ap" => specs("Samsung S5L8922", "ARM Cortex-A8, 600 MHz", "PowerVR SGX535", "256 MB", "320 × 480", false),
        "n88ap" => specs("Samsung S5L8920", "ARM Cortex-A8, 600 MHz", "PowerVR SGX535", "256 MB", "320 × 480", false),
        "n81ap" => specs("Apple A4", "ARM Cortex-A8, 800 MHz", "PowerVR SGX535", "256 MB", "640 × 960 (Retina)", true),
        "n90ap" => specs("Apple A4", "ARM Cortex-A8, 800 MHz", "PowerVR SGX535", "512 MB", "640 × 960 (Retina)", true),
        "k48ap" => specs("Apple A4", "ARM Cortex-A8, 1 GHz", "PowerVR SGX535", "256 MB", "768 × 1024", false),
        "n42ap" => specs(
            "Apple A6",
            "Apple Swift (2 cores), 1.3 GHz",
            "PowerVR SGX543MP3",
            "1 GB",
            "640 × 1136 (Retina)",
            true,
        ),
        _ => return None,
    })
}

/// Whether the space iOS frees comes back to the host when the device stops. yaFTL (the S5L8920/8922 and A4 boards
/// on iOS 4 to 6) erases the flash blocks it no longer needs while iOS shuts down, and the engine frees each erased
/// block in the device's files; the 2G's and the iPhone 5's FTLs keep freed space until their flash fills
/// (docs/STORAGE.md, "Space iOS frees"). Android's `freedSpaceReturnsAtStop` is the same rule.
pub fn freed_space_returns_at_stop(board: &str, build: &str) -> bool {
    let train: u32 = build.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
    matches!(board, "n18ap" | "n88ap" | "n81ap" | "n90ap" | "k48ap") && (8..=10).contains(&train)
}

/// The Storage section's note; `host` is "this computer" or "your phone".
pub fn storage_note(board: &str, build: &str, host: &str) -> String {
    if freed_space_returns_at_stop(board, build) {
        format!("Space iOS frees is given back to {host} when you stop the device.")
    } else {
        format!("Space iOS frees is not given back to {host} yet, so a device's changes only grow.")
    }
}

/// What a record's files say about it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    /// "6.1.6 (10B500)", or the build when the catalog does not know it.
    pub ios: String,
    /// The flash size the board's iOS sees.
    pub flash_bytes: Option<u64>,
    /// When it was prepared (RFC 3339, from device.lock.json).
    pub created: Option<String>,
    /// "MC544 LL/A"-style model number and region, serial number, UDID and Wi-Fi address (identity.json).
    pub model_number: Option<String>,
    pub serial_number: Option<String>,
    pub udid: Option<String>,
    pub wifi_mac: Option<String>,
    /// The guest tools' serial and the runtime's verdict on them ("good", …).
    pub tools_serial: Option<u32>,
    pub tools_verdict: Option<String>,
}

fn json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// The flash size: geometry.json (prepared from an IPSW), or a packed image's header (old 2G devices).
fn flash_bytes(base: &Path) -> Option<u64> {
    if let Some(g) = json(&base.join("nand/geometry.json")) {
        let n = |k: &str| g.get(k).and_then(Value::as_u64);
        // The 2G's geometry names its chip selects; the others, buses and chip enables per bus.
        let chips = n("chip_selects").or_else(|| Some(n("ce_per_bus")? * n("buses")?))?;
        return Some(n("page_bytes")? * n("pages_per_block")? * n("blocks_per_ce")? * chips);
    }
    let mut header = [0u8; 24];
    use std::io::Read;
    std::fs::File::open(base.join("nand")).ok()?.read_exact(&mut header).ok()?;
    if &header[..8] != b"ITNAND01" {
        return None;
    }
    let le = |at: usize| u64::from(u32::from_le_bytes(header[at..at + 4].try_into().unwrap()));
    Some(le(8) * le(16) * le(20)) // page bytes × chip selects × pages per chip select
}

pub fn facts(record: &Record) -> Facts {
    let (base, state) = (&record.base_path, &record.state_path);
    let identity = json(&base.join("identity.json")).unwrap_or(Value::Null);
    let verdict = std::fs::read_to_string(state.join("guest-verdicts")).ok().map(|v| v.trim().to_string());
    let model_number = text(&identity, "model-number")
        .map(|m| text(&identity, "region-info").map_or(m.clone(), |r| format!("{m} {r}")));
    let ios = match lighttouch_catalog::Catalog::get().find_build(&record.board, &record.build) {
        Some(f) => format!("{} ({})", f.label(), record.build),
        None => record.build.clone(),
    };
    Facts {
        ios,
        flash_bytes: flash_bytes(base),
        created: json(&base.join("device.lock.json")).and_then(|l| text(&l, "created")),
        model_number,
        serial_number: text(&identity, "serial-number"),
        udid: text(&identity, "udid"),
        wifi_mac: text(&identity, "wifi-mac"),
        tools_serial: verdict.as_ref().and_then(|v| v.split(' ').nth(1)?.parse().ok()),
        tools_verdict: verdict.and_then(|v| v.split(' ').next().map(str::to_string)).filter(|v| !v.is_empty()),
    }
}

/// The space a record takes, which can take a while to add up (an old 2G's changes are one file per flash page).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Space {
    pub base: u64,
    pub changes: u64,
    pub logs: u64,
    pub tools: u64,
}

impl Space {
    pub fn total(&self) -> u64 {
        self.base + self.changes + self.logs + self.tools
    }
}

fn is_log(name: &str) -> bool {
    name.starts_with("engine-") && name.ends_with(".log") || name == "guest-serial.log"
}

pub fn space(record: &Record) -> Space {
    let state = &record.state_path;
    let mut space = Space { base: stored_bytes(&record.base_path), ..Default::default() };
    if let Some(record_dir) = record.base_path.parent() {
        space.tools = stored_bytes(&record_dir.join("assets"));
    }
    for entry in std::fs::read_dir(state).into_iter().flatten().flatten() {
        let bytes = stored_bytes(&entry.path());
        if is_log(&entry.file_name().to_string_lossy()) { space.logs += bytes } else { space.changes += bytes }
    }
    space
}

/// "1.4 GB", "312 MB", "84 KB", as the Android app writes sizes.
pub fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = (1u64 << 20) as f64;
    const GIB: f64 = (1u64 << 30) as f64;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GB", b / GIB)
    } else if b >= MIB {
        format!("{:.0} MB", b / MIB)
    } else {
        format!("{:.0} KB", b / 1024.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freed_space_returns_only_on_yaftl_boards_on_ios_4_to_6() {
        assert!(freed_space_returns_at_stop("n81ap", "10B500"));
        assert!(freed_space_returns_at_stop("n88ap", "9B206"));
        assert!(freed_space_returns_at_stop("n18ap", "8C148"));
        assert!(!freed_space_returns_at_stop("n72ap", "7E18"));
        assert!(!freed_space_returns_at_stop("n72ap", "8C148"));
        assert!(!freed_space_returns_at_stop("n42ap", "14G61"));
        assert!(!freed_space_returns_at_stop("n88ap", "7E18"));
        assert!(storage_note("n42ap", "14G61", "this computer").contains("not given back to this computer"));
    }

    #[test]
    fn sizes_read_like_android() {
        assert_eq!(format_bytes(3 << 29), "1.5 GB");
        assert_eq!(format_bytes(300 << 20), "300 MB");
        assert_eq!(format_bytes(84 << 10), "84 KB");
    }
}

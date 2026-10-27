//! The bundled guest tools (`armv6.itpack`, `armv7.itpack`): which pack covers a device, and whether it is newer
//! than the one a device carries. Matched the way the library's guest offer matches it: a family's manifest lists
//! the boards and builds (exact, or `"8*"` for a major version) it supports.
//!
//! ITPACK01: the magic, a little-endian u32 index length, the JSON index, then one zlib stream with every entry.
use flate2::read::ZlibDecoder;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

const LIMIT: u64 = 64 << 20;

struct Family {
    boards: Vec<String>,
    builds: Vec<String>,
    serial: i64,
    stub: bool,
}

impl Family {
    fn covers(&self, board: &str, build: &str) -> bool {
        let major: String = build.chars().take_while(|c| c.is_ascii_digit()).collect();
        !self.stub
            && self.boards.iter().any(|b| b == board)
            && self.builds.iter().any(|b| b == build || *b == format!("{major}*"))
    }
}

fn families(path: &Path) -> Result<Vec<Family>, String> {
    let blob = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if blob.len() < 12 || &blob[..8] != b"ITPACK01" || blob.len() as u64 > LIMIT {
        return Err(format!("{} is not a guest package", path.display()));
    }
    let length = u32::from_le_bytes(blob[8..12].try_into().unwrap()) as usize;
    let index: Value = serde_json::from_slice(blob.get(12..12 + length).ok_or("truncated guest package")?)
        .map_err(|e| e.to_string())?;
    let entries = index["entries"].as_array().ok_or("guest package has no entries")?;
    let total: u64 = entries.iter().filter_map(|e| e["size"].as_u64()).sum();
    if total == 0 || total > LIMIT {
        return Err("guest package exceeds the size limit".into());
    }
    let mut payload = Vec::with_capacity(total as usize);
    ZlibDecoder::new(&blob[12 + length..]).take(total).read_to_end(&mut payload).map_err(|e| e.to_string())?;
    if payload.len() as u64 != total {
        return Err("guest package is truncated".into());
    }
    let mut offset = 0usize;
    let mut families = Vec::new();
    for entry in entries {
        let size = entry["size"].as_u64().unwrap_or(0) as usize;
        let name = entry["name"].as_str().unwrap_or("");
        let data = &payload[offset..offset + size];
        offset += size;
        if !name.ends_with("/manifest.json") {
            continue;
        }
        let Ok(manifest) = serde_json::from_slice::<Value>(data) else { continue };
        let strings = |v: &Value| -> Vec<String> {
            v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default()
        };
        families.push(Family {
            boards: strings(&manifest["requires"]["boards"]),
            builds: strings(&manifest["requires"]["builds"]),
            serial: manifest["serial"].as_i64().unwrap_or(0),
            stub: manifest["stub"].as_bool().unwrap_or(false),
        });
    }
    Ok(families)
}

/// The guest tools folder shipped with the app.
#[derive(Debug, Clone)]
pub struct GuestTools {
    pub dir: PathBuf,
}

impl GuestTools {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn packs(&self) -> Vec<PathBuf> {
        let mut packs: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "itpack"))
            .collect();
        packs.sort();
        packs
    }

    /// The bundled pack covering `board`-`build`, if any.
    pub fn for_device(&self, board: &str, build: &str) -> Option<PathBuf> {
        self.packs().into_iter().find(|p| families(p).is_ok_and(|f| f.iter().any(|f| f.covers(board, build))))
    }

    /// The bundled pack when it is newer than `installed` (a device's current pack, if any). Never older: a newer
    /// pack the user chose stays, and so does one this build cannot read.
    pub fn newer_than(&self, installed: Option<&Path>, board: &str, build: &str) -> Option<PathBuf> {
        let bundled = self.for_device(board, build)?;
        let have = match installed {
            None => 0,
            Some(path) => match serial(path, board, build) {
                Ok(serial) => serial,
                Err(_) => return None,
            },
        };
        (serial(&bundled, board, build).unwrap_or(0) > have).then_some(bundled)
    }

    /// Whether the folder holds what firmwarekit needs to prepare devices.
    pub fn can_prepare(&self) -> bool {
        self.dir.join("it_seal").is_file()
    }
}

/// The serial of the family in `pack` covering `board`-`build` (0 when none does).
pub fn serial(pack: &Path, board: &str, build: &str) -> Result<i64, String> {
    Ok(families(pack)?.iter().find(|f| f.covers(board, build)).map_or(0, |f| f.serial))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs against real packs when `LIGHTTOUCH_TEST_GUEST_TOOLS` names a folder holding them.
    #[test]
    fn bundled_packs_cover_their_boards() {
        let Some(dir) = std::env::var_os("LIGHTTOUCH_TEST_GUEST_TOOLS") else { return };
        let tools = GuestTools::new(PathBuf::from(dir));
        let armv6 = tools.for_device("n72ap", "7E18").expect("a pack covers the iPod touch 2G 3.1.3");
        assert!(armv6.ends_with("armv6.itpack"));
        let armv7 = tools.for_device("n81ap", "10B500").expect("a pack covers the iPod touch 4G 6.1.6");
        assert!(armv7.ends_with("armv7.itpack"));
        assert!(serial(&armv6, "n72ap", "7E18").unwrap() > 0);
        assert!(tools.newer_than(None, "n72ap", "7E18").is_some());
        assert!(tools.newer_than(Some(&armv6), "n72ap", "7E18").is_none());
    }
}

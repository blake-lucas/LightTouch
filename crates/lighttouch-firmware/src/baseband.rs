//! Original Mav5 modem assets required by the iPhone 5's native BBUpdater (Swift
//! `SystemEdits/BasebandFirmware.swift`). This installs firmware, not an activation record and not a
//! modified executable. The `.bbfw` is a ZIP container (`ZIPFoundation.Archive(data:)` in Swift).
use crate::error::Result;
use crate::hfs::Editor;
use crate::ipsw::{self, Ipsw, RestoreInfo};
use plist::Value;
use std::io::{Cursor, Read};

pub const MAV5_DIRECTORY: &str = "usr/local/standalone/firmware/Baseband/Mav5";
pub const PAYLOADS: [&str; 8] =
    ["apps.mbn", "dsp1.mbn", "dsp2.mbn", "dsp3.mbn", "rpm.mbn", "sbl1.mbn", "sbl2.mbn", "restoresbl1.mbn"];

fn mav5_members(components: &std::collections::BTreeMap<String, String>, board: &str) -> Result<Vec<String>> {
    if !["n41ap", "n42ap"].contains(&board.to_ascii_lowercase().as_str()) {
        return Err(crate::unsupported!("No supported Mav5 firmware for {board}"));
    }
    let member = components
        .get("BasebandFirmware")
        .ok_or_else(|| crate::unsupported!("No supported Mav5 firmware for {board}"))?;
    if !(member.starts_with("Firmware/Mav5-")
        && member.ends_with(".Release.bbfw")
        && member.split('/').count() == 2
        && !member.contains('\\')
        && !member.contains(".."))
    {
        return Err(crate::unsupported!("No supported Mav5 firmware for {board}"));
    }
    Ok(vec![member.clone(), format!("{}.plist", &member[..member.len() - 5])])
}

/// iOS 7's folder-backed BBUpdater source needs the original unpacked images as well as the
/// versioned archive and metadata; keep iOS 6's proven layout.
fn expanded_assets(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| crate::unsupported!("Missing or invalid Mav5 archive: {e}"))?;
    let mut out = Vec::new();
    for name in PAYLOADS {
        let mut entry = archive
            .by_name(name)
            .map_err(|_| crate::unsupported!("Missing or invalid Mav5 payload: {name}"))?;
        if entry.is_dir() || entry.size() == 0 || entry.size() > 64 << 20 {
            return Err(crate::unsupported!("Missing or invalid Mav5 payload: {name}"));
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut data)
            .map_err(|e| crate::unsupported!("Could not read Mav5 payload {name}: {e}"))?;
        out.push((format!("{MAV5_DIRECTORY}/{name}"), data));
    }
    Ok(out)
}

/// Installs the original Mav5 firmware and metadata; never replaces a conflicting asset.
#[allow(clippy::too_many_arguments)]
pub fn install_mav5(ipsw: &Ipsw, board: &str, system: &mut Editor) -> Result<Vec<String>> {
    let components = ipsw::build_components(ipsw, board)?;
    let members = mav5_members(&components, board)?;
    let mut assets: Vec<(String, Vec<u8>)> = Vec::new();
    for member in &members {
        let bytes = ipsw.read(member)?;
        if bytes.is_empty() {
            return Err(crate::unsupported!("Empty modem asset: {member}"));
        }
        if member.ends_with(".plist") {
            Value::from_reader(Cursor::new(&bytes))
                .ok()
                .and_then(Value::into_dictionary)
                .ok_or_else(|| crate::unsupported!("Invalid modem metadata: {member}"))?;
        }
        let name = member.rsplit('/').next().unwrap_or(member);
        assets.push((format!("{MAV5_DIRECTORY}/{name}"), bytes));
    }
    let version = RestoreInfo::read(ipsw)?.product_version;
    let major: u32 = version.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    if !(6..=10).contains(&major) {
        return Err(crate::unsupported!("Unsupported Mav5 system version: {version}"));
    }
    if major >= 7 {
        assets.extend(expanded_assets(&assets[0].1)?);
    }
    for (path, bytes) in &assets {
        if let Some(meta) = system.lstat(path)? {
            if meta.kind != crate::hfs::editor::Kind::File || system.read(path)? != *bytes {
                return Err(crate::unsupported!("Conflicting modem asset: {path}"));
            }
        }
    }
    system.mkdirs(MAV5_DIRECTORY)?;
    let mut written = vec![MAV5_DIRECTORY.to_string()];
    for (path, bytes) in assets {
        system.write(&path, bytes, Some(0o644))?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn member_validation() {
        let mut c = BTreeMap::new();
        c.insert("BasebandFirmware".into(), "Firmware/Mav5-1.Release.bbfw".into());
        assert_eq!(mav5_members(&c, "n42ap").unwrap()[1], "Firmware/Mav5-1.Release.plist");
        assert!(mav5_members(&c, "n90ap").is_err());
        c.insert("BasebandFirmware".into(), "Firmware/../x.Release.bbfw".into());
        assert!(mav5_members(&c, "n42ap").is_err());
    }
}

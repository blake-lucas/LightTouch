//! Source-built local VM helpers; stock installd owns installation and registration
//! (Swift `SystemEdits/SideloadTools.swift` plus the 14G61 standalone launchd-cache path).
use crate::activation;
use crate::codesign;
use crate::error::Result;
use crate::hfs::Editor;
use crate::plistedit;
use plist::{Dictionary, Value};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

pub const AGENT: (&str, &str, u16) = ("it_agent", "usr/local/bin/it_agent", 0o755);
pub const HTTPGET: (&str, &str, u16) = ("httpget-armv7", "usr/local/bin/ltm-httpget", 0o755);
pub const APPSYNC: (&str, &str, u16) = ("libappsync.dylib", "usr/lib/libappsync.dylib", 0o755);
pub const AGENT_JOB: &str = "System/Library/LaunchDaemons/com.qemu.it-agent.plist";
pub const OLD_AGENT_JOB: &str = "System/Library/LaunchDaemons/com.qemu.it-pbd.plist";
pub const XPCD_CACHE: &str = "System/Library/Caches/com.apple.xpcd/xpcd_cache.dylib";
const SYSTEM_VERSION: &str = "System/Library/CoreServices/SystemVersion.plist";

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7]])
}
fn put_be64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_be_bytes());
}

/// Modern installd carries a zero-sized dyld restriction marker which would silently discard the
/// job's `DYLD_INSERT_LIBRARIES`. Rename only that marker; keep all code, command sizes and
/// entitlements. `SideloadInsertion.installationServiceAllowingHelper`.
fn installation_service_allowing_helper(input: &[u8]) -> Result<Vec<u8>> {
    let images = codesign::images(input).ok_or_else(|| crate::unsupported!("Unsupported installation-service dyld restriction"))?;
    if images.len() != 1 || images[0].is_64bit || images[0].cputype != 12 {
        return Err(crate::unsupported!("Unsupported installation-service dyld restriction"));
    }
    let image = images[0];
    let b = &input[..image.size];
    let mut out = input.to_vec();
    let mut changed = false;
    let ncmds = u32::from_le_bytes([b[16], b[17], b[18], b[19]]) as usize;
    let mut at = 28usize;
    for _ in 0..ncmds {
        if at + 8 > b.len() {
            return Err(crate::unsupported!("Unsupported installation-service dyld restriction"));
        }
        let cmd = u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        let size = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
        if size < 8 || at + size > b.len() {
            return Err(crate::unsupported!("Unsupported installation-service dyld restriction"));
        }
        if cmd == 1 && size >= 56 {
            let seg = &b[at..at + size];
            if &seg[8..24] == b"__RESTRICT\0\0\0\0\0\0" || &seg[8..24] == b"__RESTRICT\0\0\0\0\0\0\0" {
                let count = u32::from_le_bytes([seg[48], seg[49], seg[50], seg[51]]) as usize;
                if count > (size - 56) / 68 {
                    return Err(crate::unsupported!("Unsupported installation-service dyld restriction"));
                }
                for i in 0..count {
                    let s = at + 56 + i * 68;
                    if &b[s..s + 10] != b"__restrict" || &b[s + 16..s + 26] != b"__RESTRICT" {
                        continue;
                    }
                    if u32::from_le_bytes([b[at + 28], b[at + 29], b[at + 30], b[at + 31]]) != 0
                        || u32::from_le_bytes([b[at + 36], b[at + 37], b[at + 38], b[at + 39]]) != 0
                        || u32::from_le_bytes([b[s + 36], b[s + 37], b[s + 38], b[s + 39]]) != 0
                    {
                        return Err(crate::unsupported!("Unsupported installation-service dyld restriction"));
                    }
                    let mut marker = b"__qemu_appsync".to_vec();
                    marker.resize(16, 0);
                    out[s..s + 16].copy_from_slice(&marker);
                    changed = true;
                }
            }
        }
        at += size;
    }
    if !changed {
        return Ok(input.to_vec());
    }
    activation::signed(&out)
}

/// `CachedLaunchdMetadata.paddedMetadata`: keep object offsets, move the offset table to the end of
/// the capacity, widen it only if the padded allocation needs it, and verify the parse.
pub fn padded_metadata(plist: &Dictionary, capacity: usize) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    Value::Dictionary(plist.clone())
        .to_writer_binary(&mut payload)
        .map_err(|e| crate::unsupported!("native launchd metadata capacity or encoding: {e}"))?;
    if payload.len() < 40 || payload.len() > capacity {
        return Err(crate::unsupported!("native launchd metadata capacity or encoding"));
    }
    let table = be64(&payload, payload.len() - 8) as usize;
    if table < 8 || table > payload.len() - 32 {
        return Err(crate::unsupported!("native launchd metadata capacity or encoding"));
    }
    let old_width = payload[payload.len() - 26] as usize;
    if ![1, 2, 4, 8].contains(&old_width) {
        return Err(crate::unsupported!("native launchd metadata capacity or encoding"));
    }
    let objects = be64(&payload, payload.len() - 24) as usize;
    if objects == 0 || objects > (payload.len() - 32 - table) / old_width {
        return Err(crate::unsupported!("native launchd metadata capacity or encoding"));
    }
    let width = [1usize, 2, 4, 8]
        .into_iter()
        .find(|&w| w >= old_width && (w == 8 || capacity < (1usize << (8 * w))))
        .ok_or_else(|| crate::unsupported!("native launchd metadata capacity or encoding"))?;
    if objects > (capacity - 32) / width {
        return Err(crate::unsupported!("native launchd metadata capacity or encoding"));
    }
    let moved = capacity - 32 - objects * width;
    if moved < table {
        return Err(crate::unsupported!("native launchd metadata capacity or encoding"));
    }
    let mut padded = payload[..table].to_vec();
    padded.resize(moved, 0);
    for index in 0..objects {
        let start = table + index * old_width;
        let mut offset: u64 = 0;
        for byte in &payload[start..start + old_width] {
            offset = (offset << 8) | u64::from(*byte);
        }
        for shift in (0..width).rev() {
            padded.push((offset >> (shift * 8)) as u8);
        }
    }
    let mut trailer = payload[payload.len() - 32..].to_vec();
    trailer[6] = width as u8;
    put_be64(&mut trailer, 24, moved as u64);
    padded.extend(trailer);
    if padded.len() != capacity {
        return Err(crate::unsupported!("native launchd metadata capacity or encoding"));
    }
    let check = Value::from_reader(Cursor::new(&padded))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| crate::unsupported!("native launchd metadata capacity or encoding"))?;
    if check != *plist {
        return Err(crate::unsupported!("native launchd metadata capacity or encoding"));
    }
    Ok(padded)
}

fn job_dictionary(bytes: &[u8]) -> Result<Dictionary> {
    Value::from_reader(Cursor::new(bytes))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| crate::unsupported!("unsupported native launchd service cache"))
}

/// `LaunchdCache.updatingStandalone`: update only named jobs in the native v4 metadata of a fat
/// `xpcd_cache.dylib`; slice sizes, offsets, unrelated jobs and other architectures stay unchanged.
pub fn updating_standalone(
    original: &[u8],
    jobs: &BTreeMap<String, Vec<u8>>,
    removing: &[String],
) -> Result<Vec<u8>> {
    let images = codesign::images(original).ok_or_else(|| crate::unsupported!("unsupported standalone launchd service cache"))?;
    if images.len() < 2 || original.len() < 8 || be32(original, 0) != 0xcafe_babe {
        return Err(crate::unsupported!("unsupported standalone launchd service cache"));
    }
    let mut result = original.to_vec();
    let mut updated = 0;
    for image in &images {
        if image.cputype != 12 || ![9, 11].contains(&(image.cpusubtype & 0x00ff_ffff)) {
            continue;
        }
        let mut slice = original[image.base..image.base + image.size].to_vec();
        let section = codesign::sections(&slice)
            .into_iter()
            .find(|s| s.name == "__xpcd_cache")
            .ok_or_else(|| crate::unsupported!("unsupported standalone launchd service cache"))?;
        let (offset, capacity) = (section.offset as usize, section.size as usize);
        if capacity < 40 || offset > slice.len() - capacity {
            return Err(crate::unsupported!("unsupported standalone launchd service cache"));
        }
        let before = slice[offset..offset + capacity].to_vec();
        let mut plist = Value::from_reader(Cursor::new(&before))
            .ok()
            .and_then(Value::into_dictionary)
            .ok_or_else(|| crate::unsupported!("unsupported standalone launchd service cache"))?;
        if plist.get("VersionNumber").and_then(Value::as_signed_integer) != Some(4) {
            return Err(crate::unsupported!("unsupported standalone launchd service cache"));
        }
        let mut daemons = plist
            .get("LaunchDaemons")
            .and_then(Value::as_dictionary)
            .cloned()
            .ok_or_else(|| crate::unsupported!("unsupported standalone launchd service cache"))?;
        for (path, data) in jobs {
            if !path.starts_with("/System/Library/LaunchDaemons/") || !path.ends_with(".plist") {
                return Err(crate::unsupported!("unsupported standalone launchd service cache"));
            }
            daemons.insert(path.clone(), Value::Dictionary(job_dictionary(data)?));
        }
        for path in removing {
            if !path.starts_with("/System/Library/LaunchDaemons/") || !path.ends_with(".plist") {
                return Err(crate::unsupported!("unsupported standalone launchd service cache"));
            }
            daemons.remove(path);
        }
        plist.insert("LaunchDaemons".into(), Value::Dictionary(daemons));
        let padded = padded_metadata(&plist, capacity)?;
        slice[offset..offset + capacity].copy_from_slice(&padded);
        let signed = codesign::signed(&slice, true)?;
        result[image.base..image.base + image.size].copy_from_slice(&signed);
        updated += 1;
    }
    if updated == 0 {
        return Err(crate::unsupported!("unsupported standalone launchd service cache"));
    }
    Ok(result)
}

fn refresh_launchd_cache(ed: &mut Editor, helpers: &Path, jobs: &[&str]) -> Result<()> {
    let mut replacements = BTreeMap::new();
    let mut removals = Vec::new();
    for job in jobs {
        if !job.starts_with("System/Library/LaunchDaemons/") || !job.ends_with(".plist") {
            return Err(crate::unsupported!("not a System daemon job: {job}"));
        }
        if ed.exists(job) {
            replacements.insert(format!("/{job}"), ed.read(job)?);
        } else {
            removals.push(format!("/{job}"));
        }
    }
    if ed.exists(XPCD_CACHE) {
        let bytes = ed.read(XPCD_CACHE)?;
        let updated = updating_standalone(&bytes, &replacements, &removals)?;
        ed.write(XPCD_CACHE, updated, Some(0o755))?;
    } else {
        // 13A344's metadata lives inside the dyld shared cache; N42 is 14G61, so an absent
        // standalone image is not silently replaced with a different-build template.
        let _ = helpers;
        return Err(crate::unsupported!("N42 sideload: no standalone xpcd cache on this image"));
    }
    Ok(())
}

fn major(ed: &Editor) -> Result<u32> {
    Ok(plistedit::read(ed, SYSTEM_VERSION)?
        .and_then(|d| d.get("ProductVersion").and_then(Value::as_string).map(|v| v.split('.').next().unwrap_or("").parse().unwrap_or(0)))
        .unwrap_or(0))
}

/// `SideloadTools.install(system:helpers:)` for 14G61.
pub fn install(system: &Path, helpers: &Path) -> Result<()> {
    let mut v = Editor::open(system)?;
    let major = major(&v)?;
    let mut root = vec![
        AGENT.1.to_string(),
        HTTPGET.1.to_string(),
        APPSYNC.1.to_string(),
        AGENT_JOB.to_string(),
        XPCD_CACHE.to_string(),
    ];
    // iOS 8+ release installd ignores the configured helper while this empty dyld restriction marker
    // is present. Older profiles are intact.
    if major >= 8 && v.exists("usr/libexec/installd") {
        let before = v.read("usr/libexec/installd")?;
        let after = installation_service_allowing_helper(&before)?;
        if after != before {
            v.write("usr/libexec/installd", after, Some(0o755))?;
            root.push("usr/libexec/installd".into());
        }
    }
    v.mkdirs("usr/local/bin")?;
    for (name, path, mode) in [AGENT, HTTPGET, APPSYNC] {
        v.write(path, std::fs::read(helpers.join(name))?, Some(mode))?;
    }
    let mut job = plistedit::dictionary([
        ("Label", "com.qemu.it-agent".into()),
        ("ProgramArguments", plistedit::strings(&["/usr/local/bin/it_agent"])),
        ("RunAtLoad", Value::Boolean(true)),
        ("KeepAlive", Value::Boolean(true)),
        ("StandardErrorPath", "/var/log/it_agent.log".into()),
    ]);
    if major >= 8 {
        job.insert("ProcessType".into(), "Interactive".into());
    }
    if major >= 9 {
        job.insert("ExecuteAllowed".into(), Value::Boolean(true));
    }
    let mut xml = Vec::new();
    Value::Dictionary(job).to_writer_xml(&mut xml).map_err(|e| crate::internal!("agent job: {e}"))?;
    v.write(AGENT_JOB, xml, Some(0o644))?;
    if v.lstat(OLD_AGENT_JOB)?.is_some() {
        v.remove(OLD_AGENT_JOB)?;
    }
    let mut patched = false;
    for name in ["com.apple.mobile.installd.plist", "com.apple.installd.plist"] {
        let path = format!("System/Library/LaunchDaemons/{name}");
        if !v.exists(&path) {
            continue;
        }
        plistedit::rewrite(&mut v, &path, |d| {
            plistedit::dyld_insert(d, "/usr/lib/libappsync.dylib");
            Ok(())
        })?;
        root.push(path);
        patched = true;
    }
    if !patched {
        return Err(crate::internal!("No supported installd job on this image"));
    }
    refresh_launchd_cache(
        &mut v,
        helpers,
        &[
            AGENT_JOB,
            OLD_AGENT_JOB,
            "System/Library/LaunchDaemons/com.apple.SpringBoard.plist",
            "System/Library/LaunchDaemons/com.apple.mobile.installd.plist",
            "System/Library/LaunchDaemons/com.apple.installd.plist",
        ],
    )?;
    v.commit()?;
    let vol = crate::hfs::Volume::open(system, true)?;
    let refs: Vec<&str> = root.iter().map(String::as_str).collect();
    vol.set_owner(&refs, 0, 0, None, None)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_jobs() {
        assert!(updating_standalone(&[0; 16], &BTreeMap::new(), &[]).is_err());
    }

    #[test]
    fn metadata_padding_round_trips() {
        let d = crate::plistedit::dictionary([
            ("VersionNumber", 4.into()),
            ("LaunchDaemons", Value::Dictionary(crate::plistedit::dictionary([("/a.plist", Value::Dictionary(crate::plistedit::dictionary([("Label", "a".into())])))]))),
        ]);
        let out = padded_metadata(&d, 4096).unwrap();
        assert_eq!(out.len(), 4096);
        let back = Value::from_reader(Cursor::new(&out)).unwrap().into_dictionary().unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn kind_path_is_public() {
        let _ = crate::hfs::editor::Kind::File;
    }
}

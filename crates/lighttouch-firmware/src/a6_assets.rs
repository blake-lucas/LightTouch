//! The iPhone 5's local guest support inventory and A6 kernel trust table (Swift `A6GuestAssets`).
//!
//! This is not the mobile-asset seeder (`assets.rs`) and not a guest package (`guest_archive.rs`).
//! It verifies every signed page of the staged helpers, derives their CodeDirectory identities, and
//! appends only those identities to the kernel's existing native suffix table. A partially validated
//! or unknown layout fails closed.
use crate::codesign;
use crate::error::Result;
use crate::files;
use crate::hfs::Volume;
use std::collections::BTreeSet;
use std::path::Path;

pub const SHARED_CACHE_PATH: &str = "System/Library/Caches/com.apple.dyld/dyld_shared_cache_armv7s";
pub const OPENGLES: &str = "System/Library/Frameworks/OpenGLES.framework/OpenGLES";
pub const PATHS: &[&str] = &[
    OPENGLES,
    "usr/local/bin/it_agent",
    "usr/local/bin/ltm-httpget",
    "usr/lib/libappsync.dylib",
    "usr/libexec/installd",
    // libappsync's weak load command (sideload::with_weak_library) changes amfid's code directory too.
    "usr/libexec/amfid",
    "System/Library/Caches/com.apple.xpcd/xpcd_cache.dylib",
    SHARED_CACHE_PATH,
];
pub const SEAL_EXECUTABLE: &str = "usr/local/bin/it_seal";
pub const SEAL_JOB: &str = "System/Library/LaunchDaemons/com.qemu.it-seal.plist";
pub const IOS9_KERNEL_SHA256: &str =
    "6591c8ae828298cf0131ed98e0dfa234c1dabadbb6bd8ce47b4a361733645bdc";
pub const KERNEL_14G61_SHA256: &str =
    "bbc90ec2bd9ce1cc43c6a98b47d39348d63443b5d6c22293f23a1f43bebe4b46";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub path: String,
    pub sha256: String,
    pub code_directories: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub kernel: Vec<u8>,
    pub assets: Vec<Asset>,
}

/// `A6GuestAssets.prepare(kernel:system:restoreRAMDisk:)`.
pub fn prepare(kernel: &[u8], system: &Path, restore_ramdisk: Option<&Path>) -> Result<Prepared> {
    let volume = Volume::open(system, false)?;
    let kernel_hash = files::sha256(kernel);
    let selected: Vec<&str> = if kernel_hash == IOS9_KERNEL_SHA256 {
        vec![SHARED_CACHE_PATH]
    } else if kernel_hash == KERNEL_14G61_SHA256 {
        let mut paths = PATHS.to_vec();
        paths.extend(crate::a6_firstboot::prepared_paths(&volume)?);
        if volume.record(SEAL_EXECUTABLE).is_ok() {
            paths.push(SEAL_EXECUTABLE);
        }
        paths
    } else {
        return Err(crate::unsupported!("A6 guest assets: unsupported kernel or asset inventory"));
    };
    let mut assets = Vec::new();
    let mut owned: BTreeSet<[u8; 20]> = BTreeSet::new();
    for path in selected {
        let record = volume.record(path)?;
        let bytes = volume.contents(&record)?;
        let directories = codesign::verified_code_directories(&bytes)?;
        owned.extend(directories.iter().copied());
        assets.push(Asset {
            path: path.to_string(),
            sha256: files::sha256(&bytes),
            code_directories: directories.iter().map(hex::encode).collect(),
        });
    }
    if let Some(rd) = restore_ramdisk
        && kernel_hash == KERNEL_14G61_SHA256
    {
        // The temporary restore environment executes our initializer and cache worker too.
        let restore = Volume::open(rd, false)?;
        for path in ["usr/local/bin/restored_external", "usr/local/bin/it_mobilegestalt"] {
            let Ok(record) = restore.record(path) else { continue };
            let bytes = restore.contents(&record)?;
            let directories = codesign::verified_code_directories(&bytes)?;
            owned.extend(directories.iter().copied());
            assets.push(Asset {
                path: format!("restore:/{path}"),
                sha256: files::sha256(&bytes),
                code_directories: directories.iter().map(hex::encode).collect(),
            });
        }
    }
    Ok(Prepared { kernel: registering(&owned, kernel)?, assets })
}

/// Where 14G61's AMFI keeps amfid's code directory hash: its audit-token check compares the program that answered
/// a code-signature question with these 20 bytes and refuses any other ("unable to verify audit token came from
/// amfid"), so an edited amfid is never believed.
pub const AMFID_CDHASH_AT: usize = 0x7b_1ec6;
pub const STOCK_AMFID_CDHASH: &str = "0b6d92f2aefd26b0e656bbd8da52cc6af9197470";
pub const AMFID_PATH: &str = "usr/libexec/amfid";

/// iOS 10 system edit (the iPhone 5's normal-boot kernel only; the restore boot keeps the stock amfid): the kernel's
/// amfid hash becomes the hash of the amfid this System volume carries (libappsync's weak load command,
/// `sideload::with_weak_library`), so the kernel keeps believing its own amfid. Data only (20 bytes), checked
/// against the stock value, which must occur once; an unchanged amfid leaves the kernel unchanged.
pub fn knowing_amfid(kernel: &[u8], assets: &[Asset]) -> Result<Vec<u8>> {
    let Some(amfid) = assets.iter().find(|a| a.path == AMFID_PATH) else { return Ok(kernel.to_vec()) };
    let [hash] = amfid.code_directories.as_slice() else {
        return Err(invalid("amfid has more than one code directory"));
    };
    let stock = hex::decode(STOCK_AMFID_CDHASH).map_err(|_| invalid("stock amfid hash"))?;
    let new = hex::decode(hash).map_err(|_| invalid("amfid hash"))?;
    if new.len() != 20 || kernel.len() < AMFID_CDHASH_AT + 20 {
        return Err(invalid("amfid hash"));
    }
    if new == stock {
        return Ok(kernel.to_vec());
    }
    if kernel[AMFID_CDHASH_AT..AMFID_CDHASH_AT + 20] != stock[..]
        || kernel.windows(20).filter(|w| *w == &stock[..]).count() != 1
    {
        return Err(invalid("the kernel's amfid hash differs"));
    }
    let mut out = kernel.to_vec();
    out[AMFID_CDHASH_AT..AMFID_CDHASH_AT + 20].copy_from_slice(&new);
    Ok(out)
}

fn invalid(detail: &str) -> crate::Error {
    crate::unsupported!("A6 guest assets: {detail}")
}
fn le16(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes([b[o], b[o + 1]]) as usize
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn put16(b: &mut [u8], o: usize, v: usize) {
    b[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// 14G61's native table uses 256 sorted suffix buckets. Retain all stock entries and lookup
/// instructions; move only expanded buckets into verified unused header space.
pub fn registering(owned: &BTreeSet<[u8; 20]>, kernel: &[u8]) -> Result<Vec<u8>> {
    if files::sha256(kernel) == IOS9_KERNEL_SHA256 {
        return registering_ios9(owned, kernel);
    }
    if files::sha256(kernel) != KERNEL_14G61_SHA256 || owned.is_empty() || owned.iter().any(|d| d.len() != 20) {
        return Err(invalid("unsupported kernel or asset inventory"));
    }
    let file = 0x790000usize;
    let header = 0x6fd4usize;
    let old_base = header + 1024;
    if kernel.len() < file + 0x26000 {
        return Err(invalid("truncated 14G61 kernel"));
    }
    let original = &kernel[file..file + 0x26000];
    if files::sha256(original) != "ad6cf379d838fcf0fc44d146c19e3e435a3c068b9f6915c8bad2ddd9c2daf2ed"
        || original[0x482..0x12f4].iter().any(|&b| b != 0)
        || le32(original, 0x23b4) != 0x5066
    {
        return Err(invalid("native table layout differs"));
    }
    let mut patched = original.to_vec();
    let mut native: BTreeSet<[u8; 20]> = BTreeSet::new();
    let new_base = 0x482 + (old_base - 0x482) % 19;
    let delta = (old_base - new_base) / 19;
    let mut cursor = new_base;
    for first in 0..256usize {
        let count = le16(original, header + first * 4);
        let index = le16(original, header + first * 4 + 2);
        if old_base + (index + count) * 19 > original.len() {
            return Err(invalid("native bucket exceeds image"));
        }
        let mut entries: Vec<[u8; 19]> = Vec::with_capacity(count);
        for i in 0..count {
            let at = old_base + (index + i) * 19;
            let mut e = [0u8; 19];
            e.copy_from_slice(&original[at..at + 19]);
            entries.push(e);
            let mut full = [0u8; 20];
            full[0] = first as u8;
            full[1..].copy_from_slice(&e);
            native.insert(full);
        }
        if entries.windows(2).any(|w| w[0] > w[1]) {
            return Err(invalid("unsorted native bucket"));
        }
        let mut additions: Vec<[u8; 19]> =
            owned.iter().filter(|d| d[0] as usize == first && !native.contains(*d)).map(|d| {
                let mut e = [0u8; 19];
                e.copy_from_slice(&d[1..]);
                e
            }).collect();
        if additions.is_empty() {
            if index + delta > 65535 {
                return Err(invalid("bucket index overflow"));
            }
            put16(&mut patched, header + first * 4 + 2, index + delta);
        } else {
            entries.append(&mut additions);
            entries.sort();
            if entries.len() > 65535 {
                return Err(invalid("bucket count overflow"));
            }
            if cursor + entries.len() * 19 > 0x12f4 {
                return registering_compact(owned, kernel, original);
            }
            put16(&mut patched, header + first * 4, entries.len());
            put16(&mut patched, header + first * 4 + 2, (cursor - new_base) / 19);
            for entry in entries {
                patched[cursor..cursor + 19].copy_from_slice(&entry);
                cursor += 19;
            }
        }
    }
    if native.len() != 5635 {
        return Err(invalid("native inventory differs"));
    }
    put32(&mut patched, 0x23b4, (0x807e_7000u32).wrapping_add(new_base as u32).wrapping_sub(0x807e_936e));
    if patched[0x234c..0x23b0] != original[0x234c..0x23b0]
        || patched[old_base..0x2160d] != original[old_base..0x2160d]
    {
        return Err(invalid("native lookup changed"));
    }
    let mut result = kernel.to_vec();
    result[file..file + original.len()].copy_from_slice(&patched);
    Ok(result)
}

/// Native-qualified 13A344 cache-identity extension.
fn registering_ios9(owned: &BTreeSet<[u8; 20]>, kernel: &[u8]) -> Result<Vec<u8>> {
    if owned.is_empty() || owned.iter().any(|d| d.len() != 20) {
        return Err(invalid("invalid iOS 9 asset inventory"));
    }
    let file = 0x70c000usize;
    let header = 0x6eb0usize;
    let old_base = header + 1024;
    let literal = 0x3bfcusize;
    let anchor = 0x3bc6usize;
    if kernel.len() < file + 0x22000 {
        return Err(invalid("truncated iOS 9 kernel"));
    }
    let original = &kernel[file..file + 0x22000];
    if files::sha256(original) != "7096ef21f4bfa88072c27ccebc0cb365a6b2d5b3d60caca5c7507f7332993e09"
        || le32(original, literal) as usize + anchor != old_base
    {
        return Err(invalid("iOS 9 native table layout differs"));
    }
    let new_base = 0x450 + (old_base - 0x450) % 19;
    let delta = (old_base - new_base) / 19;
    if original[new_base..0xb44].iter().any(|&b| b != 0) {
        return Err(invalid("iOS 9 header padding is in use"));
    }
    let mut patched = original.to_vec();
    let mut native: BTreeSet<[u8; 20]> = BTreeSet::new();
    let mut cursor = new_base;
    for first in 0..256usize {
        let count = le16(original, header + first * 4);
        let index = le16(original, header + first * 4 + 2);
        if old_base + (index + count) * 19 > 0x1dfeb {
            return Err(invalid("iOS 9 bucket exceeds native table"));
        }
        let mut entries: Vec<[u8; 19]> = Vec::with_capacity(count);
        for i in 0..count {
            let at = old_base + (index + i) * 19;
            let mut e = [0u8; 19];
            e.copy_from_slice(&original[at..at + 19]);
            entries.push(e);
            let mut full = [0u8; 20];
            full[0] = first as u8;
            full[1..].copy_from_slice(&e);
            native.insert(full);
        }
        if entries.windows(2).any(|w| w[0] > w[1]) {
            return Err(invalid("unsorted iOS 9 native bucket"));
        }
        let mut additions: Vec<[u8; 19]> =
            owned.iter().filter(|d| d[0] as usize == first && !native.contains(*d)).map(|d| {
                let mut e = [0u8; 19];
                e.copy_from_slice(&d[1..]);
                e
            }).collect();
        if additions.is_empty() {
            if index + delta > 65535 {
                return Err(invalid("iOS 9 bucket index overflow"));
            }
            put16(&mut patched, header + first * 4 + 2, index + delta);
        } else {
            entries.append(&mut additions);
            entries.sort();
            if entries.len() > 65535 || cursor + entries.len() * 19 > 0xb44 {
                return Err(invalid("iOS 9 expanded buckets exceed verified padding"));
            }
            put16(&mut patched, header + first * 4, entries.len());
            put16(&mut patched, header + first * 4 + 2, (cursor - new_base) / 19);
            for entry in entries {
                patched[cursor..cursor + 19].copy_from_slice(&entry);
                cursor += 19;
            }
        }
    }
    if native.len() != 4921 {
        return Err(invalid("iOS 9 native inventory differs"));
    }
    if owned.is_subset(&native) {
        return Ok(kernel.to_vec());
    }
    put32(&mut patched, literal, (new_base as u32).wrapping_sub(anchor as u32));
    if patched[old_base..0x1dfeb] != original[old_base..0x1dfeb] {
        return Err(invalid("iOS 9 native suffix table changed"));
    }
    for i in 0..original.len() {
        if patched[i] != original[i] {
            let allowed = (new_base..cursor).contains(&i)
                || (header..old_base).contains(&i)
                || (literal..literal + 4).contains(&i);
            if !allowed {
                return Err(invalid("iOS 9 native lookup changed"));
            }
        }
    }
    let mut result = kernel.to_vec();
    result[file..file + original.len()].copy_from_slice(&patched);
    Ok(result)
}

/// When separate expanded buckets do not fit, move the small directory into the same verified
/// padding and pack the complete catalog into its original directory+catalog allocation.
fn registering_compact(owned: &BTreeSet<[u8; 20]>, kernel: &[u8], original: &[u8]) -> Result<Vec<u8>> {
    let header = 0x6fd4usize;
    let old_base = header + 1024;
    let end = 0x2160dusize;
    let new_header = 0x484usize;
    let new_base = header;
    if le32(original, 0x23b0) != 0x4c74 || new_header + 1024 > 0x12f4 {
        return Err(invalid("native directory pointer differs"));
    }
    let mut native: BTreeSet<[u8; 20]> = BTreeSet::new();
    for first in 0..256usize {
        let count = le16(original, header + first * 4);
        let index = le16(original, header + first * 4 + 2);
        if old_base + (index + count) * 19 > end {
            return Err(invalid("native catalog exceeds allocation"));
        }
        for i in 0..count {
            let at = old_base + (index + i) * 19;
            let mut full = [0u8; 20];
            full[0] = first as u8;
            full[1..].copy_from_slice(&original[at..at + 19]);
            native.insert(full);
        }
    }
    if native.len() != 5635 {
        return Err(invalid("native inventory differs"));
    }
    let all: BTreeSet<[u8; 20]> = native.union(owned).copied().collect();
    if all.len() > (end - new_base) / 19 {
        return Err(invalid("owned assets exceed native catalog allocation"));
    }
    let mut result = original.to_vec();
    let mut index = 0usize;
    for first in 0..256usize {
        let entries: Vec<&[u8; 20]> = all.iter().filter(|d| d[0] as usize == first).collect();
        if index > 65535 || entries.len() > 65535 {
            return Err(invalid("native catalog index overflow"));
        }
        put16(&mut result, new_header + first * 4, entries.len());
        put16(&mut result, new_header + first * 4 + 2, index);
        for entry in entries {
            let start = new_base + index * 19;
            result[start..start + 19].copy_from_slice(&entry[1..]);
            index += 1;
        }
    }
    put32(&mut result, 0x23b0, (new_header as u32).wrapping_sub(0x2360));
    put32(&mut result, 0x23b4, (new_base as u32).wrapping_sub(0x236e));
    let mut verified: BTreeSet<[u8; 20]> = BTreeSet::new();
    for first in 0..256usize {
        let count = le16(&result, new_header + first * 4);
        let start_index = le16(&result, new_header + first * 4 + 2);
        for i in 0..count {
            let at = new_base + (start_index + i) * 19;
            let mut full = [0u8; 20];
            full[0] = first as u8;
            full[1..].copy_from_slice(&result[at..at + 19]);
            verified.insert(full);
        }
    }
    if verified != all || result[0x234c..0x23b0] != original[0x234c..0x23b0] {
        return Err(invalid("native catalog readback differs"));
    }
    let mut prepared = kernel.to_vec();
    prepared[0x790000..0x790000 + original.len()].copy_from_slice(&result);
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LT_14G61_KERNEL=the decrypted kernelcache.mach: only amfid's 20 bytes change, and only for another amfid.
    #[test]
    fn kernel_learns_the_edited_amfid() {
        let Some(path) = std::env::var_os("LT_14G61_KERNEL") else { return };
        let kernel = std::fs::read(path).unwrap();
        let asset = |hash: &str| Asset { path: AMFID_PATH.into(), sha256: String::new(), code_directories: vec![hash.into()] };
        assert_eq!(knowing_amfid(&kernel, &[asset(STOCK_AMFID_CDHASH)]).unwrap(), kernel);
        let new = "00112233445566778899aabbccddeeff00112233";
        let out = knowing_amfid(&kernel, &[asset(new)]).unwrap();
        let changed: Vec<usize> = (0..kernel.len()).filter(|&i| kernel[i] != out[i]).collect();
        assert!(changed.iter().all(|&i| (AMFID_CDHASH_AT..AMFID_CDHASH_AT + 20).contains(&i)));
        assert_eq!(hex::encode(&out[AMFID_CDHASH_AT..AMFID_CDHASH_AT + 20]), new);
        assert!(knowing_amfid(&out, &[asset(new)]).is_err(), "an already-edited kernel is refused");
    }

    #[test]
    fn inventory_paths_match_swift() {
        assert_eq!(PATHS.len(), 8);
        assert!(PATHS.contains(&SHARED_CACHE_PATH));
    }
}

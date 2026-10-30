//! K48IBoot: the iPad 1 (k48ap) real-iBoot boot chain (SecureROM -> LLB -> iBoot -> kernel), the artifacts the
//! `ipad1,iboot=...,nor=...,gid-blobs=...` machine boots. Ports FirmwareKit's K48IBoot (imgtools/ipad1_gid.py:
//! gid_blobs, host_usb_devicetree; build_nor.py's K48 parts; ipad1_iboot.py: nor_base, nvram_bank, the iBoot32Patcher
//! shell-out). The patcher is no longer a separate process: its sources are compiled into the crate (build.rs,
//! vendor/iBoot32Patcher) and called in-process, byte-for-byte what the tool produces.
//!
//! ```ignore
//! let (blobs, names) = k48iboot::gid_blobs(&ipsw, &entry)?;                 // AES-256 GID records + the members
//! let dt = k48iboot::host_usb_device_tree(&enc_dtre, &plaintext, &blobs)?;  // hsic-enabled, re-encrypted
//! let nor = k48iboot::build_nor(&identity, &all_flash, &order, boot_args)?;
//! let iboot = k48iboot::patch_iboot(&decrypted_iboot, boot_args, &log)?;
//! ```
use crate::entry::Entry;
use crate::error::Result;
use crate::identity::UnitIdentity;
use crate::img3;
use crate::ipsw::Ipsw;
use std::collections::BTreeMap;

/// The UID the emulated S5L8930 (iPad 1) reports: cdma_uid_key in hw/arm/s5l8930_cdma.c, an AES-256 key.
const UID_KEY: &[u8; 32] = b"K48AP-UID-S5L8930-iPad1-7B500-01";
/// The 16 bytes iBoot runs through the UID key to get the SHSH wrapping key (build_nor.SHSH_KDF_CONST).
const KDF_CONST: [u8; 16] = [0xdb, 0x1f, 0x5b, 0x33, 0x60, 0x6c, 0x5f, 0x1c, 0x19, 0x34, 0xaa, 0x66, 0x58, 0x9c, 0x06, 0x61];
const IMG3_HEADER: usize = 0x14;
const NVRAM_OFF: usize = 0xFC000;
const NOR_SIZE: usize = 0x100000;

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// AES-CBC encryption without padding; `d` must be whole blocks. 16, 24 or 32-byte keys.
fn aes_cbc_encrypt(d: &[u8], iv: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::NoPadding};
    if iv.len() != 16 || d.len() % 16 != 0 {
        return Err(crate::unsupported!("AES: key {} B, iv {} B, data {} B", key.len(), iv.len(), d.len()));
    }
    let mut out = d.to_vec();
    let n = out.len();
    let ok = match key.len() {
        16 => cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv).map(|c| c.encrypt_padded_mut::<NoPadding>(&mut out, n).map(|_| ())),
        32 => cbc::Encryptor::<aes::Aes256>::new_from_slices(key, iv).map(|c| c.encrypt_padded_mut::<NoPadding>(&mut out, n).map(|_| ())),
        _ => return Err(crate::unsupported!("AES: key {} B, iv {} B, data {} B", key.len(), iv.len(), d.len())),
    };
    match ok {
        Ok(Ok(())) => Ok(out),
        _ => Err(crate::internal!("AES-CBC encryption failed")),
    }
}

// MARK: - GID blobs (AES-256 catalog keys)

/// Encrypted KBAG (48 bytes) || plaintext IV/key (48 bytes) for every IMG3 the entry has a key for, in IPSW member
/// order, de-duplicated by KBAG (production and development KBAGs wrap the same DATA key). ipad1_gid.gid_blobs.
/// The vendored iBoot32Patcher (vendor/iBoot32Patcher/SOURCE.txt): LukeZGD's fork at this commit, its archive's sha256.
pub const IBOOT32PATCHER_COMMIT: &str = "1ff9bd14648efae691ed23ae0abb55a4635111e3";
pub const IBOOT32PATCHER_SHA256: &str = "8e6740aa1a248af8cf26aa7af1d785dbc089c67f510db282cc4c9dd529d4cb2f";

pub fn gid_blobs(ipsw: &Ipsw, entry: &Entry) -> Result<(Vec<u8>, Vec<String>)> {
    let mut records: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut seen: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    let mut names = Vec::new();
    for member in ipsw.names()? {
        let name = member.rsplit('/').next().unwrap_or(&member);
        let Some(k) = entry.keys.values().find(|k| k.file == name) else { continue };
        let (Some(iv), key) = (k.iv.as_deref().and_then(crate::entry::from_hex), crate::entry::from_hex(&k.key)) else { continue };
        let Some(key) = key else { continue };
        let mut plain = iv.clone();
        plain.extend_from_slice(&key);
        let d = ipsw.read(&member)?;
        if d.len() < IMG3_HEADER || d[..4] != [0x33, 0x67, 0x6D, 0x49] {
            continue;
        }
        let full = le32(&d, 4) as usize;
        if full < IMG3_HEADER || full > d.len() {
            return Err(crate::unsupported!("{name}: truncated IMG3"));
        }
        let mut off = IMG3_HEADER;
        let mut bags: Vec<Vec<u8>> = Vec::new();
        while off + 12 <= full {
            let total = le32(&d, off + 4) as usize;
            let size = le32(&d, off + 8) as usize;
            if total < 12 || off + total > full || size > total - 12 {
                return Err(crate::unsupported!("{name}: malformed IMG3 tag"));
            }
            if d[off..off + 4] == [0x47, 0x41, 0x42, 0x4B] && size >= 8 {
                // "GABK"
                let state = le32(&d, off + 12);
                let bits = le32(&d, off + 16);
                if state == 1 || state == 2 {
                    if bits != 256 || size != 56 || plain.len() != 48 {
                        return Err(crate::unsupported!("{name}: invalid AES-256 KBAG/key"));
                    }
                    bags.push(d[off + 20..off + 68].to_vec());
                }
            }
            off += total;
        }
        for kbag in &bags {
            if seen.insert(kbag.clone()) {
                records.push((kbag.clone(), plain.clone()));
            }
        }
        if !bags.is_empty() {
            names.push(member);
        }
    }
    if records.is_empty() {
        return Err(crate::Error::new(crate::Code::KeyMissing, format!("{}: no AES-256 IMG3 keys matched this IPSW", entry.id)));
    }
    let blobs: Vec<u8> = records.iter().flat_map(|(kbag, plain)| kbag.iter().chain(plain).copied()).collect();
    Ok((blobs, names))
}

// MARK: - DeviceTree (hsic-enabled, re-encrypted with the catalog key)

/// The NOR DeviceTree img3 with the emulated HSIC keyboard controller added and re-encrypted under the catalog key, so
/// iBoot (which bypasses image signatures) loads it. ipad1_gid.host_usb_devicetree.
pub fn host_usb_device_tree(image: &[u8], plaintext: &[u8], blobs: &[u8]) -> Result<Vec<u8>> {
    use crate::devicetree::{DeviceTree, Value};
    let mut tree = DeviceTree::new(plaintext)?;
    if !tree.contains("arm-io/usb-complex") {
        return Err(crate::unsupported!("DeviceTree has no USB complex"));
    }
    tree.add("arm-io/usb-complex", "hsic-enabled", &[])?;
    // No SGX model: unmatch the GPU node so IMGSGX535 never probes the unimplemented window, the same edit KBoot makes
    // on the direct-kernel path (qemu-ios 47fe75e824, ipad1_gid.host_usb_devicetree).
    if tree.contains("arm-io/sgx") {
        tree.set("arm-io/sgx", "compatible", Value::Str("none".into()))?;
    }
    let plain = tree.data();
    // pairs: production/development KBAG -> its 48-byte IV/key. Find the production (state 1) KBAG here.
    let mut pairs: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut i = 0;
    while i + 96 <= blobs.len() {
        pairs.insert(blobs[i..i + 48].to_vec(), blobs[i + 48..i + 96].to_vec());
        i += 96;
    }
    let img = image;
    let mut off = IMG3_HEADER;
    let mut key: Option<Vec<u8>> = None;
    while off + 12 <= img.len() {
        let size = le32(img, off + 4) as usize;
        let dlen = le32(img, off + 8) as usize;
        if size < 12 {
            break;
        }
        if img[off..off + 4] == [0x47, 0x41, 0x42, 0x4B] && dlen == 56 && le32(img, off + 12) == 1 {
            key = pairs.get(&img[off + 20..off + 68].to_vec()).cloned();
            break;
        }
        off += size;
    }
    let Some(key) = key.filter(|k| k.len() == 48) else {
        return Err(crate::Error::new(crate::Code::KeyMissing, "DeviceTree production KBAG has no catalog key".to_string()));
    };
    // Encrypt the modified plaintext (AES-256, IV = key[:16], key = key[16:]); rewrite the DATA tag.
    let mut padded = plain.to_vec();
    let pad = (16 - padded.len() % 16) % 16;
    padded.resize(padded.len() + pad, 0);
    let enc = aes_cbc_encrypt(&padded, &key[0..16], &key[16..48])?;
    let data_tag = img3::tags(image)?.get("DATA").copied().ok_or_else(|| crate::unsupported!("DeviceTree img3 has no DATA tag"))?;
    let mut out = image.to_vec();
    let old_size = le32(&out, data_tag.offset + 4) as usize;
    let mut tag: Vec<u8> = b"ATAD".to_vec();
    tag.extend_from_slice(&((12 + enc.len()) as u32).to_le_bytes());
    tag.extend_from_slice(&(plain.len() as u32).to_le_bytes());
    tag.extend_from_slice(&enc);
    let delta = tag.len() as i64 - old_size as i64;
    out.splice(data_tag.offset..data_tag.offset + old_size, tag);
    for field in [4, 8, 12] {
        let v = (le32(&out, field) as i64 + delta) as u32;
        put32(&mut out, field, v);
    }
    Ok(out)
}

// MARK: - NOR

/// The 1 MiB NOR the iBoot path boots: an IMG2 superblock, SysCfg (Mod#/Regn/SrNm/MLB#), two NVRAM banks (debug-uarts,
/// auto-boot, boot-command=fsboot, boot-args, wifiaddr, btaddr) and the all_flash img3s packed at 0x8000 in the manifest
/// order, each SHSH-wrapped under the S5L8930 UID so iBoot's in-place unwrap recovers it. ipad1_iboot.nor_base +
/// build_nor.build (K48). `all_flash` is the img3 by type (its DeviceTree already re-encrypted); `order` the manifest's.
pub fn build_nor(id: &UnitIdentity, all_flash: &BTreeMap<String, Vec<u8>>, order: &[String], boot_args: &str) -> Result<Vec<u8>> {
    let mut nor = nor_base(id, boot_args)?;
    let gran = 0x40;
    let image_start = gran * 0x200; // 64 * (0 + 0x200) = 0x8000
    // The SHSH wrapping key: AES-256 encrypt of the KDF const with the 256-bit UID; the 16-byte result is the AES-128
    // wrapping key (build_nor.shsh_wrap_key, S5L8930 branch).
    let wrap = aes_cbc_encrypt(&KDF_CONST, &[0u8; 16], UID_KEY)?;
    let wrap_key: [u8; 16] = wrap[..16].try_into().unwrap();
    let mut off = image_start;
    for t in order {
        let mut img = all_flash.get(t).ok_or_else(|| crate::unsupported!("all_flash has no img3 of type {t}"))?.clone();
        if !crate::n72nor::wrap_shsh(&mut img, &wrap_key) {
            return Err(crate::unsupported!("{t}: no SHSH tag to wrap, or its length is not a multiple of 16"));
        }
        let padded = (img.len() + gran - 1) & !(gran - 1);
        img.resize(padded, 0);
        put32(&mut img, 4, padded as u32);
        if off + padded > NVRAM_OFF {
            return Err(crate::unsupported!("NOR image area overflows the nvram partition ({t})"));
        }
        nor[off..off + padded].copy_from_slice(&img);
        off += padded;
    }
    // build_nor keeps the stock convention (over-counts by image_start)
    put32(&mut nor, 0x10, ((off + gran - 1) / gran) as u32);
    let c = crc(&nor[..0x30]);
    put32(&mut nor, 0x30, c);
    Ok(nor)
}

/// ipad1_iboot.nor_base: IMG2 + SysCfg + two NVRAM banks, an empty image area (build_nor fills it).
fn nor_base(id: &UnitIdentity, boot_args: &str) -> Result<Vec<u8>> {
    let need = |k: &str| id.get(k).map(str::to_string).ok_or_else(|| crate::unsupported!("identity: missing {k}"));
    let mut nor = vec![0u8; NOR_SIZE];
    nor[..4].copy_from_slice(b"2GMI");
    for (i, v) in [64u32, 0, 0x200, 0x3D00].into_iter().enumerate() {
        put32(&mut nor, 4 + 4 * i, v);
    }
    let c = crc(&nor[..0x30]);
    put32(&mut nor, 0x30, c);
    // SysCfg ('gfCS'): 20-byte records, a 4-byte tag then a 16-byte inline value.
    let entries = [("Mod#", need("model-number")?), ("Regn", need("region-info")?), ("SrNm", need("serial-number")?), ("MLB#", need("mlb-serial-number")?)];
    nor[0x4000..0x4004].copy_from_slice(b"gfCS");
    for (i, v) in [(24 + 20 * entries.len()) as u32, 8192, 0x10001, 0, entries.len() as u32].into_iter().enumerate() {
        put32(&mut nor, 0x4004 + 4 * i, v);
    }
    for (i, (key, value)) in entries.iter().enumerate() {
        let bytes = value.as_bytes();
        if bytes.len() > 16 {
            return Err(crate::unsupported!("invalid SysCfg value for {key}"));
        }
        let o = 0x4018 + i * 20;
        for (k, &b) in key.as_bytes().iter().rev().enumerate() {
            nor[o + k] = b;
        }
        nor[o + 4..o + 4 + bytes.len()].copy_from_slice(bytes);
    }
    let values = [
        ("debug-uarts", "1".to_string()),
        ("auto-boot", "true".to_string()),
        ("boot-command", "fsboot".to_string()),
        ("boot-args", boot_args.to_string()),
        ("wifiaddr", need("wifi-mac")?),
        ("btaddr", need("bt-mac")?),
    ];
    let bank = nvram_bank(&values)?;
    nor[0xFC000..0xFE000].copy_from_slice(&bank);
    nor[0xFE000..0x100000].copy_from_slice(&bank);
    Ok(nor)
}

/// One 8 KiB NVRAM bank as iBoot writes it (ipad1_iboot.nvram_bank): "nvram" wrapper (generation, adler32 of the rest),
/// a "common" partition of NUL-separated key=value, and the free-space partition.
fn nvram_bank(values: &[(&str, String)]) -> Result<Vec<u8>> {
    let mut bank = vec![0u8; 8192];
    bank[..16].copy_from_slice(&chrp(0x5A, "nvram", 32));
    put32(&mut bank, 20, 1); // generation
    bank[32..48].copy_from_slice(&chrp(0x70, "common", 2048));
    let mut data: Vec<u8> = Vec::new();
    for (k, v) in values {
        if k.contains('\0') || v.contains('\0') {
            return Err(crate::unsupported!("invalid NVRAM variables"));
        }
        data.extend_from_slice(format!("{k}={v}").as_bytes());
        data.push(0);
    }
    data.push(0);
    if data.len() > 2032 {
        return Err(crate::unsupported!("invalid NVRAM variables"));
    }
    bank[48..48 + data.len()].copy_from_slice(&data);
    bank[2080..2096].copy_from_slice(&chrp(0x7F, "free", 8192 - 2080));
    let a = crate::lzss::adler32(&bank[20..]);
    put32(&mut bank, 16, a);
    Ok(bank)
}

/// A CHRP nvram partition header: signature, folded checksum, length in 16-byte units, 12-byte name.
fn chrp(sig: u8, name: &str, size: usize) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[0] = sig;
    h[2] = ((size / 16) & 0xFF) as u8;
    h[3] = ((size / 16) >> 8) as u8;
    h[4..4 + name.len()].copy_from_slice(name.as_bytes());
    let mut total: u32 = h.iter().map(|&b| b as u32).sum();
    while total > 255 {
        total = (total & 255) + (total >> 8);
    }
    h[1] = total as u8;
    h
}

/// build_nor's CRC: the NOR superblock's own (the n72 NOR's, zlib CRC-32).
fn crc(b: &[u8]) -> u32 {
    crate::n72nand::crc(b)
}

// MARK: - iBoot patching

unsafe extern "C" {
    /// The vendored iBoot32Patcher's `main` (renamed by build.rs), run over two files as the separate tool was.
    fn lt_iboot32_main(argc: std::os::raw::c_int, argv: *const *const std::os::raw::c_char) -> std::os::raw::c_int;
}

/// The patcher's scratch folder: this process's id and the clock in nanoseconds. SystemTime's Debug (used here before)
/// is "SystemTime { intervals: … }", and the colon makes the name one Windows refuses ("the directory name is invalid").
fn scratch() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    std::env::temp_dir().join(format!("k48-iboot-{}-{nanos}", std::process::id()))
}

/// A pattern-patched iBoot (RSA/personalization bypass, debug boot, the boot-args string), byte-for-byte what
/// `iBoot32Patcher --rsa --debug -b <boot-args>` produces. `-a` (environment boot args) crashes 817.29, so `-b`. The
/// patcher returns status 1 even on success; the output existing, keeping its size and differing is the check. Runs the
/// vendored sources in-process (build.rs); its stdout is left as is (diagnostics the tool prints).
pub fn patch_iboot(iboot: &[u8], boot_args: &str, log: &dyn Fn(&str)) -> Result<Vec<u8>> {
    use std::ffi::CString;
    let tmp = scratch();
    std::fs::create_dir_all(&tmp)?;
    let src = tmp.join("iBoot.in");
    let dst = tmp.join("iBoot.out");
    let run = || -> Result<Vec<u8>> {
        std::fs::write(&src, iboot)?;
        let args = ["iBoot32Patcher", src.to_str().unwrap(), dst.to_str().unwrap(), "--rsa", "--debug", "-b", boot_args];
        let cstrings: Vec<CString> = args.iter().map(|a| CString::new(*a).unwrap()).collect();
        let ptrs: Vec<*const std::os::raw::c_char> = cstrings.iter().map(|c| c.as_ptr()).collect();
        // Safe: the C reads argc/argv, opens the two files, patches an owned buffer, and writes the output; no shared
        // state. The pointers outlive the call (cstrings is kept).
        let status = unsafe { lt_iboot32_main(ptrs.len() as std::os::raw::c_int, ptrs.as_ptr()) };
        if status != 0 && status != 1 {
            return Err(crate::internal!("iBoot32Patcher failed (status {status})"));
        }
        if !dst.exists() {
            return Err(crate::internal!("iBoot32Patcher wrote no output (status {status})"));
        }
        let patched = std::fs::read(&dst)?;
        if patched.len() != iboot.len() || patched == iboot {
            return Err(crate::internal!(
                "iBoot32Patcher produced {} bytes (input {}), or no change; boot-args bypass not applied",
                patched.len(),
                iboot.len()
            ));
        }
        log("iBoot patched (vendored iBoot32Patcher): --rsa --debug -b");
        Ok(patched)
    };
    let result = run();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

// MARK: - The chain's files

/// The real iBoot chain's three files in `out`, as K48Recipe.bootFiles' iboot branch writes them (the fit checks aside):
/// gid-blobs.bin (the catalog GID records), iBoot.bin (the pattern-patched decrypted iBoot) and nor.bin (the packed NOR,
/// its DeviceTree re-encrypted with hsic-enabled). `decrypted`: the decrypt directory (iBoot.bin, DeviceTree.bin).
/// Returns the IPSW members the GID records came from (the lock's gid_components).
pub fn write_boot_files(
    ipsw: &Ipsw,
    entry: &Entry,
    decrypted: &std::path::Path,
    identity: &UnitIdentity,
    boot_args: &str,
    out: &std::path::Path,
    log: &dyn Fn(&str),
) -> Result<Vec<String>> {
    let (blobs, names) = gid_blobs(ipsw, entry)?;
    std::fs::write(out.join("gid-blobs.bin"), &blobs)?;
    let prefix = format!("Firmware/all_flash/all_flash.{}.production/", entry.board);
    let mut all_flash: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for m in ipsw.names()? {
        if m.starts_with(&prefix) && m.ends_with(".img3") {
            let d = ipsw.read(&m)?;
            let t = crate::n72nor::img3_type(&d)?;
            if all_flash.contains_key(&t) {
                return Err(crate::unsupported!("duplicate img3 type {t} in all_flash"));
            }
            all_flash.insert(t, d);
        }
    }
    let dt_img3 = all_flash.get("dtre").ok_or_else(|| crate::unsupported!("{}: all_flash has no DeviceTree", entry.id))?;
    let dt = host_usb_device_tree(dt_img3, &std::fs::read(decrypted.join("DeviceTree.bin"))?, &blobs)?;
    all_flash.insert("dtre".into(), dt);
    let manifest = String::from_utf8_lossy(&ipsw.read(&format!("{prefix}manifest"))?).into_owned();
    let mut order = Vec::new();
    for m in manifest.split_whitespace() {
        order.push(crate::n72nor::img3_type(&ipsw.read(&format!("{prefix}{m}"))?)?);
    }
    let patched = patch_iboot(&std::fs::read(decrypted.join("iBoot.bin"))?, boot_args, log)?;
    std::fs::write(out.join("iBoot.bin"), patched)?;
    std::fs::write(out.join("nor.bin"), build_nor(identity, &all_flash, &order, boot_args)?)?;
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The NVRAM bank's CHRP headers and checksum, and that build_nor's SysCfg/nvram reflect the identity.
    #[test]
    fn nor_base_layout() {
        let id = UnitIdentity::synthesize("ipad1-7B500-default", "16g", None).unwrap();
        let nor = nor_base(&id, "serial=3").unwrap();
        assert_eq!(nor.len(), NOR_SIZE);
        assert_eq!(&nor[..4], b"2GMI");
        assert_eq!(le32(&nor, 0x30), crc(&nor[..0x30]));
        assert_eq!(&nor[0x4000..0x4004], b"gfCS");
        // SysCfg Mod# = the model number, tag stored reversed
        assert_eq!(&nor[0x4018..0x401c], b"#doM");
        assert_eq!(&nor[0x401c..0x401c + 5], b"MB292");
        // two identical NVRAM banks, each "nvram"
        assert_eq!(&nor[0xFC004..0xFC009], b"nvram");
        assert_eq!(nor[0xFC000..0xFE000], nor[0xFE000..0x100000]);
        let bank = &nor[0xFC000..0xFE000];
        assert_eq!(le32(bank, 16), crate::lzss::adler32(&bank[20..]));
        let common = String::from_utf8_lossy(&bank[48..48 + 200]);
        assert!(common.starts_with("debug-uarts=1\0auto-boot=true\0boot-command=fsboot\0boot-args=serial=3\0"));
    }

    /// The S5L8930 SHSH wrapping key: AES-256 of the KDF const under the K48 UID string.
    #[test]
    fn wrapping_key() {
        let wrap = aes_cbc_encrypt(&KDF_CONST, &[0u8; 16], UID_KEY).unwrap();
        assert_eq!(wrap.len(), 16);
    }

    /// build_nor packs the all_flash img3s at 0x8000, pads them to 0x40, wraps each SHSH and rewrites fullSize.
    #[test]
    fn build_nor_packs() {
        fn img3(kind: &[u8; 4], fill: u8) -> Vec<u8> {
            let mut tags = Vec::new();
            for (tag, len) in [(b"ATAD", 8usize), (b"HSHS", 16)] {
                tags.extend_from_slice(tag);
                tags.extend_from_slice(&((12 + len) as u32).to_le_bytes());
                tags.extend_from_slice(&(len as u32).to_le_bytes());
                tags.extend(std::iter::repeat_n(fill, len));
            }
            let mut d = vec![0x33, 0x67, 0x6D, 0x49];
            d.extend_from_slice(&((IMG3_HEADER + tags.len()) as u32).to_le_bytes());
            d.extend_from_slice(&(tags.len() as u32).to_le_bytes());
            d.extend_from_slice(&[0; 4]);
            d.extend(kind.iter().rev());
            d.extend_from_slice(&tags);
            d
        }
        let id = UnitIdentity::synthesize("ipad1-7B500-default", "16g", None).unwrap();
        let all: BTreeMap<String, Vec<u8>> = [("illb", img3(b"illb", 1)), ("ibot", img3(b"ibot", 2))].into_iter().map(|(k, v)| (k.into(), v)).collect();
        let order = vec!["illb".to_string(), "ibot".to_string()];
        let nor = build_nor(&id, &all, &order, "serial=3").unwrap();
        assert_eq!(nor.len(), NOR_SIZE);
        // illb packed at 0x8000, padded to the 0x40 granularity (68 bytes -> 128) with fullSize rewritten
        assert_eq!(le32(&nor, 0x8000 + 4), 128);
        assert_eq!(&nor[0x8000..0x8004], &[0x33, 0x67, 0x6D, 0x49]);
        assert_eq!(le32(&nor, 0x30), crc(&nor[..0x30]));
        // a missing type is refused
        assert!(build_nor(&id, &all, &["dtre".into()], "serial=3").is_err());
    }

    /// The vendored iBoot32Patcher refuses a non-iBoot input: exercises that build.rs linked lt_iboot32_main.
    #[test]
    fn patcher_rejects_non_iboot() {
        let r = patch_iboot(&vec![0u8; 0x1000], "serial=3", &|_| {});
        assert!(r.is_err()); // not a 32-bit iBoot reset vector: no output, status -1
    }

    unsafe extern "C" {
        /// The patcher's Thumb-2 decoders (functions.c), which read packed bit-fields from the instruction word.
        fn is_MOVW_insn(offset: *const std::os::raw::c_void) -> bool;
        fn get_MOVW_val(movw: *const std::os::raw::c_void) -> u16;
    }

    /// `movw r12, #0x5254` ('RT', bytes 45 f2 54 2c) is what find_bl_verify_shsh_5_6_7 searches for in a 5/6/7
    /// iBoot. Its struct fields must be uint32_t (see arm32_defs.h): with uint8_t/uint16_t GCC starts a new storage
    /// unit mid-word, the struct grows to 5 bytes and no MOVW ever matches, so patch_rsa_check fails on every iPad.
    #[test]
    fn patcher_decodes_movw() {
        let bytes: [u8; 4] = [0x45, 0xf2, 0x54, 0x2c];
        let at = bytes.as_ptr().cast();
        assert!(unsafe { is_MOVW_insn(at) }, "is_MOVW_insn must match 45 f2 54 2c");
        assert_eq!(unsafe { get_MOVW_val(at) }, 0x5254);
    }

    /// The scratch folder takes a name every platform accepts: SystemTime's Debug, once used here, carries a colon and
    /// Windows refuses the folder (os error 267, "the directory name is invalid"), so no iPad was ever created there.
    #[test]
    fn scratch_folder_name_is_valid() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real-iBoot chain byte for byte against LegacyPreparationGoldens.k48 (the Python pipeline's digests, frozen
    /// independently of Swift): LT_K48_IPSW_DIR holds iPad1,1_3.2.2_7B500_Restore.ipsw and/or
    /// iPad1,1_4.2.1_8C148_Restore.ipsw (LAN mirror), LT_CATALOG the firmware catalog. Seed "ipad1-<build>-default".
    #[test]
    #[ignore = "needs LT_K48_IPSW_DIR (iPad1,1 7B500/8C148 IPSWs) and LT_CATALOG"]
    fn k48_chain_matches_legacy_goldens() {
        use sha2::Digest;
        let (Ok(dir), Ok(catalog)) = (std::env::var("LT_K48_IPSW_DIR"), std::env::var("LT_CATALOG")) else { return };
        let goldens = [
            (
                "k48ap-7B500",
                "iPad1,1_3.2.2_7B500_Restore.ipsw",
                [
                    ("iBoot.bin", "414d1e6da26b62a507e5961d49768ab7b831842c474053e1bab09b8638ae72dd"),
                    ("nor.bin", "76deefe8d7476c05390138897cfc97c9100d5a80574c5a5e2e01fffeb5d252b6"),
                    ("gid-blobs.bin", "235447077f673664c5012e9b01e2e0d424fa8673017b1f22fd8e2cf254782bb0"),
                ],
            ),
            (
                "k48ap-8C148",
                "iPad1,1_4.2.1_8C148_Restore.ipsw",
                [
                    ("iBoot.bin", "c69c7425b3d204cbc0e92d1d38254d74a23120d2da7f88a2a81509134b24fd94"),
                    ("nor.bin", "17e6a7a0c01dd86a856dc022dd04d0282b96f82b90faf18e7470db35ffc03606"),
                    ("gid-blobs.bin", "895b3affcf9b25ec61dc2ce16c924f934e3d41adf6ee363bfb9287c9dcdb448b"),
                ],
            ),
        ];
        let mut ran = 0;
        for (id, file, want) in goldens {
            let path = std::path::Path::new(&dir).join(file);
            if !path.exists() {
                continue;
            }
            let entry = Entry::load_from_catalog(std::path::Path::new(&catalog), id).unwrap();
            let ipsw = Ipsw::open(&path).unwrap();
            let work = std::env::temp_dir().join(format!("lt-k48chain-{id}-{}", std::process::id()));
            let dec = work.join("dec");
            let out = work.join("out");
            std::fs::create_dir_all(&out).unwrap();
            crate::decrypt::decrypt(&ipsw, &entry, &dec, false, &|_| {}).unwrap();
            let storage = entry.recipe.as_ref().map(|r| r.storage.clone()).unwrap_or_else(|| "16g".into());
            let ident = UnitIdentity::synthesize(&format!("ipad1-{}-default", entry.build), &storage, None).unwrap();
            write_boot_files(&ipsw, &entry, &dec, &ident, crate::kboot::DEFAULT_BOOT_ARGS, &out, &|_| {}).unwrap();
            for (name, sha) in want {
                let got = hex::encode(sha2::Sha256::digest(std::fs::read(out.join(name)).unwrap()));
                assert_eq!(got, sha, "{id} {name}: legacy reference differs");
            }
            std::fs::remove_dir_all(&work).unwrap();
            eprintln!("{id}: iBoot.bin, nor.bin, gid-blobs.bin match the legacy goldens");
            ran += 1;
        }
        assert!(ran > 0, "no k48ap IPSW in LT_K48_IPSW_DIR");
    }
}

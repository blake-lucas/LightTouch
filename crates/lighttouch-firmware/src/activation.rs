//! Activation: lockdownd activated in place (csrc/activation.c, built by build.rs), the iOS 6 data-ark route, the
//! ad hoc re-signing, and the Mach-O signature checks of the guest helpers (MachOSignature).
use crate::error::Result;
use crate::macho::{self, Headers};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::ffi::{c_char, c_int, c_uint, CStr};

#[repr(C)]
struct LtActivationReport {
    strategy: *const c_char,
    isa: *const c_char,
    offset: usize,
    width: c_uint,
    original: [u8; 16],
    replacement: [u8; 16],
}

impl LtActivationReport {
    fn zeroed() -> LtActivationReport {
        LtActivationReport {
            strategy: std::ptr::null(),
            isa: std::ptr::null(),
            offset: 0,
            width: 0,
            original: [0; 16],
            replacement: [0; 16],
        }
    }
}

unsafe extern "C" {
    fn lt_activate_report(
        bytes: *mut u8,
        size: usize,
        report: *mut LtActivationReport,
        error: *mut *const c_char,
    ) -> c_int;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub strategy: String,
    pub isa: String,
    pub offset: usize,
    pub original: Vec<u8>,
    pub replacement: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationResult {
    pub input_sha256: String,
    pub output_sha256: String,
    pub patch: Option<Patch>,
    /// The data route (iOS 6): lockdownd left stock, these keys seeded into /var/root/Library/Lockdown/data_ark.plist.
    pub data_ark: Option<Vec<(String, String)>>,
}

impl ActivationResult {
    /// The lock's "activation" object (input_sha256, output_sha256, [data_ark], [patch]).
    pub fn record(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("input_sha256".into(), self.input_sha256.clone().into());
        m.insert("output_sha256".into(), self.output_sha256.clone().into());
        if let Some(ark) = &self.data_ark {
            let mut d = serde_json::Map::new();
            for (k, v) in ark {
                d.insert(k.clone(), v.clone().into());
            }
            m.insert("data_ark".into(), serde_json::Value::Object(d));
        }
        if let Some(p) = &self.patch {
            m.insert(
                "patch".into(),
                serde_json::json!({
                    "strategy": p.strategy, "isa": p.isa, "offset": p.offset,
                    "original": macho::hex_bytes(&p.original), "replacement": macho::hex_bytes(&p.replacement),
                }),
            );
        }
        serde_json::Value::Object(m)
    }
}

/// An ActivationFailure: no binary strategy matched (the caller may try `data_ark_route`).
#[derive(Debug, Clone)]
pub struct Failure(pub String);

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "activation_failed: {}", self.0)
    }
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// lockdownd's bytes activated: (the new bytes, what was done). The C matcher recognizes the guarded development
/// activation path; later paths are re-signed (1.x stays unsigned if it was).
pub fn run(lockdownd: &[u8]) -> std::result::Result<(Vec<u8>, ActivationResult), Failure> {
    let before = lockdownd.to_vec();
    let mut after = before.clone();
    let mut report = LtActivationReport::zeroed();
    let mut error: *const c_char = std::ptr::null();
    let success = unsafe { lt_activate_report(after.as_mut_ptr(), after.len(), &mut report, &mut error) };
    if success == 0 {
        let msg = if error.is_null() {
            "Unsupported activation path".to_string()
        } else {
            unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned()
        };
        return Err(Failure(msg));
    }
    // 1.x ships unsigned lockdownd and predates mandatory code signing. Only the positively recognized 1.x path
    // (success == 2) may remain unsigned; later paths still require a valid signature. Existing signatures are refreshed.
    if success != 2 || code_signature(&after).is_some() {
        after = signed(&after).map_err(|e| Failure(e.message))?;
    }
    let patch = Patch {
        strategy: cstr(report.strategy),
        isa: cstr(report.isa),
        offset: report.offset,
        original: report.original[..report.width as usize].to_vec(),
        replacement: report.replacement[..report.width as usize].to_vec(),
    };
    let result = ActivationResult {
        input_sha256: sha256_hex(&before),
        output_sha256: sha256_hex(&after),
        patch: Some(patch),
        data_ark: None,
    };
    Ok((after, result))
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// iOS 6's lockdownd, which no binary strategy matches: its data ark says FactoryActivated. Offered only when
/// lockdownd names the lockdown_cache domain and that state; the binary stays stock.
pub fn data_ark_route(lockdownd: &[u8]) -> Option<ActivationResult> {
    if memchr::memmem::find(lockdownd, b"com.apple.mobile.lockdown_cache\0").is_none()
        || memchr::memmem::find(lockdownd, b"FactoryActivated\0").is_none()
    {
        return None;
    }
    let h = sha256_hex(lockdownd);
    Some(ActivationResult {
        input_sha256: h.clone(),
        output_sha256: h,
        patch: None,
        data_ark: Some(vec![("com.apple.mobile.lockdown_cache-ActivationState".to_string(), "FactoryActivated".to_string())]),
    })
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn put_be32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

/// Rebuilds the existing code-page hashes as an ad hoc signature, entitlements and special slots kept. Patching does
/// not change the binary's size or its signing allocation.
pub fn signed(data: &[u8]) -> Result<Vec<u8>> {
    let invalid = || crate::error::Error::new(crate::error::Code::ActivationFailed, "Unsupported code signature");
    // A fat (universal) Mach-O carries several slices; sign each ARM slice in place. iOS 7's afcd is fat
    // (armv7 + armv7s), and the previous thin-only path rejected it outright.
    if data.len() >= 8 && be32(data, 0) == 0xCAFE_BABE {
        let mut out = data.to_vec();
        let nslices = be32(data, 4) as usize;
        let mut signed_any = false;
        for i in 0..nslices {
            let o = 8 + 20 * i;
            if o + 20 > data.len() {
                break;
            }
            if be32(data, o) != 12 {
                continue; // only ARM slices need (and can) be re-signed
            }
            let off = be32(data, o + 8) as usize;
            let size = be32(data, o + 12) as usize;
            if off.checked_add(size).is_none_or(|end| end > data.len()) {
                continue;
            }
            let signed_slice = signed(&data[off..off + size])?;
            if signed_slice.len() != size {
                return Err(invalid());
            }
            out[off..off + size].copy_from_slice(&signed_slice);
            signed_any = true;
        }
        if !signed_any {
            return Err(invalid());
        }
        return Ok(out);
    }
    let mut b = data.to_vec();
    let (start, size) = code_signature(data).ok_or_else(invalid)?;
    if size < 12 || start > b.len().wrapping_sub(size) || be32(&b, start) != 0xFADE_0CC0 {
        return Err(invalid());
    }
    let length = be32(&b, start + 4) as usize;
    let count = be32(&b, start + 8) as usize;
    if length < 12 || length > size || count > (length - 12) / 8 {
        return Err(invalid());
    }
    let mut entries: Vec<(u32, usize)> = Vec::new();
    let mut directories = 0;
    for i in 0..count {
        let typ = be32(&b, start + 12 + i * 8);
        let offset = be32(&b, start + 16 + i * 8) as usize;
        if offset < 12 + count * 8 || offset > length - 8 {
            return Err(invalid());
        }
        let blob = start + offset;
        let blob_length = be32(&b, blob + 4) as usize;
        if blob_length < 8 || blob_length > length - offset {
            return Err(invalid());
        }
        if typ == 0 || (0x1000..=0x1005).contains(&typ) {
            if blob_length < 44 || be32(&b, blob) != 0xFADE_0C02 {
                return Err(invalid());
            }
            let hash_offset = be32(&b, blob + 16) as usize;
            let special = be32(&b, blob + 24) as usize;
            let slots = be32(&b, blob + 28) as usize;
            let limit = be32(&b, blob + 32) as usize;
            let hash_size = b[blob + 36] as usize;
            let hash_type = b[blob + 37];
            let exponent = b[blob + 39] as usize;
            if !(exponent > 0
                && exponent <= 20
                && limit > 0
                && limit <= start
                && ((hash_type == 1 && hash_size == 20) || (hash_type == 2 && hash_size == 32))
                && hash_offset >= 44 + special * hash_size
                && hash_offset <= blob_length
                && slots <= (blob_length - hash_offset) / hash_size)
            {
                return Err(invalid());
            }
            let page = 1usize << exponent;
            if slots != limit.div_ceil(page) {
                return Err(invalid());
            }
            // Scatter layouts require a different page mapping, so fail before touching disk.
            if be32(&b, blob + 8) >= 0x20100 && (blob_length < 48 || be32(&b, blob + 44) != 0) {
                return Err(invalid());
            }
            let flags = be32(&b, blob + 12);
            put_be32(&mut b, blob + 12, flags | 2); // CS_ADHOC
            for slot in 0..slots {
                let lo = slot * page;
                let hi = limit.min((slot + 1) * page);
                let bytes = &b[lo..hi];
                let digest: Vec<u8> = if hash_type == 1 {
                    Sha1::digest(bytes).to_vec()
                } else {
                    Sha256::digest(bytes).to_vec()
                };
                let at = blob + hash_offset + slot * hash_size;
                b[at..at + hash_size].copy_from_slice(&digest);
            }
            directories += 1;
        }
        if typ != 0x10000 {
            entries.push((typ, offset)); // Remove the now-invalid CMS signature.
        }
    }
    if directories == 0 || !entries.iter().any(|&(t, _)| t == 0) {
        return Err(invalid());
    }
    for i in 0..count {
        put_be32(&mut b, start + 12 + i * 8, if i < entries.len() { entries[i].0 } else { 0 });
        put_be32(&mut b, start + 16 + i * 8, if i < entries.len() { entries[i].1 as u32 } else { 0 });
    }
    put_be32(&mut b, start + 8, entries.len() as u32);
    Ok(b)
}

/// LC_CODE_SIGNATURE's (dataoff, datasize) of a thin 32-bit Mach-O in memory.
fn code_signature(data: &[u8]) -> Option<(usize, usize)> {
    if data.len() < 28 || macho::le32(data, 0) != 0xFEED_FACE {
        return None;
    }
    let count = macho::le32(data, 16) as usize;
    let mut at = 28usize;
    for _ in 0..count {
        if at + 8 > data.len() {
            return None;
        }
        let cmd = macho::le32(data, at);
        let size = macho::le32(data, at + 4) as usize;
        if cmd == macho::LC_CODE_SIGNATURE && at + 16 <= data.len() {
            return Some((macho::le32(data, at + 8) as usize, macho::le32(data, at + 12) as usize));
        }
        if size < 8 {
            return None;
        }
        at += size;
    }
    None
}

const ARM_CPU: i32 = 12; // CPU_TYPE_ARM
const ARMV7: i32 = 9; // CPU_SUBTYPE_ARM_V7

/// Why a guest tool (it_pbd, it_seal, ...) can't be baked, if it can't (MachOSignature.guestToolProblem): a thin
/// armv7 Mach-O that 3.2's dyld takes (no LC_MAIN, no LC_VERSION_MIN_IPHONEOS) and that is signed.
pub fn guest_tool_problem(bin: &[u8]) -> Option<String> {
    let Some(m) = Headers::parse(bin) else { return Some("not a thin 32-bit Mach-O".into()) };
    let Some(s) = m.slices.first() else { return Some("not a thin 32-bit Mach-O".into()) };
    if m.fat || s.is_64bit {
        return Some("not a thin 32-bit Mach-O".into());
    }
    if s.cputype != ARM_CPU || s.cpusubtype != ARMV7 {
        return Some(format!("cpu {}/{}, not armv7", s.cputype, s.cpusubtype));
    }
    if s.commands.iter().any(|&c| c == macho::LC_MAIN || c == macho::LC_VERSION_MIN_IPHONEOS) {
        return Some("carries LC_MAIN/LC_VERSION_MIN (not run through machotool mkold)".into());
    }
    if s.commands.contains(&macho::LC_CODE_SIGNATURE) {
        None
    } else {
        Some("unsigned (ldid -S)".into())
    }
}

/// MachOSignature.appSyncProblem: nil for a fat Mach-O with an armv7 slice and a signature per slice.
pub fn app_sync_problem(bin: &[u8]) -> Option<String> {
    let Some(f) = Headers::parse(bin).filter(|h| h.fat) else {
        return Some("not a fat Mach-O (expected armv6+armv7)".into());
    };
    for (i, s) in f.slices.iter().enumerate() {
        if !s.commands.contains(&macho::LC_CODE_SIGNATURE) {
            return Some(format!("slice {i} (cpu {}/{}) is not ldid-signed", s.cputype, s.cpusubtype));
        }
    }
    if f.slices.iter().any(|s| s.cputype == ARM_CPU && s.cpusubtype == ARMV7) {
        None
    } else {
        Some("no armv7 slice".into())
    }
}

/// MachOSignature.earlyARMProblem: 2.x/3.0 dyld needs classic relocations and an ARMv6 slice.
pub fn early_arm_problem(bin: &[u8]) -> Option<String> {
    let Some(f) = Headers::parse(bin) else { return Some("not a Mach-O".into()) };
    let Some(image) = f.slices.iter().find(|s| s.cputype == ARM_CPU && s.cpusubtype == 6) else {
        return Some("no armv6 slice".into());
    };
    let newer = [macho::LC_DYLD_INFO_ONLY, macho::LC_MAIN, macho::LC_VERSION_MIN_IPHONEOS, macho::LC_BUILD_VERSION];
    if image.commands.iter().any(|c| newer.contains(c)) {
        return Some("requires a newer dyld; rebuild with LEGACY_LINK=1".into());
    }
    if image.commands.contains(&macho::LC_CODE_SIGNATURE) {
        None
    } else {
        Some("unsigned armv6 slice".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// iOS 6: lockdownd naming the lockdown_cache domain and FactoryActivated gets the data route, unchanged.
    #[test]
    fn data_ark() {
        let ld = b"xx\0com.apple.mobile.lockdown_cache\0FactoryActivated\0";
        let r = data_ark_route(ld).unwrap();
        assert_eq!(
            r.data_ark,
            Some(vec![("com.apple.mobile.lockdown_cache-ActivationState".to_string(), "FactoryActivated".to_string())])
        );
        assert_eq!(r.input_sha256, r.output_sha256);
        assert!(r.patch.is_none());
        assert!(data_ark_route(b"com.apple.mobile.lockdown_cache\0").is_none());
    }

    const CD: usize = 292;
    /// A 32-bit Mach-O of 512 bytes signed over its first page (256 bytes): a SHA-1 code directory at CD, a
    /// requirements blob at 364, an entitlements blob at 380 and a CMS slot.
    fn synthetic_macho() -> Vec<u8> {
        let mut b = vec![0u8; 512];
        let put_le = |b: &mut [u8], o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        put_le(&mut b, 0, 0xFEED_FACE);
        put_le(&mut b, 16, 1);
        put_le(&mut b, 20, 16);
        put_le(&mut b, 28, 0x1d);
        put_le(&mut b, 32, 16);
        put_le(&mut b, 36, 256);
        put_le(&mut b, 40, 256);
        put_be32(&mut b, 256, 0xFADE_0CC0);
        put_be32(&mut b, 260, 140);
        put_be32(&mut b, 264, 3);
        put_be32(&mut b, 268, 0);
        put_be32(&mut b, 272, 36);
        put_be32(&mut b, 276, 5);
        put_be32(&mut b, 280, 108);
        put_be32(&mut b, 284, 0x10000);
        put_be32(&mut b, 288, 124);
        put_be32(&mut b, CD, 0xFADE_0C02);
        put_be32(&mut b, CD + 4, 72);
        put_be32(&mut b, CD + 8, 0x20001);
        put_be32(&mut b, CD + 16, 52);
        put_be32(&mut b, CD + 20, 44);
        put_be32(&mut b, CD + 28, 1);
        put_be32(&mut b, CD + 32, 256);
        b[CD + 36] = 20;
        b[CD + 37] = 1;
        b[CD + 39] = 12;
        put_be32(&mut b, 364, 0xFADE_7171);
        put_be32(&mut b, 368, 16);
        put_be32(&mut b, 380, 0xFADE_0B01);
        put_be32(&mut b, 384, 16);
        b
    }

    #[test]
    fn signs_synthetic_macho() {
        let mut bytes = synthetic_macho();
        let out = signed(&bytes).unwrap();
        assert_eq!(be32(&out, 264), 2); // two entries left (CMS removed)
        assert_eq!(be32(&out, CD + 12), 2); // CS_ADHOC
        assert_eq!(&out[CD + 52..CD + 72], Sha1::digest(&bytes[0..256]).as_slice());
        assert_eq!(&out[364..380], &bytes[364..380]); // requirements kept
        bytes[CD + 39] = 0; // invalid page exponent
        assert!(signed(&bytes).is_err());
    }

    #[test]
    fn rejects_malformed_signatures() {
        for len in [0usize, 27, 28, 128] {
            assert!(signed(&vec![0u8; len]).is_err());
        }
    }
}

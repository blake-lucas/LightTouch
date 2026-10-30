//! The kernel patch a jailbroken device carries so Mobile Substrate's launcher, which inserts
//! /Library/MobileSubstrate/MobileSubstrate.dylib into every job through DYLD_INSERT_LIBRARIES, does not get the
//! inserted library's read denied inside a built-in sandbox profile (MobileSafari, MobileMail): the Seatbelt sandbox
//! kext's sb_evaluate is made to allow, as the era's untethers patched it, so a sandboxed process may read the dylib dyld
//! inserts. Applied only when the device is prepared jailbroken, to the decrypted kernelcache the boot runs: the kboot
//! boards bake the patched Mach-O into kboot.bin; the iBoot board installs it as the kernelcache the fsboot chain loads.
//!
//! ```ignore
//! let found = sandbox_patch::locate(&kernel)?;                 // sb_evaluate's prologue, and its bytes
//! let (patched, found) = sandbox_patch::apply(&kernel)?;
//! let (img3, found) = sandbox_patch::rewrap_kernelcache(&raw_img3, &kernel, &iv, &key)?;   // the iBoot chain's
//! ```
//! sb_evaluate is located by byte pattern, not a fixed offset: the Seatbelt kext's "bad opcode" string (the profile
//! interpreter's bad-opcode report, inside sb_evaluate) is found, its reference in the kext text walked for (both the
//! 4.x/5.x absolute literal-pool form and the 6.x/7.x PC-relative ldr/add form), and the function it sits in is taken
//! as the Thumb push{..,lr} prologue nearest before it.
use crate::error::Result;
use std::collections::HashMap;
use std::fmt;

/// Return "allowed" at once, writing the decision the caller reads back. sb_evaluate(result, …) leaves its verdict in
/// the caller's `result` struct (status at +0, filter at +4; 0 = allow) and returns 0; a bare `return 0` left that
/// struct unwritten and the sandboxed daemons faulted. So: `movs r1,#0; str r1,[r0]; str r1,[r0,#4]; movs r0,#0;
/// bx lr` — allow, result cleared, return 0. r0 is the result pointer on entry (the caller's arg0).
pub const PATCH: [u8; 10] = [0x00, 0x21, 0x01, 0x60, 0x41, 0x60, 0x00, 0x20, 0x70, 0x47];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The virtual address of sb_evaluate's prologue (the patch site).
    pub address: u32,
    /// The bytes there before the patch (a push{..,lr} prologue), for the record.
    pub original: Vec<u8>,
}

/// Why the patch site was not found. The fit check tells "no Seatbelt kext" (NoSandboxKext, NoString: iOS before the
/// prelinked sandbox) from a kernel the pattern does not resolve on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchError {
    NotMachO,
    NoSandboxKext,
    NoKextText,
    NoString,
    NoReference,
    NoPrologue,
    BadPatchSite,
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PatchError::NotMachO => "not a 32-bit Mach-O kernelcache",
            PatchError::NoSandboxKext => "no com.apple.security.sandbox kext in the prelink info",
            PatchError::NoKextText => "the sandbox kext has no __text",
            PatchError::NoString => "the sandbox kext has no \"bad opcode\" string (sb_evaluate's report)",
            PatchError::NoReference => "no reference to the \"bad opcode\" string in the kext text",
            PatchError::NoPrologue => "no push{..,lr} prologue before the reference",
            PatchError::BadPatchSite => "the patch site is not a push{..,lr} prologue",
        })
    }
}

impl std::error::Error for PatchError {}

impl From<PatchError> for crate::Error {
    fn from(e: PatchError) -> Self {
        crate::Error::unsupported(e.to_string())
    }
}

/// Locate sb_evaluate in a decrypted, decompressed kernelcache Mach-O. Fails when the kernel has no Seatbelt kext (iOS
/// before the prelinked sandbox) or the pattern does not resolve.
pub fn locate(kernel: &[u8]) -> std::result::Result<Found, PatchError> {
    let img = Image::new(kernel)?;
    let klo = img.sandbox_kext_load_addr().ok_or(PatchError::NoSandboxKext)?;
    let (taddr, tsize) = img.kext_text(klo).ok_or(PatchError::NoKextText)?;
    let saddr = crate::kboot::find(kernel, b"bad opcode\0").and_then(|so| img.off_to_addr(so)).ok_or(PatchError::NoString)?;
    let toff = img.addr_to_off(taddr).ok_or(PatchError::NoKextText)?;
    let tsize = tsize as usize;

    // Resilient Thumb walk of the kext text: collect push{..,lr} prologues and references to saddr.
    let mut prologues: Vec<u32> = Vec::new();
    let mut refs: Vec<u32> = Vec::new();
    let mut reg_word: HashMap<i32, u32> = HashMap::new(); // register -> last pc-relative word loaded
    let mut pos: usize = 0;
    while pos + 1 < tsize {
        let at = toff + pos;
        let addr = taddr.wrapping_add(pos as u32);
        let hw1 = img.u16(at);
        let len = if (hw1 & 0xF800) >= 0xE800 { 4 } else { 2 };
        if len == 4 && pos + 3 >= tsize {
            break;
        }
        let hw2 = if len == 4 { img.u16(at + 2) } else { 0 };

        if len == 2 && hw1 & 0xFE00 == 0xB400 && hw1 & 0x0100 != 0 {
            prologues.push(addr); // push {.., lr}
        } else if len == 4 && hw1 == 0xE92D && hw2 & 0x4000 != 0 {
            prologues.push(addr); // push.w {.., lr}
        } else if len == 2 && hw1 & 0xF800 == 0x4800 {
            // ldr rT, [pc, #imm]  (T1)
            let rt = ((hw1 >> 8) & 7) as i32;
            let base = addr.wrapping_add(4) & !3;
            let la = base.wrapping_add((hw1 & 0xFF) as u32 * 4);
            reference(&img, la, saddr, rt, addr, &mut reg_word, &mut refs);
        } else if len == 4 && hw1 & 0xFF7F == 0xF85F {
            // ldr.w rT, [pc, #imm12]  (T2)
            let rt = ((hw2 >> 12) & 0xF) as i32;
            let base = addr.wrapping_add(4) & !3;
            let imm = (hw2 & 0xFFF) as u32;
            let la = if hw1 & 0x0080 != 0 { base.wrapping_add(imm) } else { base.wrapping_sub(imm) };
            reference(&img, la, saddr, rt, addr, &mut reg_word, &mut refs);
        } else if len == 2 && hw1 & 0xFFF8 == 0x4478 {
            // add rD, pc  (Rm == pc): resolve a PC-relative delta loaded earlier
            let rd = (hw1 & 7) as i32;
            if let Some(&w) = reg_word.get(&rd) {
                let resolved = addr.wrapping_add(4).wrapping_add(w);
                if resolved == saddr {
                    refs.push(addr);
                }
            }
        }
        pos += len;
    }
    let x = *refs.iter().min().ok_or(PatchError::NoReference)?;
    let entry = *prologues.iter().filter(|&&p| p <= x).max().ok_or(PatchError::NoPrologue)?;
    let eo = img.addr_to_off(entry).ok_or(PatchError::NoPrologue)?;
    let orig = kernel[eo..eo + 4].to_vec();
    // A real prologue: push {..,lr} (0xB5xx) or push.w {..,lr} (0xE92D, lr bit set).
    let hw = orig[0] as u16 | (orig[1] as u16) << 8;
    let is_push16 = hw & 0xFE00 == 0xB400 && hw & 0x0100 != 0;
    let is_push32 = hw == 0xE92D && (orig[2] as u16 | (orig[3] as u16) << 8) & 0x4000 != 0;
    if !is_push16 && !is_push32 {
        return Err(PatchError::BadPatchSite);
    }
    Ok(Found { address: entry, original: orig })
}

fn reference(img: &Image, la: u32, saddr: u32, rt: i32, at: u32, reg_word: &mut HashMap<i32, u32>, refs: &mut Vec<u32>) {
    let Some(lo) = img.addr_to_off(la) else { return };
    if lo + 4 > img.bytes.len() {
        return;
    }
    let w = img.u32(lo);
    if w == saddr {
        refs.push(at); // absolute literal-pool reference (4.x/5.x)
    } else {
        reg_word.insert(rt, w); // a delta, to be resolved by a later add rD, pc (6.x/7.x)
    }
}

/// Apply the patch in place, returning the patched kernelcache and where it landed.
pub fn apply(kernel: &[u8]) -> std::result::Result<(Vec<u8>, Found), PatchError> {
    let found = locate(kernel)?;
    let mut out = kernel.to_vec();
    let o = Image::new(kernel)?.addr_to_off(found.address).ok_or(PatchError::BadPatchSite)?;
    out[o..o + PATCH.len()].copy_from_slice(&PATCH);
    Ok((out, found))
}

// MARK: - The real-iBoot kernelcache (k48)

/// Patch the plaintext kernel, then put it back in the IMG3 the fsboot iBoot loads: complzss-wrap and re-encrypt with the
/// component's own key so iBoot decrypts it through the same KBAG as the stock one, and rebuild the IMG3 around the
/// original's tags (TYPE/KBAG/SHSH/… unchanged). The patched iBoot does not check the signature.
pub fn rewrap_kernelcache(raw: &[u8], kernel: &[u8], iv: &[u8], key: &[u8]) -> Result<(Vec<u8>, Found)> {
    let (patched, found) = apply(kernel)?;
    Ok((rewrap(raw, &patched, iv, key)?, found))
}

/// Put an already-patched plaintext kernel back in its IMG3, as [`rewrap_kernelcache`] does after its own patch (the
/// caller may have applied other kernel patches first).
pub fn rewrap(raw: &[u8], patched: &[u8], iv: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    let mut payload = complzss_wrap(patched);
    if payload.len() % 16 != 0 {
        payload.resize(payload.len() + (16 - payload.len() % 16), 0);
    }
    let cipher = aes_cbc_encrypt(&payload, iv, key)?;
    rebuild_img3(raw, &cipher)
}

/// A valid Apple-LZSS stream of all literals (flag byte then eight bytes), so no compressor is needed: iBoot's
/// decompressor reproduces the kernel. Larger than a real compression, which the system volume has room for.
fn literal_lzss(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8 + 1);
    let mut i = 0;
    while i < data.len() {
        let n = (data.len() - i).min(8);
        out.push(((1u16 << n) - 1) as u8); // n low bits set: n literals follow
        out.extend_from_slice(&data[i..i + n]);
        i += n;
    }
    out
}

/// The complzss container a decrypted kernelcache is (lzss::complzss reads it back): "complzss", adler32, ulen, clen
/// big-endian in a 0x180 header, the LZSS body after it.
fn complzss_wrap(kernel: &[u8]) -> Vec<u8> {
    let body = literal_lzss(kernel);
    let mut h = vec![0u8; 0x180];
    h[..8].copy_from_slice(b"complzss");
    let be = |h: &mut [u8], o: usize, v: u32| h[o..o + 4].copy_from_slice(&v.to_be_bytes());
    be(&mut h, 8, crate::lzss::adler32(kernel));
    be(&mut h, 12, kernel.len() as u32);
    be(&mut h, 16, body.len() as u32);
    h.extend(body);
    h
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Rebuild an IMG3 with a new DATA payload, every other tag kept, the header sizes recomputed.
fn rebuild_img3(raw: &[u8], new_data: &[u8]) -> Result<Vec<u8>> {
    let d = raw;
    if d.len() < 0x14 || d[0..4] != [0x33, 0x67, 0x6D, 0x49] {
        return Err(crate::unsupported!("not an img3"));
    }
    let full = le32(d, 4) as usize;
    let mut body: Vec<u8> = Vec::new();
    let mut signed_end = 0; // tags before SHSH are the signed area
    let mut off = 0x14;
    while off + 12 <= full && off + 12 <= d.len() {
        let magic: String = d[off..off + 4].iter().rev().map(|&b| b as char).collect();
        let total = le32(d, off + 4) as usize;
        if total < 12 {
            break;
        }
        if magic == "DATA" {
            body.extend_from_slice(&d[off..off + 4]); // the file's magic bytes, as stored
            body.extend_from_slice(&((12 + new_data.len()) as u32).to_le_bytes());
            body.extend_from_slice(&(new_data.len() as u32).to_le_bytes());
            body.extend_from_slice(new_data);
        } else {
            body.extend_from_slice(&d[off..off + total]);
        }
        if magic != "SHSH" && magic != "CERT" {
            signed_end = body.len();
        }
        off += total;
    }
    let mut out = d[0..0x14].to_vec();
    out[4..8].copy_from_slice(&((0x14 + body.len()) as u32).to_le_bytes()); // fullSize
    out[8..12].copy_from_slice(&(body.len() as u32).to_le_bytes()); // size without the header
    out[12..16].copy_from_slice(&(signed_end as u32).to_le_bytes()); // signed area (tags up to SHSH)
    out.extend(body);
    Ok(out)
}

/// AES-CBC encryption without padding; `buf` must be whole blocks. 16, 24 or 32-byte keys.
fn aes_cbc_encrypt(buf: &[u8], iv: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::NoPadding};
    if iv.len() != 16 || buf.len() % 16 != 0 {
        return Err(crate::unsupported!("AES: key {} B, iv {} B, data {} B", key.len(), iv.len(), buf.len()));
    }
    let mut out = buf.to_vec();
    let n = out.len();
    let ok = match key.len() {
        16 => cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv).map(|c| c.encrypt_padded_mut::<NoPadding>(&mut out, n).map(|_| ())),
        24 => cbc::Encryptor::<aes::Aes192>::new_from_slices(key, iv).map(|c| c.encrypt_padded_mut::<NoPadding>(&mut out, n).map(|_| ())),
        32 => cbc::Encryptor::<aes::Aes256>::new_from_slices(key, iv).map(|c| c.encrypt_padded_mut::<NoPadding>(&mut out, n).map(|_| ())),
        _ => return Err(crate::unsupported!("AES: key {} B, iv {} B, data {} B", key.len(), iv.len(), buf.len())),
    };
    match ok {
        Ok(Ok(())) => Ok(out),
        _ => Err(crate::internal!("AES-CBC encryption failed")),
    }
}

/// A read view over the kernelcache Mach-O: outer segments for address<->offset, the prelink info, and the kext Mach-O's
/// own __text. Kept small and local to the patch; the shared macho.rs reads commands, not segment maps.
struct Image<'a> {
    bytes: &'a [u8],
    // (vmaddr, vmsize, fileoff, filesize) per outer LC_SEGMENT.
    segs: Vec<(u32, u32, u32, u32)>,
    // (sectname, segname, addr, size, off) per outer section.
    sects: Vec<(String, String, u32, u32, u32)>,
}

impl<'a> Image<'a> {
    fn new(b: &'a [u8]) -> std::result::Result<Image<'a>, PatchError> {
        if b.len() < 28 || le32(b, 0) != 0xFEED_FACE || le32(b, 4) != 12 {
            return Err(PatchError::NotMachO);
        }
        let mut segs = Vec::new();
        let mut sects = Vec::new();
        let mut off = 28;
        for _ in 0..le32(b, 16) {
            if off + 8 > b.len() {
                break;
            }
            let cmd = le32(b, off);
            let size = le32(b, off + 4) as usize;
            if size < 8 || off + size > b.len() {
                break;
            }
            if cmd == 1 {
                // LC_SEGMENT
                segs.push((le32(b, off + 24), le32(b, off + 28), le32(b, off + 32), le32(b, off + 36)));
                let nsects = le32(b, off + 48) as usize;
                let mut so = off + 56;
                for _ in 0..nsects {
                    if so + 68 > off + size {
                        break;
                    }
                    sects.push((cstr(b, so, 16), cstr(b, so + 16, 16), le32(b, so + 32), le32(b, so + 36), le32(b, so + 40)));
                    so += 68;
                }
            }
            off += size;
        }
        Ok(Image { bytes: b, segs, sects })
    }

    fn u16(&self, o: usize) -> u16 {
        self.bytes[o] as u16 | (self.bytes[o + 1] as u16) << 8
    }
    fn u32(&self, o: usize) -> u32 {
        le32(self.bytes, o)
    }

    fn addr_to_off(&self, a: u32) -> Option<usize> {
        self.segs
            .iter()
            .find(|&&(vmaddr, vmsize, _, filesize)| a >= vmaddr && a < vmaddr.wrapping_add(vmsize) && a.wrapping_sub(vmaddr) < filesize)
            .map(|&(vmaddr, _, fileoff, _)| (fileoff.wrapping_add(a.wrapping_sub(vmaddr))) as usize)
    }

    fn off_to_addr(&self, o: usize) -> Option<u32> {
        self.segs
            .iter()
            .find(|&&(_, _, fileoff, filesize)| o >= fileoff as usize && o < fileoff.wrapping_add(filesize) as usize)
            .map(|&(vmaddr, _, fileoff, _)| vmaddr.wrapping_add((o - fileoff as usize) as u32))
    }

    /// The load address of the com.apple.security.sandbox kext: the _PrelinkExecutableLoadAddr in the same kext dict as
    /// the bundle id in __PRELINK_INFO. The key sits ahead of the id on 5.x-7.x but after it on 4.x, so take the
    /// occurrence nearest the id (its own dict's) rather than the last one before it (which on 4.x is the prior kext's).
    fn sandbox_kext_load_addr(&self) -> Option<u32> {
        let (_, _, _, size, off) = *self.sects.iter().find(|s| s.0 == "__info" && s.1 == "__PRELINK_INFO")?;
        let end = (off as usize + size as usize).min(self.bytes.len());
        let info = String::from_utf8_lossy(&self.bytes[off as usize..end]);
        let anchor = info.find("com.apple.security.sandbox")?;
        // "<key>_PrelinkExecutableLoadAddr</key><integer size="64">0x805f1000</integer>": the value follows the
        // integer element's own '>' (the one before it closes </key>), and the element may carry attributes.
        let key = "_PrelinkExecutableLoadAddr</key><integer";
        let mut before: Option<(usize, u32)> = None; // last occurrence before the id
        let mut after: Option<(usize, u32)> = None; // first occurrence at or after the id
        let mut search = 0;
        while let Some(rel) = info[search..].find(key) {
            let k = search + rel;
            let value = k + key.len();
            if let Some(gtr) = info[value..].find('>') {
                let gt = value + gtr + 1;
                if let Some(ltr) = info[gt..].find('<') {
                    let hex = info[gt..gt + ltr].trim();
                    if let Some(h) = hex.strip_prefix("0x")
                        && let Ok(v) = u32::from_str_radix(h, 16)
                    {
                        if k < anchor {
                            before = Some((k, v));
                        } else if after.is_none() {
                            after = Some((k, v));
                        }
                    }
                }
            }
            search = k + key.len();
        }
        match (before, after) {
            (None, a) => a.map(|(_, v)| v),
            (Some((_, v)), None) => Some(v),
            (Some((bk, bv)), Some((ak, av))) => {
                // nearest the id by key position (distances into the same ASCII info string)
                if anchor - bk <= ak - anchor { Some(bv) } else { Some(av) }
            }
        }
    }

    /// The kext Mach-O's own __text (addr, size) given its load address.
    fn kext_text(&self, kaddr: u32) -> Option<(u32, u32)> {
        let koff = self.addr_to_off(kaddr)?;
        if koff + 28 > self.bytes.len() || le32(self.bytes, koff) != 0xFEED_FACE {
            return None;
        }
        let mut off = koff + 28;
        for _ in 0..le32(self.bytes, koff + 16) {
            if off + 8 > self.bytes.len() {
                break;
            }
            let cmd = le32(self.bytes, off);
            let size = le32(self.bytes, off + 4) as usize;
            if size < 8 {
                break;
            }
            if cmd == 1 {
                let nsects = le32(self.bytes, off + 48) as usize;
                let mut so = off + 56;
                for _ in 0..nsects {
                    if so + 68 > off + size {
                        break;
                    }
                    if cstr(self.bytes, so, 16) == "__text" {
                        return Some((le32(self.bytes, so + 32), le32(self.bytes, so + 36)));
                    }
                    so += 68;
                }
            }
            off += size;
        }
        None
    }
}

fn cstr(b: &[u8], o: usize, n: usize) -> String {
    let end = (o..o + n).find(|&i| b[i] == 0).unwrap_or(o + n);
    String::from_utf8_lossy(&b[o..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A kernelcache Mach-O whose __PRELINK_INFO.__info is `info`, for the prelink lookups.
    fn prelinked_kernel(info: &str) -> Vec<u8> {
        const SEGMENT: usize = 28; // LC_SEGMENT
        const SECTION: usize = SEGMENT + 56;
        const DATA: usize = 0x100;
        let mut b = vec![0u8; DATA];
        b[0..4].copy_from_slice(&0xFEED_FACEu32.to_le_bytes());
        b[4..8].copy_from_slice(&12u32.to_le_bytes()); // CPU_TYPE_ARM
        b[16..20].copy_from_slice(&1u32.to_le_bytes()); // ncmds
        b[20..24].copy_from_slice(&(56u32 + 68).to_le_bytes()); // sizeofcmds
        b[SEGMENT..SEGMENT + 4].copy_from_slice(&1u32.to_le_bytes()); // LC_SEGMENT
        b[SEGMENT + 4..SEGMENT + 8].copy_from_slice(&(56u32 + 68).to_le_bytes());
        b[SEGMENT + 8..SEGMENT + 22].copy_from_slice(b"__PRELINK_INFO");
        b[SEGMENT + 48..SEGMENT + 52].copy_from_slice(&1u32.to_le_bytes()); // nsects
        b[SECTION..SECTION + 6].copy_from_slice(b"__info");
        b[SECTION + 16..SECTION + 30].copy_from_slice(b"__PRELINK_INFO");
        b[SECTION + 36..SECTION + 40].copy_from_slice(&(info.len() as u32).to_le_bytes());
        b[SECTION + 40..SECTION + 44].copy_from_slice(&(DATA as u32).to_le_bytes());
        b.extend_from_slice(info.as_bytes());
        b
    }

    /// The load address sits in an <integer> element that carries a size attribute: the value follows that element's
    /// own '>'. Reading the first '>' after the key finds the one closing </key>, so no address ever parsed and every
    /// kernel came back as "no com.apple.security.sandbox kext" — the sandbox patch never ran, and MobileSafari died
    /// at dyld on MobileSubstrate's inserted library.
    #[test]
    fn prelink_load_addr_reads_the_integer_element() {
        let info = concat!(
            "<dict><key>CFBundleIdentifier</key><string>com.apple.security.sandbox</string>",
            "<key>_PrelinkExecutableLoadAddr</key><integer size=\"64\">0x805f1000</integer></dict>"
        );
        let kernel = prelinked_kernel(info);
        let img = Image::new(&kernel).unwrap();
        assert_eq!(img.sandbox_kext_load_addr(), Some(0x805f1000));
    }

    #[test]
    fn literal_lzss_round_trips() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let stream = literal_lzss(&data);
        assert_eq!(crate::lzss::decompress(&stream, 0x20), data);
    }

    #[test]
    fn complzss_wrap_reads_back() {
        let kernel: Vec<u8> = (0..5000u32).map(|i| (i ^ 0x5A) as u8).collect();
        let wrapped = complzss_wrap(&kernel);
        assert_eq!(crate::lzss::complzss(&wrapped).unwrap(), kernel);
    }

    /// rebuild_img3 keeps every tag but DATA, recomputes the sizes, and the signed area ends before SHSH.
    #[test]
    fn rebuild_img3_keeps_tags() {
        let mut d = vec![0x33, 0x67, 0x6D, 0x49]; // "Img3"
        let mut tags = Vec::new();
        for (tag, len) in [(b"EPYT", 4usize), (b"ATAD", 16), (b"HSHS", 8)] {
            tags.extend_from_slice(tag);
            tags.extend_from_slice(&((12 + len) as u32).to_le_bytes());
            tags.extend_from_slice(&(len as u32).to_le_bytes());
            tags.extend(std::iter::repeat_n(0xAu8, len));
        }
        d.extend_from_slice(&((0x14 + tags.len()) as u32).to_le_bytes());
        d.extend_from_slice(&(tags.len() as u32).to_le_bytes());
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(&tags);
        let new = vec![0xCCu8; 32];
        let out = rebuild_img3(&d, &new).unwrap();
        let full = le32(&out, 4) as usize;
        assert_eq!(full, out.len());
        // the DATA payload is the new one
        assert_eq!(&out[0x14 + 16 + 12..0x14 + 16 + 12 + 32], &new[..]);
        // signed area ends before SHSH: TYPE (16) + DATA (12 + 32)
        assert_eq!(le32(&out, 12), (16 + 12 + 32) as u32);
    }

    /// The decrypted-kernelcache corpus (set LTM_SANDBOX_KERNELS to the decrypted cache): every kernel with a Seatbelt
    /// kext resolves to a push{..,lr} prologue, and applying leaves only the four patch bytes changed.
    #[test]
    #[ignore = "set LTM_SANDBOX_KERNELS to a tree of kernelcache.mach files"]
    fn locates_across_cached_kernels() {
        let Ok(root) = std::env::var("LTM_SANDBOX_KERNELS") else { return };
        let mut ok = 0;
        for entry in walkdir(std::path::Path::new(&root)) {
            if entry.file_name().and_then(|n| n.to_str()) != Some("kernelcache.mach") {
                continue;
            }
            let kernel = std::fs::read(&entry).unwrap();
            match apply(&kernel) {
                Ok((patched, found)) => {
                    let img = Image::new(&patched).unwrap();
                    let o = img.addr_to_off(found.address).unwrap();
                    assert_eq!(&patched[o..o + PATCH.len()], &PATCH);
                    let changed: Vec<usize> = kernel.iter().zip(&patched).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
                    assert!(changed.iter().all(|&i| (o..o + PATCH.len()).contains(&i)));
                    ok += 1;
                }
                Err(PatchError::NoSandboxKext) | Err(PatchError::NoString) => {} // iOS before the prelinked kext
                Err(e) => panic!("{}: {e}", entry.display()),
            }
        }
        assert!(ok > 0);
    }

    fn walkdir(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(root) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.extend(walkdir(&p));
                } else {
                    out.push(p);
                }
            }
        }
        out
    }
}

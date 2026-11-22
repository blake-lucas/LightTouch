//! The dyld shared cache: mappings, images, their symbols and C strings (SharedCache/SharedCache.swift), and
//! AppSync's cache patch. dyld_v1 caches (iOS 3.x-6.x, armv6/armv7) keep each cached image's own LC_SYMTAB with cache
//! file offsets (an unslid linkedit), so an image's symbols and sections read straight out of the cache file.
//! Port of imgtools/appsync_cachepatch.py and ipad1_rootfs.cache_images/image_strings.
use crate::error::Result;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use crate::macho::{c_bytes, hex, hex_bytes, latin1, le16, le32, le64, LC_SEGMENT, LC_SYMTAB};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    pub address: u64,
    pub size: u64,
    pub file_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub path: String,
    pub address: u64,
    /// The image's mach header in the cache file; None when its address is in no mapping.
    pub header_offset: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: u8,
    pub desc: u16,
    pub value: u32,
}

pub struct DyldSharedCache {
    /// The whole cache (hundreds of MB on 5.x/6.x; read once).
    pub data: Vec<u8>,
    pub mappings: Vec<Mapping>,
    pub images: Vec<Image>,
}

fn offset_in(maps: &[Mapping], a: u64) -> Option<usize> {
    maps.iter().find(|m| m.address <= a && a < m.address + m.size).map(|m| (m.file_offset + a - m.address) as usize)
}

impl DyldSharedCache {
    pub fn parse(data: Vec<u8>) -> Result<DyldSharedCache> {
        if data.len() < 0x20 || !data.starts_with(b"dyld_v1") {
            return Err(crate::unsupported!("not a dyld_v1 shared cache"));
        }
        let (mo, mc) = (le32(&data, 0x10) as usize, le32(&data, 0x14) as usize);
        let (io, ic) = (le32(&data, 0x18) as usize, le32(&data, 0x1c) as usize);
        if mo + 32 * mc > data.len() || io + 32 * ic > data.len() {
            return Err(crate::unsupported!("a truncated dyld_v1 shared cache"));
        }
        let mappings: Vec<Mapping> = (0..mc)
            .map(|i| Mapping {
                address: le64(&data, mo + 32 * i),
                size: le64(&data, mo + 32 * i + 8),
                file_offset: le64(&data, mo + 32 * i + 16),
            })
            .collect();
        let images = (0..ic)
            .map(|i| {
                let address = le64(&data, io + 32 * i);
                let path = latin1(c_bytes(&data, le32(&data, io + 32 * i + 24) as usize));
                Image { path, address, header_offset: offset_in(&mappings, address) }
            })
            .collect();
        Ok(DyldSharedCache { data, mappings, images })
    }

    pub fn file_offset(&self, address: u64) -> Option<usize> {
        offset_in(&self.mappings, address)
    }

    pub fn image(&self, path: &str) -> Option<&Image> {
        self.images.iter().find(|i| i.path == path)
    }

    /// An image's load commands: (cmd, offset of the command in the cache file) for each.
    pub fn load_commands(&self, img: &Image) -> Vec<(u32, usize)> {
        let b = &self.data;
        let Some(h) = img.header_offset else { return Vec::new() };
        if le32(b, h) != 0xFEED_FACE {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut off = h + 28;
        for _ in 0..le32(b, h + 16) {
            if off + 8 > b.len() {
                break;
            }
            out.push((le32(b, off), off));
            let size = le32(b, off + 4) as usize;
            if size == 0 {
                break;
            }
            off += size;
        }
        out
    }

    /// The load command string at `off` (a dylib's name): its lc_str offset is at `off + 8`.
    pub fn command_string(&self, off: usize) -> String {
        latin1(c_bytes(&self.data, off + le32(&self.data, off + 8) as usize))
    }

    /// (symoff, nsyms, stroff) of the image's LC_SYMTAB (the last one, as the Swift walk keeps).
    fn symtab(&self, img: &Image) -> Option<(usize, usize, usize)> {
        let b = &self.data;
        self.load_commands(img)
            .into_iter()
            .filter(|&(cmd, _)| cmd == LC_SYMTAB)
            .map(|(_, off)| (le32(b, off + 8) as usize, le32(b, off + 12) as usize, le32(b, off + 16) as usize))
            .last()
    }

    /// The image's LC_SYMTAB entries in table order; `body` returns false to stop.
    pub fn for_each_symbol(&self, img: &Image, mut body: impl FnMut(Symbol) -> bool) {
        let Some((symoff, nsyms, stroff)) = self.symtab(img) else { return };
        let b = &self.data;
        for s in 0..nsyms {
            let e = symoff + 12 * s;
            if e + 12 > b.len() {
                return;
            }
            let sym = Symbol {
                name: latin1(c_bytes(b, stroff + le32(b, e) as usize)),
                kind: b[e + 4],
                desc: le16(b, e + 6),
                value: le32(b, e + 8),
            };
            if !body(sym) {
                return;
            }
        }
    }

    pub fn symbol_names(&self, img: &Image) -> Vec<String> {
        let mut out = Vec::new();
        self.for_each_symbol(img, |s| {
            out.push(s.name);
            true
        });
        out
    }

    /// The C strings of one of the image's sections (e.g. "__cstring"), as image_strings(section=) gives them:
    /// split on NUL, empty ones dropped, each byte one character.
    pub fn c_strings(&self, img: &Image, section: &str) -> Vec<String> {
        let b = &self.data;
        let mut out = Vec::new();
        for (cmd, off) in self.load_commands(img) {
            if cmd != LC_SEGMENT {
                continue;
            }
            for k in 0..le32(b, off + 48) as usize {
                let so = off + 56 + 68 * k;
                let name = c_bytes(b, so);
                if String::from_utf8_lossy(&name[..name.len().min(16)]) != section {
                    continue;
                }
                let Some(start) = self.file_offset(le32(b, so + 32) as u64) else { continue };
                let Some(bytes) = b.get(start..start + le32(b, so + 36) as usize) else { continue };
                out.extend(bytes.split(|&c| c == 0).filter(|s| !s.is_empty()).map(latin1));
            }
        }
        out
    }

    /// (address, Thumb) of an N_SECT symbol with a non-zero value, searching every image's symbol table in
    /// cache order (appsync_cachepatch.find_symbol).
    pub fn find_symbol(&self, name: &str) -> Result<(u64, bool)> {
        let b = &self.data;
        let want = name.as_bytes();
        for img in &self.images {
            if img.header_offset.is_none() {
                return Err(crate::internal!("VA {} not in any mapping", hex(img.address)));
            }
            let Some((symoff, nsyms, stroff)) = self.symtab(img) else { continue };
            for s in 0..nsyms {
                let e = symoff + 12 * s;
                if e + 12 > b.len() {
                    break;
                }
                let value = le32(b, e + 8);
                if value == 0 || b[e + 4] & 0x0e != 0x0e {
                    continue;
                }
                if c_bytes(b, stroff + le32(b, e) as usize) == want {
                    return Ok((value as u64, le16(b, e + 6) & 0x0008 != 0)); // N_ARM_THUMB_DEF
                }
            }
        }
        Err(crate::unsupported!("{name} not found in cache image symbol tables"))
    }

    /// Refresh only existing code-page hash slots after intentional cache edits
    /// (`SharedCacheSignature.rehashingCodeSignature`). Layout, special slots, scatter vectors and
    /// payload are retained; this neither creates a signature nor changes admission policy.
    pub fn rehash_code_signature(&self) -> Result<Vec<u8>> {
        let data = &self.data;
        let invalid = |d: &str| crate::unsupported!("shared cache signature: {d}");
        let word = |at: usize, count: usize, big: bool| -> Result<u64> {
            if !(1..=8).contains(&count) || at > data.len().saturating_sub(count) {
                return Err(invalid("truncated metadata"));
            }
            let mut value: u64 = 0;
            for i in 0..count {
                let byte = data[at + i] as u64;
                value |= byte << (8 * if big { count - 1 - i } else { i });
            }
            Ok(value)
        };
        let signature = word(40, 8, false)? as usize;
        let mut allocation = word(48, 8, false)? as usize;
        if signature < 56 || signature > data.len().saturating_sub(12) {
            return Err(invalid("missing signature"));
        }
        if allocation == 0 {
            allocation = data.len() - signature;
        }
        if allocation < 12 || allocation > data.len() - signature || word(signature, 4, true)? != 0xfade0cc0 {
            return Err(invalid("invalid signature extent"));
        }
        let length = word(signature + 4, 4, true)? as usize;
        let count = word(signature + 8, 4, true)? as usize;
        if length < 12 || length > allocation || count > (length - 12) / 8 {
            return Err(invalid("invalid signature index"));
        }
        struct Directory {
            start: usize,
            hashes: usize,
            pages: usize,
            limit: usize,
            width: usize,
            kind: u8,
            page_size: usize,
        }
        let mut directories: Vec<Directory> = Vec::new();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for i in 0..count {
            let relative = word(signature + 16 + i * 8, 4, true)? as usize;
            if relative < 12 + count * 8 || relative > length - 8 {
                return Err(invalid("invalid blob offset"));
            }
            let cd = signature + relative;
            let size = word(cd + 4, 4, true)? as usize;
            if size < 8 || size > length - relative {
                return Err(invalid("truncated blob"));
            }
            spans.push((relative, relative + size));
            if word(cd, 4, true)? != 0xfade0c02 {
                continue;
            }
            if size < 44 {
                return Err(invalid("truncated CodeDirectory"));
            }
            let version = word(cd + 8, 4, true)? as usize;
            if !(0x20000..=0x20100).contains(&version) {
                return Err(invalid("unsupported CodeDirectory version"));
            }
            let minimum = if version >= 0x20100 { 48 } else { 44 };
            if size < minimum {
                return Err(invalid("truncated CodeDirectory header"));
            }
            let hashes = word(cd + 16, 4, true)? as usize;
            let identifier = word(cd + 20, 4, true)? as usize;
            let special = word(cd + 24, 4, true)? as usize;
            let pages = word(cd + 28, 4, true)? as usize;
            let limit = word(cd + 32, 4, true)? as usize;
            let width = data[cd + 36] as usize;
            let kind = data[cd + 37];
            let exponent = data[cd + 39] as usize;
            if !((kind == 1 && width == 20) || (kind == 2 && width == 32))
                || !(1..=20).contains(&exponent)
                || limit == 0
                || limit > signature
                || hashes < minimum + special * width
                || hashes > size
                || pages > (size - hashes) / width
            {
                return Err(invalid("unsupported code-page layout"));
            }
            let page_size = 1usize << exponent;
            let metadata_end = hashes - special * width;
            if pages != (limit + page_size - 1) / page_size
                || identifier < minimum
                || identifier >= metadata_end
                || !data[cd + identifier..cd + metadata_end].contains(&0)
            {
                return Err(invalid("invalid code-page coverage or identifier"));
            }
            if version >= 0x20100 {
                let scatter = word(cd + 44, 4, true)? as usize;
                if scatter != 0 {
                    if scatter < minimum {
                        return Err(invalid("invalid scatter vector"));
                    }
                    let mut entry = scatter;
                    let mut previous_end = 0usize;
                    loop {
                        if entry > metadata_end.saturating_sub(24) {
                            return Err(invalid("unterminated scatter vector"));
                        }
                        let amount = word(cd + entry, 4, true)? as usize;
                        let first = word(cd + entry + 4, 4, true)? as usize;
                        let target = word(cd + entry + 8, 8, true)?;
                        let spare = word(cd + entry + 16, 8, true)?;
                        if spare != 0 {
                            return Err(invalid("unsupported scatter record"));
                        }
                        if amount == 0 {
                            if first != 0 || target != 0 {
                                return Err(invalid("invalid scatter terminator"));
                            }
                            break;
                        }
                        if first < previous_end
                            || amount > pages
                            || first > pages - amount
                            || target == 0
                            || target % page_size as u64 != 0
                        {
                            return Err(invalid("invalid scatter mapping"));
                        }
                        previous_end = first + amount;
                        entry += 24;
                    }
                }
            }
            directories.push(Directory { start: cd, hashes, pages, limit, width, kind, page_size });
        }
        spans.sort_unstable();
        for pair in spans.windows(2) {
            if pair[0].1 > pair[1].0 {
                return Err(invalid("overlapping signature blobs"));
            }
        }
        if directories.is_empty() {
            return Err(invalid("no supported CodeDirectory"));
        }
        let mut result = data.clone();
        for cd in directories {
            for page in 0..cd.pages {
                let lo = page * cd.page_size;
                let hi = cd.limit.min((page + 1) * cd.page_size);
                let digest: Vec<u8> = if cd.kind == 1 {
                    Sha1::digest(&data[lo..hi]).to_vec()
                } else {
                    Sha256::digest(&data[lo..hi]).to_vec()
                };
                let at = cd.start + cd.hashes + page * cd.width;
                if data[at..at + cd.width] != digest[..] {
                    result[at..at + cd.width].copy_from_slice(&digest);
                }
            }
        }
        Ok(result)
    }
}

/// The amfid half of AppSync: MISValidateSignature -> `movs r0,#0; bx lr`, located by symbol and byte-checked (a
/// Thumb `push {..., lr}` entry) before anything is written. Historical: preparation interposes libmis in the
/// installation service instead, so only the fit check's inspection (FitCheck.appSyncCache) uses it.
pub mod app_sync_cache_patch {
    use super::*;

    pub const TARGET: &str = "_MISValidateSignature";
    pub const PATCH: [u8; 4] = [0x00, 0x20, 0x70, 0x47];

    pub fn looks_like_thumb_entry(b: &[u8]) -> bool {
        let hw = u16::from_le_bytes([b[0], b[1]]);
        let hw2 = u16::from_le_bytes([b[2], b[3]]);
        if hw & 0xFF00 == 0xB500 {
            return true; // push {..., lr}  (3.x/4.x, 5.0 beta 9A5220p)
        }
        if hw == 0xE92D {
            return hw2 & 0x4000 != 0; // push.w with LR
        }
        // iOS 5.x libmis exports MISValidateSignature as a tail-thunk `movs rN,#imm ; b.w <impl>` (9A5288d..9B206:
        // 0022 fff7). Overwriting its first word with `movs r0,#0 ; bx lr` returns success just as patching a framed
        // entry does. Recognized by shape, not by build.
        if hw & 0xF800 == 0x2000 {
            return hw2 & 0xF800 == 0xF000; // movs rN,#imm then a 32-bit branch
        }
        false
    }

    /// The patch target in `cache`: (its VA, file offset, current first word); an error where it is not a Thumb
    /// function entry (or already the patch).
    pub fn locate(cache: &DyldSharedCache) -> Result<(u64, usize, [u8; 4])> {
        let (va, thumb) = cache.find_symbol(TARGET)?;
        let foff = cache.file_offset(va).ok_or_else(|| crate::internal!("VA {} not in any mapping", hex(va)))?;
        let cur: [u8; 4] = cache
            .data
            .get(foff..foff + 4)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| crate::internal!("VA {} past the end of the cache", hex(va)))?;
        if !(cur == PATCH || (thumb && looks_like_thumb_entry(&cur))) {
            return Err(crate::unsupported!(
                "{TARGET} prologue {} at {} is not a Thumb function entry — refusing to patch",
                hex_bytes(&cur),
                hex(foff)
            ));
        }
        Ok((va, foff, cur))
    }

}

#[cfg(test)]
mod tests {
    use super::app_sync_cache_patch::looks_like_thumb_entry;

    #[test]
    fn thumb_entry_check() {
        assert!(looks_like_thumb_entry(&[0x80, 0xb5, 0x00, 0xaf])); // push {r7,lr}  (3.x/4.x/5.0b)
        assert!(looks_like_thumb_entry(&[0x2d, 0xe9, 0xf0, 0x4f])); // push.w with lr
        assert!(looks_like_thumb_entry(&[0x00, 0x22, 0xff, 0xf7])); // 5.x thunk: movs r2,#0 ; b.w
        assert!(!looks_like_thumb_entry(&[0x2d, 0xe9, 0xf0, 0x0f])); // push.w without lr
        assert!(!looks_like_thumb_entry(&[0x00, 0x20, 0x70, 0x47])); // movs r0,#0 ; bx lr (no branch)
        assert!(!looks_like_thumb_entry(&[0x00, 0x22, 0x00, 0x22])); // movs ; movs (no branch)
        assert!(!looks_like_thumb_entry(&[0x00, 0x00, 0x00, 0x00])); // data
    }

    #[test]
    fn not_a_cache() {
        assert!(super::DyldSharedCache::parse(b"dyld_v2 nope".to_vec()).is_err());
    }
}

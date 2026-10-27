//! The dyld shared cache: mappings, images, their symbols and C strings (SharedCache/SharedCache.swift), and
//! AppSync's cache patch. dyld_v1 caches (iOS 3.x-6.x, armv6/armv7) keep each cached image's own LC_SYMTAB with cache
//! file offsets (an unslid linkedit), so an image's symbols and sections read straight out of the cache file.
//! Port of imgtools/appsync_cachepatch.py and ipad1_rootfs.cache_images/image_strings.
use crate::error::Result;
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

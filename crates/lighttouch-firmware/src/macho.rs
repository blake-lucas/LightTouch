//! Public: the N45 recipe scans exported symbols with it (N72Board.exportedSymbols).
//! Just enough of a 32-bit little-endian Mach-O for the fit checks and the helpers' signature checks
//! (FitCheck.swift's MachO32, Portability/MachOHeaders.swift): load commands, linked images, nlist, the export
//! trie's re-exports, and the header facts MachOSignature reads. Malformed input reads as "not a Mach-O" or as
//! fewer symbols, never as a panic.

pub const REQ_DYLD: u32 = 0x8000_0000;
pub const LC_SEGMENT: u32 = 0x1;
pub const LC_SYMTAB: u32 = 0x2;
pub const LC_LOAD_DYLIB: u32 = 0xC;
pub const LC_SUB_UMBRELLA: u32 = 0x13;
pub const LC_SUB_LIBRARY: u32 = 0x15;
pub const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018;
pub const LC_CODE_SIGNATURE: u32 = 0x1D;
pub const LC_REEXPORT_DYLIB: u32 = 0x8000_001F;
pub const LC_DYLD_INFO: u32 = 0x22;
pub const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
pub const LC_LOAD_UPWARD_DYLIB: u32 = 0x8000_0023;
pub const LC_VERSION_MIN_IPHONEOS: u32 = 0x25;
pub const LC_MAIN: u32 = 0x8000_0028;
pub const LC_BUILD_VERSION: u32 = 0x32;

/// The commands that link an image (ordinals count these, in order).
pub fn is_link_command(cmd: u32) -> bool {
    matches!(cmd, LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB | LC_REEXPORT_DYLIB | LC_LOAD_UPWARD_DYLIB)
}

pub fn le16(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
pub fn le32(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
pub fn le64(b: &[u8], o: usize) -> u64 {
    le32(b, o) as u64 | (le32(b, o + 4) as u64) << 32
}
pub fn be32(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// The NUL-terminated bytes at `o` (to the end if there is no NUL; empty past the end).
pub fn c_bytes(b: &[u8], o: usize) -> &[u8] {
    let Some(rest) = b.get(o..) else { return &[] };
    &rest[..rest.iter().position(|&c| c == 0).unwrap_or(rest.len())]
}

/// Bytes as Latin-1 text, as the shared cache's strings are read (every byte one character).
pub fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// `0x` and lowercase hex, as the proofs print addresses and load commands.
pub fn hex(v: impl std::fmt::LowerHex) -> String {
    format!("0x{v:x}")
}

pub fn hex_bytes(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub cmd: u32,
    pub off: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: u8,
    pub desc: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dylib {
    pub name: String,
    pub weak: bool,
}

/// One thin 32-bit ARM image.
#[derive(Debug, Clone)]
pub struct MachO32 {
    pub b: Vec<u8>,
    #[allow(dead_code)]
    pub filetype: u32,
    pub flags: u32,
    pub commands: Vec<Command>,
}

impl MachO32 {
    /// A thin 0xFEEDFACE image of CPU_TYPE_ARM whose load commands all parse.
    pub fn parse(b: Vec<u8>) -> Option<MachO32> {
        if b.len() < 28 || le32(&b, 0) != 0xFEED_FACE || le32(&b, 4) != 12 {
            return None;
        }
        let mut commands = Vec::new();
        let mut off = 28usize;
        for _ in 0..le32(&b, 16) {
            if off + 8 > b.len() {
                return None;
            }
            commands.push(Command { cmd: le32(&b, off), off });
            let size = le32(&b, off + 4) as usize;
            if size < 8 {
                return None;
            }
            off += size;
        }
        Some(MachO32 { filetype: le32(&b, 12), flags: le32(&b, 24), commands, b })
    }

    /// The slice a CPU of `arch` runs: the exact subtype, else (armv7) an armv6 or ARM_ALL one; with the arch the
    /// slice is built for ("armv7", "armv6", "arm").
    pub fn slice(d: &[u8], arch: &str) -> Option<(MachO32, &'static str)> {
        let want = subtype(arch).unwrap_or(-1);
        let mut slices: Vec<(i32, &[u8])> = Vec::new();
        if d.len() >= 8 && be32(d, 0) == 0xCAFE_BABE {
            for i in 0..be32(d, 4) as usize {
                let o = 8 + 20 * i;
                if o + 20 > d.len() {
                    break; // every later entry is past the end too
                }
                if be32(d, o) != 12 {
                    continue;
                }
                let off = be32(d, o + 8) as usize;
                let size = be32(d, o + 12) as usize;
                if off.checked_add(size).is_none_or(|end| end > d.len()) {
                    continue;
                }
                slices.push((be32(d, o + 4) as i32, &d[off..off + size]));
            }
        } else if d.len() >= 12 && le32(d, 4) == 12 {
            slices.push((le32(d, 8) as i32, d));
        }
        let runnable: &[i32] = if arch == "armv7" { &[want, 6, 0] } else { &[want, 0] };
        for &s in runnable {
            if let Some(hit) = slices.iter().find(|x| x.0 == s) {
                if let Some(m) = MachO32::parse(hit.1.to_vec()) {
                    return Some((m, if s == 9 { "armv7" } else if s == 6 { "armv6" } else { "arm" }));
                }
            }
        }
        None
    }

    /// The load command string at `off` (dylib name, sub-umbrella): its lc_str offset is at `off + 8`.
    pub fn name(&self, off: usize) -> String {
        let start = off + le32(&self.b, off + 8) as usize;
        if start >= self.b.len() {
            return String::new();
        }
        String::from_utf8_lossy(c_bytes(&self.b, start)).into_owned()
    }

    /// Linked images in ordinal order.
    pub fn dylibs(&self) -> Vec<Dylib> {
        self.commands
            .iter()
            .filter(|c| is_link_command(c.cmd))
            .map(|c| Dylib { name: self.name(c.off), weak: c.cmd == LC_LOAD_WEAK_DYLIB })
            .collect()
    }

    /// Images whose symbols this one exports as its own: LC_REEXPORT_DYLIB, and the old umbrella form
    /// (LC_SUB_UMBRELLA / LC_SUB_LIBRARY naming one of its linked images).
    pub fn reexported(&self) -> Vec<String> {
        let mut out: Vec<String> =
            self.commands.iter().filter(|c| c.cmd == LC_REEXPORT_DYLIB).map(|c| self.name(c.off)).collect();
        let subs: std::collections::HashSet<String> = self
            .commands
            .iter()
            .filter(|c| c.cmd == LC_SUB_UMBRELLA || c.cmd == LC_SUB_LIBRARY)
            .map(|c| self.name(c.off))
            .collect();
        if !subs.is_empty() {
            for d in self.dylibs() {
                let leaf = last_component(&d.name);
                let stem: String = leaf.chars().take_while(|&c| c != '.').collect();
                if subs.contains(leaf) || subs.contains(&stem) {
                    out.push(d.name);
                }
            }
        }
        out
    }

    /// The nlist entries of the first LC_SYMTAB, in table order.
    pub fn symbols(&self) -> Vec<Symbol> {
        let Some(st) = self.commands.iter().find(|c| c.cmd == LC_SYMTAB) else { return Vec::new() };
        let b = &self.b;
        let symoff = le32(b, st.off + 8) as usize;
        let nsyms = le32(b, st.off + 12) as usize;
        let stroff = le32(b, st.off + 16) as usize;
        let mut out = Vec::new();
        for i in 0..nsyms {
            let e = symoff + 12 * i;
            if e + 12 > b.len() {
                break;
            }
            let s = stroff + le32(b, e) as usize;
            if s >= b.len() {
                continue;
            }
            out.push(Symbol {
                name: String::from_utf8_lossy(c_bytes(b, s)).into_owned(),
                kind: b[e + 4],
                desc: le16(b, e + 6),
            });
        }
        out
    }

    /// The external symbols this image imports (N_UNDF, or N_PBUD: a prebound 2.x/3.0 image's).
    pub fn imports(&self) -> std::collections::HashSet<String> {
        self.symbols().into_iter().filter(|s| is_import(s.kind)).map(|s| s.name).collect()
    }

    /// The external symbols this image defines.
    pub fn exports(&self) -> std::collections::HashSet<String> {
        self.symbols().into_iter().filter(|s| is_export(s.kind)).map(|s| s.name).collect()
    }
}

/// The ARM subtypes the recipes' archs name.
pub fn subtype(arch: &str) -> Option<i32> {
    match arch {
        "armv6" => Some(6),
        "armv7" => Some(9),
        _ => None,
    }
}

pub fn last_component(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// A defined external symbol: N_EXT and a defined type (N_ABS, N_SECT, N_INDR), not N_UNDF nor N_PBUD (a prebound
/// 1.x/2.x image's imports are N_PBUD, 0xC: counting them made every name a prebound framework imports its export).
pub fn is_export(kind: u8) -> bool {
    kind & 0xE0 == 0 && kind & 0x01 != 0 && kind & 0x0E != 0 && kind & 0x0E != 0x0C
}

/// N_UNDF, or N_PBUD (a prebound import: 2.x and 3.0 executables).
pub fn is_import(kind: u8) -> bool {
    kind & 0xE0 == 0 && kind & 0x01 != 0 && (kind & 0x0E == 0 || kind & 0x0E == 0x0C)
}

/// The names an export trie re-exports one by one (EXPORT_SYMBOL_FLAGS_REEXPORT). iOS 6 moved NSObject to libobjc
/// and left CoreFoundation re-exporting _OBJC_CLASS_$_NSObject this way, with no nlist entry, so images that bind
/// NSObject to CoreFoundation (built against an older SDK) still load.
pub fn trie_reexports(b: &[u8], start: usize, size: usize) -> Vec<String> {
    if start == 0 || size == 0 || start.checked_add(size).is_none_or(|end| end > b.len()) {
        return Vec::new();
    }
    let end = start + size;
    let uleb = |p: &mut usize| -> Option<usize> {
        let mut r = 0usize;
        let mut shift = 0u32;
        while *p < end && shift < 63 {
            let x = b[*p];
            *p += 1;
            r |= ((x & 0x7F) as usize) << shift;
            shift += 7;
            if x < 0x80 {
                return Some(r);
            }
        }
        None
    };
    let mut out = Vec::new();
    let mut stack: Vec<(usize, Vec<u8>)> = vec![(0, Vec::new())];
    let mut seen = std::collections::HashSet::new();
    while let Some((node, prefix)) = stack.pop() {
        if !seen.insert(node) {
            continue; // a malformed trie cannot loop
        }
        let mut p = match start.checked_add(node) {
            Some(p) if p < end => p,
            _ => continue,
        };
        let Some(info) = uleb(&mut p) else { continue };
        if info > 0 {
            let mut q = p;
            if let Some(flags) = uleb(&mut q) {
                if flags & 0x8 != 0 {
                    out.push(String::from_utf8_lossy(&prefix).into_owned());
                }
            }
        }
        p = match p.checked_add(info) {
            Some(p) if p < end => p,
            _ => continue,
        };
        let children = b[p] as usize;
        p += 1;
        for _ in 0..children {
            let Some(nul) = b[p..end].iter().position(|&c| c == 0).map(|i| p + i) else { break };
            let label = &b[p..nul];
            p = nul + 1;
            let Some(child) = uleb(&mut p) else { break };
            let mut name = prefix.clone();
            name.extend_from_slice(label);
            stack.push((child, name));
        }
    }
    out
}

/// The Mach-O header facts MachOSignature checks (Portability/MachOHeaders.swift): a thin or fat file's slices,
/// each one's cpu and load command IDs.
#[derive(Debug, Clone)]
pub struct Headers {
    pub fat: bool,
    pub slices: Vec<HeaderSlice>,
}

#[derive(Debug, Clone)]
pub struct HeaderSlice {
    pub is_64bit: bool,
    pub cputype: i32,
    pub cpusubtype: i32,
    pub commands: Vec<u32>,
}

impl Headers {
    /// A fat file whose every slice parses, or a thin one; None otherwise.
    pub fn parse(data: &[u8]) -> Option<Headers> {
        if data.len() < 4 {
            return None;
        }
        if data[..4] == [0xCA, 0xFE, 0xBA, 0xBE] {
            let be = |o: usize| be32(data, o) as usize;
            let mut slices = Vec::new();
            for i in 0..be(4) {
                let at = 8 + 20 * i;
                let (offset, size) = (be(at + 8), be(at + 12));
                if offset.checked_add(size).is_none_or(|end| end > data.len()) {
                    return None;
                }
                slices.push(Headers::slice(&data[offset..offset + size])?);
            }
            Some(Headers { fat: true, slices })
        } else {
            Some(Headers { fat: false, slices: vec![Headers::slice(data)?] })
        }
    }

    /// A thin little-endian Mach-O: 0xFEEDFACE (32-bit, 28-byte header) or 0xFEEDFACF (64-bit, 32).
    fn slice(b: &[u8]) -> Option<HeaderSlice> {
        if b.len() < 28 {
            return None;
        }
        let magic = le32(b, 0);
        if magic != 0xFEED_FACE && magic != 0xFEED_FACF {
            return None;
        }
        let is_64bit = magic == 0xFEED_FACF;
        let mut s = HeaderSlice { is_64bit, cputype: le32(b, 4) as i32, cpusubtype: le32(b, 8) as i32, commands: Vec::new() };
        let mut at = if is_64bit { 32 } else { 28 };
        for _ in 0..le32(b, 16) {
            if at + 8 > b.len() {
                return None;
            }
            s.commands.push(le32(b, at));
            let size = le32(b, at + 4) as usize;
            if size < 8 {
                return None;
            }
            at += size;
        }
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An export trie's one-by-one re-export (iOS 6 CoreFoundation's _OBJC_CLASS_$_NSObject) counts as an export;
    /// a plain export, which the nlist already lists, does not come from here.
    #[test]
    fn trie_reexports_one_by_one() {
        // (one byte before the trie) root: no info, children "_A" -> 10 and "_B" -> 15;
        // _A: flags 8 (re-export), ordinal 2, same name; _B: flags 0, address 4
        let mut b = vec![0xEE, 0x00, 0x02];
        b.extend_from_slice(b"_A");
        b.extend_from_slice(&[0, 10]);
        b.extend_from_slice(b"_B");
        b.extend_from_slice(&[0, 15]);
        b.extend_from_slice(&[0x03, 0x08, 0x02, 0x00, 0x00]);
        b.extend_from_slice(&[0x02, 0x00, 0x04, 0x00]);
        assert_eq!(trie_reexports(&b, 1, b.len() - 1), vec!["_A".to_string()]);
        assert!(trie_reexports(&b, 1, 3).is_empty()); // truncated: nothing, no crash
    }

    #[test]
    fn symbol_kinds() {
        assert!(is_export(0x0F)); // N_SECT | N_EXT
        assert!(!is_export(0x0D)); // N_PBUD | N_EXT
        assert!(is_import(0x01) && is_import(0x0D));
        assert!(!is_import(0x0F) && !is_import(0x1F)); // private extern
    }

    #[test]
    fn slices_pick_what_the_cpu_runs() {
        // a thin armv6 image with no commands
        let mut thin = vec![0u8; 28];
        thin[..4].copy_from_slice(&0xFEED_FACEu32.to_le_bytes());
        thin[4..8].copy_from_slice(&12u32.to_le_bytes());
        thin[8..12].copy_from_slice(&6u32.to_le_bytes());
        assert_eq!(MachO32::slice(&thin, "armv7").map(|s| s.1), Some("armv6"));
        assert_eq!(MachO32::slice(&thin, "armv6").map(|s| s.1), Some("armv6"));
        thin[8..12].copy_from_slice(&9u32.to_le_bytes());
        assert!(MachO32::slice(&thin, "armv6").is_none());
        let h = Headers::parse(&thin).unwrap();
        assert!(!h.fat && h.slices[0].cputype == 12 && h.slices[0].cpusubtype == 9);
        assert!(Headers::parse(&thin[..27]).is_none());
    }
}

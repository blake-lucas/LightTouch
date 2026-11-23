//! Generic Mach-O / dyld-cache code-signature helpers.
//!
//! Ports the parts of Swift's `MachOSignature` and `A6GuestAssets.verifiedCodeDirectories` that the
//! data path needs, plus `Activation.signed`'s ad-hoc page-hash refresh with an optional policy for
//! the empty CMS wrapper carried by the iOS 10 `xpcd_cache` image.
//!
//! Nothing here creates a new signature allocation or changes admission policy: existing page hashes
//! are refreshed in place, and the caller keeps the existing SuperBlob layout.
use crate::error::Result;
use sha1::Sha1;
use sha2::{Digest, Sha256};

pub const ARM_CPU: i32 = 12;
pub const ARMV7: i32 = 9;
pub const ARMV7S: i32 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    pub base: usize,
    pub size: usize,
    pub cputype: i32,
    pub cpusubtype: i32,
    pub is_64bit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub addr: u32,
    pub size: u32,
    pub offset: u32,
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7]])
}
fn put_be32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Every thin slice of a thin or fat Mach-O, with its file extent. `None` for anything malformed.
pub fn images(data: &[u8]) -> Option<Vec<Image>> {
    if data.len() >= 8 && data[0..4] == [0xca, 0xfe, 0xba, 0xbe] {
        let count = be32(data, 4) as usize;
        if count == 0 || count >= 32 || 8 + 20 * count > data.len() {
            return None;
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let at = 8 + 20 * i;
            let base = be32(data, at + 8) as usize;
            let size = be32(data, at + 12) as usize;
            if size == 0 || base.checked_add(size).is_none_or(|e| e > data.len()) {
                return None;
            }
            out.push(thin(&data[base..base + size], base)?);
        }
        return Some(out);
    }
    Some(vec![thin(data, 0)?])
}

fn thin(b: &[u8], base: usize) -> Option<Image> {
    if b.len() < 28 {
        return None;
    }
    let magic = le32(b, 0);
    if magic != 0xfeed_face && magic != 0xfeed_facf {
        return None;
    }
    let is_64 = magic == 0xfeed_facf;
    let mut at = if is_64 { 32 } else { 28 };
    let ncmds = le32(b, 16) as usize;
    for _ in 0..ncmds {
        if at + 8 > b.len() {
            return None;
        }
        let size = le32(b, at + 4) as usize;
        if size < 8 || at + size > b.len() {
            return None;
        }
        at += size;
    }
    Some(Image { base, size: b.len(), cputype: le32(b, 4) as i32, cpusubtype: le32(b, 8) as i32, is_64bit: is_64 })
}

/// The native ARM slice Swift's A6 checks select: exact armv7s, else armv7, else ARM_ALL.
pub fn native_arm_slice(images: &[Image]) -> Option<&Image> {
    [ARMV7S, ARMV7, 0].into_iter().find_map(|want| {
        images.iter().find(|i| {
            i.cputype == ARM_CPU && (i.cpusubtype & 0x00ff_ffff) == want && !i.is_64bit
        })
    })
}

/// A thin Mach-O's `__TEXT` segments' sections, as their file offsets (for `__xpcd_cache`).
pub fn sections(b: &[u8]) -> Vec<Section> {
    let mut out = Vec::new();
    if b.len() < 28 || le32(b, 0) != 0xfeed_face {
        return out;
    }
    let ncmds = le32(b, 16) as usize;
    let mut at = 28usize;
    for _ in 0..ncmds {
        if at + 8 > b.len() {
            break;
        }
        let cmd = le32(b, at);
        let size = le32(b, at + 4) as usize;
        if size < 8 || at + size > b.len() {
            break;
        }
        if cmd == 1 && size >= 56 {
            let nsects = le32(b, at + 48) as usize;
            if nsects <= (size - 56) / 68 {
                for i in 0..nsects {
                    let s = at + 56 + i * 68;
                    out.push(Section {
                        name: cstr(&b[s..s + 16]),
                        addr: le32(b, s + 32),
                        size: le32(b, s + 36),
                        offset: le32(b, s + 40),
                    });
                }
            }
        }
        at += size;
    }
    out
}

/// LC_CODE_SIGNATURE's (dataoff, datasize) within a thin Mach-O, relative to the slice.
pub fn code_signature(b: &[u8]) -> Option<(usize, usize)> {
    if b.len() < 28 || le32(b, 0) != 0xfeed_face {
        return None;
    }
    let ncmds = le32(b, 16) as usize;
    let mut at = 28usize;
    for _ in 0..ncmds {
        if at + 8 > b.len() {
            return None;
        }
        let cmd = le32(b, at);
        let size = le32(b, at + 4) as usize;
        if cmd == 0x1d && at + 16 <= b.len() {
            return Some((le32(b, at + 8) as usize, le32(b, at + 12) as usize));
        }
        if size < 8 {
            return None;
        }
        at += size;
    }
    None
}

fn hash(kind: u8, bytes: &[u8]) -> Vec<u8> {
    if kind == 1 {
        Sha1::digest(bytes).to_vec()
    } else {
        Sha256::digest(bytes).to_vec()
    }
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

/// Parse and fully verify one SuperBlob's CodeDirectories. Returns the 20-byte identities (the
/// first 20 bytes of the SHA-1/SHA-256 of each CodeDirectory) in blob order.
fn verified_superblob(data: &[u8], signature: usize, allocation: usize, base: usize, size: usize) -> Result<Vec<[u8; 20]>> {
    let invalid = |what: &str| crate::unsupported!("code signature: {what}");
    if allocation < 12 || signature < base || signature > base + size - allocation {
        return Err(invalid("invalid signature extent"));
    }
    if be32(data, signature) != 0xfade_0cc0 {
        return Err(invalid("missing SuperBlob"));
    }
    let length = be32(data, signature + 4) as usize;
    let count = be32(data, signature + 8) as usize;
    if length < 12 || length > allocation || count > (length - 12) / 8 {
        return Err(invalid("invalid signature index"));
    }
    let mut dirs: Vec<Directory> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for i in 0..count {
        let relative = be32(data, signature + 16 + i * 8) as usize;
        if relative < 12 + count * 8 || relative > length - 8 {
            return Err(invalid("invalid blob offset"));
        }
        let cd = signature + relative;
        let cd_len = be32(data, cd + 4) as usize;
        if cd_len < 8 || cd_len > length - relative {
            return Err(invalid("truncated blob"));
        }
        spans.push((relative, relative + cd_len));
        if be32(data, cd) != 0xfade_0c02 {
            continue;
        }
        if cd_len < 44 {
            return Err(invalid("truncated CodeDirectory"));
        }
        let version = be32(data, cd + 8);
        if !(0x20000..0x30000).contains(&version) {
            return Err(invalid("unsupported CodeDirectory version"));
        }
        let minimum = if version >= 0x20100 { 48 } else { 44 };
        if cd_len < minimum {
            return Err(invalid("truncated CodeDirectory header"));
        }
        let hashes = be32(data, cd + 16) as usize;
        let special = be32(data, cd + 24) as usize;
        let pages = be32(data, cd + 28) as usize;
        let limit = be32(data, cd + 32) as usize;
        let width = data[cd + 36] as usize;
        let kind = data[cd + 37];
        let exponent = data[cd + 39] as usize;
        if !((kind == 1 && width == 20) || (kind == 2 && width == 32))
            || !(1..=20).contains(&exponent)
            || limit == 0
            || limit > signature - base
            || hashes < minimum + special * width
            || hashes > cd_len
            || pages > (cd_len - hashes) / width
        {
            return Err(invalid("unsupported code-page layout"));
        }
        let page_size = 1usize << exponent;
        if pages != (limit + page_size - 1) / page_size {
            return Err(invalid("incomplete signed pages"));
        }
        if version >= 0x20100 && cd_len >= 48 {
            let scatter = be32(data, cd + 44) as usize;
            if scatter != 0 {
                // A cache's scatter vectors target virtual mappings; page hashes still cover the
                // signed file range in slot order. Validate the vector and every page below.
                if scatter < minimum {
                    return Err(invalid("unsupported scatter signature"));
                }
                let mut entry = scatter;
                let mut previous_end = 0usize;
                loop {
                    if entry > cd_len - 24 {
                        return Err(invalid("unterminated scatter vector"));
                    }
                    let amount = be32(data, cd + entry) as usize;
                    let first = be32(data, cd + entry + 4) as usize;
                    let target = u64::from(be32(data, cd + entry + 8)) << 32 | u64::from(be32(data, cd + entry + 12));
                    let spare = u64::from(be32(data, cd + entry + 16)) << 32 | u64::from(be32(data, cd + entry + 20));
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
        dirs.push(Directory { start: cd, hashes, pages, limit, width, kind, page_size });
    }
    spans.sort_unstable();
    for pair in spans.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(invalid("overlapping signature blobs"));
        }
    }
    if dirs.is_empty() {
        return Err(invalid("no verified CodeDirectory"));
    }
    let mut out = Vec::with_capacity(dirs.len());
    for d in dirs {
        for page in 0..d.pages {
            let lo = base + page * d.page_size;
            let hi = base + d.limit.min((page + 1) * d.page_size);
            let expected = &data[d.start + d.hashes + page * d.width..d.start + d.hashes + (page + 1) * d.width];
            if hash(d.kind, &data[lo..hi]).as_slice() != expected {
                return Err(invalid("signed page does not match its CodeDirectory"));
            }
        }
        let digest = hash(d.kind, &data[d.start..d.start + be32(data, d.start + 4) as usize]);
        let mut id = [0u8; 20];
        id.copy_from_slice(&digest[..20]);
        out.push(id);
    }
    Ok(out)
}

/// `A6GuestAssets.verifiedCodeDirectories` for a dyld cache or a Mach-O.
pub fn verified_code_directories(data: &[u8]) -> Result<Vec<[u8; 20]>> {
    if data.len() >= 112 && data.starts_with(b"dyld_v1") {
        if !data[..16.min(data.len())].windows(6).any(|w| w == b"armv7s") {
            return Err(crate::unsupported!("A6 guest assets: not an armv7s cache"));
        }
        let signature = le64(data, 40) as usize;
        let mut allocation = le64(data, 48) as usize;
        if allocation == 0 {
            if signature < 56 || signature > data.len() - 12 {
                return Err(crate::unsupported!("code signature: invalid legacy cache signature extent"));
            }
            allocation = data.len() - signature;
        }
        return verified_superblob(data, signature, allocation, 0, data.len());
    }
    let images = images(data).ok_or_else(|| crate::unsupported!("A6 guest assets: not a signed Mach-O"))?;
    // A fat file's armv7s or armv7 slice; a thin file is taken whatever its subtype (the Swift's rule): the guest
    // helpers it_agent and libappsync's armv6 slice are built for every board.
    let fat = data.len() >= 4 && data[0..4] == [0xca, 0xfe, 0xba, 0xbe];
    let image = if fat {
        native_arm_slice(&images)
    } else {
        images.first().filter(|i| i.cputype == ARM_CPU && !i.is_64bit)
    }
    .ok_or_else(|| crate::unsupported!("A6 guest assets: no native ARM slice"))?;
    let slice = &data[image.base..image.base + image.size];
    let (off, size) = code_signature(slice).ok_or_else(|| crate::unsupported!("A6 guest assets: missing code signature"))?;
    verified_superblob(data, image.base + off, size, image.base, image.size)
}

/// `Activation.signed` with the empty-CMS policy. `preserve_empty_cms`: keep a zero-length
/// `0xfade0b01` wrapper (the iOS 10 xpcd cache's representation); a nonempty CMS is never kept.
pub fn signed(data: &[u8], preserve_empty_cms: bool) -> Result<Vec<u8>> {
    if data.len() >= 8 && data[0..4] == [0xca, 0xfe, 0xba, 0xbe] {
        let images = images(data).ok_or_else(|| crate::unsupported!("code signature: malformed fat Mach-O"))?;
        let mut out = data.to_vec();
        for i in images.iter().filter(|i| i.cputype == ARM_CPU) {
            let slice = signed(&data[i.base..i.base + i.size], preserve_empty_cms)?;
            out[i.base..i.base + i.size].copy_from_slice(&slice);
        }
        return Ok(out);
    }
    let invalid = || crate::unsupported!("Unsupported code signature");
    let (start, size) = code_signature(data).ok_or_else(invalid)?;
    if size < 12 || start > data.len().wrapping_sub(size) || be32(data, start) != 0xfade_0cc0 {
        return Err(invalid());
    }
    let mut b = data.to_vec();
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
        if typ == 0x10000 {
            if preserve_empty_cms && !(blob_length == 8 && be32(&b, blob) == 0xfade_0b01) {
                return Err(invalid());
            }
            if !preserve_empty_cms {
                continue;
            }
        }
        if typ == 0 || (0x1000..=0x1005).contains(&typ) {
            if blob_length < 44 || be32(&b, blob) != 0xfade_0c02 {
                return Err(invalid());
            }
            let hash_offset = be32(&b, blob + 16) as usize;
            let special = be32(&b, blob + 24) as usize;
            let slots = be32(&b, blob + 28) as usize;
            let limit = be32(&b, blob + 32) as usize;
            let width = b[blob + 36] as usize;
            let kind = b[blob + 37];
            let exponent = b[blob + 39] as usize;
            if !((kind == 1 && width == 20) || (kind == 2 && width == 32))
                || !(1..=20).contains(&exponent)
                || limit == 0
                || limit > start
                || hash_offset < 44 + special * width
                || hash_offset > blob_length
                || slots > (blob_length - hash_offset) / width
            {
                return Err(invalid());
            }
            let page = 1usize << exponent;
            if slots != (limit + page - 1) / page {
                return Err(invalid());
            }
            if be32(&b, blob + 8) >= 0x20100 && (blob_length < 48 || be32(&b, blob + 44) != 0) {
                return Err(invalid());
            }
            let flags = be32(&b, blob + 12);
            put_be32(&mut b, blob + 12, flags | 2); // CS_ADHOC
            for slot in 0..slots {
                let lo = slot * page;
                let hi = limit.min((slot + 1) * page);
                let digest = hash(kind, &b[lo..hi]);
                let at = blob + hash_offset + slot * width;
                b[at..at + width].copy_from_slice(&digest[..width]);
            }
            directories += 1;
        }
        entries.push((typ, offset));
    }
    if directories == 0 || !entries.iter().any(|&(t, _)| t == 0) {
        return Err(invalid());
    }
    for i in 0..count {
        put_be32(&mut b, start + 12 + i * 8, entries.get(i).map_or(0, |e| e.0));
        put_be32(&mut b, start + 16 + i * 8, entries.get(i).map_or(0, |e| e.1 as u32));
    }
    put_be32(&mut b, start + 8, entries.len() as u32);
    Ok(b)
}

/// `Activation.signed` without an empty-CMS policy change.
pub fn sign(data: &[u8]) -> Result<Vec<u8>> {
    signed(data, false)
}

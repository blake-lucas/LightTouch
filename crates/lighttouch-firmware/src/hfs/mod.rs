//! HFS+/HFSX volume images without a mount, on every host.
//!
//!   volume   read a volume (catalog, contents incl. HFS compression, listings) and patch catalog records in place
//!            (owners, modes, folder counts, dates, the journal)
//!   editor   file-level edits (read, write, mkdir, symlink, remove, rename, chmod) through an in-memory tree,
//!            committed back into the image: no host folder, so names, modes and owners survive on any host
//!   casefold the catalog order of a case-insensitive volume
//!
//! Paths are volume-relative, "/"-separated, no leading slash ("" is the root folder). A catalog name may hold
//! "/", which paths show as ":" (as macOS presents it).
pub mod casefold;
pub mod editor;
pub mod volume;

pub use editor::Editor;
pub use volume::Volume;

pub(crate) const ROOT_ID: u32 = 2;
pub(crate) const EXTENTS_ID: u32 = 3;
pub(crate) const CATALOG_ID: u32 = 4;
pub(crate) const ALLOCATION_ID: u32 = 6;
pub(crate) const ATTRIBUTES_ID: u32 = 8;
/// Seconds from 1904-01-01 (HFS dates) to 1970-01-01.
pub const HFS_EPOCH: i64 = 2_082_844_800;
/// UF_COMPRESSED: the content is in com.apple.decmpfs (and the resource fork).
pub(crate) const COMPRESSED: u8 = 0x20;

pub(crate) fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
pub(crate) fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub(crate) fn be64(b: &[u8], o: usize) -> u64 {
    (be32(b, o) as u64) << 32 | be32(b, o + 4) as u64
}
pub(crate) fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub(crate) fn le64(b: &[u8], o: usize) -> u64 {
    le32(b, o) as u64 | (le32(b, o + 4) as u64) << 32
}
pub(crate) fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_be_bytes());
}
pub(crate) fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}
pub(crate) fn put64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_be_bytes());
}

/// An HFS date for a Unix time.
pub fn hfs_date(unix: i64) -> u32 {
    (unix + HFS_EPOCH).clamp(0, u32::MAX as i64) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub start: u32,
    pub count: u32,
}

/// A fork: its logical size, clump size and every extent (inline, then overflow), in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fork {
    pub logical_size: u64,
    pub clump_size: u32,
    pub extents: Vec<Extent>,
    /// The record's totalBlocks, which the extents reach once the overflow ones are added.
    pub recorded_blocks: u32,
}

impl Fork {
    /// HFSPlusForkData at `o`: the first eight extents inline.
    pub(crate) fn parse(b: &[u8], o: usize) -> Fork {
        Fork {
            logical_size: be64(b, o),
            clump_size: be32(b, o + 8),
            recorded_blocks: be32(b, o + 12),
            extents: (0..8)
                .map(|i| Extent { start: be32(b, o + 16 + 8 * i), count: be32(b, o + 20 + 8 * i) })
                .filter(|e| e.count > 0)
                .collect(),
        }
    }

    pub fn total_blocks(&self) -> u32 {
        self.extents.iter().map(|e| e.count).sum()
    }

    /// HFSPlusForkData (80 bytes).
    pub(crate) fn record(&self) -> [u8; 80] {
        let mut r = [0u8; 80];
        put64(&mut r, 0, self.logical_size);
        put32(&mut r, 8, self.clump_size);
        put32(&mut r, 12, self.total_blocks());
        for (i, e) in self.extents.iter().take(8).enumerate() {
            put32(&mut r, 16 + 8 * i, e.start);
            put32(&mut r, 20 + 8 * i, e.count);
        }
        r
    }
}

/// A catalog name as a path shows it, and back: HFS+ names may hold "/", which paths show as ":".
pub fn posix_name(name: &str) -> String {
    name.replace('/', ":")
}
pub fn catalog_name(name: &str) -> String {
    name.replace(':', "/")
}

/// The catalog key: keyLength, parent ID, the name (length-prefixed UTF-16).
pub(crate) fn catalog_key(parent: u32, name: &[u16]) -> Vec<u8> {
    let mut key = vec![0u8; 8 + 2 * name.len()];
    put16(&mut key, 0, (6 + 2 * name.len()) as u16);
    put32(&mut key, 2, parent);
    put16(&mut key, 6, name.len() as u16);
    for (i, &u) in name.iter().enumerate() {
        put16(&mut key, 8 + 2 * i, u);
    }
    key
}

//! File-level edits of an HFS+/HFSX volume image without a mount or a host folder: the catalog is loaded into an
//! in-memory tree, edits change the tree, and `commit` writes back what changed. Names, modes, owners and symlinks
//! never touch the host filesystem, so the same edits work on Linux, macOS, Windows and Android.
//!
//! ```ignore
//! let mut ed = Editor::open(image)?;
//! let fstab = ed.read("etc/fstab")?;                       // through symlinks (etc -> private/etc)
//! ed.write("etc/fstab", new_fstab, None)?;                 // in place: the record, owner and mode stay
//! ed.mkdirs("usr/local/bin")?;
//! ed.write("usr/local/bin/tool", bytes, Some(0o755))?;     // new: 501:20, like a noowners mount
//! ed.symlink("usr/bin/tool", "/usr/local/bin/tool")?;
//! ed.commit()?;
//! Editor::grow(image, bytes)?;
//! Editor::format(image, size, "Data", true, now)?;
//! ```
//!
//! What commit keeps, as an edit through macOS's driver would: an untouched file or folder its catalog record (ID,
//! owner, flags, dates), blocks and extended attributes (HFS compression included), wherever a rename moved it; a
//! rewritten file its record, owner and attributes but not its compression (stored plain, as the driver stores a
//! compressed file once written to); a new item the next catalog ID and the owner 501:20, as a noowners mount
//! records it (callers set owners with Volume::set_owner afterwards). The catalog, attributes and extents overflow
//! B-trees are written afresh from their records into their files' blocks (grown when they no longer fit); blocks
//! that come free are zeroed. The hidden items at the root (the journal files, the hard-link folders) are kept as
//! they are and can't be reached by path. Hard links can be read but not written, moved or removed.
use super::volume::{CatalogRecord, Volume};
use super::{
    ALLOCATION_ID, ATTRIBUTES_ID, CATALOG_ID, COMPRESSED, EXTENTS_ID, Extent, Fork, ROOT_ID, be16, be32, casefold,
    catalog_key, catalog_name, hfs_date, posix_name, put16, put32, put64,
};
use crate::error::Result;
use crate::posio;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};

const S_IFMT: u16 = 0o170000;
const S_IFDIR: u16 = 0o040000;
const S_IFREG: u16 = 0o100000;
const S_IFLNK: u16 = 0o120000;

/// Root items macOS's driver hides, kept as they are.
const HIDDEN: [&str; 5] =
    [".journal", ".journal_info_block", "\0\0\0\0HFS+ Private Data", ".HFS+ Private Directory Data\r", ".hotfiles.btree"];
/// A noowners mount's owner for what an edit adds (the Mac user's, as macOS records it).
pub const NEW_OWNER: (u32, u32) = (501, 20);

/// A folder or file record of the catalog: the record body as stored (88 or 248 bytes), forks kept apart.
#[derive(Debug, Clone)]
struct Item {
    parent: u32,
    name: Vec<u16>,
    body: Vec<u8>,
    data: Fork,
    resource: Fork,
}

impl Item {
    fn cnid(&self) -> u32 {
        be32(&self.body, 8)
    }
    fn is_folder(&self) -> bool {
        be16(&self.body, 0) == 1
    }
    fn mode(&self) -> u16 {
        be16(&self.body, 42)
    }
    fn is_symlink(&self) -> bool {
        !self.is_folder() && self.mode() & S_IFMT == S_IFLNK
    }
    fn is_hard_link(&self) -> bool {
        !self.is_folder() && be32(&self.body, 48) == 0x686C_6E6B && be32(&self.body, 52) == 0x6866_732B
    }
    fn compressed(&self) -> bool {
        self.body[41] & COMPRESSED != 0
    }
}

/// An attributes B-tree record: its key fields and its body (inline data, fork data or overflow extents).
#[derive(Debug, Clone)]
struct Attribute {
    file_id: u32,
    name: Vec<u16>,
    start_block: u32,
    body: Vec<u8>,
}

impl Attribute {
    /// The blocks a fork-data or extents record holds.
    fn extents(&self) -> Vec<Extent> {
        match be32(&self.body, 0) {
            0x20 => Fork::parse(&self.body, 8).extents,
            0x30 => (0..8)
                .map(|i| Extent { start: be32(&self.body, 8 + 8 * i), count: be32(&self.body, 12 + 8 * i) })
                .filter(|e| e.count > 0)
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// What a B-tree's header record says of its shape, kept when the tree is written again.
#[derive(Debug, Clone)]
struct TreeShape {
    node_size: usize,
    /// The 106-byte header record.
    header: Vec<u8>,
}

impl TreeShape {
    fn variable_index_keys(&self) -> bool {
        be32(&self.header, 38) & 0x4 != 0
    }
    fn max_key_length(&self) -> usize {
        be16(&self.header, 20) as usize
    }
}

/// New content for a file or symlink, written into fresh blocks at commit.
#[derive(Debug, Clone)]
enum Content {
    Memory(Vec<u8>),
    /// A host file streamed in at commit (large payloads stay out of memory).
    Host(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    File,
    Symlink,
}

/// What `stat`/`lstat` say of an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    /// The permission bits (0o7777).
    pub perm: u16,
    pub size: u64,
    pub uid: u32,
    pub gid: u32,
    pub cnid: u32,
}

pub struct Editor {
    path: PathBuf,
    file: File,
    /// The 512-byte volume header.
    header: Vec<u8>,
    block_size: u64,
    total_blocks: u64,
    bitmap: Vec<u8>,
    allocation: Fork,
    extents_file: Fork,
    catalog_file: Fork,
    attributes_file: Fork,
    items: HashMap<u32, Item>,
    attributes: Vec<Attribute>,
    catalog_shape: TreeShape,
    attributes_shape: Option<TreeShape>,
    extents_shape: TreeShape,
    case_sensitive: bool,
    /// The records as opened (their contents are read through `reader`).
    original: HashMap<u32, CatalogRecord>,
    reader: Option<Volume>,
    /// (parent, name) -> cnid of every item.
    children: HashMap<u32, BTreeMap<Vec<u16>, u32>>,
    written: HashMap<u32, Content>,
    /// The blocks of removed items, freed at commit.
    removed: Vec<Extent>,
    /// A snapshot (or a read-only opening): reads only, no commit.
    read_only: bool,
    /// enable_journal's info block and journal (and its size in bytes), written at commit.
    new_journal: Option<(Extent, Extent, u64)>,
    next_id: u32,
    /// The time edits are dated with (Unix seconds); now by default.
    pub now: i64,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A path component as catalog UTF-16: ":" back to "/", canonically decomposed as HFS+ stores names.
fn component(name: &str) -> Vec<u16> {
    use unicode_normalization::UnicodeNormalization;
    catalog_name(name).nfd().collect::<String>().encode_utf16().collect()
}

impl Editor {
    pub fn open(path: &Path) -> Result<Editor> {
        Editor::open_with(path, true)
    }

    fn open_with(path: &Path, writable: bool) -> Result<Editor> {
        let file = File::options()
            .read(true)
            .write(writable)
            .open(path)
            .map_err(|e| crate::internal!("open {}: {e}", path.display()))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut vh = vec![0u8; 512];
        if posio::read_exact_at(&file, &mut vh, 1024).is_err() || vh[0] != 0x48 || !(vh[1] == 0x2B || vh[1] == 0x58) {
            return Err(crate::unsupported!("{name}: no HFS+ volume header"));
        }
        let shape = TreeShape { node_size: 512, header: Vec::new() };
        let mut ed = Editor {
            block_size: be32(&vh, 40) as u64,
            total_blocks: be32(&vh, 44) as u64,
            allocation: Fork::parse(&vh, 112),
            extents_file: Fork::parse(&vh, 192),
            catalog_file: Fork::parse(&vh, 272),
            attributes_file: Fork::parse(&vh, 352),
            case_sensitive: vh[1] == 0x58,
            next_id: be32(&vh, 64),
            header: vh,
            path: path.to_path_buf(),
            file,
            bitmap: Vec::new(),
            items: HashMap::new(),
            attributes: Vec::new(),
            catalog_shape: shape.clone(),
            attributes_shape: None,
            extents_shape: shape,
            original: HashMap::new(),
            reader: None,
            children: HashMap::new(),
            written: HashMap::new(),
            removed: Vec::new(),
            read_only: false,
            new_journal: None,
            now: unix_now(),
        };
        ed.load()?;
        Ok(ed)
    }

    /// A read-only view of the volume as edited so far (later edits to `self` don't show in it), for readers that
    /// outlive a borrow, as FitCheck's firmware does. Drop it before `commit`.
    pub fn snapshot(&self) -> Result<Editor> {
        Ok(Editor {
            path: self.path.clone(),
            file: self.file.try_clone()?,
            header: self.header.clone(),
            block_size: self.block_size,
            total_blocks: self.total_blocks,
            bitmap: Vec::new(),
            allocation: self.allocation.clone(),
            extents_file: self.extents_file.clone(),
            catalog_file: self.catalog_file.clone(),
            attributes_file: self.attributes_file.clone(),
            items: self.items.clone(),
            attributes: Vec::new(),
            catalog_shape: self.catalog_shape.clone(),
            attributes_shape: self.attributes_shape.clone(),
            extents_shape: self.extents_shape.clone(),
            case_sensitive: self.case_sensitive,
            original: self.original.clone(),
            reader: Some(Volume::open(&self.path, false)?),
            children: self.children.clone(),
            written: self.written.clone(),
            removed: Vec::new(),
            read_only: true,
            new_journal: None,
            next_id: self.next_id,
            now: self.now,
        })
    }

    /// The volume in `path` for reading only.
    pub fn open_read_only(path: &Path) -> Result<Editor> {
        let mut ed = Editor::open_with(path, false)?;
        ed.read_only = true;
        Ok(ed)
    }

    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    fn load(&mut self) -> Result<()> {
        // Overflow extents first: the other files' forks may continue there.
        let overflow = self.read_overflow()?;
        let complete = |f: &mut Fork, id: u32, kind: u8| {
            if f.total_blocks() < f.recorded_blocks {
                let mut more = overflow.get(&((id as u64) << 8 | kind as u64)).cloned().unwrap_or_default();
                more.sort_by_key(|(start, _)| *start);
                f.extents.extend(more.into_iter().flat_map(|(_, e)| e));
            }
        };
        complete(&mut self.allocation, ALLOCATION_ID, 0);
        complete(&mut self.catalog_file, CATALOG_ID, 0);
        complete(&mut self.attributes_file, ATTRIBUTES_ID, 0);
        self.bitmap = self.read_fork(&self.allocation.clone(), self.total_blocks.div_ceil(8) as usize)?;
        let (cat_shape, cat_records) = self.leaf_records(&self.catalog_file.clone())?;
        if self.header[1] == 0x58 && cat_shape.header.len() > 37 {
            self.case_sensitive = cat_shape.header[37] == 0xBC;
        }
        self.catalog_shape = cat_shape;
        if !self.case_sensitive {
            // The folded order is rebuilt on commit: the volume's own must be that order, or the table is not Apple's.
            let mut last: Option<(u32, Vec<u16>)> = None;
            for r in &cat_records {
                let key = (be32(r, 2), (0..be16(r, 6) as usize).map(|k| be16(r, 8 + 2 * k)).collect::<Vec<u16>>());
                if let Some((p, n)) = &last {
                    if *p > key.0 || (*p == key.0 && casefold::compare(n, &key.1) != Ordering::Less) {
                        return Err(crate::unsupported!("{}: catalog not in HFS+'s case-folded order", self.name()));
                    }
                }
                last = Some(key);
            }
        }
        for r in &cat_records {
            let key_length = be16(r, 0) as usize;
            let mut at = 2 + key_length;
            if at % 2 == 1 {
                at += 1;
            }
            let kind = be16(r, at);
            if !(kind == 1 || kind == 2) {
                continue;
            }
            let name_length = be16(r, 6) as usize;
            let mut body = r[at..r.len().min(at + if kind == 1 { 88 } else { 248 })].to_vec();
            body.resize(if kind == 1 { 88 } else { 248 }, 0);
            let mut item = Item {
                parent: be32(r, 2),
                name: (0..name_length).map(|k| be16(r, 8 + 2 * k)).collect(),
                data: Fork::default(),
                resource: Fork::default(),
                body,
            };
            if kind == 2 {
                item.data = Fork::parse(&item.body, 88);
                item.resource = Fork::parse(&item.body, 168);
                let cnid = item.cnid();
                complete(&mut item.data, cnid, 0);
                complete(&mut item.resource, cnid, 0xFF);
            }
            self.items.insert(item.cnid(), item);
        }
        self.extents_shape = self.leaf_records(&self.extents_file.clone())?.0;
        if self.attributes_file.total_blocks() > 0 {
            let (shape, records) = self.leaf_records(&self.attributes_file.clone())?;
            self.attributes_shape = Some(shape);
            for r in records {
                let key_length = be16(&r, 0) as usize;
                let name_length = be16(&r, 12) as usize;
                self.attributes.push(Attribute {
                    file_id: be32(&r, 4),
                    name: (0..name_length).map(|k| be16(&r, 14 + 2 * k)).collect(),
                    start_block: be32(&r, 8),
                    body: r[(2 + key_length).min(r.len())..].to_vec(),
                });
            }
        }
        for (cnid, item) in &self.items {
            if *cnid != ROOT_ID {
                self.children.entry(item.parent).or_default().insert(item.name.clone(), *cnid);
            }
        }
        let reader = Volume::open(&self.path, false)?;
        self.original = reader.catalog()?.iter().map(|r| (r.cnid, r.clone())).collect();
        self.reader = Some(reader);
        Ok(())
    }

    // ---- Reading the image

    fn read_fork(&self, fork: &Fork, count: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; count];
        let mut done = 0;
        for e in &fork.extents {
            if done >= count {
                break;
            }
            let n = (count - done).min((e.count as u64 * self.block_size) as usize);
            posio::read_exact_at(&self.file, &mut out[done..done + n], e.start as u64 * self.block_size)
                .map_err(|_| crate::unsupported!("{}: short read", self.name()))?;
            done += n;
        }
        Ok(out)
    }

    /// The overflow extents: (file ID << 8 | fork type) -> [(start block, extents)].
    fn read_overflow(&self) -> Result<HashMap<u64, Vec<(u32, Vec<Extent>)>>> {
        let mut out: HashMap<u64, Vec<(u32, Vec<Extent>)>> = HashMap::new();
        if self.extents_file.total_blocks() == 0 {
            return Ok(out);
        }
        for r in self.leaf_records(&self.extents_file)?.1 {
            if r.len() < 76 {
                continue;
            }
            let key = (be32(&r, 4) as u64) << 8 | r[2] as u64;
            let extents = (0..8)
                .map(|k| Extent { start: be32(&r, 12 + 8 * k), count: be32(&r, 16 + 8 * k) })
                .filter(|e| e.count > 0)
                .collect();
            out.entry(key).or_default().push((be32(&r, 8), extents));
        }
        Ok(out)
    }

    /// A B-tree's header record and every leaf record (key and data), in order.
    fn leaf_records(&self, fork: &Fork) -> Result<(TreeShape, Vec<Vec<u8>>)> {
        let file = self.read_fork(fork, (fork.total_blocks() as u64 * self.block_size) as usize)?;
        if file.len() < 512 {
            return Err(crate::unsupported!("{}: no B-tree", self.name()));
        }
        let node_size = be16(&file, 14 + 18) as usize;
        let shape = TreeShape { node_size, header: file[14..120].to_vec() };
        let mut records = Vec::new();
        let mut node = be32(&file, 14 + 10) as usize;
        let mut seen = HashSet::new();
        while node != 0 {
            if !seen.insert(node) || (node + 1) * node_size > file.len() {
                return Err(crate::unsupported!("{}: damaged B-tree at node {node}", self.name()));
            }
            let base = node * node_size;
            let count = be16(&file, base + 10) as usize;
            let offsets: Vec<usize> = (0..=count).map(|k| be16(&file, base + node_size - 2 * (k + 1)) as usize).collect();
            for i in 0..count {
                records.push(file[base + offsets[i]..base + offsets[i + 1]].to_vec());
            }
            node = be32(&file, base) as usize;
        }
        Ok((shape, records))
    }

    // ---- Paths

    fn child(&self, parent: u32, name: &[u16]) -> Option<u32> {
        let children = self.children.get(&parent)?;
        // a case-insensitive volume finds a name in any case, as its driver does
        let id = match children.get(name) {
            Some(&id) => id,
            None if !self.case_sensitive => {
                *children.iter().find(|(n, _)| casefold::compare(n, name) == Ordering::Equal)?.1
            }
            None => return None,
        };
        // the hidden root items can't be reached by path
        if parent == ROOT_ID && HIDDEN.contains(&String::from_utf16_lossy(name).as_str()) {
            return None;
        }
        Some(id)
    }

    fn link_target(&self, cnid: u32) -> Result<String> {
        let bytes = self.contents(cnid)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The item at `path`; `follow` resolves a symlink in the last component too (intermediate ones always are).
    /// Absolute symlink targets resolve from the volume root.
    fn lookup(&self, path: &str, follow: bool) -> Result<Option<u32>> {
        let mut hops = 0;
        let mut stack: Vec<u32> = Vec::new(); // the folders down to the current one, for ".."
        let mut pending: Vec<String> = path.split('/').filter(|p| !p.is_empty()).map(str::to_string).rev().collect();
        let mut current = ROOT_ID;
        while let Some(part) = pending.pop() {
            match part.as_str() {
                "." => continue,
                ".." => {
                    current = stack.pop().unwrap_or(ROOT_ID);
                    continue;
                }
                _ => {}
            }
            let Some(id) = self.child(current, &component(&part)) else { return Ok(None) };
            let item = &self.items[&id];
            let last = pending.is_empty();
            if item.is_symlink() && (!last || follow) {
                hops += 1;
                if hops > 40 {
                    return Err(crate::unsupported!("{}: too many symlinks resolving {path}", self.name()));
                }
                let target = self.link_target(id)?;
                if target.starts_with('/') {
                    stack.clear();
                    current = ROOT_ID;
                }
                pending.extend(target.split('/').filter(|p| !p.is_empty()).map(str::to_string).rev());
                continue;
            }
            if last {
                return Ok(Some(id));
            }
            if !item.is_folder() {
                return Ok(None);
            }
            stack.push(current);
            current = id;
        }
        Ok(Some(current))
    }

    fn require(&self, path: &str, follow: bool) -> Result<u32> {
        self.lookup(path, follow)?.ok_or_else(|| crate::internal!("{}: no such file or folder: {path}", self.name()))
    }

    /// (the folder holding `path`'s last component, that component).
    fn parent_of(&self, path: &str) -> Result<(u32, String)> {
        let trimmed = path.trim_end_matches('/');
        let (dir, name) = match trimmed.rfind('/') {
            Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
            None => ("", trimmed),
        };
        if name.is_empty() || name == "." || name == ".." {
            return Err(crate::internal!("{}: not a name: {path}", self.name()));
        }
        let parent = self.require(dir, true)?;
        if !self.items[&parent].is_folder() {
            return Err(crate::internal!("{}: not a folder: {dir}", self.name()));
        }
        Ok((parent, name.to_string()))
    }

    fn meta(&self, id: u32) -> Result<Meta> {
        let item = &self.items[&id];
        let kind = if item.is_folder() {
            Kind::Folder
        } else if item.is_symlink() {
            Kind::Symlink
        } else {
            Kind::File
        };
        let (uid, gid, perm) = if item.is_hard_link() {
            let r = self.reader()?.inode(&self.original[&id])?;
            (r.uid, r.gid, r.mode & 0o7777)
        } else {
            (be32(&item.body, 32), be32(&item.body, 36), item.mode() & 0o7777)
        };
        let size = match (&self.written.get(&id), kind) {
            (_, Kind::Folder) => 0,
            (Some(Content::Memory(b)), _) => b.len() as u64,
            (Some(Content::Host(p)), _) => std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
            (None, _) => match self.original.get(&id) {
                Some(r) => self.reader()?.size(r)?,
                None => 0,
            },
        };
        Ok(Meta { kind, perm, size, uid, gid, cnid: id })
    }

    fn reader(&self) -> Result<&Volume> {
        self.reader.as_ref().ok_or_else(|| crate::internal!("{}: already committed", self.name()))
    }

    /// The bytes of a file or a symlink's target.
    fn contents(&self, id: u32) -> Result<Vec<u8>> {
        match self.written.get(&id) {
            Some(Content::Memory(b)) => Ok(b.clone()),
            Some(Content::Host(p)) => std::fs::read(p).map_err(|e| crate::internal!("{}: {e}", p.display())),
            None => match self.original.get(&id) {
                Some(r) => self.reader()?.contents(r),
                None => Ok(Vec::new()),
            },
        }
    }

    // ---- The edit API

    /// What `path` is, following a symlink at its end.
    pub fn stat(&self, path: &str) -> Result<Option<Meta>> {
        self.lookup(path, true)?.map(|id| self.meta(id)).transpose()
    }

    /// What `path` is, a symlink at its end as the symlink.
    pub fn lstat(&self, path: &str) -> Result<Option<Meta>> {
        self.lookup(path, false)?.map(|id| self.meta(id)).transpose()
    }

    /// Whether `path` exists, following symlinks (a dangling one doesn't).
    pub fn exists(&self, path: &str) -> bool {
        matches!(self.lookup(path, true), Ok(Some(_)))
    }

    pub fn is_folder(&self, path: &str) -> bool {
        matches!(self.stat(path), Ok(Some(m)) if m.kind == Kind::Folder)
    }

    /// A file's bytes, through symlinks.
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let id = self.require(path, true)?;
        if self.items[&id].is_folder() {
            return Err(crate::internal!("{}: a folder, not a file: {path}", self.name()));
        }
        self.contents(id)
    }

    pub fn read_link(&self, path: &str) -> Result<String> {
        let id = self.require(path, false)?;
        if !self.items[&id].is_symlink() {
            return Err(crate::internal!("{}: not a symlink: {path}", self.name()));
        }
        self.link_target(id)
    }

    /// The names in a folder (through symlinks), sorted.
    pub fn list(&self, path: &str) -> Result<Vec<String>> {
        let id = self.require(path, true)?;
        if !self.items[&id].is_folder() {
            return Err(crate::internal!("{}: not a folder: {path}", self.name()));
        }
        let mut names: Vec<String> = self
            .children
            .get(&id)
            .map(|c| c.keys().filter(|n| self.child(id, n).is_some()).map(|n| posix_name(&String::from_utf16_lossy(n))).collect())
            .unwrap_or_default();
        names.sort();
        Ok(names)
    }

    /// Every path under `path` (not following symlinks below it), depth first, sorted, relative to `path`.
    pub fn walk(&self, path: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let root = self.require(path, true)?;
        let mut stack = vec![(root, String::new())];
        while let Some((id, rel)) = stack.pop() {
            let mut names: Vec<(String, u32)> = self
                .children
                .get(&id)
                .map(|c| c.iter().filter(|(n, _)| self.child(id, n).is_some()).map(|(n, &c)| (posix_name(&String::from_utf16_lossy(n)), c)).collect())
                .unwrap_or_default();
            names.sort();
            for (name, child) in names.into_iter().rev() {
                let p = if rel.is_empty() { name } else { format!("{rel}/{name}") };
                if self.items[&child].is_folder() {
                    stack.push((child, p.clone()));
                }
                out.push(p);
            }
        }
        out.sort();
        Ok(out)
    }

    fn new_item(&mut self, parent: u32, name: Vec<u16>, kind: u16, perm: u16) -> u32 {
        let cnid = self.next_id;
        self.next_id += 1;
        let date = hfs_date(self.now);
        let item = new_item(cnid, parent, name.clone(), kind, perm, date, self.case_sensitive);
        self.items.insert(cnid, item);
        self.children.entry(parent).or_default().insert(name, cnid);
        cnid
    }

    fn set_content(&mut self, path: &str, content: Content, mode: Option<u16>) -> Result<()> {
        let (parent, name) = self.parent_of(path)?;
        let existing = match self.child(parent, &component(&name)) {
            Some(id) if self.items[&id].is_symlink() => self.lookup(path, true)?,
            other => other,
        };
        let id = match existing {
            Some(id) => {
                let item = &self.items[&id];
                if item.is_folder() {
                    return Err(crate::internal!("{}: a folder, not a file: {path}", self.name()));
                }
                if item.is_hard_link() {
                    return Err(crate::unsupported!("{path}: a hard link written (not supported here)"));
                }
                id
            }
            // A dangling symlink's target isn't created: write where the name is.
            None => {
                if self.child(parent, &component(&name)).is_some() {
                    return Err(crate::internal!("{}: dangling symlink {path}", self.name()));
                }
                self.new_item(parent, component(&name), S_IFREG, 0o644)
            }
        };
        if let Some(mode) = mode {
            let item = self.items.get_mut(&id).expect("looked up");
            let m = item.mode() & !0o7777 | mode & 0o7777;
            put16(&mut item.body, 42, m);
        }
        self.written.insert(id, content);
        Ok(())
    }

    /// Writes a file: an existing one in place (its record, owner and mode stay unless `mode` is given), else a
    /// new one with `mode` or 0644. Writes through a symlink at the end of `path`.
    pub fn write(&mut self, path: &str, data: Vec<u8>, mode: Option<u16>) -> Result<()> {
        self.set_content(path, Content::Memory(data), mode)
    }

    /// `write` with the bytes of a host file, read at commit.
    pub fn write_from(&mut self, path: &str, source: &Path, mode: Option<u16>) -> Result<()> {
        if !source.is_file() {
            return Err(crate::internal!("{}: not a file", source.display()));
        }
        self.set_content(path, Content::Host(source.to_path_buf()), mode)
    }

    /// A new folder (0755); an existing one is an error.
    pub fn mkdir(&mut self, path: &str, mode: Option<u16>) -> Result<()> {
        let (parent, name) = self.parent_of(path)?;
        if self.child(parent, &component(&name)).is_some() {
            return Err(crate::internal!("{}: already exists: {path}", self.name()));
        }
        self.new_item(parent, component(&name), S_IFDIR, mode.unwrap_or(0o755) & 0o7777);
        Ok(())
    }

    /// Makes `path` and every missing folder above it (0755).
    pub fn mkdirs(&mut self, path: &str) -> Result<()> {
        let mut at = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            at = if at.is_empty() { part.to_string() } else { format!("{at}/{part}") };
            match self.stat(&at)? {
                Some(m) if m.kind == Kind::Folder => {}
                Some(_) => return Err(crate::internal!("{}: not a folder: {at}", self.name())),
                None => self.mkdir(&at, None)?,
            }
        }
        Ok(())
    }

    /// A new symlink at `path` to `target` (0755, as macOS makes them).
    pub fn symlink(&mut self, path: &str, target: &str) -> Result<()> {
        let (parent, name) = self.parent_of(path)?;
        if self.child(parent, &component(&name)).is_some() {
            return Err(crate::internal!("{}: already exists: {path}", self.name()));
        }
        let id = self.new_item(parent, component(&name), S_IFLNK, 0o755);
        self.written.insert(id, Content::Memory(target.as_bytes().to_vec()));
        Ok(())
    }

    /// Removes `path` (a symlink itself, not its target), a folder with everything in it.
    pub fn remove(&mut self, path: &str) -> Result<()> {
        let id = self.require(path, false)?;
        if id == ROOT_ID {
            return Err(crate::internal!("refusing to remove the volume root"));
        }
        let mut doomed = vec![id];
        let mut i = 0;
        while i < doomed.len() {
            if let Some(c) = self.children.get(&doomed[i]) {
                doomed.extend(c.values().copied());
            }
            i += 1;
        }
        if let Some(&link) = doomed.iter().find(|d| self.items[d].is_hard_link()) {
            return Err(crate::unsupported!(
                "{}: a hard link removed (not supported here)",
                String::from_utf16_lossy(&self.items[&link].name)
            ));
        }
        let item = &self.items[&id];
        if let Some(c) = self.children.get_mut(&item.parent) {
            c.remove(&item.name);
        }
        for d in doomed {
            if let Some(item) = self.items.remove(&d) {
                self.removed.extend(item.data.extents);
                self.removed.extend(item.resource.extents);
            }
            self.children.remove(&d);
            self.written.remove(&d);
        }
        Ok(())
    }

    /// Moves `from` to `to` (which must not exist), keeping its record.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let id = self.require(from, false)?;
        if self.items[&id].is_hard_link() {
            return Err(crate::unsupported!("{from}: a hard link moved (not supported here)"));
        }
        let (parent, name) = self.parent_of(to)?;
        if self.child(parent, &component(&name)).is_some() {
            return Err(crate::internal!("{}: already exists: {to}", self.name()));
        }
        // not into itself
        let mut up = parent;
        while up != ROOT_ID {
            if up == id {
                return Err(crate::internal!("{}: {from} can't move into itself", self.name()));
            }
            up = self.items[&up].parent;
        }
        let item = self.items.get_mut(&id).expect("looked up");
        let old = (item.parent, std::mem::take(&mut item.name));
        item.parent = parent;
        item.name = component(&name);
        let new_name = item.name.clone();
        if let Some(c) = self.children.get_mut(&old.0) {
            c.remove(&old.1);
        }
        self.children.entry(parent).or_default().insert(new_name, id);
        Ok(())
    }

    /// Sets the permission bits (through a symlink at the end of `path`).
    pub fn chmod(&mut self, path: &str, perm: u16) -> Result<()> {
        let id = self.require(path, true)?;
        let item = self.items.get_mut(&id).expect("looked up");
        let m = item.mode() & !0o7777 | perm & 0o7777;
        put16(&mut item.body, 42, m);
        Ok(())
    }

    // ---- Commit

    /// The full paths of the written items, for a stable allocation order.
    fn path_of(&self, mut id: u32) -> String {
        let mut parts = Vec::new();
        while id != ROOT_ID {
            let Some(item) = self.items.get(&id) else { break };
            parts.push(posix_name(&String::from_utf16_lossy(&item.name)));
            id = item.parent;
        }
        parts.reverse();
        parts.join("/")
    }

    /// Writes the edits into the image. The editor is spent afterwards.
    pub fn commit(mut self) -> Result<()> {
        if self.read_only {
            return Err(crate::internal!("{}: a read-only view can't commit", self.name()));
        }
        let hfs_now = hfs_date(self.now);
        // The image is written from here on; nothing more is read through the opening's records.
        self.reader = None;
        let mut freed: Vec<Extent> = std::mem::take(&mut self.removed);
        let mut drop_attributes: HashSet<u32> = HashSet::new(); // decmpfs of files written plain
        let mut written: Vec<(String, u32)> = self.written.keys().map(|&id| (self.path_of(id), id)).collect();
        written.sort();
        for (_, id) in &written {
            if !self.original.contains_key(id) {
                continue;
            }
            let item = self.items.get_mut(id).expect("written items exist");
            // written: stored plain, as the driver stores a compressed file once written
            freed.extend(std::mem::take(&mut item.data).extents);
            if item.compressed() {
                freed.extend(std::mem::take(&mut item.resource).extents);
                item.body[41] &= !COMPRESSED;
                drop_attributes.insert(*id);
            }
            put32(&mut item.body, 16, hfs_now);
            put32(&mut item.body, 20, hfs_now);
        }
        let decmpfs: Vec<u16> = "com.apple.decmpfs".encode_utf16().collect();
        let mut kept = Vec::new();
        for a in std::mem::take(&mut self.attributes) {
            let gone = !self.items.contains_key(&a.file_id);
            let decompressed = drop_attributes.contains(&a.file_id) && a.name == decmpfs;
            if gone || decompressed {
                freed.extend(a.extents());
            } else {
                kept.push(a);
            }
        }
        self.attributes = kept;
        // Everything is placed in memory first (contents, then the trees as they grow): a volume too full for the
        // edits fails here with the image untouched.
        self.free(&freed);
        let mut contents = Vec::new();
        for (_, id) in written {
            let content = self.written.remove(&id).expect("listed");
            let size = match &content {
                Content::Memory(b) => b.len() as u64,
                Content::Host(p) => std::fs::metadata(p).map_err(|e| crate::internal!("{}: {e}", p.display()))?.len(),
            };
            let extents = self.allocate(size.div_ceil(self.block_size) as u32)?;
            let fork = Fork { logical_size: size, clump_size: 0, extents, recorded_blocks: 0 };
            self.items.get_mut(&id).expect("written items exist").data = fork.clone();
            contents.push((fork, content));
        }
        if let Some((jib, journal, bytes)) = self.new_journal {
            // blocks free until now: writing them leaves the volume as it was should the layout below fail
            let mut info = vec![0u8; self.block_size as usize];
            put32(&mut info, 0, 0x1 | 0x4); // in the file system, needs init
            put64(&mut info, 36, journal.start as u64 * self.block_size);
            put64(&mut info, 44, bytes);
            self.zero(&[journal])?;
            posio::write_all_at(&self.file, &info, jib.start as u64 * self.block_size)
                .map_err(|e| crate::internal!("write {}: {e}", self.name()))?;
            let attributes = be32(&self.header, 4) | 0x2000; // kHFSVolumeJournaledBit
            put32(&mut self.header, 4, attributes);
            put32(&mut self.header, 8, 0x4846_534A); // 'HFSJ'
            put32(&mut self.header, 12, jib.start);
        }
        put32(&mut self.header, 64, self.next_id);
        self.write_metadata(hfs_now, &freed, contents)
    }

    /// Journals the volume at commit, as `diskutil enableJournal` and then a hand-over to the device leave it: the
    /// .journal and .journal_info_block files at the root, the journal zeroed and its info block marked needs-init,
    /// so the device's kernel writes the header for its own block size at the first mount (a header the host wrote
    /// says 512-byte blocks, which iPhone OS 1.x on 2048-byte NAND pages then fails its I/O with). A volume already
    /// journaled is left as it is.
    pub fn enable_journal(&mut self, bytes: u64) -> Result<()> {
        if self.read_only {
            return Err(crate::internal!("{}: a read-only view can't be journaled", self.name()));
        }
        if be32(&self.header, 4) & 0x2000 != 0 || self.new_journal.is_some() {
            return Ok(());
        }
        let blocks = bytes.div_ceil(self.block_size);
        let jib = self.allocate(1)?;
        let journal = self.allocate(blocks as u32)?;
        if journal.len() != 1 {
            self.free(&jib);
            self.free(&journal);
            return Err(crate::unsupported!("{}: no contiguous {bytes} bytes free for the journal", self.name()));
        }
        let (jib, journal) = (jib[0], journal[0]);
        let date = hfs_date(self.now);
        for (file_name, extent, size) in [(".journal", journal, blocks * self.block_size), (".journal_info_block", jib, self.block_size)] {
            let cnid = self.next_id;
            self.next_id += 1;
            let name: Vec<u16> = file_name.encode_utf16().collect();
            let mut f = new_item(cnid, ROOT_ID, name.clone(), S_IFREG, 0, date, self.case_sensitive);
            put32(&mut f.body, 32, 0);
            put32(&mut f.body, 36, 0);
            put32(&mut f.body, 48, 0x6A72_6E6C); // 'jrnl'
            put32(&mut f.body, 52, 0x6866_732B); // 'hfs+'
            put16(&mut f.body, 56, 0x5000); // invisible, name locked
            f.data.extents = vec![extent];
            f.data.logical_size = size;
            self.items.insert(cnid, f);
            self.children.entry(ROOT_ID).or_default().insert(name, cnid);
        }
        self.new_journal = Some((jib, journal, blocks * self.block_size));
        Ok(())
    }

    // ---- Blocks

    fn is_used(&self, block: u64) -> bool {
        self.bitmap[(block >> 3) as usize] & (0x80 >> (block & 7)) != 0
    }

    fn mark(&mut self, e: Extent, used: bool) {
        for b in e.start as u64..e.start as u64 + e.count as u64 {
            let (byte, bit) = ((b >> 3) as usize, 0x80u8 >> (b & 7));
            if used {
                self.bitmap[byte] |= bit;
            } else {
                self.bitmap[byte] &= !bit;
            }
        }
    }

    fn free(&mut self, extents: &[Extent]) {
        for &e in extents {
            self.mark(e, false);
        }
    }

    /// Zeroes `extents` on disk, so freed blocks pack as empty pages.
    fn zero(&self, extents: &[Extent]) -> Result<()> {
        let zeros = vec![0u8; 1 << 20];
        for e in extents {
            let (start, length) = (e.start as u64 * self.block_size, e.count as u64 * self.block_size);
            let mut done = 0;
            while done < length {
                let n = (zeros.len() as u64).min(length - done) as usize;
                posio::write_all_at(&self.file, &zeros[..n], start + done)?;
                done += n as u64;
            }
        }
        Ok(())
    }

    /// `count` free blocks, as few extents as there are runs for (first fit).
    fn allocate(&mut self, count: u32) -> Result<Vec<Extent>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let count = count as u64;
        let mut runs: Vec<Extent> = Vec::new();
        let mut b = 0u64;
        while b < self.total_blocks {
            if b & 7 == 0 && self.bitmap[(b >> 3) as usize] == 0xFF {
                b += 8;
                continue;
            }
            if self.is_used(b) {
                b += 1;
                continue;
            }
            let mut e = b;
            while e < self.total_blocks && !self.is_used(e) && e - b < count {
                e += 1;
            }
            if e - b == count {
                let hit = Extent { start: b as u32, count: count as u32 };
                self.mark(hit, true);
                return Ok(vec![hit]);
            }
            runs.push(Extent { start: b as u32, count: (e - b) as u32 });
            b = e;
        }
        // No single run: the largest ones.
        runs.sort_by(|a, b| b.count.cmp(&a.count));
        let mut out = Vec::new();
        let mut left = count;
        for r in runs {
            if left == 0 {
                break;
            }
            let take = Extent { start: r.start, count: (r.count as u64).min(left) as u32 };
            left -= take.count as u64;
            out.push(take);
        }
        if left != 0 {
            return Err(crate::unsupported!("{}: the volume is full ({count} blocks wanted)", self.name()));
        }
        for &e in &out {
            self.mark(e, true);
        }
        out.sort_by_key(|e| e.start);
        Ok(out)
    }

    /// Writes `bytes` into `fork`'s blocks, zero-filling the rest of its last block.
    fn write_fork(&self, fork: &Fork, bytes: &[u8]) -> Result<()> {
        let mut done = 0usize;
        for e in &fork.extents {
            let length = (e.count as u64 * self.block_size) as usize;
            let mut chunk = vec![0u8; length];
            let n = length.min(bytes.len().saturating_sub(done));
            chunk[..n].copy_from_slice(&bytes[done..done + n]);
            posio::write_all_at(&self.file, &chunk, e.start as u64 * self.block_size)
                .map_err(|err| crate::internal!("write {}: {err}", self.name()))?;
            done += n;
        }
        Ok(())
    }

    /// Streams the host file `source` into `fork`'s blocks.
    fn write_fork_from(&self, fork: &Fork, source: &Path) -> Result<()> {
        use std::io::Read;
        let mut input = File::open(source).map_err(|e| crate::internal!("open {}: {e}", source.display()))?;
        let mut buffer = vec![0u8; 1 << 20];
        for e in &fork.extents {
            let length = e.count as u64 * self.block_size;
            let mut at = 0u64;
            while at < length {
                let want = (buffer.len() as u64).min(length - at) as usize;
                let mut got = 0;
                while got < want {
                    match input.read(&mut buffer[got..want]) {
                        Ok(0) => break,
                        Ok(n) => got += n,
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(err) => return Err(crate::internal!("read {}: {err}", source.display())),
                    }
                }
                buffer[got..want].fill(0); // past the host file's end
                posio::write_all_at(&self.file, &buffer[..want], e.start as u64 * self.block_size + at)
                    .map_err(|err| crate::internal!("write {}: {err}", self.name()))?;
                at += want as u64;
            }
        }
        Ok(())
    }

    // ---- B-trees and the header

    /// Lays out the catalog, attributes and extents overflow B-trees, then writes: `freed` zeroed, `contents` into
    /// their forks, the trees, the allocation bitmap and the volume headers.
    fn write_metadata(&mut self, hfs_now: u32, freed: &[Extent], contents: Vec<(Fork, Content)>) -> Result<()> {
        let mut trees: Vec<(Fork, Vec<u8>)> = Vec::new();
        // Folder valences and folder counts; the has-attributes flag.
        let mut valence: HashMap<u32, u32> = HashMap::new();
        let mut subfolders: HashMap<u32, u32> = HashMap::new();
        for item in self.items.values() {
            if item.cnid() != ROOT_ID {
                *valence.entry(item.parent).or_default() += 1;
                if item.is_folder() {
                    *subfolders.entry(item.parent).or_default() += 1;
                }
            }
        }
        let with_attributes: HashSet<u32> = self.attributes.iter().map(|a| a.file_id).collect();
        for (id, item) in self.items.iter_mut() {
            if item.is_folder() {
                put32(&mut item.body, 4, valence.get(id).copied().unwrap_or(0));
                if be16(&item.body, 2) & 0x10 != 0 {
                    put32(&mut item.body, 84, subfolders.get(id).copied().unwrap_or(0));
                }
            }
            let flags = be16(&item.body, 2);
            put16(&mut item.body, 2, if with_attributes.contains(id) { flags | 0x4 } else { flags & !0x4 });
        }
        // Catalog: every record and its thread, by (parent, name).
        let mut catalog: Vec<(u32, Vec<u16>, Vec<u8>)> = Vec::new();
        let (mut files, mut folders) = (0u32, 0u32);
        for item in self.items.values() {
            let mut body = item.body.clone();
            if !item.is_folder() {
                body[88..168].copy_from_slice(&item.data.record());
                body[168..248].copy_from_slice(&item.resource.record());
                files += 1;
            } else if item.cnid() != ROOT_ID {
                folders += 1;
            }
            let mut record = catalog_key(item.parent, &item.name);
            record.extend_from_slice(&body);
            catalog.push((item.parent, item.name.clone(), record));
            let mut thread = vec![0u8; 10 + 2 * item.name.len()];
            put16(&mut thread, 0, if item.is_folder() { 3 } else { 4 });
            put32(&mut thread, 4, item.parent);
            put16(&mut thread, 8, item.name.len() as u16);
            for (i, &u) in item.name.iter().enumerate() {
                put16(&mut thread, 10 + 2 * i, u);
            }
            let mut record = catalog_key(item.cnid(), &[]);
            record.extend_from_slice(&thread);
            catalog.push((item.cnid(), Vec::new(), record));
        }
        if self.case_sensitive {
            catalog.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        } else {
            catalog.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| casefold::compare(&a.1, &b.1)));
            for w in catalog.windows(2) {
                if w[0].0 == w[1].0 && casefold::compare(&w[0].1, &w[1].1) == Ordering::Equal {
                    return Err(crate::unsupported!(
                        "{}: two names in one folder differ only in case ({})",
                        self.name(),
                        String::from_utf16_lossy(&w[1].1)
                    ));
                }
            }
        }
        let records: Vec<Vec<u8>> = catalog.into_iter().map(|c| c.2).collect();
        let (shape, mut fork) = (self.catalog_shape.clone(), self.catalog_file.clone());
        let bytes = self.build_tree(&records, &shape, &mut fork, CATALOG_ID)?;
        trees.push((fork.clone(), bytes));
        self.catalog_file = fork;

        if let Some(shape) = self.attributes_shape.clone() {
            let mut sorted = self.attributes.clone();
            sorted.sort_by(|a, b| a.file_id.cmp(&b.file_id).then_with(|| a.name.cmp(&b.name)).then_with(|| a.start_block.cmp(&b.start_block)));
            let records: Vec<Vec<u8>> = sorted
                .iter()
                .map(|a| {
                    let mut key = vec![0u8; 14 + 2 * a.name.len()];
                    put16(&mut key, 0, (12 + 2 * a.name.len()) as u16);
                    put32(&mut key, 4, a.file_id);
                    put32(&mut key, 8, a.start_block);
                    put16(&mut key, 12, a.name.len() as u16);
                    for (i, &u) in a.name.iter().enumerate() {
                        put16(&mut key, 14 + 2 * i, u);
                    }
                    key.extend_from_slice(&a.body);
                    if a.body.len() % 2 == 1 {
                        key.push(0);
                    }
                    key
                })
                .collect();
            let mut fork = self.attributes_file.clone();
            let bytes = self.build_tree(&records, &shape, &mut fork, ATTRIBUTES_ID)?;
            trees.push((fork.clone(), bytes));
            self.attributes_file = fork;
        }

        // Extents overflow: every fork's extents past its eighth, as (file, fork type, start block) records.
        let mut overflow: Vec<(u32, u8, u32, Vec<u8>)> = Vec::new();
        let spill = |fork: &Fork, id: u32, kind: u8, overflow: &mut Vec<(u32, u8, u32, Vec<u8>)>| {
            if fork.extents.len() <= 8 {
                return;
            }
            let mut start: u32 = fork.extents[..8].iter().map(|e| e.count).sum();
            for group in fork.extents[8..].chunks(8) {
                let mut r = vec![0u8; 12 + 64];
                put16(&mut r, 0, 10);
                r[2] = kind;
                put32(&mut r, 4, id);
                put32(&mut r, 8, start);
                for (i, e) in group.iter().enumerate() {
                    put32(&mut r, 12 + 8 * i, e.start);
                    put32(&mut r, 16 + 8 * i, e.count);
                }
                overflow.push((id, kind, start, r));
                start += group.iter().map(|e| e.count).sum::<u32>();
            }
        };
        for item in self.items.values().filter(|i| !i.is_folder()) {
            spill(&item.data, item.cnid(), 0, &mut overflow);
            spill(&item.resource, item.cnid(), 0xFF, &mut overflow);
        }
        spill(&self.allocation, ALLOCATION_ID, 0, &mut overflow);
        spill(&self.catalog_file, CATALOG_ID, 0, &mut overflow);
        spill(&self.attributes_file, ATTRIBUTES_ID, 0, &mut overflow);
        overflow.sort_by_key(|o| (o.0, o.1, o.2));
        let records: Vec<Vec<u8>> = overflow.into_iter().map(|o| o.3).collect();
        let (shape, mut fork) = (self.extents_shape.clone(), self.extents_file.clone());
        let bytes = self.build_tree(&records, &shape, &mut fork, EXTENTS_ID)?;
        trees.push((fork.clone(), bytes));
        self.extents_file = fork;
        if self.extents_file.extents.len() > 8 {
            return Err(crate::unsupported!("{}: the extents file is too fragmented", self.name()));
        }

        // Laid out: now the writes. Freed blocks are zeroed before anything reuses them.
        self.zero(freed)?;
        for (fork, content) in &contents {
            match content {
                Content::Memory(b) => self.write_fork(fork, b)?,
                Content::Host(p) => self.write_fork_from(fork, p)?,
            }
        }
        for (fork, bytes) in &trees {
            self.write_fork(fork, bytes)?;
        }
        // The bitmap, then the header (counts, forks) at 1024 and the alternate at the end.
        let mut map = self.bitmap.clone();
        let map_bytes = (self.allocation.total_blocks() as u64 * self.block_size) as usize;
        if map.len() < map_bytes {
            map.resize(map_bytes, 0);
        }
        let allocation = self.allocation.clone();
        self.write_fork(&allocation, &map)?;
        let used = (0..self.total_blocks).filter(|&b| self.is_used(b)).count() as u64;
        let attributes = be32(&self.header, 4) & !0x800 | 0x100; // unmounted, not inconsistent
        put32(&mut self.header, 4, attributes);
        put32(&mut self.header, 20, hfs_now);
        put32(&mut self.header, 32, files);
        put32(&mut self.header, 36, folders);
        put32(&mut self.header, 48, (self.total_blocks - used) as u32);
        let write_count = be32(&self.header, 68).wrapping_add(1);
        put32(&mut self.header, 68, write_count);
        self.header[112..192].copy_from_slice(&self.allocation.record());
        self.header[192..272].copy_from_slice(&self.extents_file.record());
        self.header[272..352].copy_from_slice(&self.catalog_file.record());
        self.header[352..432].copy_from_slice(&self.attributes_file.record());
        self.write_headers()
    }

    fn write_headers(&self) -> Result<()> {
        let size = self.file.metadata()?.len();
        let volume_end = be32(&self.header, 44) as u64 * self.block_size;
        let mut at: Vec<u64> = vec![1024, volume_end - 1024, size.saturating_sub(1024)];
        at.sort();
        at.dedup();
        for o in at.into_iter().filter(|&o| o >= volume_end - 1024 || o == 1024) {
            posio::write_all_at(&self.file, &self.header, o)
                .map_err(|_| crate::internal!("write the volume header of {}", self.name()))?;
        }
        Ok(())
    }

    /// The B-tree file of `records` (sorted; each its key from its keyLength field, then its data) for `fork`,
    /// which grows by whole clumps when its nodes no longer fit.
    fn build_tree(&mut self, records: &[Vec<u8>], shape: &TreeShape, fork: &mut Fork, file_id: u32) -> Result<Vec<u8>> {
        let node_size = shape.node_size;
        let room = node_size - 14 - 2;
        // Leaves, then index levels until one node holds them all.
        type Node = (Vec<Vec<u8>>, Vec<u8>);
        let pack = |recs: &[Vec<u8>]| -> Vec<Node> {
            let mut nodes: Vec<Node> = Vec::new();
            let mut current: Vec<Vec<u8>> = Vec::new();
            let mut used = 0;
            for r in recs {
                if !current.is_empty() && used + r.len() + 2 > room {
                    let first = current[0].clone();
                    nodes.push((std::mem::take(&mut current), first));
                    used = 0;
                }
                current.push(r.clone());
                used += r.len() + 2;
            }
            if !current.is_empty() {
                let first = current[0].clone();
                nodes.push((current, first));
            }
            nodes
        };
        let mut levels: Vec<Vec<Node>> = Vec::new();
        if !records.is_empty() {
            levels.push(pack(records));
            let mut number = 1 + levels[0].len(); // index node numbers follow the leaves
            let mut first_numbers: Vec<usize> = (1..=levels[0].len()).collect();
            while levels.last().expect("a level").len() > 1 {
                let below = levels.last().expect("a level");
                let index: Vec<Vec<u8>> = below
                    .iter()
                    .zip(&first_numbers)
                    .map(|((_, first_key), &n)| {
                        let mut key = first_key[..2 + be16(first_key, 0) as usize].to_vec();
                        if !shape.variable_index_keys() {
                            let want = shape.max_key_length() + 2;
                            if key.len() < want {
                                key.resize(want, 0);
                            }
                            put16(&mut key, 0, shape.max_key_length() as u16);
                        }
                        key.extend_from_slice(&(n as u32).to_be_bytes());
                        key
                    })
                    .collect();
                let level = pack(&index);
                first_numbers = (number..number + level.len()).collect();
                number += level.len();
                levels.push(level);
            }
        }
        let tree_nodes = 1 + levels.iter().map(Vec::len).sum::<usize>();
        let header_map_bits = (node_size - 256) * 8;
        let map_node_bits = (node_size - 14 - 4) * 8;
        let map_nodes = |total: usize| {
            if total <= header_map_bits { 0 } else { (total - header_map_bits).div_ceil(map_node_bits) }
        };
        // Room: the file's nodes, grown by clumps with a quarter spare when the tree outgrows them.
        let mut capacity = (fork.total_blocks() as u64 * self.block_size) as usize / node_size;
        if tree_nodes + map_nodes(capacity) > capacity {
            let want = (tree_nodes + tree_nodes / 4 + 8) * node_size;
            let clump = (fork.clump_size as usize).max(self.block_size as usize);
            let grow = (want - capacity * node_size).div_ceil(clump) * clump / self.block_size as usize;
            let more = self.allocate(grow as u32)?;
            fork.extents.extend(more);
            capacity = (fork.total_blocks() as u64 * self.block_size) as usize / node_size;
        }
        let total_nodes = capacity;
        let maps = map_nodes(total_nodes);
        let used = tree_nodes + maps;
        if used > total_nodes {
            return Err(crate::internal!("B-tree {file_id} does not fit"));
        }
        fork.logical_size = (total_nodes * node_size) as u64;

        let mut file = vec![0u8; total_nodes * node_size];
        let node = |file: &mut Vec<u8>, n: usize, kind: u8, height: u8, recs: &[Vec<u8>], f_link: usize, b_link: usize| {
            let base = n * node_size;
            put32(file, base, f_link as u32);
            put32(file, base + 4, b_link as u32);
            file[base + 8] = kind;
            file[base + 9] = height;
            put16(file, base + 10, recs.len() as u16);
            let mut at = 14;
            for (i, r) in recs.iter().enumerate() {
                put16(file, base + node_size - 2 * (i + 1), at as u16);
                file[base + at..base + at + r.len()].copy_from_slice(r);
                at += r.len();
            }
            put16(file, base + node_size - 2 * (recs.len() + 1), at as u16);
        };
        let mut number = 1;
        let (mut root, mut first_leaf, mut last_leaf) = (0, 0, 0);
        for (depth, level) in levels.iter().enumerate() {
            let first = number;
            for (i, (recs, _)) in level.iter().enumerate() {
                node(
                    &mut file,
                    first + i,
                    if depth == 0 { 0xFF } else { 0 },
                    (depth + 1) as u8,
                    recs,
                    if i + 1 < level.len() { first + i + 1 } else { 0 },
                    if i > 0 { first + i - 1 } else { 0 },
                );
            }
            if depth == 0 {
                (first_leaf, last_leaf) = (first, first + level.len() - 1);
            }
            number += level.len();
            root = first;
        }
        // Map: bits for the used nodes (the header, the tree, the map nodes), MSB first.
        let mut bits = vec![0u8; total_nodes.div_ceil(8)];
        for n in 0..used {
            bits[n >> 3] |= 0x80 >> (n & 7);
        }
        let mut hdr = shape.header.clone();
        put16(&mut hdr, 0, levels.len() as u16);
        put32(&mut hdr, 2, root as u32);
        put32(&mut hdr, 6, records.len() as u32);
        put32(&mut hdr, 10, first_leaf as u32);
        put32(&mut hdr, 14, last_leaf as u32);
        put32(&mut hdr, 22, total_nodes as u32);
        put32(&mut hdr, 26, (total_nodes - used) as u32);
        let mut header_map = bits[..bits.len().min(node_size - 256)].to_vec();
        header_map.resize(node_size - 256, 0);
        node(&mut file, 0, 1, 0, &[hdr, vec![0u8; 128], header_map], if maps > 0 { tree_nodes } else { 0 }, 0);
        for m in 0..maps {
            let from = node_size - 256 + m * (node_size - 18);
            let mut chunk: Vec<u8> = bits.iter().skip(from).take(node_size - 18).copied().collect();
            chunk.resize(node_size - 18, 0);
            node(&mut file, tree_nodes + m, 2, 0, &[chunk], if m + 1 < maps { tree_nodes + m + 1 } else { 0 }, 0);
        }
        Ok(file)
    }

    // ---- Grow and format

    /// Grows the volume in `image` to `bytes` (whole blocks; the file grows with it): the new blocks free, the
    /// alternate header moved to the new end.
    pub fn grow(image: &Path, bytes: u64) -> Result<()> {
        let mut ed = Editor::open(image)?;
        ed.grow_to(bytes)
    }

    fn grow_to(&mut self, bytes: u64) -> Result<()> {
        let old_size = self.file.metadata()?.len();
        let new_total = bytes / self.block_size;
        if new_total < self.total_blocks {
            return Err(crate::internal!("{}: cannot shrink", self.name()));
        }
        self.file.set_len(bytes).map_err(|e| crate::internal!("grow {}: {e}", self.name()))?;
        let mut map = self.bitmap.clone();
        if map.len() < new_total.div_ceil(8) as usize {
            map.resize(new_total.div_ceil(8) as usize, 0);
        }
        let set = |map: &mut Vec<u8>, b: u64, used: bool| {
            let (byte, bit) = ((b >> 3) as usize, 0x80u8 >> (b & 7));
            if used { map[byte] |= bit } else { map[byte] &= !bit }
        };
        // The alternate header sits 1024 bytes before the volume's end, in its last block, which is kept allocated:
        // the old last block comes free, the new one is taken.
        if new_total > self.total_blocks {
            set(&mut map, self.total_blocks - 1, false);
            set(&mut map, new_total - 1, true);
        }
        let zeros = [0u8; 1024];
        for at in [self.total_blocks * self.block_size - 1024, old_size.saturating_sub(1024)] {
            let _ = posio::write_all_at(&self.file, &zeros, at);
        }
        // The bitmap must cover the new blocks: the allocation file grows into the new space if it is too short.
        if (self.allocation.total_blocks() as u64 * self.block_size) < map.len() as u64 {
            let more = (map.len() as u64).div_ceil(self.block_size) - self.allocation.total_blocks() as u64;
            let mut start = self.total_blocks;
            while start < new_total && map[(start >> 3) as usize] & (0x80 >> (start & 7)) != 0 {
                start += 1;
            }
            if start + more > new_total {
                return Err(crate::unsupported!("no room for the bitmap"));
            }
            for b in start..start + more {
                set(&mut map, b, true);
            }
            self.allocation.extents.push(Extent { start: start as u32, count: more as u32 });
            self.allocation.logical_size = self.allocation.total_blocks() as u64 * self.block_size;
            if self.allocation.extents.len() > 8 {
                return Err(crate::unsupported!("fragmented bitmap"));
            }
        }
        let map_bytes = (self.allocation.total_blocks() as u64 * self.block_size) as usize;
        let mut padded = map.clone();
        padded.resize(map_bytes.max(map.len()), 0);
        let allocation = self.allocation.clone();
        self.write_fork(&allocation, &padded)?;
        let used = (0..new_total).filter(|&b| map[(b >> 3) as usize] & (0x80 >> (b & 7)) != 0).count() as u64;
        put32(&mut self.header, 44, new_total as u32);
        put32(&mut self.header, 48, (new_total - used) as u32);
        self.header[112..192].copy_from_slice(&self.allocation.record());
        self.write_headers()
    }

    /// A new, empty HFSX volume (case-sensitive; journaled with the journal left for the device's kernel to
    /// initialize, as newfs_hfs -J leaves one) in a sparse file of `size` bytes, rounded down to 4 KiB.
    pub fn format(image: &Path, size: u64, name: &str, journaled: bool, now: i64) -> Result<()> {
        let block_size: u64 = 4096;
        let total = size / block_size;
        let file_name = image.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if total <= 64 {
            return Err(crate::internal!("{file_name}: too small for HFS+"));
        }
        let file = posio::create_sparse(image).map_err(|_| crate::internal!("cannot create {}", image.display()))?;
        file.set_len(total * block_size).map_err(|_| crate::internal!("cannot create {}", image.display()))?;
        let date = hfs_date(now);
        // Layout: block 0 (boot blocks, the header), the bitmap, the extents, catalog and attributes files, the
        // journal info block and the journal; the last block holds the alternate header.
        let mut next: u64 = 1;
        let mut take = |blocks: u64| {
            let e = Extent { start: next as u32, count: blocks as u32 };
            next += blocks;
            e
        };
        let map_blocks = total.div_ceil(8).div_ceil(block_size);
        let allocation = Fork {
            extents: vec![take(map_blocks)],
            logical_size: map_blocks * block_size,
            clump_size: block_size as u32,
            recorded_blocks: 0,
        };
        let mut tree_fork = |bytes: u64| Fork {
            extents: vec![take(bytes / block_size)],
            logical_size: bytes,
            clump_size: bytes as u32,
            recorded_blocks: 0,
        };
        let mib = 1u64 << 20;
        let extents = tree_fork(mib);
        let catalog = tree_fork(4 * mib);
        let attributes = tree_fork(mib);
        let journal_bytes = 8 * mib;
        let jib = journaled.then(|| take(1));
        let journal = journaled.then(|| take(journal_bytes / block_size));
        if next >= total - 1 {
            return Err(crate::internal!("{file_name}: too small for HFS+"));
        }
        let mut vh = vec![0u8; 512];
        vh[0] = 0x48;
        vh[1] = 0x58; // HX
        put16(&mut vh, 2, 5);
        put32(&mut vh, 4, 0x100 | if journaled { 0x2000 } else { 0 }); // unmounted, journaled
        put32(&mut vh, 8, if journaled { 0x4846_534A } else { 0x3130_2E30 }); // 'HFSJ' / '10.0'
        put32(&mut vh, 12, jib.map(|e| e.start).unwrap_or(0));
        for at in [16, 20, 24, 28] {
            put32(&mut vh, at, date); // create, modify, backup, checked
        }
        put32(&mut vh, 40, block_size as u32);
        put32(&mut vh, 44, total as u32);
        put32(&mut vh, 56, block_size as u32); // rsrcClumpSize
        put32(&mut vh, 60, block_size as u32); // dataClumpSize
        put32(&mut vh, 64, 16); // nextCatalogID
        put32(&mut vh, 76, 1); // encodingsBitmap: MacRoman
        let empty_tree = |fork: &Fork, node_size: usize, max_key: u16, compare: u8, attributes: u32| -> Result<()> {
            let mut hdr = vec![0u8; 106];
            put16(&mut hdr, 18, node_size as u16);
            put16(&mut hdr, 20, max_key);
            put32(&mut hdr, 32, fork.clump_size);
            hdr[36] = 0; // btreeType: HFS
            hdr[37] = compare;
            put32(&mut hdr, 38, attributes);
            let mut node = vec![0u8; node_size];
            node[8] = 1;
            put16(&mut node, 10, 3);
            node[14..120].copy_from_slice(&hdr);
            for (i, at) in [14, 120, 248, node_size - 8].into_iter().enumerate() {
                put16(&mut node, node_size - 2 * (i + 1), at as u16);
            }
            posio::write_all_at(&file, &node, fork.extents[0].start as u64 * block_size)
                .map_err(|_| crate::internal!("write {file_name}"))
        };
        empty_tree(&extents, 4096, 10, 0, 0x2)?;
        empty_tree(&catalog, 8192, 516, 0xBC, 0x6)?;
        empty_tree(&attributes, 8192, 266, 0, 0x6)?;
        vh[112..192].copy_from_slice(&allocation.record());
        vh[192..272].copy_from_slice(&extents.record());
        vh[272..352].copy_from_slice(&catalog.record());
        vh[352..432].copy_from_slice(&attributes.record());
        posio::write_all_at(&file, &vh, 1024).map_err(|_| crate::internal!("write {file_name}"))?;
        posio::write_all_at(&file, &vh, total * block_size - 1024).map_err(|_| crate::internal!("write {file_name}"))?;
        if let (Some(jib), Some(_)) = (jib, journal) {
            // flags: in the file system, needs init; then device_signature[8], offset, size
            let mut info = vec![0u8; block_size as usize];
            put32(&mut info, 0, 0x1 | 0x4);
            put64(&mut info, 36, journal.expect("journaled").start as u64 * block_size);
            put64(&mut info, 44, journal_bytes);
            posio::write_all_at(&file, &info, jib.start as u64 * block_size).map_err(|_| crate::internal!("write {file_name}"))?;
        }
        drop(file);

        // The rest through an editor: the bitmap, the root folder and the journal's files.
        let mut ed = Editor::open_bare(image)?;
        for e in [Extent { start: 0, count: next as u32 }, Extent { start: (total - 1) as u32, count: 1 }] {
            ed.mark(e, true);
        }
        let mut root = new_item(ROOT_ID, 1, name.encode_utf16().collect(), S_IFDIR, 0o755, date, true);
        put32(&mut root.body, 32, 0);
        put32(&mut root.body, 36, 0);
        ed.items.insert(ROOT_ID, root);
        if let (Some(jib), Some(journal)) = (jib, journal) {
            for (cnid, file_name, extent, bytes) in
                [(16u32, ".journal", journal, journal_bytes), (17, ".journal_info_block", jib, block_size)]
            {
                let mut f = new_item(cnid, ROOT_ID, file_name.encode_utf16().collect(), S_IFREG, 0, date, true);
                put32(&mut f.body, 32, 0);
                put32(&mut f.body, 36, 0);
                f.body[40] = 0;
                f.body[41] = 0;
                put32(&mut f.body, 48, 0x6A72_6E6C); // 'jrnl'
                put32(&mut f.body, 52, 0x6866_732B); // 'hfs+'
                put16(&mut f.body, 56, 0x5000); // invisible, name locked
                f.data.extents = vec![extent];
                f.data.logical_size = bytes;
                ed.items.insert(cnid, f);
            }
            put32(&mut ed.header, 64, 18);
        }
        ed.write_metadata(date, &[], Vec::new())
    }

    /// An editor over a volume whose trees are empty (format's second half): no catalog to load.
    fn open_bare(path: &Path) -> Result<Editor> {
        let file = File::options().read(true).write(true).open(path).map_err(|e| crate::internal!("open {}: {e}", path.display()))?;
        let mut vh = vec![0u8; 512];
        posio::read_exact_at(&file, &mut vh, 1024)?;
        let read_shape = |fork: &Fork| -> Result<TreeShape> {
            let mut head = vec![0u8; 512];
            posio::read_exact_at(&file, &mut head, fork.extents[0].start as u64 * be32(&vh, 40) as u64)?;
            Ok(TreeShape { node_size: be16(&head, 14 + 18) as usize, header: head[14..120].to_vec() })
        };
        let (allocation, extents_file, catalog_file, attributes_file) =
            (Fork::parse(&vh, 112), Fork::parse(&vh, 192), Fork::parse(&vh, 272), Fork::parse(&vh, 352));
        let total_blocks = be32(&vh, 44) as u64;
        Ok(Editor {
            block_size: be32(&vh, 40) as u64,
            total_blocks,
            catalog_shape: read_shape(&catalog_file)?,
            extents_shape: read_shape(&extents_file)?,
            attributes_shape: Some(read_shape(&attributes_file)?),
            allocation,
            extents_file,
            catalog_file,
            attributes_file,
            case_sensitive: true,
            next_id: be32(&vh, 64),
            header: vh,
            path: path.to_path_buf(),
            file,
            bitmap: vec![0u8; total_blocks.div_ceil(8) as usize],
            items: HashMap::new(),
            attributes: Vec::new(),
            original: HashMap::new(),
            reader: None,
            children: HashMap::new(),
            written: HashMap::new(),
            removed: Vec::new(),
            read_only: false,
            new_journal: None,
            now: unix_now(),
        })
    }
}

/// A new folder, file or symlink record, owned as a noowners mount records it.
fn new_item(cnid: u32, parent: u32, name: Vec<u16>, kind: u16, perm: u16, date: u32, case_sensitive: bool) -> Item {
    let folder = kind == S_IFDIR;
    let mut b = vec![0u8; if folder { 88 } else { 248 }];
    put16(&mut b, 0, if folder { 1 } else { 2 });
    put16(&mut b, 2, if folder { if case_sensitive { 0x10 } else { 0 } } else { 0x2 }); // folder count / thread exists
    put32(&mut b, 8, cnid);
    for at in [12, 16, 20, 24] {
        put32(&mut b, at, date);
    }
    put32(&mut b, 32, NEW_OWNER.0);
    put32(&mut b, 36, NEW_OWNER.1);
    put16(&mut b, 42, kind | perm);
    if kind == S_IFLNK {
        put32(&mut b, 48, 0x736C_6E6B); // 'slnk'
        put32(&mut b, 52, 0x7268_6170); // 'rhap'
    }
    Item { parent, name, body: b, data: Fork::default(), resource: Fork::default() }
}

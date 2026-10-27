//! Reads a raw HFS+/HFSX volume image (volume header, B-trees, catalog, extents overflow) and edits catalog
//! records in place (owners, modes, flags, folder counts, dates), plus the journal and header bits a restore sets.
//!
//! ```ignore
//! let vol = Volume::open(image, false)?;
//! let r = vol.record("private/var/mobile")?;            // cnid, uid, gid, mode, forks
//! vol.listing("", true)?;                                 // every path: owner, mode, size, sha256, link
//! let rw = Volume::open(image, true)?;
//! rw.set_owner(&["usr/local", "usr/local/bin"], 0, 0, None, None)?;
//! ```
use super::{
    ATTRIBUTES_ID, CATALOG_ID, COMPRESSED, EXTENTS_ID, Extent, Fork, ROOT_ID, be16, be32, be64, catalog_name, le32,
    le64, posix_name, put32,
};
use crate::error::Result;
use crate::posio;
use sha2::{Digest, Sha256};
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    File,
}

/// One folder or file record of the catalog, with where it sits (for in-place edits).
#[derive(Debug, Clone)]
pub struct CatalogRecord {
    pub kind: Kind,
    pub parent: u32,
    pub name: String,
    pub cnid: u32,
    pub uid: u32,
    pub gid: u32,
    pub admin_flags: u8,
    pub owner_flags: u8,
    pub mode: u16,
    pub special: u32,
    pub file_type: u32,
    pub creator: u32,
    pub data: Option<Fork>,
    pub resource: Option<Fork>,
    /// create, content-modified, attribute-modified, accessed, backup: seconds since 1904-01-01.
    pub dates: [u32; 5],
    /// B-tree node number and the byte offset of the record body (past the key) within that node.
    pub node: usize,
    pub body_offset: usize,
}

impl CatalogRecord {
    pub fn is_symlink(&self) -> bool {
        self.kind == Kind::File && self.mode & 0o170000 == 0o120000
    }
    /// An HFS+ file hard link ('hlnk'/'hfs+'): the content is the private directory's iNode<special>.
    pub fn is_hard_link(&self) -> bool {
        self.kind == Kind::File && self.file_type == 0x686C_6E6B && self.creator == 0x6866_732B
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub path: String,
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
    pub flags: u32,
    pub size: u64,
    #[serde(rename = "resourceSize")]
    pub resource_size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

pub struct Volume {
    path: PathBuf,
    file: File,
    writable: bool,
    /// "H+" or "HX".
    pub signature: String,
    pub block_size: u64,
    pub total_blocks: u64,
    pub free_blocks: u64,
    pub file_count: u64,
    pub folder_count: u64,
    pub next_catalog_id: u32,
    extents_fork: Fork,
    catalog_fork: Fork,
    attributes_fork: Fork,
    overflow: OnceCell<HashMap<u64, Vec<(u32, Vec<Extent>)>>>,
    decmpfs: OnceCell<HashMap<u32, Vec<u8>>>,
    catalog: RefCell<Option<std::rc::Rc<Vec<CatalogRecord>>>>,
}

const PRIVATE_DIRS: [&str; 2] = ["\0\0\0\0HFS+ Private Data", ".HFS+ Private Directory Data\r"];

struct BTree {
    fork: Fork,
    file_id: u32,
    node_size: usize,
    first_leaf: u32,
}

impl Volume {
    pub fn open(path: &Path, writable: bool) -> Result<Volume> {
        let file = File::options()
            .read(true)
            .write(writable)
            .open(path)
            .map_err(|e| crate::internal!("open {}: {e}", path.display()))?;
        let mut vh = [0u8; 512];
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if posio::read_exact_at(&file, &mut vh, 1024).is_err() || vh[0] != 0x48 || !(vh[1] == 0x2B || vh[1] == 0x58) {
            return Err(crate::unsupported!("{name}: no HFS+ volume header"));
        }
        Ok(Volume {
            path: path.to_path_buf(),
            signature: if vh[1] == 0x58 { "HX" } else { "H+" }.to_string(),
            file_count: be32(&vh, 32) as u64,
            folder_count: be32(&vh, 36) as u64,
            block_size: be32(&vh, 40) as u64,
            total_blocks: be32(&vh, 44) as u64,
            free_blocks: be32(&vh, 48) as u64,
            next_catalog_id: be32(&vh, 64),
            extents_fork: Fork::parse(&vh, 192),
            catalog_fork: Fork::parse(&vh, 272),
            attributes_fork: Fork::parse(&vh, 352),
            file,
            writable,
            overflow: OnceCell::new(),
            decmpfs: OnceCell::new(),
            catalog: RefCell::new(None),
        })
    }

    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    // ---- Raw fork I/O

    fn overflow(&self) -> Result<&HashMap<u64, Vec<(u32, Vec<Extent>)>>> {
        if let Some(o) = self.overflow.get() {
            return Ok(o);
        }
        let o = self.read_overflow()?;
        Ok(self.overflow.get_or_init(|| o))
    }

    /// The fork's extents in file order: the eight inline ones, then any in the extents overflow file.
    fn extents(&self, fork: &Fork, file_id: u32, resource: bool) -> Result<Vec<Extent>> {
        let inline: u64 = fork.extents.iter().map(|e| e.count as u64).sum();
        if inline >= fork.recorded_blocks as u64 || file_id == EXTENTS_ID {
            return Ok(fork.extents.clone());
        }
        let mut more = self.overflow()?.get(&((file_id as u64) << 8 | if resource { 0xFF } else { 0 })).cloned().unwrap_or_default();
        more.sort_by_key(|(start, _)| *start);
        let mut all = fork.extents.clone();
        all.extend(more.into_iter().flat_map(|(_, e)| e));
        if all.iter().map(|e| e.count as u64).sum::<u64>() < fork.recorded_blocks as u64 {
            return Err(crate::unsupported!("{}: file {file_id} is missing overflow extents", self.name()));
        }
        Ok(all)
    }

    /// Calls `body(disk offset, buffer offset, length)` for each piece of `count` bytes at `offset` in the fork.
    fn io(
        &self,
        fork: &Fork,
        file_id: u32,
        resource: bool,
        offset: u64,
        count: usize,
        mut body: impl FnMut(u64, usize, usize) -> Result<()>,
    ) -> Result<()> {
        let (mut off, mut done, mut base) = (offset, 0usize, 0u64);
        for e in self.extents(fork, file_id, resource)? {
            if done >= count {
                break;
            }
            let len = e.count as u64 * self.block_size;
            if off < base + len {
                let n = (count - done).min((base + len - off) as usize);
                body(e.start as u64 * self.block_size + (off - base), done, n)?;
                done += n;
                off += n as u64;
            }
            base += len;
        }
        if done != count {
            return Err(crate::unsupported!("{}: read past the end of file {file_id}", self.name()));
        }
        Ok(())
    }

    pub(crate) fn read(&self, fork: &Fork, file_id: u32, resource: bool, offset: u64, count: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; count];
        self.io(fork, file_id, resource, offset, count, |disk, at, n| {
            // past the end of a sparse or short image reads as zeros
            let mut got = 0;
            while got < n {
                match posio::read_at(&self.file, &mut out[at + got..at + n], disk + got as u64) {
                    Ok(0) => break,
                    Ok(k) => got += k,
                    Err(e) => return Err(crate::internal!("read {}: {e}", self.path.display())),
                }
            }
            Ok(())
        })?;
        Ok(out)
    }

    fn write(&self, fork: &Fork, file_id: u32, offset: u64, bytes: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(crate::internal!("{} is open read-only", self.name()));
        }
        self.io(fork, file_id, false, offset, bytes.len(), |disk, at, n| {
            posio::write_all_at(&self.file, &bytes[at..at + n], disk)
                .map_err(|e| crate::internal!("write {}: {e}", self.path.display()))
        })
    }

    fn write_catalog(&self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.write(&self.catalog_fork, CATALOG_ID, offset, bytes)
    }

    /// Streams a file's content through `body`: its data fork, following a hard link to its inode, and
    /// decompressed when the file is HFS-compressed (decmpfs zlib, inline or in the resource fork).
    pub fn read_contents(&self, r: &CatalogRecord, mut body: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let r = if r.is_hard_link() { self.inode(r)? } else { r.clone() };
        if r.owner_flags & COMPRESSED != 0 {
            return body(&self.decompressed(&r)?);
        }
        let Some(fork) = &r.data else { return Ok(()) };
        let (mut off, size) = (0u64, fork.logical_size);
        while off < size {
            let n = (1u64 << 22).min(size - off) as usize;
            body(&self.read(fork, r.cnid, false, off, n)?)?;
            off += n as u64;
        }
        Ok(())
    }

    pub fn contents(&self, r: &CatalogRecord) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.read_contents(r, |s| {
            out.extend_from_slice(s);
            Ok(())
        })?;
        Ok(out)
    }

    /// The content size: the data fork's, or a compressed file's decmpfs uncompressed size.
    pub fn size(&self, r: &CatalogRecord) -> Result<u64> {
        let r = if r.is_hard_link() { self.inode(r)? } else { r.clone() };
        if r.owner_flags & COMPRESSED != 0 {
            if let Some(x) = self.decmpfs_header(r.cnid)? {
                return Ok(le64(&x, 8));
            }
        }
        Ok(r.data.map(|d| d.logical_size).unwrap_or(0))
    }

    fn decmpfs_table(&self) -> Result<&HashMap<u32, Vec<u8>>> {
        if let Some(d) = self.decmpfs.get() {
            return Ok(d);
        }
        let d = self.read_decmpfs()?;
        Ok(self.decmpfs.get_or_init(|| d))
    }

    fn decmpfs_header(&self, cnid: u32) -> Result<Option<Vec<u8>>> {
        Ok(self.decmpfs_table()?.get(&cnid).filter(|x| x.len() >= 16 && le32(x, 0) == 0x636D_7066).cloned())
    }

    /// The catalog IDs that own any extended attribute.
    pub fn attribute_owners(&self) -> Result<HashSet<u32>> {
        let mut out = HashSet::new();
        if self.attributes_fork.logical_size == 0 {
            return Ok(out);
        }
        self.leaves(&self.btree(&self.attributes_fork, ATTRIBUTES_ID)?, |_, buf, offs| {
            for i in 0..offs.len() - 1 {
                if offs[i + 1] >= offs[i] + 14 {
                    out.insert(be32(buf, offs[i] + 4));
                }
            }
            Ok(())
        })?;
        Ok(out)
    }

    fn read_decmpfs(&self) -> Result<HashMap<u32, Vec<u8>>> {
        let mut out = HashMap::new();
        if self.attributes_fork.logical_size == 0 {
            return Ok(out);
        }
        let want: Vec<u16> = "com.apple.decmpfs".encode_utf16().collect();
        self.leaves(&self.btree(&self.attributes_fork, ATTRIBUTES_ID)?, |_, buf, offs| {
            for i in 0..offs.len() - 1 {
                let (o, end) = (offs[i], offs[i + 1]);
                if end < o + 14 {
                    continue;
                }
                let key_len = be16(buf, o) as usize;
                let name_len = be16(buf, o + 12) as usize;
                if name_len != want.len() || o + 14 + 2 * name_len > end || (0..name_len).any(|k| be16(buf, o + 14 + 2 * k) != want[k]) {
                    continue;
                }
                let body = o + 2 + key_len;
                if body + 16 > end || be32(buf, body) != 0x10 {
                    continue; // kHFSPlusAttrInlineData only
                }
                let size = be32(buf, body + 12) as usize;
                if body + 16 + size > end {
                    continue;
                }
                out.insert(be32(buf, o + 4), buf[body + 16..body + 16 + size].to_vec());
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// decmpfs types 3 (zlib, inline in the attribute) and 4 (zlib, 64 KiB blocks in the resource fork).
    fn decompressed(&self, r: &CatalogRecord) -> Result<Vec<u8>> {
        let x = self.decmpfs_header(r.cnid)?.ok_or_else(|| crate::unsupported!("{}: compressed, no decmpfs attribute", r.name))?;
        let kind = le32(&x, 4);
        let size = le64(&x, 8) as usize;
        let inflate = |c: &[u8], max: usize| -> Result<Vec<u8>> {
            let Some(&first) = c.first() else { return Ok(Vec::new()) };
            if first & 0x0F == 0x0F {
                return Ok(c[1..].to_vec()); // stored
            }
            if max == 0 {
                return Ok(Vec::new());
            }
            let mut out = vec![0u8; max];
            let mut z = flate2::Decompress::new(true);
            z.decompress(c, &mut out, flate2::FlushDecompress::Finish)
                .map_err(|_| crate::unsupported!("{}: bad decmpfs zlib data", r.name))?;
            let n = z.total_out() as usize;
            if n == 0 {
                return Err(crate::unsupported!("{}: bad decmpfs zlib data", r.name));
            }
            out.truncate(n);
            Ok(out)
        };
        let out = match kind {
            3 => inflate(&x[16..], size)?,
            4 => {
                let rf = r.resource.as_ref().filter(|f| f.logical_size >= 260).ok_or_else(|| crate::unsupported!("{}: no compressed resource fork", r.name))?;
                let fork = self.read(rf, r.cnid, true, 0, rf.logical_size as usize)?;
                let base = be32(&fork, 0) as usize + 4;
                let n = le32(&fork, base) as usize;
                let mut out = Vec::with_capacity(size);
                for k in 0..n {
                    let off = base + le32(&fork, base + 4 + 8 * k) as usize;
                    let len = le32(&fork, base + 8 + 8 * k) as usize;
                    if off + len > fork.len() {
                        return Err(crate::unsupported!("{}: decmpfs block past the fork", r.name));
                    }
                    out.extend(inflate(&fork[off..off + len], (1 << 16).min(size - out.len()))?);
                }
                out
            }
            other => return Err(crate::unsupported!("{}: decmpfs type {other}", r.name)),
        };
        if out.len() != size {
            return Err(crate::unsupported!("{}: decompressed {} of {size} bytes", r.name, out.len()));
        }
        Ok(out)
    }

    // ---- B-trees

    fn btree(&self, fork: &Fork, file_id: u32) -> Result<BTree> {
        let h = self.read(fork, file_id, false, 0, 512)?;
        Ok(BTree { fork: fork.clone(), file_id, node_size: be16(&h, 14 + 18) as usize, first_leaf: be32(&h, 14 + 10) })
    }

    /// Every leaf node, in order: (node number, bytes, record offsets incl. the free-space offset).
    fn leaves(&self, t: &BTree, mut body: impl FnMut(usize, &[u8], &[usize]) -> Result<()>) -> Result<()> {
        let mut n = t.first_leaf;
        let mut seen = HashSet::new();
        while n != 0 {
            if !seen.insert(n) {
                return Err(crate::unsupported!("{}: B-tree leaf loop at node {n}", self.name()));
            }
            let buf = self.read(&t.fork, t.file_id, false, n as u64 * t.node_size as u64, t.node_size)?;
            let count = be16(&buf, 10) as usize;
            let offs: Vec<usize> = (0..=count).map(|k| be16(&buf, t.node_size - 2 * (k + 1)) as usize).collect();
            body(n as usize, &buf, &offs)?;
            n = be32(&buf, 0);
        }
        Ok(())
    }

    fn read_overflow(&self) -> Result<HashMap<u64, Vec<(u32, Vec<Extent>)>>> {
        let mut out: HashMap<u64, Vec<(u32, Vec<Extent>)>> = HashMap::new();
        if self.extents_fork.logical_size == 0 {
            return Ok(out);
        }
        self.leaves(&self.btree(&self.extents_fork, EXTENTS_ID)?, |_, buf, offs| {
            for i in 0..offs.len() - 1 {
                let o = offs[i];
                if offs[i + 1] < o + 12 + 64 {
                    continue;
                }
                let key = (be32(buf, o + 4) as u64) << 8 | buf[o + 2] as u64;
                let exts = (0..8)
                    .map(|k| Extent { start: be32(buf, o + 12 + 8 * k), count: be32(buf, o + 16 + 8 * k) })
                    .filter(|e| e.count > 0)
                    .collect();
                out.entry(key).or_default().push((be32(buf, o + 8), exts));
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// Every folder and file record, in catalog (leaf) order.
    pub fn catalog(&self) -> Result<std::rc::Rc<Vec<CatalogRecord>>> {
        if let Some(c) = self.catalog.borrow().as_ref() {
            return Ok(c.clone());
        }
        let mut out = Vec::new();
        self.leaves(&self.btree(&self.catalog_fork, CATALOG_ID)?, |node, buf, offs| {
            for i in 0..offs.len() - 1 {
                let (start, end) = (offs[i], offs[i + 1]);
                if end < start + 10 {
                    continue;
                }
                let key_len = be16(buf, start) as usize;
                let parent = be32(buf, start + 2);
                let name_len = be16(buf, start + 6) as usize;
                let mut body = start + 2 + key_len;
                if body % 2 == 1 {
                    body += 1;
                }
                if body + 2 > end || start + 8 + 2 * name_len > end {
                    continue;
                }
                let kind = be16(buf, body);
                if !(kind == 1 || kind == 2) || body + 48 > end {
                    continue; // threads skipped
                }
                let units: Vec<u16> = (0..name_len).map(|k| be16(buf, start + 8 + 2 * k)).collect();
                let file = kind == 2 && body + 248 <= end;
                out.push(CatalogRecord {
                    kind: if kind == 1 { Kind::Folder } else { Kind::File },
                    parent,
                    name: String::from_utf16_lossy(&units),
                    cnid: be32(buf, body + 8),
                    uid: be32(buf, body + 32),
                    gid: be32(buf, body + 36),
                    admin_flags: buf[body + 40],
                    owner_flags: buf[body + 41],
                    mode: be16(buf, body + 42),
                    special: be32(buf, body + 44),
                    file_type: if body + 56 <= end { be32(buf, body + 48) } else { 0 },
                    creator: if body + 56 <= end { be32(buf, body + 52) } else { 0 },
                    data: file.then(|| Fork::parse(buf, body + 88)),
                    resource: file.then(|| Fork::parse(buf, body + 168)),
                    dates: [0, 1, 2, 3, 4].map(|k| be32(buf, body + 12 + 4 * k)),
                    node,
                    body_offset: body,
                });
            }
            Ok(())
        })?;
        let rc = std::rc::Rc::new(out);
        *self.catalog.borrow_mut() = Some(rc.clone());
        Ok(rc)
    }

    fn invalidate(&self) {
        *self.catalog.borrow_mut() = None;
    }

    /// (parent CNID, name) -> record.
    pub fn index(&self) -> Result<HashMap<(u32, String), CatalogRecord>> {
        Ok(self.catalog()?.iter().map(|r| ((r.parent, r.name.clone()), r.clone())).collect())
    }

    pub fn record(&self, path: &str) -> Result<CatalogRecord> {
        self.resolve(path, &self.index()?)
    }

    fn resolve(&self, path: &str, idx: &HashMap<(u32, String), CatalogRecord>) -> Result<CatalogRecord> {
        let mut cid = ROOT_ID;
        let mut hit = None;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let r = idx.get(&(cid, catalog_name(part))).ok_or_else(|| {
                crate::internal!("{}: not in the catalog: {path} (stuck at {part})", self.name())
            })?;
            cid = r.cnid;
            hit = Some(r.clone());
        }
        match hit {
            Some(r) => Ok(r),
            None => self.catalog()?.iter().find(|r| r.cnid == ROOT_ID).cloned().ok_or_else(|| crate::unsupported!("no root folder")),
        }
    }

    pub fn inode(&self, link: &CatalogRecord) -> Result<CatalogRecord> {
        let idx = self.index()?;
        idx.get(&(ROOT_ID, PRIVATE_DIRS[0].to_string()))
            .and_then(|dir| idx.get(&(dir.cnid, format!("iNode{}", link.special))))
            .cloned()
            .ok_or_else(|| crate::unsupported!("{}: hard link {} has no iNode{}", self.name(), link.name, link.special))
    }

    // ---- Listings

    /// Volume-relative path of every folder and file record (the root is ""), skipping the hard-link private
    /// directories the kernel hides; sorted by path.
    pub fn paths(&self) -> Result<Vec<(String, CatalogRecord)>> {
        let recs = self.catalog()?;
        let by_id: HashMap<u32, &CatalogRecord> = recs.iter().map(|r| (r.cnid, r)).collect();
        let mut memo: HashMap<u32, Option<String>> = HashMap::from([(ROOT_ID, Some(String::new()))]);
        fn path(id: u32, by_id: &HashMap<u32, &CatalogRecord>, memo: &mut HashMap<u32, Option<String>>) -> Option<String> {
            if let Some(p) = memo.get(&id) {
                return p.clone();
            }
            let p = match by_id.get(&id) {
                Some(r) if r.parent != 1 => {
                    if r.parent == ROOT_ID && PRIVATE_DIRS.contains(&r.name.as_str()) {
                        None
                    } else {
                        path(r.parent, by_id, memo).map(|parent| {
                            if parent.is_empty() { posix_name(&r.name) } else { format!("{parent}/{}", posix_name(&r.name)) }
                        })
                    }
                }
                _ => None,
            };
            memo.insert(id, p.clone());
            p
        }
        let mut out: Vec<(String, CatalogRecord)> =
            recs.iter().filter_map(|r| path(r.cnid, &by_id, &mut memo).map(|p| (p, r.clone()))).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Owner, mode, flags, sizes, content sha256 (files) and symlink targets for every path under `prefix`.
    pub fn listing(&self, prefix: &str, hashes: bool) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        for (p, r) in self.paths()? {
            if !(prefix.is_empty() || p == prefix || p.starts_with(&format!("{prefix}/"))) {
                continue;
            }
            // a hard link's own record reuses the BSD fields for the link chain; the inode has the real ones
            let c = if r.is_hard_link() { self.inode(&r)? } else { r.clone() };
            let mut e = Entry {
                path: p,
                uid: c.uid,
                gid: c.gid,
                mode: c.mode,
                flags: (c.admin_flags as u32) << 16 | c.owner_flags as u32,
                size: self.size(&r)?,
                resource_size: c.resource.as_ref().map(|f| f.logical_size).unwrap_or(0),
                sha256: None,
                link: None,
            };
            if r.is_symlink() {
                e.link = Some(String::from_utf8_lossy(&self.contents(&r)?).into_owned());
            } else if r.kind == Kind::File && hashes {
                let mut h = Sha256::new();
                self.read_contents(&r, |s| {
                    h.update(s);
                    Ok(())
                })?;
                e.sha256 = Some(hex::encode(h.finalize()));
            }
            out.push(e);
        }
        Ok(out)
    }

    /// {path relative to `top`: (uid, gid)} for everything below it.
    pub fn owners(&self, top: &str) -> Result<BTreeMap<String, (u32, u32)>> {
        let prefix = format!("{top}/");
        Ok(self
            .paths()?
            .into_iter()
            .filter(|(p, _)| p.starts_with(&prefix))
            .map(|(p, r)| (p[prefix.len()..].to_string(), (r.uid, r.gid)))
            .collect())
    }

    // ---- Edits

    /// Sets uid/gid (and with `mode`, the permission bits; the file type is kept) on each path's catalog record, in
    /// place. Without `mode` a record already at (uid, gid) is left alone; returns the number of records changed.
    /// Fails before writing anything if a path is missing.
    pub fn set_owner(&self, paths: &[&str], uid: u32, gid: u32, mode: Option<u16>, flags: Option<u32>) -> Result<usize> {
        let idx = self.index()?;
        let mut targets = Vec::new();
        for p in paths {
            if p.split('/').all(str::is_empty) {
                return Err(crate::internal!("refusing to touch the volume root"));
            }
            let r = self.resolve(p, &idx)?;
            targets.push(if r.is_hard_link() { self.inode(&r)? } else { r });
        }
        let t = self.btree(&self.catalog_fork, CATALOG_ID)?;
        let mut changed = 0;
        let mut done = HashSet::new();
        for r in targets {
            if !done.insert(r.cnid) {
                continue;
            }
            let mut patch = [0u8; 8];
            put32(&mut patch, 0, uid);
            put32(&mut patch, 4, gid);
            let at = (r.node * t.node_size + r.body_offset + 32) as u64;
            if let Some(mode) = mode {
                let m = r.mode & 0o170000 | mode & 0o7777;
                self.write_catalog(at, &patch)?;
                self.write_catalog(at + 10, &m.to_be_bytes())?;
                changed += 1;
            } else if (r.uid, r.gid) != (uid, gid) {
                self.write_catalog(at, &patch)?;
                changed += 1;
            }
            if let Some(flags) = flags {
                self.write_catalog(at + 8, &[(flags >> 16) as u8, flags as u8])?;
            }
        }
        self.invalidate();
        Ok(changed)
    }

    /// Sets each folder's HFSX folder count (subfolders; kept only on records with kHFSHasFolderCountMask) in
    /// place. Fails before writing anything if a CNID is not such a folder.
    pub fn set_folder_counts(&self, counts: &HashMap<u32, u32>) -> Result<()> {
        if !self.writable {
            return Err(crate::internal!("{} is open read-only", self.name()));
        }
        let by_id: HashMap<u32, CatalogRecord> =
            self.catalog()?.iter().filter(|r| r.kind == Kind::Folder).map(|r| (r.cnid, r.clone())).collect();
        let t = self.btree(&self.catalog_fork, CATALOG_ID)?;
        let mut targets = Vec::new();
        for (cnid, n) in counts {
            let r = by_id.get(cnid).ok_or_else(|| crate::internal!("{}: no folder {cnid}", self.name()))?;
            let body = (r.node * t.node_size + r.body_offset) as u64;
            if self.read(&self.catalog_fork, CATALOG_ID, false, body + 3, 1)?[0] & 0x10 == 0 {
                return Err(crate::internal!("{}: folder {cnid} keeps no folder count", self.name()));
            }
            targets.push((body + 84, *n));
        }
        for (at, n) in targets {
            self.write_catalog(at, &n.to_be_bytes())?;
        }
        self.invalidate();
        Ok(())
    }

    // ---- Determinism (what a mount leaves behind: dates, a fresh volume's identifier, the journal)

    /// The newest create/modify/attribute/access date in the catalog: on a pristine IPSW volume, its newest file.
    pub fn newest_date(&self) -> Result<u32> {
        Ok(self.catalog()?.iter().map(|r| r.dates[..4].iter().copied().max().unwrap_or(0)).max().unwrap_or(0))
    }

    /// Makes an edited volume the same for the same edits: every catalog date later than `after` becomes `to`
    /// (and the record's Finder "date added" is cleared), the volume headers' dates likewise, with `uuid` the
    /// 8-byte Finder-info volume identifier, and the unused bytes of every B-tree node zeroed. Returns the number
    /// of catalog records dated.
    pub fn normalize(&self, after: u32, to: u32, uuid: Option<[u8; 8]>) -> Result<usize> {
        if !self.writable {
            return Err(crate::internal!("{} is open read-only", self.name()));
        }
        let t = self.btree(&self.catalog_fork, CATALOG_ID)?;
        let mut changed = 0;
        for r in self.catalog()?.iter().filter(|r| r.dates[..4].iter().any(|&d| d > after)) {
            let mut patch = [0u8; 16];
            for (i, &d) in r.dates[..4].iter().enumerate() {
                put32(&mut patch, 4 * i, if d > after { to } else { d });
            }
            let body = (r.node * t.node_size + r.body_offset) as u64;
            self.write_catalog(body + 12, &patch)?;
            self.write_catalog(body + 68, &[0, 0, 0, 0])?; // finderInfo.date_added
            changed += 1;
        }
        self.invalidate();
        for at in self.header_offsets() {
            let mut vh = [0u8; 512];
            posio::read_exact_at(&self.file, &mut vh, at)
                .map_err(|_| crate::internal!("read volume header at {at} of {}", self.name()))?;
            for o in (16..=28).step_by(4) {
                if be32(&vh, o) > after {
                    put32(&mut vh, o, to);
                }
            }
            if let Some(uuid) = uuid {
                vh[104..112].copy_from_slice(&uuid);
            }
            posio::write_all_at(&self.file, &vh, at)
                .map_err(|_| crate::internal!("write volume header at {at} of {}", self.name()))?;
        }
        self.zero_btree_slack()?;
        Ok(changed)
    }

    /// Zeroes each B-tree node's unused bytes (between its last record and its offset table).
    fn zero_btree_slack(&self) -> Result<()> {
        for (fork, id) in [
            (self.catalog_fork.clone(), CATALOG_ID),
            (self.extents_fork.clone(), EXTENTS_ID),
            (self.attributes_fork.clone(), ATTRIBUTES_ID),
        ] {
            if fork.logical_size == 0 {
                continue;
            }
            let t = self.btree(&fork, id)?;
            for node in 0..(fork.logical_size as usize / t.node_size) {
                let buf = self.read(&fork, id, false, (node * t.node_size) as u64, t.node_size)?;
                let records = be16(&buf, 10) as usize;
                let Some(table) = t.node_size.checked_sub(2 * (records + 1)) else { continue };
                if records == 0 || table <= 14 {
                    continue;
                }
                let free = be16(&buf, table) as usize;
                if free < 14 || free >= table || buf[free..table].iter().all(|&b| b == 0) {
                    continue;
                }
                self.write(&fork, id, (node * t.node_size + free) as u64, &vec![0u8; table - free])?;
            }
        }
        Ok(())
    }

    /// The volume header and its alternate(s): at 1024, the volume's end - 1024 and the file's end - 1024;
    /// whichever carry the signature.
    fn header_offsets(&self) -> Vec<u64> {
        let size = self.file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut at: Vec<u64> = [1024, (self.total_blocks * self.block_size).wrapping_sub(1024), size.wrapping_sub(1024)].into();
        at.sort();
        at.dedup();
        at.into_iter()
            .filter(|&o| {
                let mut sig = [0u8; 2];
                o >= 1024
                    && o < size
                    && posio::read_exact_at(&self.file, &mut sig, o).is_ok()
                    && sig[0] == 0x48
                    && (sig[1] == 0x2B || sig[1] == 0x58)
            })
            .collect()
    }

    fn header(&self) -> Result<[u8; 512]> {
        let mut vh = [0u8; 512];
        posio::read_exact_at(&self.file, &mut vh, 1024).map_err(|_| crate::internal!("read the header of {}", self.name()))?;
        Ok(vh)
    }

    /// The journal's byte offset and size in the volume when it is journaled there; `needs_init` while the
    /// kernel has yet to write its header (a fresh volume).
    pub fn journal(&self) -> Result<Option<(u64, u64, bool)>> {
        let vh = self.header()?;
        // kHFSVolumeJournaledBit, journalInfoBlock
        if be32(&vh, 4) & (1 << 13) == 0 || be32(&vh, 12) == 0 {
            return Ok(None);
        }
        let mut jib = [0u8; 52]; // flags, device_signature[8], offset, size
        if posio::read_exact_at(&self.file, &mut jib, be32(&vh, 12) as u64 * self.block_size).is_err() || be32(&jib, 0) & 3 != 1 {
            return Ok(None);
        }
        let (offset, size) = (be64(&jib, 36), be64(&jib, 44));
        Ok((size > 0).then_some((offset, size, be32(&jib, 0) & 4 != 0)))
    }

    /// The journal info block and the journal (offset, bytes) while the journal is still to be initialized or is
    /// empty (start == end); None otherwise. Put back after an edit, they leave the journal as it was.
    pub fn journal_snapshot(&self) -> Result<Option<Vec<(u64, Vec<u8>)>>> {
        let Some((offset, size, needs_init)) = self.journal()? else { return Ok(None) };
        let vh = self.header()?;
        if !needs_init {
            let mut hdr = [0u8; 32]; // magic, endian, start, end, ...
            if posio::read_exact_at(&self.file, &mut hdr, offset).is_err() || hdr[8..16] != hdr[16..24] {
                return Ok(None);
            }
        }
        let mut out = Vec::new();
        for (at, n) in [(be32(&vh, 12) as u64 * self.block_size, self.block_size), (offset, size)] {
            let mut bytes = vec![0u8; n as usize];
            posio::read_exact_at(&self.file, &mut bytes, at).map_err(|_| crate::internal!("read the journal of {}", self.name()))?;
            out.push((at, bytes));
        }
        Ok(Some(out))
    }

    /// Hands the journal to the device: zeroes it and sets the info block's kJIJournalNeedInitMask, as newfs_hfs
    /// leaves it, so the device's kernel writes the header for its own block size at the first mount.
    pub fn leave_journal_to_device(&self) -> Result<()> {
        if !self.writable {
            return Err(crate::internal!("{} is open read-only", self.name()));
        }
        let (offset, size, _) = self.journal()?.ok_or_else(|| crate::internal!("{} has no journal in the volume", self.name()))?;
        let at = be32(&self.header()?, 12) as u64 * self.block_size;
        let mut jib = [0u8; 4];
        posio::read_exact_at(&self.file, &mut jib, at).map_err(|_| crate::internal!("read the journal info block of {}", self.name()))?;
        let flags = be32(&jib, 0) | 4;
        self.restore(&[(offset, vec![0u8; size as usize]), (at, flags.to_be_bytes().to_vec())])
    }

    /// Sets kHFSContentProtectionBit in the volume header's attributes, as a restore formats iOS's data volume.
    pub fn set_content_protection(&self) -> Result<()> {
        if !self.writable {
            return Err(crate::internal!("{} is open read-only", self.name()));
        }
        for at in self.header_offsets() {
            let mut a = [0u8; 4];
            posio::read_exact_at(&self.file, &mut a, at + 4).map_err(|_| crate::internal!("read volume header at {at} of {}", self.name()))?;
            let v = be32(&a, 0) | 0x4000_0000;
            posio::write_all_at(&self.file, &v.to_be_bytes(), at + 4)
                .map_err(|_| crate::internal!("write volume header at {at} of {}", self.name()))?;
        }
        Ok(())
    }

    pub fn restore(&self, pieces: &[(u64, Vec<u8>)]) -> Result<()> {
        if !self.writable {
            return Err(crate::internal!("{} is open read-only", self.name()));
        }
        for (offset, bytes) in pieces {
            posio::write_all_at(&self.file, bytes, *offset).map_err(|_| crate::internal!("write {} at {offset}", self.name()))?;
        }
        Ok(())
    }
}

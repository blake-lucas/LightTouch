//! A device's NAND as one qcow2 disk (qemu-ios hw/arm/nand-image.h, "NAND container"): every chip in nand.qcow2,
//! chip `c`'s byte `at` (its chip file's offset) at `c * chip_stride + (at / block_bytes) * block_stride +
//! at % block_bytes`. Erase blocks start on a 64 KiB cluster, so the engine's erase frees whole clusters of the
//! device's overlay. This is the system image: written once at the end of preparation (or import) from the chip
//! files' data, never by the guest, which writes into its own overlay (state/nand/nand.qcow2, the engine's).
//!
//! geometry.json keeps every key the engine checks and gains `block_stride`, `chip_stride` and
//! `"container": "qcow2"`. Portable/src/NandDisk.hpp is the same writer for the core's imports; for the same input
//! both write the same bytes.
//!
//! ```ignore
//! let layout = nand_disk::pack(&nand, &|bytes| progress(bytes))?;   // chip files -> nand.qcow2, files removed
//! ```
use crate::error::Result;
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::path::Path;

pub const FILE: &str = "nand.qcow2";
pub const CLUSTER: u64 = 1 << 16;
const CLUSTER_BITS: u32 = 16;
const L2_ENTRIES: u64 = CLUSTER / 8;
/// 16-bit refcounts (refcount_order 4): a refcount block covers this many clusters.
const REFCOUNTS_PER_BLOCK: u64 = CLUSTER / 2;
/// An L1/L2 entry whose cluster has refcount 1.
const COPIED: u64 = 1 << 63;
/// What `put` checks for zeros in, and skips: a host filesystem block.
const SPARSE: usize = 4096;

/// Where a store's chip files sit in the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub chips: u64,
    pub stride: u64,
    pub pages_per_block: u64,
    pub blocks_per_chip: u64,
    pub block_stride: u64,
    pub chip_stride: u64,
}

impl Layout {
    pub fn new(chips: u64, stride: u64, pages_per_block: u64, blocks_per_chip: u64) -> Layout {
        let block_stride = (pages_per_block * stride).div_ceil(CLUSTER) * CLUSTER;
        Layout {
            chips,
            stride,
            pages_per_block,
            blocks_per_chip,
            block_stride,
            chip_stride: blocks_per_chip * block_stride,
        }
    }

    /// The layout and chip file names (in chip order) of the store geometry.json describes: an A4 board's
    /// (`buses` x `ce_per_bus`, bus<b>-ce<c>.pages) or a page-directory board's (`chip_selects`, cs<N>.pages).
    pub fn of_geometry(g: &serde_json::Value) -> Option<(Layout, Vec<String>)> {
        let int = |k: &str| g.get(k).and_then(serde_json::Value::as_u64);
        let stride = int("page_bytes")? + int("spare_bytes")?;
        let (ppb, blocks) = (int("pages_per_block")?, int("blocks_per_ce")?);
        let names: Vec<String> = match (int("buses"), int("ce_per_bus"), int("chip_selects")) {
            (Some(buses), Some(per_bus), _) => {
                (0..buses * per_bus).map(|k| format!("bus{}-ce{}.pages", k / per_bus, k % per_bus)).collect()
            }
            (_, _, Some(cs)) => (0..cs).map(|k| format!("cs{k}.pages")).collect(),
            _ => return None,
        };
        (stride > 0 && ppb > 0 && blocks > 0 && !names.is_empty())
            .then(|| (Layout::new(names.len() as u64, stride, ppb, blocks), names))
    }

    pub fn block_bytes(&self) -> u64 {
        self.pages_per_block * self.stride
    }
    pub fn chip_bytes(&self) -> u64 {
        self.blocks_per_chip * self.block_bytes()
    }
    /// The disk's size.
    pub fn size(&self) -> u64 {
        self.chips * self.chip_stride
    }
    /// Where byte `at` of chip `chip`'s file sits in the disk.
    pub fn place(&self, chip: u64, at: u64) -> u64 {
        chip * self.chip_stride + at / self.block_bytes() * self.block_stride + at % self.block_bytes()
    }

    /// `geometry` (a store's geometry.json text) with the container's keys added before its closing brace.
    pub fn geometry(&self, geometry: &str) -> String {
        let body = geometry.trim_end().strip_suffix('}').unwrap_or(geometry).trim_end();
        format!(
            "{body},\n \"block_stride\": {},\n \"chip_stride\": {},\n \"container\": \"qcow2\"\n}}",
            self.block_stride, self.chip_stride
        )
    }
}

/// A qcow2 image (version 3, 64 KiB clusters, 16-bit refcounts, no backing file) written once: data clusters in
/// the order they are first given, then the L2 tables, L1 table, refcount table and refcount blocks, then the
/// header. Only clusters holding a nonzero byte are stored, and within them only nonzero 4 KiB blocks are written,
/// so the rest stays a hole where the filesystem has them.
pub struct Writer {
    file: File,
    size: u64,
    /// guest cluster -> host offset
    map: BTreeMap<u64, u64>,
    next: u64,
}

fn zero(b: &[u8]) -> bool {
    let (head, words, tail) = unsafe { b.align_to::<u64>() };
    head.iter().all(|&x| x == 0) && words.iter().all(|&x| x == 0) && tail.iter().all(|&x| x == 0)
}

impl Writer {
    /// A new image of `size` guest bytes at `path` (which must not exist).
    pub fn create(path: &Path, size: u64) -> io::Result<Writer> {
        let file = File::options().read(true).write(true).create_new(true).open(path)?;
        Ok(Writer { file, size, map: BTreeMap::new(), next: CLUSTER })
    }

    /// `data` at guest offset `at`. Each guest byte is given at most once (zeros need not be given at all).
    pub fn put(&mut self, mut at: u64, mut data: &[u8]) -> io::Result<()> {
        if at.checked_add(data.len() as u64).is_none_or(|end| end > self.size) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "write past the end of the NAND disk"));
        }
        while !data.is_empty() {
            let within = at % CLUSTER;
            let n = data.len().min((CLUSTER - within) as usize);
            let (piece, rest) = data.split_at(n);
            if !zero(piece) {
                let host = match self.map.get(&(at / CLUSTER)) {
                    Some(&host) => host,
                    None => {
                        let host = self.next;
                        self.next += CLUSTER;
                        self.map.insert(at / CLUSTER, host);
                        host
                    }
                };
                self.write_sparse(host + within, piece)?;
            }
            at += n as u64;
            data = rest;
        }
        Ok(())
    }

    /// `piece` at host offset `at`, leaving out its all-zero host blocks.
    fn write_sparse(&self, at: u64, piece: &[u8]) -> io::Result<()> {
        let mut i = 0;
        while i < piece.len() {
            let end = |i: usize| (i + SPARSE - ((at as usize + i) % SPARSE)).min(piece.len());
            if zero(&piece[i..end(i)]) {
                i = end(i);
                continue;
            }
            let start = i;
            while i < piece.len() && !zero(&piece[i..end(i)]) {
                i = end(i);
            }
            crate::posio::write_all_at(&self.file, &piece[start..i], at + start as u64)?;
        }
        Ok(())
    }

    /// Writes the metadata and header and syncs the file.
    pub fn finish(self) -> io::Result<File> {
        let Writer { file, size, map, next } = self;
        let l1_size = size.div_ceil(CLUSTER * L2_ENTRIES);
        let tables: Vec<u64> = {
            let mut t: Vec<u64> = map.keys().map(|c| c / L2_ENTRIES).collect();
            t.dedup();
            t
        };
        let l1_clusters = (l1_size * 8).div_ceil(CLUSTER).max(1);
        let fixed = next / CLUSTER + tables.len() as u64 + l1_clusters;
        let (mut table_clusters, mut blocks) = (1, 1);
        loop {
            let total = fixed + table_clusters + blocks;
            let b = total.div_ceil(REFCOUNTS_PER_BLOCK);
            let t = (b * 8).div_ceil(CLUSTER);
            if (b, t) == (blocks, table_clusters) {
                break;
            }
            (blocks, table_clusters) = (b, t);
        }
        let total = fixed + table_clusters + blocks;
        let mut at = next;
        // L2 tables, one per L1 entry with data.
        let mut l1 = vec![0u8; (l1_clusters * CLUSTER) as usize];
        let mut entries = map.iter().peekable();
        for &t in &tables {
            let mut l2 = vec![0u8; CLUSTER as usize];
            while let Some((&guest, &host)) = entries.next_if(|(g, _)| **g / L2_ENTRIES == t) {
                let i = (guest % L2_ENTRIES) as usize * 8;
                l2[i..i + 8].copy_from_slice(&(host | COPIED).to_be_bytes());
            }
            crate::posio::write_all_at(&file, &l2, at)?;
            l1[t as usize * 8..t as usize * 8 + 8].copy_from_slice(&(at | COPIED).to_be_bytes());
            at += CLUSTER;
        }
        let l1_at = at;
        crate::posio::write_all_at(&file, &l1, l1_at)?;
        at += l1_clusters * CLUSTER;
        let table_at = at;
        let mut table = vec![0u8; (table_clusters * CLUSTER) as usize];
        for b in 0..blocks {
            let block_at = table_at + (table_clusters + b) * CLUSTER;
            table[b as usize * 8..b as usize * 8 + 8].copy_from_slice(&block_at.to_be_bytes());
        }
        crate::posio::write_all_at(&file, &table, table_at)?;
        at += table_clusters * CLUSTER;
        for b in 0..blocks {
            let mut block = vec![0u8; CLUSTER as usize];
            let first = b * REFCOUNTS_PER_BLOCK;
            for c in first..total.min(first + REFCOUNTS_PER_BLOCK) {
                let i = ((c - first) * 2) as usize;
                block[i..i + 2].copy_from_slice(&1u16.to_be_bytes());
            }
            crate::posio::write_all_at(&file, &block, at)?;
            at += CLUSTER;
        }
        debug_assert_eq!(at, total * CLUSTER);
        crate::posio::write_all_at(&file, &header(size, l1_size, l1_at, table_at, table_clusters), 0)?;
        file.sync_all()?;
        Ok(file)
    }
}

/// The version 3 header (104 bytes) and the end of its (empty) extension list.
fn header(size: u64, l1_size: u64, l1_at: u64, table_at: u64, table_clusters: u64) -> Vec<u8> {
    let mut h = Vec::with_capacity(112);
    h.extend_from_slice(&0x5146_49fbu32.to_be_bytes()); // "QFI\xfb"
    h.extend_from_slice(&3u32.to_be_bytes()); // version
    h.extend_from_slice(&0u64.to_be_bytes()); // backing_file_offset
    h.extend_from_slice(&0u32.to_be_bytes()); // backing_file_size
    h.extend_from_slice(&CLUSTER_BITS.to_be_bytes());
    h.extend_from_slice(&size.to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes()); // crypt_method
    h.extend_from_slice(&(l1_size as u32).to_be_bytes());
    h.extend_from_slice(&l1_at.to_be_bytes());
    h.extend_from_slice(&table_at.to_be_bytes());
    h.extend_from_slice(&(table_clusters as u32).to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes()); // nb_snapshots
    h.extend_from_slice(&0u64.to_be_bytes()); // snapshots_offset
    h.extend_from_slice(&0u64.to_be_bytes()); // incompatible_features
    h.extend_from_slice(&0u64.to_be_bytes()); // compatible_features
    h.extend_from_slice(&0u64.to_be_bytes()); // autoclear_features
    h.extend_from_slice(&4u32.to_be_bytes()); // refcount_order: 16-bit
    h.extend_from_slice(&104u32.to_be_bytes()); // header_length
    h.extend_from_slice(&[0u8; 8]); // header extensions: the end marker
    h
}

/// The store in `nand`: geometry.json's text, its layout and its chip files.
fn store(nand: &Path) -> Result<(String, Layout, Vec<String>)> {
    let text = std::fs::read_to_string(nand.join("geometry.json"))?;
    let g: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| crate::internal!("{}: geometry.json: {e}", nand.display()))?;
    let (layout, names) = Layout::of_geometry(&g)
        .ok_or_else(|| crate::internal!("{}: geometry.json names no chip files", nand.display()))?;
    Ok((text, layout, names))
}

/// The bytes `pack` will read: the chip files' data extents.
pub fn data_bytes(nand: &Path) -> Result<u64> {
    let (_, _, names) = store(nand)?;
    let mut total = 0;
    for name in names {
        let p = nand.join(name);
        let file = File::open(&p).map_err(|e| crate::internal!("{}: {e}", p.display()))?;
        let extents =
            crate::k48nand_image::data_extents(&file).map_err(|e| crate::internal!("{}: {e}", p.display()))?;
        total += extents.iter().map(|(start, end)| end - start).sum::<u64>();
    }
    Ok(total)
}

/// Packs the chip-file store in `nand` into nand.qcow2: each chip's data extents are read once, its file removed
/// once read; geometry.json gains the container keys. `count` hears the bytes read.
pub fn pack(nand: &Path, count: &dyn Fn(u64)) -> Result<Layout> {
    let (text, layout, names) = store(nand)?;
    let path = nand.join(FILE);
    let mut writer = Writer::create(&path, layout.size()).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    let mut buffer = vec![0u8; 1 << 20];
    for (chip, name) in names.iter().enumerate() {
        let p = nand.join(name);
        let file = File::open(&p).map_err(|e| crate::internal!("{}: {e}", p.display()))?;
        let len = file.metadata()?.len();
        if len != layout.chip_bytes() {
            return Err(crate::internal!(
                "{}: {len} bytes, the geometry's chip is {}",
                p.display(),
                layout.chip_bytes()
            ));
        }
        for (start, end) in
            crate::k48nand_image::data_extents(&file).map_err(|e| crate::internal!("{}: {e}", p.display()))?
        {
            let mut at = start;
            while at < end {
                // A read never crosses an erase block: the next one sits at its own place.
                let left = layout.block_bytes() - at % layout.block_bytes();
                let n = (end - at).min(left).min(buffer.len() as u64) as usize;
                crate::posio::read_exact_at(&file, &mut buffer[..n], at)
                    .map_err(|e| crate::internal!("{}: {e}", p.display()))?;
                writer
                    .put(layout.place(chip as u64, at), &buffer[..n])
                    .map_err(|e| crate::internal!("{}: {e}", path.display()))?;
                count(n as u64);
                at += n as u64;
            }
        }
        drop(file);
        std::fs::remove_file(&p).map_err(|e| crate::internal!("{}: {e}", p.display()))?;
    }
    writer.finish().map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    std::fs::write(nand.join("geometry.json"), layout.geometry(&text))?;
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lt-nand-disk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn be64(b: &[u8], at: usize) -> u64 {
        u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
    }

    /// Reads guest bytes back by walking the image's own tables, as a qcow2 reader does.
    fn read(image: &[u8], at: u64, n: usize) -> Vec<u8> {
        let l1_at = be64(image, 40) as usize;
        (at..at + n as u64)
            .map(|g| {
                let l1 = be64(image, l1_at + (g / CLUSTER / L2_ENTRIES) as usize * 8) & !COPIED;
                if l1 == 0 {
                    return 0;
                }
                let l2 = be64(image, l1 as usize + (g / CLUSTER % L2_ENTRIES) as usize * 8) & !COPIED;
                if l2 == 0 { 0 } else { image[l2 as usize + (g % CLUSTER) as usize] }
            })
            .collect()
    }

    #[test]
    fn layouts_align_erase_blocks() {
        // iPod touch 4G: 8448-byte records, 128 a block (33 x 32 KiB): 17 clusters a block.
        let n81 = Layout::new(8, 8448, 128, 4152);
        assert_eq!((n81.block_stride, n81.chip_stride), (17 * CLUSTER, 4152 * 17 * CLUSTER));
        assert_eq!(n81.place(1, n81.block_bytes() + 5), n81.chip_stride + n81.block_stride + 5);
        // iPod touch 2G: 4160-byte records: 9 clusters a block.
        let n72 = Layout::new(4, 4160, 128, 4096);
        assert_eq!(n72.block_stride, 9 * CLUSTER);
        assert_eq!(
            n72.geometry("{\n \"a\": 1\n}"),
            "{\n \"a\": 1,\n \"block_stride\": 589824,\n \"chip_stride\": 2415919104,\n \"container\": \"qcow2\"\n}"
        );
    }

    #[test]
    fn writer_stores_only_data_in_any_order() {
        let dir = temp("writer");
        let path = dir.join(FILE);
        let size = 3 * CLUSTER * L2_ENTRIES + 12345 * 4096; // four L1 entries, the last partial
        let mut w = Writer::create(&path, size).unwrap();
        let a: Vec<u8> = (0..10000u32).map(|i| (i % 251) as u8 + 1).collect();
        w.put(2 * CLUSTER * L2_ENTRIES + 100, &a).unwrap(); // a third L2 table
        w.put(5, &a[..300]).unwrap();
        w.put(CLUSTER - 7, &a[..20]).unwrap(); // across a cluster boundary
        w.put(9 * CLUSTER, &[0u8; 5000]).unwrap(); // zeros: nothing stored
        assert!(w.put(size - 1, &[1, 2]).is_err());
        w.finish().unwrap();
        let image = std::fs::read(&path).unwrap();
        assert_eq!(&image[..4], b"QFI\xfb");
        // header, 3 data clusters, 2 L2 tables, L1, refcount table, 1 refcount block
        assert_eq!(image.len() as u64, 9 * CLUSTER);
        assert_eq!(read(&image, 2 * CLUSTER * L2_ENTRIES + 100, a.len()), a);
        assert_eq!(read(&image, 5, 300), &a[..300]);
        assert_eq!(read(&image, CLUSTER - 7, 20), &a[..20]);
        assert!(read(&image, 9 * CLUSTER, 64).iter().all(|&b| b == 0));
        // every cluster counted once
        let table_at = be64(&image, 48) as usize;
        let block = be64(&image, table_at) as usize;
        let counts: Vec<u16> =
            (0..10).map(|c| u16::from_be_bytes([image[block + 2 * c], image[block + 2 * c + 1]])).collect();
        assert_eq!(counts, [1, 1, 1, 1, 1, 1, 1, 1, 1, 0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bytes Portable/tests/NandDisk.cpp expects of its writer for the same puts.
    #[test]
    fn writer_matches_the_core() {
        use sha2::{Digest, Sha256};
        let dir = temp("golden");
        let path = dir.join(FILE);
        let mut w = Writer::create(&path, 2 * CLUSTER * L2_ENTRIES).unwrap();
        let a: Vec<u8> = (0..70000u32).map(|i| (i * 7 % 256) as u8).collect();
        w.put(CLUSTER * L2_ENTRIES + 4000, &a).unwrap();
        w.put(3, b"LightTouch").unwrap();
        w.finish().unwrap();
        let digest = hex::encode(Sha256::digest(std::fs::read(&path).unwrap()));
        assert_eq!(digest, GOLDEN);
        let _ = std::fs::remove_dir_all(&dir);
    }
    const GOLDEN: &str = "49fd8076235f5fe2c6c40391b2b7d61c9bdab4f8181d4a93987d9b0422ef1364";

    #[test]
    fn pack_reads_a_store() {
        let dir = temp("pack");
        let geometry = "{\n  \"chip_selects\" : 2,\n  \"page_bytes\" : 4096,\n  \"spare_bytes\" : 64,\n  \"pages_per_block\" : 128,\n  \"blocks_per_ce\" : 4\n}";
        std::fs::write(dir.join("geometry.json"), geometry).unwrap();
        let layout = Layout::new(2, 4160, 128, 4);
        let record: Vec<u8> = (0..4160u32).map(|i| (i % 200) as u8 + 3).collect();
        for chip in 0..2u64 {
            let f = File::create(dir.join(format!("cs{chip}.pages"))).unwrap();
            f.set_len(layout.chip_bytes()).unwrap();
            // the last record of block 1, and the first of block 2
            crate::posio::write_all_at(&f, &record, (2 * 128 - 1) * 4160).unwrap();
            crate::posio::write_all_at(&f, &record, 2 * 128 * 4160).unwrap();
        }
        let packed = pack(&dir, &|_| {}).unwrap();
        assert_eq!(packed, layout);
        assert!(!dir.join("cs0.pages").exists() && !dir.join("cs1.pages").exists());
        let image = std::fs::read(dir.join(FILE)).unwrap();
        for chip in 0..2 {
            assert_eq!(read(&image, layout.place(chip, (2 * 128 - 1) * 4160), 4160), record);
            assert_eq!(read(&image, chip * layout.chip_stride + 2 * layout.block_stride, 4160), record);
            assert!(read(&image, chip * layout.chip_stride + 2 * layout.block_stride - 64, 64).iter().all(|&b| b == 0));
        }
        let g: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("geometry.json")).unwrap()).unwrap();
        assert_eq!(g["container"], "qcow2");
        assert_eq!(g["block_stride"], layout.block_stride);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

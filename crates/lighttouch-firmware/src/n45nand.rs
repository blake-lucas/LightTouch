//! The iPod touch 1G's and the original iPhone's NAND (eight or four banks of 2048 data + 64 spare-byte pages) that
//! qemu-ios s5l8900_fmc.c serves, written as an image store (page_nand, Image::s5l8900): the legacy VFL/FTL layout of
//! devos50's qemu-ios-generate-nand (FIL id, DEVICEINFOBBT, VFL context at block 35, the FTL from physical block 201
//! on, one HFS+ partition behind an MBR + GPT), with what that tool left for later:
//!
//! - Real spares. Every page carries the 12 bytes the 1.x FTL itself writes: data [lpn u32][write age u32 = 0][ff]
//!   [0x40][ff][ff], context [age u32][index u16][ff ff][ff][type][ff][ff] (0x43 index/meta, 0x46 map). A page never
//!   programmed reads erased (all ones), so every page of the volume's blocks is written, zeros included.
//! - The FTL's own blocks kept out of the data: context in virtual blocks 0-2, free pool 3-22, filesystem from 23.
//! - A VFL context as VFL_Format stores it (vfl_context): age, next context page, reserved pool and its remap of the
//!   BBT block, both checksums.
//! - The FIL id (NAND signature) the build's driver wants, read off its iBoot (fil_id): C002 for 3A101a-3B48b, C003
//!   for 4A93-4B1, C000 for the iPhone's 1.0.
//!
//! ```ignore
//! let fil = n45nand::fil_id(&iboot)?;
//! let (volume, metadata) = n45nand::write(&img, &nand_dir, fil, banks, bbt_map)?;   // 8 banks (n45), 4 + bbt_map (m68)
//! n45nand::location_lpn(lpn, banks)                                                  // (bank, page) of a logical page
//! ```
use crate::error::Result;
use crate::n72nand::{HFS_TYPE, crc, put, vfl_checksum};
use crate::page_nand::{Image, Store};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const PAGE: usize = 2048;
pub const SPARE: usize = 64;
/// The N45's eight chip enables; the M68 populates four (a superblock spans one block of each bank).
pub const BANKS: usize = 8;
pub const PAGES_PER_BLOCK: usize = 128;
pub const BLOCKS_PER_BANK: usize = 4096;
/// The first physical block (all banks) the FTL's virtual block 0 lives in.
pub const FTL_START: usize = 201;
pub const CXT_BLOCKS: usize = 3;
const FREE_BLOCKS: usize = 20;
pub const DATA_START: usize = 23;
const VFL_CXT_BLOCK: usize = 35;
const VFL_CXT_COPIES: usize = 8;
const LOG_CXTS: usize = 18;
/// The map (logical -> virtual block, u16) is one bank's 4096 blocks: four 2 KiB 0x46 pages (indices 0-3; an index
/// past 3 makes _FTLRestore write past the kernel's 8 KiB table).
pub const MAP_TABLES: usize = BLOCKS_PER_BANK * 2 / PAGE;
/// The reserved (bad-block replacement) pool: after the four VFL info blocks, up to the FTL.
const RESERVED_START: usize = VFL_CXT_BLOCK + 4;
const MAX_RESERVED: usize = 820;
/// Logical blocks mapped; the rest are 0xffff.
pub const MAPPED_LBNS: usize = 3800;
pub const FIRST_LBA: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Page {
    pub bank: usize,
    pub page: usize,
}

pub fn superblock(banks: usize) -> usize {
    PAGES_PER_BLOCK * banks
}

/// A virtual page (FTL numbering, virtual block 0 = physical block 201) on its bank and physical page.
pub fn location_vpn(vpn: usize, banks: usize) -> Page {
    let sb = superblock(banks);
    let v = vpn + FTL_START * sb;
    Page { bank: v % banks, page: v / sb * PAGES_PER_BLOCK + (v / banks) % PAGES_PER_BLOCK }
}

pub fn location_lpn(lpn: usize, banks: usize) -> Page {
    location_vpn(DATA_START * superblock(banks) + lpn, banks)
}

pub fn data_spare(lpn: usize) -> [u8; SPARE] {
    let mut s = [0u8; SPARE];
    put(&mut s, 0, lpn as u64, 4);
    s[8] = 0xFF;
    s[9] = 0x40;
    s[10] = 0xFF;
    s[11] = 0xFF;
    s
}

pub fn cxt_spare(kind: u8, index: usize, age: u32) -> [u8; SPARE] {
    let mut s = [0u8; SPARE];
    put(&mut s, 0, age as u64, 4);
    put(&mut s, 4, index as u64, 2);
    s[6] = 0xFF;
    s[7] = 0xFF;
    s[8] = 0xFF;
    s[9] = kind;
    s[10] = 0xFF;
    s[11] = 0xFF;
    s
}

/// VFLMeta (Whimory VFLTypes.h: VFLCxt, then the version and two checksums) as VFL_Format leaves bank `bank`: the FTL
/// context blocks, the four info blocks (35-38, the context in the first, eight copies on pages 0-7, so the next
/// store goes to page 8), the reserved pool up to the FTL (39-200), and the BBT's own block 4095 remapped to the
/// pool's first block so the FTL, whose virtual block 3894 it is, finds that erased block and never the table.
fn vfl_context(bank: usize) -> Vec<u8> {
    let mut d = vec![0u8; PAGE];
    put(&mut d, 0, bank as u64, 4); // dwGlobalCxtAge
    for i in 0..CXT_BLOCKS {
        put(&mut d, 4 + 2 * i, i as u64, 2); // aFTLCxtVbn
    }
    put(&mut d, 0xC, 0xFFFF_FFFF, 4); // dwCxtAge: 0, less the one store
    put(&mut d, 0x12, VFL_CXT_COPIES as u64, 2); // wNextCxtPOffset
    put(&mut d, 0x14, 1, 2); // wNumOfInitBadBlk: the BBT block
    put(&mut d, 0x1A, 1, 2); // wBadMapTableMaxIdx
    put(&mut d, 0x1C, RESERVED_START as u64, 2); // wReservedSecStart
    put(&mut d, 0x1E, (FTL_START - RESERVED_START) as u64, 2); // wReservedSecSize
    put(&mut d, 0x20, (BLOCKS_PER_BANK - 1) as u64, 2); // aBadMapTable[0]: 4095 -> 39
    d[0x688..0x7A2].fill(0xFF); // aBadMark, one bit per 8 blocks
    d[0x688 + (BLOCKS_PER_BANK - 1) / 64] &= !(1u8 << (7 - ((BLOCKS_PER_BANK - 1) / 8) % 8));
    for i in 0..4 {
        put(&mut d, 0x7A2 + 2 * i, (VFL_CXT_BLOCK + i) as u64, 2); // awInfoBlk
    }
    put(&mut d, 0x7AA, MAX_RESERVED as u64, 2); // wBadMapTableScrubIdx
    vfl_checksum(&mut d);
    d
}

/// VFLSpare as the VFL writes its context pages: dwCxtAge, dwReserved, status mark 0 (valid), type 0x80.
const VFL_SPARE: [u8; SPARE] = {
    let mut s = [0xFFu8; SPARE];
    s[8] = 0;
    s[9] = 0x80;
    s
};

/// FTLMeta (ftl.h, FTLCxt2): a flushed, valid context with an empty log and the free pool 3-22.
pub fn ftl_meta(banks: usize) -> Vec<u8> {
    let mut d = vec![0u8; PAGE];
    put(&mut d, 0, 0xFFFF_FFFE, 4);
    put(&mut d, 4, 1, 4); // dwAge, dwWriteAge (data pages are 0)
    put(&mut d, 8, FREE_BLOCKS as u64, 2); // wNumOfFreeVb
    for i in 0..FREE_BLOCKS {
        put(&mut d, 14 + 2 * i, (CXT_BLOCKS + i) as u64, 2);
    }
    for i in 0..MAP_TABLES {
        put(&mut d, 56 + 4 * i, (1 + i) as u64, 4); // adwMapTablePtrs: pages 1-4 of block 0
    }
    for i in 0..LOG_CXTS {
        put(&mut d, 420 + 20 * i + 4, 0xFFFF, 2); // aLOGCxtTable[].wVbn: no log
    }
    for i in 0..CXT_BLOCKS {
        put(&mut d, 786 + 2 * i, i as u64, 2); // awMapCxtVbn
    }
    put(&mut d, 792, (superblock(banks) - 1) as u64, 4); // dwCurrMapCxtPage: this page
    put(&mut d, 796, 1, 4); // boolFlashCxtIsValid
    put(&mut d, 2040, 0x4656_0000, 4);
    put(&mut d, 2044, !0x4656_0000u32 as u64, 4);
    d
}

const DISK_GUID: [u8; 16] =
    [0x6a, 0x2e, 0x5c, 0x10, 0x9f, 0x3b, 0x4e, 0x41, 0x8d, 0x27, 0x1c, 0x44, 0xa5, 0x10, 0xe7, 0x01];
const PART_GUID: [u8; 16] =
    [0x3c, 0x1f, 0x8e, 0x52, 0x06, 0x7d, 0x4b, 0x0a, 0x9b, 0x61, 0x2f, 0x0e, 0x88, 0x14, 0xc3, 0x5d];

/// LBA 0-2: an MBR whose one partition (type 0xEE) covers the filesystem (iBoot mounts it), the GPT header and its
/// one Apple_HFS entry ("System").
fn partition_pages(fs_pages: usize) -> [Vec<u8>; 3] {
    let mut mbr = vec![0u8; PAGE];
    mbr[0x1BE + 4] = 0xEE;
    put(&mut mbr, 0x1BE + 8, FIRST_LBA as u64, 4);
    put(&mut mbr, 0x1BE + 12, fs_pages as u64, 4);
    mbr[0x1FE] = 0x55;
    mbr[0x1FF] = 0xAA;
    let mut ent = vec![0u8; PAGE];
    ent[..16].copy_from_slice(&HFS_TYPE);
    ent[16..32].copy_from_slice(&PART_GUID);
    put(&mut ent, 0x20, FIRST_LBA as u64, 8);
    put(&mut ent, 0x28, (FIRST_LBA + fs_pages - 1) as u64, 8);
    for (i, c) in "System".encode_utf16().enumerate() {
        put(&mut ent, 0x38 + 2 * i, c as u64, 2);
    }
    let mut hdr = vec![0u8; PAGE];
    hdr[..8].copy_from_slice(b"EFI PART");
    put(&mut hdr, 8, 0x0001_0000, 4);
    put(&mut hdr, 12, 0x5C, 4);
    put(&mut hdr, 0x18, 1, 8);
    put(&mut hdr, 0x28, FIRST_LBA as u64, 8);
    put(&mut hdr, 0x30, (FIRST_LBA + fs_pages - 1) as u64, 8);
    hdr[0x38..0x48].copy_from_slice(&DISK_GUID);
    put(&mut hdr, 0x48, 2, 8);
    put(&mut hdr, 0x50, 1, 4);
    put(&mut hdr, 0x54, 0x80, 4);
    put(&mut hdr, 0x58, crc(&ent[..0x80]) as u64, 4);
    let c = crc(&hdr[..0x5C]);
    put(&mut hdr, 0x10, c as u64, 4);
    [mbr, hdr, ent]
}

/// The NAND signature WMR_Init wants: the driver's own version ('C00N' little-endian), the one such literal in the
/// iBoot (the kernel's FTL carries the same one).
pub fn fil_id(iboot: &[u8]) -> Result<u32> {
    let mut ids = std::collections::BTreeSet::new();
    for w in iboot.chunks_exact(4) {
        let w = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        if w & 0xFFFF_FFF0 == 0x4330_3030 && w & 0xF <= 9 {
            ids.insert(w);
        }
    }
    match (ids.len(), ids.first()) {
        (1, Some(&id)) => Ok(id),
        (n, _) => Err(crate::unsupported!("iBoot: {n} NAND driver versions ('C00N' literals), wanted one")),
    }
}

fn record(data: &[u8], spare: &[u8; SPARE]) -> Vec<u8> {
    let mut r = data.to_vec();
    r.extend_from_slice(spare);
    r
}

/// Every page besides the filesystem's: FIL id (`fil_id`), bad block tables, VFL contexts, the FTL context block, and
/// LBA 0-2. `bbt_map`: the factory table carries its good-block bitmap (length at 0x34, one bit per block from 0x38,
/// LSB first, set = good; the BBT's own block 4095 bad). iBoot-159 (C000, the iPhone's) scans for the VFL context
/// only in blocks the bitmap marks good; the C002/C003 iBoots read none, and devos50's table has none.
pub fn metadata_pages(fs_pages: usize, fil_id: u32, banks: usize, bbt_map: bool) -> BTreeMap<Page, Vec<u8>> {
    let zero = [0u8; SPARE];
    let mut fil = vec![0u8; PAGE];
    put(&mut fil, 0, fil_id as u64, 4);
    let mut pages = BTreeMap::new();
    pages.insert(Page { bank: 0, page: 0 }, record(&fil, &zero));
    let mut bbt = b"DEVICEINFOBBT".to_vec();
    bbt.resize(PAGE, 0);
    if bbt_map {
        put(&mut bbt, 0x34, (BLOCKS_PER_BANK / 8) as u64, 4);
        bbt[0x38..0x38 + BLOCKS_PER_BANK / 8].fill(0xFF);
        bbt[0x38 + (BLOCKS_PER_BANK - 1) / 8] &= !(1u8 << ((BLOCKS_PER_BANK - 1) % 8));
    }
    for b in 0..banks {
        pages.insert(Page { bank: b, page: (BLOCKS_PER_BANK - 1) * PAGES_PER_BLOCK }, record(&bbt, &zero));
        let cxt = record(&vfl_context(b), &VFL_SPARE);
        for c in 0..VFL_CXT_COPIES {
            pages.insert(Page { bank: b, page: VFL_CXT_BLOCK * PAGES_PER_BLOCK + c }, cxt.clone());
        }
    }
    pages.insert(location_vpn(0, banks), record(&[0u8; PAGE], &cxt_spare(0x43, 0, 0xFFFF_FFFF)));
    for i in 0..MAP_TABLES {
        let mut d = vec![0u8; PAGE];
        for j in 0..PAGE / 2 {
            let lbn = i * PAGE / 2 + j;
            let v = if lbn < MAPPED_LBNS { lbn + DATA_START } else { 0xFFFF };
            put(&mut d, 2 * j, v as u64, 2);
        }
        pages.insert(location_vpn(1 + i, banks), record(&d, &cxt_spare(0x46, i, 0xFFFF_FFFF)));
    }
    pages.insert(
        location_vpn(superblock(banks) - 1, banks),
        record(&ftl_meta(banks), &cxt_spare(0x43, 0, 0xFFFF_FFFE)),
    );
    for (lba, d) in partition_pages(fs_pages).iter().enumerate() {
        pages.insert(location_lpn(lba, banks), record(d, &data_spare(lba)));
    }
    pages
}

/// The image store at `out` (created) for a flat HFS+ volume: metadata pages, then every page of every logical block
/// the volume spans, zeros included, as a restore writes the whole image (the FTL fails the read of an erased page it
/// maps). Returns (filesystem pages, metadata pages) written.
pub fn write(volume: &Path, out: &Path, fil_id: u32, banks: usize, bbt_map: bool) -> Result<(usize, usize)> {
    if banks == 0 || banks > BANKS {
        return Err(crate::internal!("{banks} NAND banks"));
    }
    let mut f = File::open(volume).map_err(|e| crate::internal!("{}: {e}", volume.display()))?;
    let size = f.metadata()?.len() as usize;
    let sb = superblock(banks);
    let fs_pages = size.div_ceil(PAGE);
    let blocks = (FIRST_LBA + fs_pages).div_ceil(sb);
    if blocks > MAPPED_LBNS {
        return Err(crate::unsupported!("a {size}-byte volume is larger than the NAND's mapped logical blocks"));
    }
    let store = Store::create(out, &Image::s5l8900(banks))?;
    let meta = metadata_pages(fs_pages, fil_id, banks, bbt_map);
    for (p, d) in &meta {
        store.write(p.bank, p.page, d)?;
    }
    let mut buf = vec![0u8; sb * PAGE];
    let mut rec = vec![0u8; PAGE + SPARE];
    let mut written = 0;
    for lbn in 0..blocks {
        // logical block 0 starts with the partition pages (LBA 0-2), written above
        let first = lbn * sb;
        let skip = FIRST_LBA.saturating_sub(first);
        let n = (sb - skip) * PAGE;
        f.seek(SeekFrom::Start(((first + skip - FIRST_LBA) * PAGE) as u64))?;
        let mut got = 0;
        while got < n {
            match f.read(&mut buf[got..n]) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        buf[got..n].fill(0);
        for (i, data) in buf[..n].chunks_exact(PAGE).enumerate() {
            let lpn = first + skip + i;
            rec[..PAGE].copy_from_slice(data);
            rec[PAGE..].copy_from_slice(&data_spare(lpn));
            let p = location_lpn(lpn, banks);
            store.write(p.bank, p.page, &rec)?;
            written += 1;
        }
    }
    store.sync()?;
    Ok((written, meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lt-n45nand-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn le(b: &[u8], o: usize, n: usize) -> u64 {
        (0..n).fold(0, |a, k| a | (b[o + k] as u64) << (8 * k))
    }

    /// The ipod-1g-fires agent's oracle (N45Tests.oracle): logical.py's reading of the store (per logical page, the
    /// newest type-0x40 copy by the spare's lpn and age) as a flat image from LBA 3, over the FTL's blocks, and
    /// mkstore.py's VFL context.
    fn oracle(store: &Store, superblocks: usize) -> (Vec<u8>, Vec<u8>) {
        let mut best: HashMap<u32, ((u32, usize), Vec<u8>)> = HashMap::new();
        for b in 0..8 {
            for blk in FTL_START..FTL_START + DATA_START + superblocks {
                for p in blk * 128..(blk + 1) * 128 {
                    let raw = store.read(b, p).unwrap();
                    if raw[2048 + 9] != 0x40 {
                        continue;
                    }
                    let (lpn, key) = (le(&raw, 2048, 4) as u32, (le(&raw, 2052, 4) as u32, (p % 128) * 8 + b));
                    if best.get(&lpn).is_some_and(|old| old.0 >= key) {
                        continue;
                    }
                    best.insert(lpn, (key, raw[..2048].to_vec()));
                }
            }
        }
        let max = *best.keys().max().unwrap();
        let mut image = Vec::new();
        for lpn in 3..=max {
            image.extend(best.get(&lpn).map(|b| b.1.clone()).unwrap_or(vec![0; 2048]));
        }
        let mut d = vec![0u8; 2048];
        for i in 0..3 {
            put(&mut d, 4 + 2 * i, i as u64, 2);
        }
        d[1672..1672 + 281].fill(0xFF);
        put(&mut d, 1954, 35, 2);
        put(&mut d, 0xC, 0xFFFF_FFFF, 4);
        put(&mut d, 0x12, 8, 2);
        vfl_checksum(&mut d);
        (image, d)
    }

    /// A volume with data in its first and third logical blocks and none in its second: every page of all three is
    /// written, the VFL context is VFL_Format's, and the oracle's reading of the store is the volume.
    #[test]
    fn store_layout() {
        let dir = temp("layout");
        let (pp, ps) = (PAGE, superblock(BANKS));
        let fs_pages = 3 * ps - 3 - 100; // ends inside block 2
        let mut vol = vec![0u8; fs_pages * pp];
        for (i, v) in [(0, 0x48u8), (2 * ps, 0x11), (fs_pages - 1, 0x7F)] {
            vol[i * pp..i * pp + 4].fill(v);
        }
        std::fs::write(dir.join("volume.img"), &vol).unwrap();
        let out = dir.join("nand");
        let (written, meta) = write(&dir.join("volume.img"), &out, 0x4330_3032, BANKS, false).unwrap();
        assert_eq!((written, meta), (3 * ps - 3, 1 + 8 + 8 * 8 + 1 + 4 + 1 + 3));
        let store = Store::open(&out, false).unwrap();
        let blank = Image::s5l8900(8).blank_record;
        let file = |p: Page| Some(store.read(p.bank, p.page).unwrap()).filter(|r| *r != blank);
        let absent: Vec<usize> = (0..3 * ps).filter(|&l| file(location_lpn(l, BANKS)).is_none()).collect();
        assert!(absent.is_empty() && file(location_lpn(3 * ps, BANKS)).is_none(), "absent {absent:?}");
        // the map pages as _FTLRestore finds them: indices 0-3 only
        let mut maps: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        for bank in 0..BANKS {
            for bp in 0..PAGES_PER_BLOCK {
                if let Some(b) = file(Page { bank, page: FTL_START * PAGES_PER_BLOCK + bp })
                    && b[pp + 9] == 0x46
                {
                    maps.insert(le(&b, pp + 4, 2) as usize, b[..pp].to_vec());
                }
            }
        }
        assert_eq!(maps.keys().copied().collect::<Vec<_>>(), [0, 1, 2, 3]);
        let entry = |lbn: usize| le(&maps[&(lbn / 1024)], 2 * (lbn % 1024), 2);
        assert_eq!([entry(0), entry(3799), entry(3800), entry(4095)], [23, 3799 + 23, 0xFFFF, 0xFFFF]);

        let (image, theirs) = oracle(&store, 3);
        let mut want = vol.clone();
        want.resize(vol.len() + 100 * pp, 0);
        assert_eq!(image.len() / pp, 3 * ps - 3);
        assert!(image == want);

        for bank in 0..BANKS {
            let first = file(Page { bank, page: 35 * 128 }).unwrap();
            for c in 1..8 {
                assert_eq!(file(Page { bank, page: 35 * 128 + c }).as_ref(), Some(&first));
            }
            assert!(file(Page { bank, page: 35 * 128 + 8 }).is_none());
            let d = &first[..pp];
            let mut spare = vec![0xFFu8; 8];
            spare.extend_from_slice(&[0, 0x80]);
            spare.resize(64, 0xFF);
            assert_eq!(first[pp..], spare[..]);
            assert_eq!([le(d, 0, 4), le(d, 0xC, 4), le(d, 0x12, 2)], [bank as u64, 0xFFFF_FFFF, 8]);
            assert_eq!(
                [le(d, 0x14, 2), le(d, 0x1A, 2), le(d, 0x1C, 2), le(d, 0x1E, 2), le(d, 0x20, 2)],
                [1, 1, 39, 162, 4095]
            );
            assert_eq!(d[0x688 + 63], 0xFE);
            assert_eq!((0..4).map(|i| le(d, 0x7A2 + 2 * i, 2)).collect::<Vec<_>>(), [35, 36, 37, 38]);
            assert_eq!(le(d, 0x7AA, 2), 820);
            let mut check = d.to_vec();
            vfl_checksum(&mut check);
            assert_eq!(check, d);
            // against mkstore.py's context only the bank's age, the reserved pool, the other info blocks, the last
            // aBadMark byte it leaves 0 and the checksums differ
            let allowed = |i: usize| {
                i < 4
                    || (0x14..0x22).contains(&i)
                    || i == 0x688 + 63
                    || i == 0x7A1
                    || (0x7A4..0x7AC).contains(&i)
                    || (0x7F8..0x800).contains(&i)
            };
            let diff: Vec<usize> = (0..pp).filter(|&i| d[i] != theirs[i] && !allowed(i)).collect();
            assert!(diff.is_empty(), "bank {bank}: {diff:x?}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nand_signature_metadata_and_rejection() {
        assert!(fil_id(&[0u8; 64]).is_err());
        let mut iboot = vec![0u8; 64];
        iboot[16..20].copy_from_slice(b"300C");
        assert_eq!(fil_id(&iboot).unwrap(), 0x4330_3033);
        iboot[32..36].copy_from_slice(b"200C");
        assert!(fil_id(&iboot).is_err());
        let c2 = metadata_pages(100, 0x4330_3032, BANKS, false);
        let c3 = metadata_pages(100, 0x4330_3033, BANKS, false);
        let page0 = Page { bank: 0, page: 0 };
        assert!(c2.keys().eq(c3.keys()) && c2.iter().filter(|(k, _)| **k != page0).all(|(k, v)| c3[k] == *v));
        assert_eq!(c2[&page0][..4], [0x32, 0x30, 0x30, 0x43]);
        assert_eq!(c3[&page0][..4], [0x33, 0x30, 0x30, 0x43]);
    }

    /// The M68's four chip enables: a superblock spans four banks, nothing lands on banks 4-7, and the factory table
    /// carries its good-block bitmap.
    #[test]
    fn iphone_store_layout() {
        let dir = temp("m68");
        let ps = superblock(4);
        std::fs::write(dir.join("volume.img"), vec![0u8; (2 * ps - 3) * PAGE]).unwrap();
        let out = dir.join("nand");
        let (written, _) = write(&dir.join("volume.img"), &out, 0x4330_3030, 4, true).unwrap();
        assert_eq!(written, 2 * ps - 3);
        let mut names: Vec<String> =
            std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["cs0.pages", "cs1.pages", "cs2.pages", "cs3.pages", "geometry.json"]);
        assert_eq!(Image::of(&out), Some(Image::s5l8900(4)));
        assert_eq!(location_lpn(5, 4), Page { bank: 1, page: (201 + 23) * 128 + 1 });
        let store = Store::open(&out, false).unwrap();
        let bbt = store.read(3, 4095 * 128).unwrap();
        assert_eq!(&bbt[..13], b"DEVICEINFOBBT");
        assert_eq!([bbt[0x34], bbt[0x35], bbt[0x38], bbt[0x38 + 511]], [0x00, 0x02, 0xFF, 0x7F]);
        assert_eq!(&store.read(0, 0).unwrap()[..4], b"000C");
        // the N45's table stays as devos50 wrote it: no bitmap
        let n45 = &metadata_pages(16, 0x4330_3032, BANKS, false)[&Page { bank: 0, page: 4095 * 128 }];
        assert_eq!([n45[0x34], n45[0x38]], [0, 0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

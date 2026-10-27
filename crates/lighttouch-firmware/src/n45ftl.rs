//! The 1.x legacy FTL read back from an image store (page_nand, Image::s5l8900) as the guest left it: the device's
//! live clone (overlay/nand) when there is one, else the base. An erased page (every bit set) reads as none. The FTL's
//! context is openiBoot's s5l8900 FTLCxt (what n45nand::ftl_meta writes): the map's four 0x46 pages listed at +0x38,
//! the log-offset pages at +0x110, the log table at +0x1A4 (20-byte entries: usn, vbn, lbn), the context blocks at
//! +0x312. The context moves (the FTL takes new context blocks from its free pool), so the current one is the
//! context-typed (0x43-0x4F) virtual block whose first page has the lowest usnDec, and its last written page must be
//! the context itself (0x43); anything else is an unclean shutdown, refused (that state needs FTL_Restore's scan).
//!
//! ```ignore
//! let ftl = N45Ftl::open(&nand, overlay.as_deref())?;
//! ftl.read(lpn)?          // the logical page's 2048 bytes and spare, None when it was never written
//! ftl.location(lpn)       // where it lives now (a log block's page, or map[lbn] at the same offset)
//! ```
//! Format 1 stores (bank<N>/ page directories) are refused; the Swift tool migrates them.
use crate::error::Result;
use crate::n45nand::{self, BLOCKS_PER_BANK, FTL_START, MAP_TABLES, PAGE, Page};
use crate::page_nand::{self, Store};
use std::collections::HashMap;
use std::path::Path;

/// FTLCxt.pLog slots the FTL uses (the 18th is not).
pub const LOGS: usize = 17;
const VIRTUAL_BLOCKS: usize = BLOCKS_PER_BANK - FTL_START;

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub struct N45Ftl {
    pub banks: usize,
    store: Store,
    map: Vec<u16>,
    /// lbn -> (log vbn, slot)
    log_table: HashMap<u16, (u16, usize)>,
    offsets: Vec<u16>,
}

impl N45Ftl {
    pub fn open(base: &Path, overlay: Option<&Path>) -> Result<N45Ftl> {
        let store = page_nand::reader(base, overlay)?;
        if store.image.board != "s5l8900" {
            return Err(crate::unsupported!("{}: not a 1.x NAND", base.display()));
        }
        let banks = store.image.chips;
        let sb = n45nand::superblock(banks);
        let mut ftl = N45Ftl { banks, store, map: Vec::new(), log_table: HashMap::new(), offsets: Vec::new() };
        let read = |ftl: &N45Ftl, vpn: usize| ftl.page(n45nand::location_vpn(vpn, banks));

        let mut newest: Option<(u32, usize)> = None;
        for vb in 0..VIRTUAL_BLOCKS {
            let Some((_, spare)) = read(&ftl, vb * sb)? else { continue };
            if !(0x43..=0x4F).contains(&spare[9]) {
                continue;
            }
            let usn = le32(&spare, 0);
            if newest.is_none_or(|(u, _)| usn < u) {
                newest = Some((usn, vb));
            }
        }
        let Some((_, vb)) = newest else { return Err(crate::unsupported!("{}: no FTL context", base.display())) };
        let mut last = None;
        for p in (1..sb).rev() {
            if let Some(page) = read(&ftl, vb * sb + p)? {
                last = Some(page);
                break;
            }
        }
        let cxt = match last {
            Some((data, spare)) if spare[9] == 0x43 => data,
            _ => {
                return Err(crate::unsupported!(
                    "the 1.x FTL was not shut down cleanly (virtual block {vb} does not end in its context); power the device off from the guest first"
                ));
            }
        };
        if !(0..3).any(|i| le16(&cxt, 0x312 + 2 * i) as usize == vb) {
            return Err(crate::unsupported!("FTL context in virtual block {vb} does not list itself as a context block"));
        }
        let mut map = Vec::with_capacity(MAP_TABLES * PAGE / 2);
        for i in 0..MAP_TABLES {
            let Some((page, _)) = read(&ftl, le32(&cxt, 0x38 + 4 * i) as usize)? else {
                return Err(crate::unsupported!("FTL map page {i} is missing"));
            };
            map.extend(page.chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]])));
        }
        let mut table = HashMap::new();
        for slot in 0..LOGS {
            let vbn = le16(&cxt, 0x1A4 + 20 * slot + 4);
            let lbn = le16(&cxt, 0x1A4 + 20 * slot + 6);
            if vbn != 0xFFFF {
                table.insert(lbn, (vbn, slot));
            }
        }
        let mut offsets = Vec::new();
        if !table.is_empty() {
            let pages = (LOGS * sb * 2).div_ceil(PAGE);
            let mut raw = Vec::with_capacity(pages * PAGE);
            for i in 0..pages {
                let Some((page, _)) = read(&ftl, le32(&cxt, 0x110 + 4 * i) as usize)? else {
                    return Err(crate::unsupported!("FTL log-offset page {i} is missing"));
                };
                raw.extend(page);
            }
            offsets = raw[..LOGS * sb * 2].chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]])).collect();
        }
        ftl.map = map;
        ftl.log_table = table;
        ftl.offsets = offsets;
        Ok(ftl)
    }

    pub fn log_blocks_in_use(&self) -> usize {
        self.log_table.len()
    }

    /// Where logical page `lpn` lives now.
    pub fn location(&self, lpn: usize) -> Result<Page> {
        let sb = n45nand::superblock(self.banks);
        let (lbn, off) = (lpn / sb, lpn % sb);
        if let Some(&(vbn, slot)) = u16::try_from(lbn).ok().and_then(|l| self.log_table.get(&l)) {
            let o = self.offsets[slot * sb + off];
            if o != 0xFFFF {
                return Ok(n45nand::location_vpn(vbn as usize * sb + o as usize, self.banks));
            }
        }
        let Some(&vbn) = self.map.get(lbn) else {
            return Err(crate::unsupported!("logical page {lpn} is past the FTL map"));
        };
        Ok(n45nand::location_vpn(vbn as usize * sb + off, self.banks))
    }

    /// The logical page's data and spare; None when it reads erased.
    pub fn read(&self, lpn: usize) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        self.page(self.location(lpn)?)
    }

    /// A physical page as the guest reads it; None for an erased page (or one past the chip).
    fn page(&self, p: Page) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
        if p.bank >= self.banks || p.page >= self.store.image.pages_per_chip() {
            return Ok(None);
        }
        let mut record = self.store.read(p.bank, p.page)?;
        if record == self.store.image.blank_record {
            return Ok(None);
        }
        let spare = record.split_off(PAGE);
        Ok(Some((record, spare)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::n45nand::{DATA_START, MAPPED_LBNS, cxt_spare, data_spare, ftl_meta, location_lpn, location_vpn};
    use crate::page_nand::Image;
    use std::path::PathBuf;

    const BANKS: usize = 4; // the M68's; the N45's 8 take the same paths
    const SB: usize = 128 * BANKS;
    const PP: usize = PAGE;

    fn le(b: &mut [u8], o: usize, v: usize, n: usize) {
        for k in 0..n {
            b[o + k] = (v >> (8 * k)) as u8;
        }
    }

    /// A prepared store (n45nand::write) of a small HFSX-headed volume, and the volume.
    fn prepared(dir: &Path) -> (PathBuf, Vec<u8>) {
        let fs_pages = 3 * SB - 3 - 50;
        let mut vol = vec![0u8; fs_pages * PP];
        vol[1024] = b'H';
        vol[1025] = b'X';
        let blocks = fs_pages * PP / 4096;
        vol[1024 + 40..1024 + 44].copy_from_slice(&4096u32.to_be_bytes());
        vol[1024 + 44..1024 + 48].copy_from_slice(&(blocks as u32).to_be_bytes());
        for p in 1..fs_pages {
            vol[p * PP] = (p % 251) as u8;
            vol[p * PP + 1] = 0x5A;
        }
        let image = dir.join("volume.img");
        let base = dir.join("nand");
        std::fs::write(&image, &vol).unwrap();
        n45nand::write(&image, &base, 0x4330_3030, BANKS, false).unwrap();
        vol.truncate(blocks * 4096);
        (base, vol)
    }

    /// What a booted guest leaves in the live clone (overlay/nand): the context moved to virtual block 3 (lower
    /// usnDec), logical block 0 remapped to virtual block 30 with page 10 changed, and a log block (virtual block 31)
    /// holding a newer copy of logical block 1's page 5. `clean: false` ends the context block on a map page.
    fn booted(ovl: &Path, base: &Path, clean: bool) {
        std::fs::create_dir_all(ovl).unwrap();
        let live_dir = ovl.join("nand");
        let live = Store::create(&live_dir, &Image::s5l8900(BANKS)).unwrap();
        let src = Store::open(base, false).unwrap();
        for chip in 0..BANKS {
            // a clone of the base, block by block over what the base wrote
            for blk in (0..BLOCKS_PER_BANK).filter(|b| (FTL_START..FTL_START + 40).contains(b) || [0, 35, 4095].contains(b))
            {
                for p in blk * 128..(blk + 1) * 128 {
                    let r = src.read(chip, p).unwrap();
                    if r != live.image.blank_record {
                        live.write(chip, p, &r).unwrap();
                    }
                }
            }
        }
        let put = |vpn: usize, data: &[u8], spare: &[u8]| {
            let p = location_vpn(vpn, BANKS);
            live.write(p.bank, p.page, &[data, spare].concat()).unwrap();
        };
        let ctx_vb = 3;
        let start = ctx_vb * SB;
        put(start, &[0u8; PP], &cxt_spare(0x43, 0, 0xFFFF_FFF0));
        for i in 0..MAP_TABLES {
            let mut page = vec![0xFFu8; PP];
            for j in 0..PP / 2 {
                let lbn = i * PP / 2 + j;
                if lbn < MAPPED_LBNS {
                    le(&mut page, 2 * j, if lbn == 0 { 30 } else { lbn + DATA_START }, 2);
                }
            }
            put(start + 1 + i, &page, &cxt_spare(0x46, i, 0xFFFF_FFF0));
        }
        let offset_pages = (LOGS * SB * 2).div_ceil(PP);
        let mut offsets = vec![0xFFu8; offset_pages * PP];
        le(&mut offsets, 2 * 5, 2, 2); // slot 0, offset 5 -> page 2 of its log block
        for i in 0..offset_pages {
            put(start + 5 + i, &offsets[i * PP..(i + 1) * PP], &cxt_spare(0x44, i, 0xFFFF_FFF0));
        }
        let mut cxt = ftl_meta(BANKS);
        for i in 0..MAP_TABLES {
            le(&mut cxt, 0x38 + 4 * i, start + 1 + i, 4);
        }
        for i in 0..offset_pages {
            le(&mut cxt, 0x110 + 4 * i, start + 5 + i, 4);
        }
        le(&mut cxt, 0x1A4 + 4, 31, 2);
        le(&mut cxt, 0x1A4 + 6, 1, 2);
        for i in 0..3 {
            le(&mut cxt, 0x312 + 2 * i, ctx_vb + i, 2);
        }
        let last = start + 5 + offset_pages;
        if clean {
            put(last, &cxt, &cxt_spare(0x43, 0, 0xFFFF_FFF0));
        } else {
            put(last, &[0u8; PP], &cxt_spare(0x46, 0, 0xFFFF_FFF0));
        }
        for off in 0..SB {
            let p = location_lpn(off, BANKS);
            let r = src.read(p.bank, p.page).unwrap();
            if r == live.image.blank_record {
                continue;
            }
            let data = if off == 10 { vec![0xA1; PP] } else { r[..PP].to_vec() };
            put(30 * SB + off, &data, &r[PP..]);
        }
        put(31 * SB + 2, &[0xB2; PP], &data_spare(SB + 5));
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lt-n45ftl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fresh store reads back as its volume, through the prepared context.
    #[test]
    fn prepared_store() {
        let dir = temp("prepared");
        let (base, vol) = prepared(&dir);
        let ftl = N45Ftl::open(&base, None).unwrap();
        assert_eq!((ftl.banks, ftl.log_blocks_in_use()), (BANKS, 0));
        let mut img = Vec::new();
        for lpn in 3..3 + vol.len() / PP {
            img.extend(ftl.read(lpn).unwrap().unwrap().0);
        }
        assert!(img == vol);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A booted overlay: the moved context, the remapped block and the log page win; the rest is the base's.
    #[test]
    fn booted_overlay_and_unclean_refused() {
        let dir = temp("booted");
        let (base, vol) = prepared(&dir);
        let ovl = dir.join("overlay");
        booted(&ovl, &base, true);
        let ftl = N45Ftl::open(&base, Some(&ovl)).unwrap();
        assert_eq!(ftl.log_blocks_in_use(), 1);
        assert_eq!(ftl.location(10).unwrap(), location_vpn(30 * SB + 10, BANKS));
        assert_eq!(ftl.location(SB + 5).unwrap(), location_vpn(31 * SB + 2, BANKS));
        assert_eq!(ftl.location(SB + 6).unwrap(), location_lpn(SB + 6, BANKS));
        let page = |lpn: usize| ftl.read(lpn).unwrap().unwrap().0;
        let want = |lpn: usize| vol[(lpn - 3) * PP..(lpn - 2) * PP].to_vec();
        assert!(page(10).iter().all(|&b| b == 0xA1) && page(SB + 5).iter().all(|&b| b == 0xB2));
        assert!(page(11) == want(11) && page(SB + 6) == want(SB + 6) && page(2 * SB + 1) == want(2 * SB + 1));
        // the base alone still reads as prepared
        assert_eq!(N45Ftl::open(&base, None).unwrap().read(10).unwrap().unwrap().0, want(10));

        let ovl2 = dir.join("overlay2");
        booted(&ovl2, &base, false);
        assert!(N45Ftl::open(&base, Some(&ovl2)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

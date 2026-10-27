//! The K48-family NAND store (iPad 1 k48ap, iPod touch 4G n81ap, iPhone 4 n90ap, iPhone 3GS n88ap, iPod touch 3G
//! n18ap), written offline in the state a 7B500 restore leaves a device in after a power cut before the FTL context
//! flush: special pages (BBT, NANDDRIVERSIGN), VSVFL contexts, closed YaFTL user/index blocks with BTOCs, no CX01
//! context. Port of FirmwareKit's K48NAND (itself a port of qemu-ios imgtools/ipad1_nand.py, whose comments carry
//! the kernel addresses each structure was checked against); the bytes it writes are the Swift tool's.
//!
//! Store: geometry.json + one SPARSE file per chip select (bus<b>-ce<c>.pages), a page record every
//! page_bytes+spare_bytes, stored XOR 0xFF (StoreFormat::Image, nand-xor-ff-v2): unwritten pages are holes, which
//! read as erased.
//!
//! ```ignore
//! let geo = Geometry::for_vendor_type(recipe.nand_vendor_type);          // the K48 recipe's store geometry
//! std::fs::write(&mbr, k48nand::make_mbr(&geo, system_mib))?;           // volumes step
//! let parts = k48nand::partitions(&std::fs::read(&mbr)?);
//! // store step
//! let kc = dec.join("kernelcache.mach");
//! let mut epoch = k48nand::signature_epoch(&kc)?;
//! if s5l8920 { if let Some(scep) = k48nand::restore_epoch_of(&ipsw, &board)? { epoch = scep } }
//! let mut b = Build::new(&mbr, k48nand::kernel_version(&kc)?, &system, DataVolume::Image(data), &nand);
//! (b.geometry, b.epoch, b.whitening, b.sig_flags) = (geo, epoch, whitening, recipe.nand_sig_flags.map(|f| f as u32));
//! k48nand::build(&b, &log)?;
//! ```
use crate::error::Result;
use crate::k48nand_check::StoreReader;
use crate::k48nand_image;
use crate::posio;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) const META: usize = 12;
/// NANDDRIVERSIGN nSig: "C11" + the epoch digit in the low byte (the FIL reads it as '0' + epoch); epoch 1 for
/// IOFlashStorage up to 410.3 (iOS 4.3.0), 2 from 410.4 (4.3.5), see signature_epoch.
pub(crate) const NSIG_BASE: u32 = 0x4331_3130;
/// flags 5, + 0x10000 (metadata whitening) where the DT asks for it; the S5L8920 boards' DTs don't, and their WMR
/// refuses a whitened store ("Metadata whitening not supported"). Their plain stores carry flags 4: what their own
/// FIL writes when it formats (3.1.3 N88), the only value 3.1.3 accepts (flags > 4 is "Incompatible Signature");
/// 4.x and 5.x take it too (as qemu-ios ipad1_nand.py --no-whitening).
pub(crate) const SIG_FLAGS: u32 = 0x0001_0005;
pub(crate) const PLAIN_SIG_FLAGS: u32 = 0x4;
pub(crate) const T_INDEX: u8 = 0x4;
pub(crate) const T_CLOSED: u8 = 0x8;
pub(crate) const T_USER: u8 = 0x10;
pub(crate) const T_VFL: u8 = 0x80;
pub(crate) const UNMAPPED: u32 = 0xFFFF_FFFF;

fn nsig(epoch: u8) -> u32 {
    NSIG_BASE.wrapping_add(epoch as u32)
}

pub(crate) fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn put16(b: &mut [u8], o: usize, v: usize) {
    b[o] = v as u8;
    b[o + 1] = (v >> 8) as u8;
}

fn lcg_table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        let mut v: u32 = 0x50F4_546A;
        for slot in t.iter_mut() {
            for _ in 0..763 {
                v = 0x19660D_u32.wrapping_mul(v).wrapping_add(0x3C6E_F35F);
            }
            *slot = v;
        }
        t
    })
}

/// XOR of the three meta words with the page-indexed LCG table; bytes 10-11 stay 00 on flash. Its own inverse
/// (bar those two bytes), so the reader un-whitens with it too.
pub(crate) fn whiten(m: &[u8], ppage: usize) -> [u8; META] {
    let t = lcg_table();
    let mut out = [0u8; META];
    for i in 0..3 {
        let w = le32(m, 4 * i) ^ t[(i + ppage) % 256];
        out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
    }
    out[10] = 0;
    out[11] = 0;
    out
}

// MARK: - Geometry

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geometry {
    pub name: &'static str,
    pub chip_id: u32,
    pub buses: usize,
    pub ce_per_bus: usize,
    pub blocks_per_ce: usize,
    pub pages_per_block: usize,
    pub page_size: usize,
    pub spare_bytes: usize,
    pub vendor_type: u32,
    // derived exactly as the kernel does (VSVFL_Init / VSVFL_Format / YAFTL_Init)
    pub num_cs: usize,
    pub pages_per_ce: usize,
    pub vfl_banks: usize,
    pub banks_total: usize,
    pub blocks_per_bank: usize,
    pub ppsublk: usize,
    pub usable: usize,
    pub pool: usize,
    pub num_blocks: usize,
    pub toc: usize,
    pub toc_entries: usize,
    pub data_pages: usize,
    pub num_iblocks: usize,
    pub total_pages: usize,
    pub exported_pages: usize,
    /// Per chip select, the special-block candidates from the top block down (5 on cs0, 2 elsewhere).
    pub(crate) cand: Vec<Vec<usize>>,
    pub(crate) bbt_len: usize,
    /// The physical blocks 0...vfl_blocks[3] remapped into the reserved pool: (bank, slot), indexed by block.
    pub(crate) remap: Vec<(usize, usize)>,
    pub(crate) replaced_count: Vec<usize>,
}

pub(crate) const VFL_BLOCKS: [usize; 4] = [1, 2, 3, 4];
pub(crate) const CTRL_BLOCKS: [usize; 3] = [0, 1, 2];

impl Geometry {
    #[allow(clippy::too_many_arguments)]
    fn new(
        name: &'static str,
        chip_id: u32,
        buses: usize,
        ce_per_bus: usize,
        blocks_per_ce: usize,
        pages_per_block: usize,
        page_size: usize,
        spare_bytes: usize,
        vendor_type: u32,
    ) -> Geometry {
        let num_cs = buses * ce_per_bus;
        let pages_per_ce = blocks_per_ce * pages_per_block;
        // VSVFL: two banks per CE for vendor type 0x100014, one for 0x10001 (3.0's own table for this part on 2x4 CEs)
        let vfl_banks = if vendor_type == 0x100014 { 2 } else { 1 };
        let banks_total = num_cs * vfl_banks;
        let blocks_per_bank = blocks_per_ce / vfl_banks;
        let ppsublk = pages_per_block * banks_total;
        let s = (usize::BITS - blocks_per_bank.leading_zeros()) as i64 - 1 - 10;
        let usable = if s >= 0 { 0x3D0 << s } else { 0x3D0 >> -s };
        let pool = blocks_per_bank - usable;
        let num_blocks = usable;
        let toc = (4 * ppsublk).div_ceil(page_size);
        let toc_entries = page_size / 4;
        let data_pages = ppsublk - toc;
        let n = (num_blocks - 8) * data_pages;
        let d = data_pages * toc_entries;
        let num_iblocks = 3 * n.div_ceil(d);
        let total_pages = n - num_iblocks * ppsublk;
        let exported_pages = (total_pages - 1) / 100 * 99;
        let top = blocks_per_ce - 1;
        let cand = (0..num_cs).map(|cs| (0..if cs == 0 { 5 } else { 2 }).map(|k| top - k).collect()).collect();
        let bbt_len = blocks_per_ce.div_ceil(8);
        let mut remap = Vec::new();
        let mut slots = vec![0usize; vfl_banks];
        for pbn in 0..=VFL_BLOCKS[3] {
            let bank = pbn % vfl_banks;
            remap.push((bank, slots[bank]));
            slots[bank] += 1;
        }
        Geometry {
            name,
            chip_id,
            buses,
            ce_per_bus,
            blocks_per_ce,
            pages_per_block,
            page_size,
            spare_bytes,
            vendor_type,
            num_cs,
            pages_per_ce,
            vfl_banks,
            banks_total,
            blocks_per_bank,
            ppsublk,
            usable,
            pool,
            num_blocks,
            toc,
            toc_entries,
            data_pages,
            num_iblocks,
            total_pages,
            exported_pages,
            cand,
            bbt_len,
            remap,
            replaced_count: slots,
        }
    }

    /// iBoot-817.29's 0xB614D5AD row: 2 buses x 4 CE x 4096 blocks x 128 pages x 4 KiB; the captured 16 GB unit.
    pub fn k48_with_16gb() -> Geometry {
        Geometry::new("k48-16g", 0xB614_D5AD, 2, 4, 0x1000, 128, 4096, 0x80, 0x100014)
    }

    /// The same part as 3.0's AppleS5L8920XIOPFMI table lists it on 2x4 CEs: vendor type 0x10001, one VFL bank per
    /// CE, 1024-page superblocks whose block TOC fits one page. 3.0's yaFTL assumes one TOC page (YAFTL_Init
    /// 0xc05c74ec on N88 7A341; fixed in 3.1), so k48_with_16gb's 2048-page superblocks restore a garbage map there.
    pub fn k48_with_16gb_v1() -> Geometry {
        Geometry::new("k48-16g-v1", 0xB614_D5AD, 2, 4, 0x1000, 128, 4096, 0x80, 0x10001)
    }

    /// ipad1_nand.py's tiny synthetic geometry (--selfcheck): same math, 16-page blocks.
    pub fn selfcheck() -> Geometry {
        Geometry::new("selfcheck", 0xB614_D5AD, 2, 2, 0x1000, 16, 4096, 0x80, 0x100014)
    }

    pub fn known() -> Vec<Geometry> {
        vec![Self::k48_with_16gb(), Self::k48_with_16gb_v1(), Self::selfcheck()]
    }

    /// The K48 recipe's store geometry: the part's default, or the recipe's nand_vendor_type variant (iOS 3.0:
    /// one VFL bank per CE).
    pub fn for_vendor_type(nand_vendor_type: Option<i64>) -> Geometry {
        if nand_vendor_type == Some(0x10001) { Self::k48_with_16gb_v1() } else { Self::k48_with_16gb() }
    }

    fn pool_pblock(&self, bank: usize, slot: usize) -> usize {
        self.vfl_banks * (self.usable + slot) + bank
    }

    pub(crate) fn bus_ce(&self, cs: usize) -> (usize, usize) {
        (cs / self.ce_per_bus, cs % self.ce_per_bus)
    }

    pub(crate) fn ppage(&self, pblock: usize, page: usize) -> usize {
        pblock * self.pages_per_block + page
    }

    fn phys(&self, pblock: usize, page: usize) -> usize {
        match self.remap.get(pblock) {
            Some(&(bank, slot)) => self.ppage(self.pool_pblock(bank, slot), page),
            None => self.ppage(pblock, page),
        }
    }

    /// YaFTL vpn -> (cs, physical page).
    pub fn vpn_to_phys(&self, vpn: usize) -> (usize, usize) {
        let (vblock, j) = (vpn / self.ppsublk, vpn % self.ppsublk);
        let bank = j % self.banks_total;
        let page = j / self.banks_total;
        let cs = bank % self.num_cs;
        let bit = bank / self.num_cs;
        (cs, self.phys(self.vfl_banks * vblock + bit, page))
    }

    /// geometry.json exactly as Python's json.dump(..., indent=1) writes it.
    pub fn json(&self) -> String {
        let mut s = format!(
            "{{\n \"page_bytes\": {},\n \"spare_bytes\": {},\n \"pages_per_block\": {},\n \"blocks_per_ce\": {},\n \"ce_per_bus\": {},\n \"buses\": {},\n \"chip_id\": \"0x{:08X}\"",
            self.page_size,
            self.spare_bytes,
            self.pages_per_block,
            self.blocks_per_ce,
            self.ce_per_bus,
            self.buses,
            self.chip_id
        );
        // only off the default, as ipad1_nand
        if self.vendor_type != 0x100014 {
            s += &format!(",\n \"vendor_type\": {}", self.vendor_type);
        }
        s + "\n}"
    }

    /// geometry.json for a store of `format`: an image store also names its format and FirmwareKit's layout.
    pub fn json_for(&self, format: StoreFormat) -> String {
        let json = self.json();
        if format != StoreFormat::Image {
            return json;
        }
        format!(
            "{},\n \"storage_format\": \"{}\",\n \"layout\": \"{GENERATED_LAYOUT}\"\n}}",
            &json[..json.len() - 2],
            format.as_str()
        )
    }
}

/// How a store's records sit in its files (geometry.json "storage_format", which qemu-ios's IOP reads too).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreFormat {
    /// As programmed; a hole, and an all-zero record, read as the erased page. No key, or this one.
    Legacy,
    /// XOR 0xFF, so a hole is the erased page and every programmed record reads back as written (format 2).
    Image,
}

impl StoreFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            StoreFormat::Legacy => "legacy-zero-blank-v1",
            StoreFormat::Image => "nand-xor-ff-v2",
        }
    }
}

/// geometry.json's "layout" on a store FirmwareKit laid out: the generated VFL the reader follows. A store
/// qemu-ios's restore wrote is also nand-xor-ff-v2 but physical: it needs the native VFL/crypto mapping.
pub const GENERATED_LAYOUT: &str = "generated";

// MARK: - Store

/// The chip files of a store being written.
pub struct Store {
    geo: Geometry,
    stride: usize,
    files: Vec<File>,
    pub records: usize,
    rec: Vec<u8>,
    /// Meta written as is (no whitening): the S5L8920 boards.
    pub plain: bool,
    format: StoreFormat,
}

impl Store {
    pub fn create(dir: &Path, geo: &Geometry, plain: bool, format: StoreFormat) -> Result<Store> {
        let stride = geo.page_size + geo.spare_bytes;
        std::fs::create_dir_all(dir).map_err(|e| crate::internal!("{}: {e}", dir.display()))?;
        let json = dir.join("geometry.json");
        std::fs::write(&json, geo.json_for(format)).map_err(|e| crate::internal!("{}: {e}", json.display()))?;
        let mut files = Vec::with_capacity(geo.num_cs);
        for b in 0..geo.buses {
            for c in 0..geo.ce_per_bus {
                let p = dir.join(format!("bus{b}-ce{c}.pages"));
                let f = posio::create_sparse(&p).map_err(|e| crate::internal!("{}: {e}", p.display()))?;
                f.set_len((geo.pages_per_ce * stride) as u64).map_err(|e| crate::internal!("{}: {e}", p.display()))?;
                files.push(f);
            }
        }
        Ok(Store { geo: geo.clone(), stride, files, records: 0, rec: vec![0; stride], plain, format })
    }

    /// One page record: data, then the (whitened unless raw) meta, spare[12...] = 0.
    pub fn write(&mut self, cs: usize, ppage: usize, data: &[u8], meta: &[u8], raw: bool) -> Result<()> {
        let geo = &self.geo;
        if data.len() != geo.page_size || meta.len() != META {
            return Err(crate::internal!("NAND store write: a {}-byte page with {}-byte meta", data.len(), meta.len()));
        }
        // plain as whitened: bytes 10-11 never travel through the meta DMA, so 00 00 on flash (ipad1_nand.whiten)
        let mut m = [0u8; META];
        if raw {
            m.copy_from_slice(meta);
        } else if self.plain {
            m[..10].copy_from_slice(&meta[..10]);
        } else {
            m = whiten(meta, ppage);
        }
        if m.iter().all(|&b| b == 0) {
            return Err(crate::internal!("NAND store write: spare must not be all zero (hole == blank)"));
        }
        let ps = geo.page_size;
        self.rec[..ps].copy_from_slice(data);
        self.rec[ps..ps + META].copy_from_slice(&m);
        self.rec[ps + META..].fill(0);
        if self.format == StoreFormat::Image {
            k48nand_image::invert(&mut self.rec);
        }
        let (b, c) = geo.bus_ce(cs);
        posio::write_all_at(&self.files[b * geo.ce_per_bus + c], &self.rec, (ppage * self.stride) as u64)
            .map_err(|e| crate::internal!("NAND store write: {e}"))?;
        self.records += 1;
        Ok(())
    }
}

// MARK: - On-flash structures

fn spare(lpn: u32, usn: u32, typ: u8) -> [u8; META] {
    let mut s = [0u8; META];
    put32(&mut s, 0, lpn);
    put32(&mut s, 4, usn);
    s[8] = 0;
    s[9] = typ;
    s[10] = 0xFF;
    s[11] = 0xFF;
    s
}

fn special_page(geo: &Geometry, magic: &str, hdr_ver: u32, cands: &[usize], payload: &[u8]) -> (Vec<u8>, [u8; META]) {
    let mut page = vec![0u8; geo.page_size];
    page[..magic.len()].copy_from_slice(magic.as_bytes());
    put32(&mut page, 16, hdr_ver);
    for i in 0..8 {
        put32(&mut page, 20 + 4 * i, cands.get(i).map_or(0xFFFF_FFFF, |&c| c as u32));
    }
    put32(&mut page, 0x34, payload.len() as u32);
    page[0x38..0x38 + payload.len()].copy_from_slice(payload);
    let mut meta = [0xA5u8; META];
    meta[10] = 0;
    meta[11] = 0;
    (page, meta)
}

fn bad_blocks(geo: &Geometry, cs: usize) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(geo.cand[cs].iter().copied());
    v.sort_unstable();
    v.dedup();
    v
}

fn bbt_bitmap(geo: &Geometry, cs: usize) -> Vec<u8> {
    let mut bits = vec![0xFFu8; geo.bbt_len];
    for b in bad_blocks(geo, cs).into_iter().filter(|&b| b < geo.blocks_per_ce) {
        bits[b / 8] &= !(1u8 << (b % 8));
    }
    bits
}

fn vfl_context(geo: &Geometry, cs: usize) -> Vec<u8> {
    let mut c = vec![0u8; 0x800];
    put32(&mut c, 0, cs as u32 + 1);
    put32(&mut c, 4, 0xFFFF_FFFF);
    put32(&mut c, 8, 2);
    put16(&mut c, 12, 0);
    put16(&mut c, 14, 8);
    put16(&mut c, 0x10, 1 + geo.cand[cs].len());
    for (bank, &n) in geo.replaced_count.iter().enumerate() {
        put16(&mut c, 0x16 + 2 * bank, n);
    }
    let mut pool = vec![0xFFF0usize; geo.vfl_banks * geo.pool];
    for pbn in bad_blocks(geo, cs).into_iter().filter(|&p| p >= geo.vfl_banks * geo.usable) {
        pool[(pbn % geo.vfl_banks) * geo.pool + pbn / geo.vfl_banks - geo.usable] = 0xFFFF;
    }
    for (pbn, &(bank, slot)) in geo.remap.iter().enumerate() {
        pool[bank * geo.pool + slot] = pbn;
    }
    for (i, &v) in pool.iter().enumerate() {
        put16(&mut c, 0x26 + 2 * i, v);
    }
    for (i, &v) in VFL_BLOCKS.iter().enumerate() {
        put16(&mut c, 0x68e + 2 * i, v);
    }
    put16(&mut c, 0x696, geo.usable);
    put16(&mut c, 0x698, geo.usable);
    for (i, &v) in CTRL_BLOCKS.iter().enumerate() {
        put16(&mut c, 0x69a + 2 * i, v);
    }
    put32(&mut c, 0x6da, geo.vendor_type);
    put32(&mut c, 0x7f4, 2);
    vfl_checksum(&c)
}

pub(crate) fn vfl_checksum(c0: &[u8]) -> Vec<u8> {
    let mut c = c0.to_vec();
    let (mut sum, mut x) = (0u32, 0u32);
    for i in 0..0x7f8 / 4 {
        let w = le32(&c, 4 * i);
        sum = sum.wrapping_add(w);
        x ^= w;
    }
    put32(&mut c, 0x7f8, sum.wrapping_add(0xAABB_CCDD));
    put32(&mut c, 0x7fc, x ^ 0xAABB_CCDD);
    c
}

fn write_metadata(st: &mut Store, geo: &Geometry, kernel_version: &[u8], epoch: u8, flags: Option<u32>) -> Result<()> {
    for cs in 0..geo.num_cs {
        let (data, meta) = special_page(geo, "DEVICEINFOBBT", 4, &geo.cand[cs], &bbt_bitmap(geo, cs));
        for &blk in geo.cand[cs].iter().take(2) {
            for p in 0..geo.pages_per_block {
                st.write(cs, geo.ppage(blk, p), &data, &meta, true)?;
            }
        }
        let mut ctx = vfl_context(geo, cs);
        ctx.resize(geo.page_size, 0);
        let m = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x80, 0xFF, 0xFF];
        for p in 0..8 {
            st.write(cs, geo.ppage(VFL_BLOCKS[0], p), &ctx, &m, false)?;
        }
    }
    let mut payload = vec![0u8; 8 + 0x100];
    put32(&mut payload, 0, nsig(epoch));
    put32(&mut payload, 4, flags.unwrap_or(if st.plain { PLAIN_SIG_FLAGS } else { SIG_FLAGS }));
    let kv = &kernel_version[..kernel_version.len().min(0x100)];
    payload[8..8 + kv.len()].copy_from_slice(kv);
    let (data, meta) = special_page(geo, "NANDDRIVERSIGN", 0, &[0; 8], &payload);
    for p in 0..geo.pages_per_block {
        st.write(0, geo.ppage(geo.cand[0][4], p), &data, &meta, true)?;
    }
    Ok(())
}

/// Lays out user pages (then index pages) into consecutive YaFTL vblocks.
struct FtlWriter<'a> {
    st: &'a mut Store,
    geo: Geometry,
    vblock: usize,
    j: usize,
    usn: u32,
    btoc: Vec<u32>,
    toc: BTreeMap<usize, Vec<u32>>,
}

impl<'a> FtlWriter<'a> {
    fn new(st: &'a mut Store) -> FtlWriter<'a> {
        let geo = st.geo.clone();
        FtlWriter { st, geo, vblock: 3, j: 0, usn: 1, btoc: Vec::new(), toc: BTreeMap::new() }
    }

    fn put(&mut self, lpn: u32, data: &[u8], typ: u8) -> Result<usize> {
        if self.j == 0 {
            if self.vblock >= self.geo.num_blocks {
                return Err(crate::unsupported!("image does not fit: needs more than {} vblocks", self.geo.num_blocks));
            }
            self.btoc.clear();
        }
        let vpn = self.vblock * self.geo.ppsublk + self.j;
        let (cs, pp) = self.geo.vpn_to_phys(vpn);
        self.st.write(cs, pp, data, &spare(lpn, self.usn, typ), false)?;
        self.btoc.push(lpn);
        self.j += 1;
        if self.j == self.geo.data_pages {
            self.close_block(typ)?;
        }
        Ok(vpn)
    }

    fn close_block(&mut self, typ: u8) -> Result<()> {
        let ps = self.geo.page_size;
        let mut table = vec![0xFFu8; self.geo.toc * ps];
        for (i, &l) in self.btoc.iter().enumerate() {
            put32(&mut table, 4 * i, l);
        }
        let meta = spare(UNMAPPED, self.usn, T_CLOSED | if typ == T_INDEX { T_INDEX } else { 0 });
        for i in 0..self.geo.toc {
            let (cs, pp) = self.geo.vpn_to_phys(self.vblock * self.geo.ppsublk + self.j + i);
            self.st.write(cs, pp, &table[i * ps..(i + 1) * ps], &meta, false)?;
        }
        self.vblock += 1;
        self.usn += 1;
        self.j = 0;
        Ok(())
    }

    fn next_block(&mut self) {
        if self.j != 0 {
            self.vblock += 1;
            self.usn += 1;
            self.j = 0;
        }
    }

    fn user(&mut self, lpn: usize, data: &[u8]) -> Result<()> {
        let vpn = self.put(lpn as u32, data, T_USER)?;
        let (t, i) = (lpn / self.geo.toc_entries, lpn % self.geo.toc_entries);
        let entries = self.geo.toc_entries;
        self.toc.entry(t).or_insert_with(|| vec![UNMAPPED; entries])[i] = vpn as u32;
        Ok(())
    }

    fn index_pages(&mut self) -> Result<()> {
        self.next_block();
        let toc = std::mem::take(&mut self.toc);
        for (&t, entries) in &toc {
            let bytes: Vec<u8> = entries.iter().flat_map(|e| e.to_le_bytes()).collect();
            self.put(t as u32, &bytes, T_INDEX)?;
        }
        self.toc = toc;
        self.next_block();
        Ok(())
    }
}

// MARK: - Inputs

/// The logical disk's head as a 7B500 restore leaves it on a K48, up to partition 1 (LBA 63): p1 Apple_HFS system
/// at 63, p3 an 8-sector 0xAF stub one sector past its end, p2 0xAE data to 45 sectors before the exported end
/// (make_mbr; the gaps are measured on one 16 GB unit).
pub fn make_mbr(geo: &Geometry, system_mib: usize) -> Vec<u8> {
    let ps = geo.page_size;
    let n = system_mib * (1 << 20) / ps;
    let mut head = vec![0u8; 63 * ps];
    let p3 = 63 + n + 1;
    let entries = [(0xAFu8, 63, n), (0xAE, p3 + 45, geo.exported_pages - (p3 + 45) - 45), (0xAF, p3, 8)];
    for (i, &(typ, lba, cnt)) in entries.iter().enumerate() {
        let o = 0x1be + 16 * i;
        let chs: [u8; 3] = if lba == 63 { [0x01, 0x01, 0x00] } else { [0xFE, 0xFF, 0xFF] };
        head[o] = 0;
        head[o + 1..o + 4].copy_from_slice(&chs);
        head[o + 4..o + 8].copy_from_slice(&[typ, 0xFE, 0xFF, 0xFF]);
        put32(&mut head, o + 8, lba as u32);
        put32(&mut head, o + 12, cnt as u32);
    }
    head[510] = 0x55;
    head[511] = 0xAA;
    head
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    pub typ: u8,
    pub lba: usize,
    pub count: usize,
}

/// The four MBR entries of a sector 0 (at least 512 bytes).
pub fn partitions(mbr: &[u8]) -> [Partition; 4] {
    std::array::from_fn(|i| {
        let o = 0x1be + 16 * i;
        Partition { typ: mbr[o + 4], lba: le32(mbr, o + 8) as usize, count: le32(mbr, o + 12) as usize }
    })
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i] == needle[0] && &hay[i..i + needle.len()] == needle)
}

/// The "Darwin Kernel Version ..." string (up to 0xff bytes) out of a decrypted kernelcache.
pub fn kernel_version(kernelcache: &Path) -> Result<Vec<u8>> {
    let d = std::fs::read(kernelcache).map_err(|e| crate::internal!("{}: {e}", kernelcache.display()))?;
    let start = find(&d, b"Darwin Kernel Version ", 0)
        .ok_or_else(|| crate::unsupported!("{}: no Darwin Kernel Version string", kernelcache.display()))?;
    let end = d[start..].iter().position(|&b| b == 0).map_or(d.len(), |n| start + n);
    Ok(d[start..end.min(start + 0xff)].to_vec())
}

/// A 32-bit Mach-O's LC_SEGMENTs: (vmaddr, vmsize, fileoff), as KBoot's MachO reads them.
fn segments(d: &[u8]) -> Result<Vec<(u32, u32, u32)>> {
    if d.len() < 28 || le32(d, 0) != 0xFEED_FACE {
        return Err(crate::unsupported!("not a 32-bit Mach-O"));
    }
    let mut segs = Vec::new();
    let mut off = 28;
    for _ in 0..le32(d, 16) {
        if off + 8 > d.len() {
            return Err(crate::unsupported!("Mach-O load commands run past the file"));
        }
        let (cmd, size) = (le32(d, off), le32(d, off + 4) as usize);
        if cmd == 1 {
            if off + 40 > d.len() {
                return Err(crate::unsupported!("Mach-O load commands run past the file"));
            }
            segs.push((le32(d, off + 24), le32(d, off + 28), le32(d, off + 32)));
        }
        if size == 0 {
            break;
        }
        off += size;
    }
    Ok(segs)
}

/// The NAND-signature epoch this kernel's FIL wants (nSig's low byte, '0' + epoch). IOFlashStorage-410.4 (4.3.5)
/// wants 2 and parks a '1' store in "AppleNANDLegacyFTL: epoch roll wait" until a restore rolls it; 410.3 (4.3.0)
/// and earlier have no such getter and take '1'. Read off the getter's own shape in the FIL,
/// `ldr rN, [pc, #imm]; blx rN; adds r0, #0x30; uxtb r0, r0; pop {r7, pc}` (r3 on 4.3.5, r0 on the 5.0 betas), whose
/// literal names `movs r0, #N; bx lr` (8F190 -> 1, 8L1 -> 2, 9A5288d -> 2; 5.0 GM on have no such getter and take 1).
pub fn signature_epoch(kernelcache: &Path) -> Result<u8> {
    let d = std::fs::read(kernelcache).map_err(|e| crate::internal!("{}: {e}", kernelcache.display()))?;
    signature_epoch_of(&d)
}

pub fn signature_epoch_of(d: &[u8]) -> Result<u8> {
    let segs = segments(d)?;
    let shape = [0x30u8, 0x30, 0xC0, 0xB2, 0x80, 0xBD];
    let mut from = 0;
    while let Some(i) = find(d, &shape, from) {
        from = i + 1;
        // ldr rN, [pc, #imm] (0x48 | N); blx rN (0x47, 0x80 | N << 3), N a low register
        if i < 4 || d[i - 3] & 0xF8 != 0x48 || d[i - 2] != 0x80 | (d[i - 3] & 7) << 3 || d[i - 1] != 0x47 {
            continue;
        }
        let pool = ((i - 4) & !3) + 4 + d[i - 4] as usize * 4;
        if pool + 4 > d.len() {
            continue;
        }
        let target = (le32(d, pool) & !1) as u64;
        let Some(&(vmaddr, _, fileoff)) =
            segs.iter().find(|&&(vm, size, _)| vm as u64 <= target && target < vm as u64 + size as u64)
        else {
            continue;
        };
        let o = (fileoff as u64 + (target - vmaddr as u64)) as usize;
        if o + 2 > d.len() || d[o + 1] != 0x20 {
            continue; // movs r0, #N
        }
        return Ok(d[o]);
    }
    Ok(1)
}

/// Restore.plist's DeviceMap SCEP for `board` (the NAND epoch a restore would write), None when it names none.
pub fn restore_epoch(restore_plist: &[u8], board: &str) -> Result<Option<u8>> {
    let p = plist::Value::from_reader(std::io::Cursor::new(restore_plist))
        .map_err(|e| crate::unsupported!("Restore.plist: {e}"))?;
    let maps: Vec<&plist::Dictionary> = p
        .as_dictionary()
        .and_then(|d| d.get("DeviceMap"))
        .and_then(plist::Value::as_array)
        .map(|a| a.iter().filter_map(plist::Value::as_dictionary).collect())
        .unwrap_or_default();
    let board = board.to_lowercase();
    let map = maps
        .iter()
        .find(|m| m.get("BoardConfig").and_then(plist::Value::as_string).map(str::to_lowercase) == Some(board.clone()))
        .or(maps.first());
    Ok(map.and_then(|m| m.get("SCEP")).and_then(|v| {
        v.as_signed_integer()
            .or_else(|| v.as_unsigned_integer().map(|u| u as i64))
            .or_else(|| v.as_real().map(|r| r as i64))
            .map(|n| n as u8)
    }))
}

/// restore_epoch over the IPSW's Restore.plist.
pub fn restore_epoch_of(ipsw: &crate::ipsw::Ipsw, board: &str) -> Result<Option<u8>> {
    restore_epoch(&ipsw.read("Restore.plist")?, board)
}

/// Byte patches, by page: (offset in the page, new bytes).
type Patch = HashMap<usize, Vec<(usize, Vec<u8>)>>;

/// A page-granular reader over a (possibly sparse) raw image, with in-place byte patches.
struct FilePages {
    file: File,
    path: PathBuf,
    page: usize,
    pages: usize,
    patch: Patch,
    buf: Vec<u8>,
}

impl FilePages {
    fn open(path: &Path, page: usize, patch: Patch) -> Result<FilePages> {
        let file = File::open(path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
        let size = file.metadata().map_err(|e| crate::internal!("{}: {e}", path.display()))?.len() as usize;
        Ok(FilePages { file, path: path.to_path_buf(), page, pages: size.div_ceil(page), patch, buf: vec![0; page] })
    }

    /// Page numbers inside the file's data extents, in order: the pages a sparse volume ever had written.
    fn written(&self) -> Result<Vec<usize>> {
        let ext =
            k48nand_image::data_extents(&self.file).map_err(|e| crate::internal!("{}: {e}", self.path.display()))?;
        let p = self.page as u64;
        Ok(ext.iter().flat_map(|&(s, e)| (s / p) as usize..e.div_ceil(p) as usize).collect())
    }

    /// Page `n`, zero-filled past the end of the file.
    fn get(&mut self, n: usize) -> Result<&[u8]> {
        let mut got = 0;
        while got < self.page {
            match posio::read_at(&self.file, &mut self.buf[got..], (n * self.page + got) as u64) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(crate::internal!("{}: read: {e}", self.path.display())),
            }
        }
        self.buf[got..].fill(0);
        if let Some(patches) = self.patch.get(&n) {
            for (o, new) in patches {
                self.buf[*o..*o + new.len()].copy_from_slice(new);
            }
        }
        Ok(&self.buf)
    }
}

/// In-place, same-length /dev/disk0s2s1 -> /dev/disk0s2 rewrite of every etc/fstab copy on the volume (the data
/// partition is a plain Apple_HFS slice here, not an encrypted container's).
fn fstab_patch(system: &Path, page: usize) -> Result<Patch> {
    let old: &[u8] = b"/dev/disk0s2s1 /private/var";
    let new: &[u8] = b"/dev/disk0s2   /private/var";
    let f = File::open(system).map_err(|e| crate::internal!("{}: {e}", system.display()))?;
    let size = f.metadata().map_err(|e| crate::internal!("{}: {e}", system.display()))?.len() as usize;
    let chunk = 8 << 20;
    let mut buf = vec![0u8; chunk + old.len()];
    let mut patch = Patch::new();
    let mut at = 0;
    while at < size {
        // each window overlaps the next by one pattern length, so a hit across a chunk edge is seen once
        let n = (chunk + old.len()).min(size - at);
        posio::read_exact_at(&f, &mut buf[..n], at as u64)
            .map_err(|e| crate::internal!("{}: {e}", system.display()))?;
        let mut from = 0;
        while let Some(hit) = find(&buf[..n], old, from) {
            if hit >= chunk {
                break; // the next window's
            }
            let pos = at + hit;
            let (pg, o) = (pos / page, pos % page);
            if o + new.len() > page {
                return Err(crate::internal!("{}: an fstab line crosses a page", system.display()));
            }
            patch.entry(pg).or_default().push((o, new.to_vec()));
            from = hit + 1;
        }
        at += chunk;
    }
    Ok(patch)
}

// MARK: - Build

pub enum DataVolume {
    None,
    /// A (sparse) raw HFS+ image: only the pages in its data extents are laid out.
    Image(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildResult {
    pub records: usize,
    pub vblocks_used: usize,
    pub toc_pages: usize,
}

/// build's inputs; `Build::new` gives FirmwareKit's defaults for the rest.
pub struct Build {
    pub geometry: Geometry,
    /// The disk head up to partition 1 (make_mbr's output, or a store's own read back).
    pub mbr: PathBuf,
    pub kernel_version: Vec<u8>,
    pub epoch: u8,
    pub system: PathBuf,
    pub s3: Option<PathBuf>,
    pub data: DataVolume,
    pub out: PathBuf,
    pub force: bool,
    pub whitening: bool,
    /// The recipe's nand_sig_flags; None: SIG_FLAGS whitened, PLAIN_SIG_FLAGS plain.
    pub sig_flags: Option<u32>,
    pub format: StoreFormat,
}

impl Build {
    pub fn new(mbr: &Path, kernel_version: Vec<u8>, system: &Path, data: DataVolume, out: &Path) -> Build {
        Build {
            geometry: Geometry::k48_with_16gb(),
            mbr: mbr.to_path_buf(),
            kernel_version,
            epoch: 1,
            system: system.to_path_buf(),
            s3: None,
            data,
            out: out.to_path_buf(),
            force: false,
            whitening: true,
            sig_flags: None,
            format: StoreFormat::Image,
        }
    }
}

/// ipad1_nand.py build: the store for `system` (+ `s3` + the data volume) at the MBR's partitions, into `out`.
pub fn build(b: &Build, log: &dyn Fn(&str)) -> Result<BuildResult> {
    let geo = &b.geometry;
    let out = &b.out;
    if out.join("geometry.json").exists() && !b.force {
        return Err(crate::internal!("{} exists; pass force to overwrite", out.display()));
    }
    if b.force
        && let Ok(names) = std::fs::read_dir(out)
    {
        for entry in names.flatten() {
            let n = entry.file_name().to_string_lossy().into_owned();
            if n == "geometry.json" || n.ends_with(".pages") {
                std::fs::remove_file(entry.path()).map_err(|e| crate::internal!("{}: {e}", entry.path().display()))?;
            }
        }
    }
    let ps = geo.page_size;
    if ps != 4096 {
        return Err(crate::unsupported!(
            "{} has {ps}-byte pages; the device MBR/partitions are 4 KiB-sectored",
            geo.name
        ));
    }
    let mut head = std::fs::read(&b.mbr).map_err(|e| crate::internal!("{}: {e}", b.mbr.display()))?;
    if head.len() < 512 || head[510] != 0x55 || head[511] != 0xAA {
        return Err(crate::unsupported!("{}: no MBR signature", b.mbr.display()));
    }
    let parts = partitions(&head);
    let (p1, mut p2, p3) = (parts[0], parts[1], parts[2]);
    let patch = match b.data {
        DataVolume::None => Patch::new(),
        DataVolume::Image(_) => fstab_patch(&b.system, ps)?,
    };
    let mut sys = FilePages::open(&b.system, ps, patch)?;
    if sys.pages != p1.count {
        log(&format!("warning: system image is {} pages, partition 1 is {}", sys.pages, p1.count));
    }
    let mut data_pages = match &b.data {
        DataVolume::None => None,
        DataVolume::Image(u) => Some(FilePages::open(u, ps, Patch::new())?),
    };
    if let Some(d) = &data_pages {
        // EncryptedMediaFilter refuses an 0xAE partition without a key block: plain Apple_HFS at /dev/disk0s2
        head[0x1be + 16 + 4] = 0xAF;
        put32(&mut head, 0x1be + 16 + 12, d.pages as u32);
        p2 = Partition { typ: 0xAF, lba: p2.lba, count: d.pages };
    }
    if p2.lba + p2.count > geo.exported_pages {
        return Err(crate::unsupported!(
            "partition 2 ends at {} > exported {} sectors",
            p2.lba + p2.count,
            geo.exported_pages
        ));
    }

    let mut st = Store::create(out, geo, !b.whitening, b.format)?;
    write_metadata(&mut st, geo, &b.kernel_version, b.epoch, b.sig_flags)?;
    let mut ftl = FtlWriter::new(&mut st);
    // LPN == 4 KiB LBA, segments in ascending LBA order
    for n in 0..p1.lba.min(head.len() / ps) {
        ftl.user(n, &head[n * ps..(n + 1) * ps])?;
    }
    for n in 0..sys.pages {
        ftl.user(p1.lba + n, sys.get(n)?)?;
    }
    if let Some(s3) = &b.s3
        && p3.count > 0
    {
        let mut f = FilePages::open(s3, ps, Patch::new())?;
        for n in 0..p3.count.min(f.pages) {
            ftl.user(p3.lba + n, f.get(n)?)?;
        }
    }
    if let Some(d) = &mut data_pages {
        let w = d.written()?;
        log(&format!("data partition: {} pages, {} written (the rest are holes)", d.pages, w.len()));
        for n in w {
            ftl.user(p2.lba + n, d.get(n)?)?;
        }
    }
    ftl.index_pages()?;
    let (vblocks_used, toc_pages) = (ftl.vblock, ftl.toc.len());
    drop(ftl);
    // the chip files close here; their writes went straight to the files (no buffering to flush)
    let records = st.records;
    drop(st);
    log(&format!(
        "wrote {}: {records} pages, {vblocks_used} user/index vblocks used of {}, {toc_pages} TOC pages",
        out.display(),
        geo.num_blocks
    ));
    Ok(BuildResult { records, vblocks_used, toc_pages })
}

/// A stopped edit's store: `system` and `data` laid out fresh into `out` as `build` lays out a prepared store, with
/// the MBR, the third partition and the signature (epoch, flags, kernel version, whitening) that `store` plus
/// `overlay` hold now. Whatever YaFTL state the guest left goes; the device restores its map on boot as it does
/// from a prepared store. `work` keeps the MBR and third partition read back.
pub fn rebuild(
    store: &Path,
    overlay: Option<&Path>,
    system: &Path,
    data: Option<&Path>,
    out: &Path,
    work: &Path,
    log: &dyn Fn(&str),
) -> Result<BuildResult> {
    let geo = crate::k48nand_check::geometry(store)?;
    let st = StoreReader::open(store, &geo, overlay)?;
    let sig = st.signature()?.ok_or_else(|| crate::unsupported!("{}: no NAND signature", store.display()))?;
    let map = st.yaftl_map()?;
    let page = |lpn: usize| -> Result<Vec<u8>> {
        if lpn < map.len()
            && map[lpn] != 0
            && let Some((d, _)) = st.read_vpn(map[lpn] as usize - 1)?
        {
            return Ok(d);
        }
        Ok(vec![0; geo.page_size])
    };
    let parts = partitions(&page(0)?);
    let write = |name: &str, range: std::ops::Range<usize>| -> Result<PathBuf> {
        let url = work.join(name);
        let mut bytes = Vec::with_capacity(range.len() * geo.page_size);
        for lpn in range {
            bytes.extend_from_slice(&page(lpn)?);
        }
        std::fs::write(&url, bytes).map_err(|e| crate::internal!("{}: {e}", url.display()))?;
        Ok(url)
    };
    let mbr = write("mbr.bin", 0..parts[0].lba)?;
    let s3 = if parts[2].typ != 0 && parts[2].count > 0 {
        Some(write("s3.img", parts[2].lba..parts[2].lba + parts[2].count)?)
    } else {
        None
    };
    let data = data.map_or(DataVolume::None, |d| DataVolume::Image(d.to_path_buf()));
    let mut b = Build::new(&mbr, sig.kernel_version, system, data, out);
    b.geometry = geo;
    b.epoch = sig.epoch;
    b.s3 = s3;
    b.whitening = !st.plain;
    b.sig_flags = Some(sig.flags);
    build(&b, log)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::k48nand_check;
    use sha2::{Digest, Sha256};
    use std::io::Read;

    pub(crate) fn temp(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("k48nand-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    #[test]
    fn geometry_matches_kernel() {
        let g = Geometry::k48_with_16gb();
        assert!(g.usable == 1952 && g.toc == 2 && g.data_pages == 2046);
        assert_eq!(g.exported_pages, 3_925_449); // the real 16 GB unit's sector count
    }

    /// 3.0's layout of the same part (vendor type 0x10001): one VFL bank, 1024-page superblocks, a one-page block TOC.
    #[test]
    fn one_bank_geometry_for_ios30() {
        let g = Geometry::k48_with_16gb_v1();
        assert!(g.vfl_banks == 1 && g.ppsublk == 1024 && g.toc == 1 && g.data_pages == 1023);
        assert!(
            g.json().contains("\"vendor_type\": 65537") && !Geometry::k48_with_16gb().json().contains("vendor_type")
        );
        assert_eq!(Geometry::for_vendor_type(Some(0x10001)), g);
        assert_eq!(Geometry::for_vendor_type(None).name, "k48-16g");
    }

    /// KBootTests.kernel(at:): a Mach-O with __TEXT at base + 0x1000 from file offset 0.
    fn kernel(base: u32) -> Vec<u8> {
        fn le(v: &[u32]) -> Vec<u8> {
            v.iter().flat_map(|w| w.to_le_bytes()).collect()
        }
        fn seg(name: &str, vm: u32, vs: u32, fo: u32, fs: u32) -> Vec<u8> {
            let mut n = [0u8; 16];
            n[..name.len()].copy_from_slice(name.as_bytes());
            [le(&[1, 56]), n.to_vec(), le(&[vm, vs, fo, fs, 7, 7, 0, 0])].concat()
        }
        let mut thread = vec![5, 16 + 64, 1, 16];
        thread.extend([0u32; 15]);
        thread.push(base + 0x1040);
        let cmds = [
            seg("__TEXT", base + 0x1000, 0x2000, 0, 0x2000),
            seg("__DATA", base + 0x3000, 0x1800, 0x2000, 0x10),
            le(&thread),
        ]
        .concat();
        let mut k = [le(&[0xFEED_FACE, 12, 9, 2, 3, cmds.len() as u32, 0]), cmds].concat();
        k.resize(0x2000, 0);
        k.extend([0x44u8; 16]);
        k
    }

    /// The FIL's epoch getter, `ldr rN, [pc, #8]; blx rN; adds r0, #0x30; uxtb r0, r0; pop {r7, pc}` naming
    /// `movs r0, #2; bx lr`: r3 as on 4.3.5, r0 as on the 5.0 betas; no getter (4.3.0, 5.0 GM on) is epoch 1.
    #[test]
    fn signature_epoch_from_the_getter() -> Result<()> {
        for (reg, want) in [(Some(3u8), 2u8), (Some(0), 2), (None, 1)] {
            let mut k = kernel(0x8000_0000);
            if let Some(reg) = reg {
                k[0x200..0x208].copy_from_slice(&[0x02, 0x48 | reg, 0x80 | reg << 3, 0x47, 0x30, 0x30, 0xC0, 0xB2]);
                k[0x208..0x20a].copy_from_slice(&[0x80, 0xBD]);
                k[0x20c..0x210].copy_from_slice(&0x8000_1301u32.to_le_bytes()); // the getter, Thumb
                k[0x300..0x304].copy_from_slice(&[0x02, 0x20, 0x70, 0x47]); // movs r0, #2; bx lr
            }
            let dir = temp("epoch");
            let url = dir.join("kernelcache.mach");
            std::fs::write(&url, &k).map_err(crate::Error::from)?;
            assert_eq!(signature_epoch(&url)?, want, "reg {reg:?}");
            std::fs::remove_dir_all(&dir).map_err(crate::Error::from)?;
        }
        Ok(())
    }

    #[test]
    fn restore_epoch_reads_the_boards_scep() -> Result<()> {
        let plist = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>DeviceMap</key><array>
<dict><key>BoardConfig</key><string>n81ap</string><key>SCEP</key><integer>1</integer></dict>
<dict><key>BoardConfig</key><string>n88ap</string><key>SCEP</key><integer>2</integer></dict>
</array></dict></plist>"#;
        assert_eq!(restore_epoch(plist, "N88AP")?, Some(2));
        assert_eq!(restore_epoch(plist, "k48ap")?, Some(1)); // none named: the first map
        Ok(())
    }

    /// make_mbr reproduces the 16 GB unit's sector 0.
    #[test]
    fn mbr_matches_unit() {
        let head = make_mbr(&Geometry::k48_with_16gb(), 1280);
        let unit = hex::decode(
            "00010100affeffff3f00000000000500".to_owned()
                + "00feffffaefeffff6d0005002fe53600"
                + "00feffffaffeffff4000050008000000",
        )
        .expect("hex");
        assert!(head[0x1be..0x1ee] == unit[..] && head[510] == 0x55 && head[511] == 0xAA);
        assert!(head[..0x1be].iter().chain(&head[0x1ee..510]).chain(&head[512..]).all(|&b| b == 0));
    }

    fn mbr_with(entries: [(u8, u32, u32); 3]) -> Vec<u8> {
        let mut mbr = vec![0u8; 4096 * 63];
        mbr[510] = 0x55;
        mbr[511] = 0xAA;
        for (i, (typ, lba, cnt)) in entries.into_iter().enumerate() {
            let o = 0x1be + 16 * i;
            mbr[o + 4] = typ;
            put32(&mut mbr, o + 8, lba);
            put32(&mut mbr, o + 12, cnt);
        }
        mbr
    }

    fn sha256_prefix(dir: &Path) -> String {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("store")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 8 << 20];
        for n in names {
            let mut f = File::open(dir.join(n)).expect("open");
            loop {
                let k = f.read(&mut buf).expect("read");
                if k == 0 {
                    break;
                }
                hash.update(&buf[..k]);
            }
        }
        hex::encode(hash.finalize())[..16].to_string()
    }

    /// A small store's bytes, whitened and plain, are the ones FirmwareKit's K48NANDTests recorded
    /// (storeBytesAreUnchanged): the Swift writer's output, byte for byte.
    #[test]
    fn store_bytes_match_swift() -> Result<()> {
        for (whitening, recorded) in [(true, "219fe8ceb8e5a1dc"), (false, "6bf30b844fe046c4")] {
            let dir = temp("bytes");
            let ps = 4096usize;
            let mbr = dir.join("mbr");
            let system = dir.join("system.img");
            std::fs::write(&mbr, mbr_with([(0xAF, 63, 300), (0xAE, 400, 100), (0xAF, 363, 8)]))?;
            // Swift's UInt8(truncatingIfNeeded: $0 &* 2_654_435_761 >> 13): a shift binds tighter than &* there
            let sys: Vec<u8> = (0..ps * 300).map(|i| i.wrapping_mul(2_654_435_761 >> 13) as u8).collect();
            std::fs::write(&system, sys)?;
            let out = dir.join("store");
            let mut b = Build::new(&mbr, b"Darwin Kernel Version selfcheck".to_vec(), &system, DataVolume::None, &out);
            b.geometry = Geometry::selfcheck();
            b.whitening = whitening;
            build(&b, &|_| {})?;
            let got = sha256_prefix(&out);
            std::fs::remove_dir_all(&dir)?;
            assert_eq!(got, recorded, "whitening {whitening}");
        }
        Ok(())
    }

    /// Small synthetic store (the selfcheck geometry), including a sparse data image and an fstab line: the checker
    /// accepts it and its index pages map exactly; whitened (K48, N81) and plain (N18). Then rebuild reads its MBR,
    /// third partition and signature back and lays out the same store again.
    #[test]
    fn synthetic_store() -> Result<()> {
        for whitening in [true, false] {
            let dir = temp("synthetic");
            let ps = 4096usize;
            let mbr = mbr_with([(0xAF, 63, 700), (0xAE, 800, 4000), (0xAF, 763, 8)]);
            let mut seed = 0x2545_F491_4F6C_DD1Du64;
            let mut sys: Vec<u8> = (0..ps * 700)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            sys[1024] = b'H';
            sys[1025] = b'X';
            let fstab = b"/dev/disk0s1 / hfs rw 0 1\n/dev/disk0s2s1 /private/var hfs rw 0 2\n";
            sys[ps * 300..ps * 300 + fstab.len()].copy_from_slice(fstab);
            let paths: Vec<PathBuf> = ["mbr", "system.img", "s3.bin", "data.img"].iter().map(|n| dir.join(n)).collect();
            std::fs::write(&paths[0], &mbr)?;
            std::fs::write(&paths[1], &sys)?;
            std::fs::write(&paths[2], vec![0x5Au8; ps * 8])?;
            // sparse data volume: 4000 pages, a header and two islands of data
            let f = posio::create_sparse(&paths[3])?;
            f.set_len((ps * 4000) as u64)?;
            for (page, n) in [(0usize, 3usize), (1000, 5), (3990, 2)] {
                let bytes: Vec<u8> = (0..n * ps).map(|i| (i * 7 + page) as u8).collect();
                posio::write_all_at(&f, &bytes, (page * ps) as u64)?;
            }
            posio::write_all_at(&f, b"H+", 1024)?;
            drop(f);

            let kv = b"Darwin Kernel Version selfcheck".to_vec();
            let mine = dir.join("rust");
            let mut b = Build::new(&paths[0], kv, &paths[1], DataVolume::Image(paths[3].clone()), &mine);
            b.geometry = Geometry::selfcheck();
            b.s3 = Some(paths[2].clone());
            b.whitening = whitening;
            let res = build(&b, &|_| {})?;
            let lines = std::cell::RefCell::new(Vec::<String>::new());
            let passed =
                k48nand_check::check(&mine, Some(&paths[0]), Some(&paths[1]), &|l| lines.borrow_mut().push(l.into()))?;
            let lines = lines.into_inner();
            assert!(passed, "{:?}", lines.iter().filter(|l| l.starts_with("FAIL")).collect::<Vec<_>>());
            assert!(lines.iter().any(|l| l.contains("index pages map exactly")));
            assert!(lines.iter().any(|l| l.contains(if whitening { "whitening on" } else { "whitening off" })));

            // the fstab went over as /dev/disk0s2, the store reads back as written
            let st = StoreReader::open(&mine, &Geometry::selfcheck(), None)?;
            let map = st.yaftl_map()?;
            let (d, _) = st.read_vpn(map[63 + 300] as usize - 1)?.expect("fstab page");
            assert!(find(&d, b"/dev/disk0s2   /private/var", 0).is_some());
            let sig = st.signature()?.expect("signature");
            assert_eq!((sig.epoch, sig.flags), (1, if whitening { SIG_FLAGS } else { PLAIN_SIG_FLAGS }));

            // rebuild from the store's own MBR, s3 and signature: the same store
            let again = dir.join("again");
            let work = dir.join("work");
            std::fs::create_dir_all(&work)?;
            let res2 = rebuild(&mine, None, &paths[1], Some(&paths[3]), &again, &work, &|_| {})?;
            assert_eq!(res, res2);
            assert_eq!(sha256_prefix(&mine), sha256_prefix(&again));
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

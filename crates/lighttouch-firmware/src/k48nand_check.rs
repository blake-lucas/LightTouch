//! A K48 NAND store read back the way the kernel would, and ipad1_nand.py check: special pages, VFL contexts, a
//! YaFTL R/O-restore walk over every vblock, the index pages, then the MBR and HFS headers. Port of FirmwareKit's
//! K48NANDCheck (StoreReader, the store format and geometry readers, check) and VolumeRebuild.yaftlMap.
//!
//! ```ignore
//! let geo = k48nand_check::geometry(&nand)?;                       // the known geometry geometry.json names
//! let st = StoreReader::open(&nand, &geo, overlay.as_deref())?;   // as the IOP serves it
//! let sig = st.signature()?;                                       // epoch, flags, kernel version
//! let map = st.yaftl_map()?;                                       // lpn -> vpn + 1
//! let ok = k48nand_check::check(&nand, Some(&mbr), Some(&system), &|line| eprintln!("{line}"))?;
//! ```
use crate::error::Result;
use crate::k48nand::{
    self, CTRL_BLOCKS, Geometry, META, NSIG_BASE, SIG_FLAGS, StoreFormat, T_CLOSED, T_INDEX, T_USER, T_VFL, UNMAPPED,
    VFL_BLOCKS, le32,
};
use crate::k48nand_image;
use crate::posio;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

fn store_format_of(value: Option<&Value>, layout: Option<&Value>, dir: &Path) -> Result<StoreFormat> {
    match value {
        None => return Ok(StoreFormat::Legacy),
        Some(Value::String(s)) if s == StoreFormat::Legacy.as_str() => return Ok(StoreFormat::Legacy),
        Some(Value::String(s))
            if s == StoreFormat::Image.as_str()
                && layout.and_then(Value::as_str) == Some(k48nand::GENERATED_LAYOUT) =>
        {
            return Ok(StoreFormat::Image);
        }
        _ => {}
    }
    Err(crate::unsupported!(
        "{}: unsupported NAND storage_format {}; physical NAND export requires native VFL/crypto mapping; use guest filesystem services",
        dir.display(),
        value.map_or("nil".into(), |v| v.to_string())
    ))
}

fn geometry_object(dir: &Path) -> Result<Map<String, Value>> {
    let path = dir.join("geometry.json");
    let bytes = std::fs::read(&path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(g)) => Ok(g),
        Ok(_) => Err(crate::unsupported!("{}: geometry.json must contain an object", dir.display())),
        Err(e) => Err(crate::unsupported!("{}: geometry.json: {e}", dir.display())),
    }
}

/// How a store's records sit in its files (geometry.json "storage_format"). A generated-layout image store or a
/// legacy one; anything else (a physical store qemu-ios's restore wrote, an unknown format) is refused.
pub fn store_format(dir: &Path) -> Result<StoreFormat> {
    let g = geometry_object(dir)?;
    store_format_of(g.get("storage_format"), g.get("layout"), dir)
}

fn int(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    v.as_i64().or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
}

/// The known geometry a store's geometry.json describes.
pub fn geometry(dir: &Path) -> Result<Geometry> {
    let g = geometry_object(dir)?;
    store_format_of(g.get("storage_format"), g.get("layout"), dir)?;
    let chip = g
        .get("chip_id")
        .and_then(Value::as_str)
        .and_then(|s| u32::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok());
    let vendor = int(g.get("vendor_type")).unwrap_or(0x100014);
    Geometry::known()
        .into_iter()
        .find(|geo| {
            int(g.get("buses")) == Some(geo.buses as i64)
                && int(g.get("ce_per_bus")) == Some(geo.ce_per_bus as i64)
                && int(g.get("blocks_per_ce")) == Some(geo.blocks_per_ce as i64)
                && int(g.get("pages_per_block")) == Some(geo.pages_per_block as i64)
                && int(g.get("page_bytes")) == Some(geo.page_size as i64)
                && int(g.get("spare_bytes")) == Some(geo.spare_bytes as i64)
                && vendor == geo.vendor_type as i64
                && chip == Some(geo.chip_id)
        })
        .ok_or_else(|| crate::unsupported!("{}: no known geometry matches geometry.json", dir.display()))
}

/// NANDDRIVERSIGN's payload, as the writer lays it out on cs0's signature block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub epoch: u8,
    pub flags: u32,
    pub kernel_version: Vec<u8>,
}

/// A store read the way the IOP serves it: with an overlay (s5l8930_iop.c "nand-overlay"), a page whose bit is set
/// in bus<b>-ce<c>.dirty comes from the overlay's .pages file (a hole there = erased), else the base. An image store
/// whose overlay holds its live clone (`overlay/nand`) is read from that clone alone: it is the device's NAND.
pub struct StoreReader {
    pub geo: Geometry,
    stride: usize,
    files: Vec<File>,
    overlay: Vec<File>,
    dirty: Vec<Vec<u8>>,
    /// Records are stored XOR 0xFF (StoreFormat::Image), so a hole reads as the erased page.
    xor_ff: bool,
    /// From the signature's flags (PLAIN_SIG_FLAGS: an S5L8920 store): meta is not whitened.
    pub plain: bool,
}

impl StoreReader {
    pub fn open(dir: &Path, geo: &Geometry, overlay: Option<&Path>) -> Result<StoreReader> {
        if geometry(dir)?.json() != geo.json() {
            return Err(crate::unsupported!("{}: reader geometry does not match geometry.json", dir.display()));
        }
        let format = store_format(dir)?;
        let mut dir: PathBuf = dir.to_path_buf();
        let mut overlay: Option<PathBuf> = overlay.map(Path::to_path_buf);
        if format == StoreFormat::Image
            && let Some(live) = overlay.as_ref().map(|o| o.join("nand")).filter(|l| l.exists())
        {
            if store_format(&live)? != StoreFormat::Image || geometry(&live)?.json() != geo.json() {
                return Err(crate::unsupported!("{}: the live NAND is not its base's format", live.display()));
            }
            dir = live;
            overlay = None;
        }
        if let Some(o) = &overlay {
            let marker = o.join("storage-format");
            if marker.exists() {
                let saved =
                    std::fs::read_to_string(&marker).map_err(|e| crate::internal!("{}: {e}", marker.display()))?;
                if saved.trim() != format.as_str() {
                    return Err(crate::unsupported!(
                        "{}: NAND overlay format {} is not its base's",
                        o.display(),
                        saved.trim()
                    ));
                }
            }
        }
        let stride = geo.page_size + geo.spare_bytes;
        let names = |ext: &str| -> Vec<String> { (0..geo.num_cs).map(|i| k48nand_image::name(i, geo, ext)).collect() };
        let open_all = |d: &Path, kind: &str| -> Result<Vec<File>> {
            let page_file_bytes = (geo.blocks_per_ce * geo.pages_per_block * stride) as u64;
            names("pages")
                .iter()
                .map(|n| {
                    let p = d.join(n);
                    let f = File::open(&p).map_err(|e| crate::internal!("{}: {e}", p.display()))?;
                    let len = f.metadata().map_err(|e| crate::internal!("{}: {e}", p.display()))?.len();
                    if len != page_file_bytes {
                        return Err(crate::unsupported!("{}: invalid {kind} file size for NAND geometry", d.display()));
                    }
                    Ok(f)
                })
                .collect()
        };
        let files = open_all(&dir, "pages")?;
        if let Some(o) = &overlay {
            let entries: Vec<String> = std::fs::read_dir(o)
                .map_err(|e| crate::internal!("{}: {e}", o.display()))?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            if entries.iter().any(|e| e.ends_with(".pages") || e.ends_with(".dirty"))
                && !entries.iter().any(|e| e == "bus0-ce0.dirty")
            {
                return Err(crate::unsupported!(
                    "{}: NAND overlay has page data without its dirty bitmap",
                    o.display()
                ));
            }
        }
        let o = overlay.filter(|o| o.join("bus0-ce0.dirty").exists());
        let (overlay_files, dirty) = match &o {
            None => (Vec::new(), Vec::new()),
            Some(o) => {
                let files = open_all(o, "overlay pages")?;
                let bitmap_bytes = (geo.blocks_per_ce * geo.pages_per_block).div_ceil(8);
                let mut dirty = Vec::with_capacity(geo.num_cs);
                for n in names("dirty") {
                    let p = o.join(n);
                    let d = std::fs::read(&p).map_err(|e| crate::internal!("{}: {e}", p.display()))?;
                    if d.len() != bitmap_bytes {
                        return Err(crate::unsupported!(
                            "{}: invalid dirty bitmap file size for NAND geometry",
                            o.display()
                        ));
                    }
                    dirty.push(d);
                }
                (files, dirty)
            }
        };
        let mut st = StoreReader {
            geo: geo.clone(),
            stride,
            files,
            overlay: overlay_files,
            dirty,
            xor_ff: format == StoreFormat::Image,
            plain: false,
        };
        st.plain = st.signature()?.is_some_and(|s| s.flags & 0x10000 == 0);
        Ok(st)
    }

    /// NANDDRIVERSIGN's payload, as the writer lays it out on cs0's signature block; None without one.
    pub fn signature(&self) -> Result<Option<Signature>> {
        let Some((d, _)) = self.read(0, self.geo.ppage(self.geo.cand[0][4], 0), false)? else { return Ok(None) };
        if !d.starts_with(b"NANDDRIVERSIGN") {
            return Ok(None);
        }
        Ok(Some(Signature {
            epoch: (le32(&d, 0x38) as u8).wrapping_sub(0x30),
            flags: le32(&d, 0x3c),
            kernel_version: d[0x40..0x140].iter().take_while(|&&b| b != 0).copied().collect(),
        }))
    }

    /// The file serving (cs, ppage).
    fn file(&self, cs: usize, ppage: usize) -> &File {
        let (b, c) = self.geo.bus_ce(cs);
        let i = b * self.geo.ce_per_bus + c;
        if !self.dirty.is_empty() && ppage / 8 < self.dirty[i].len() && self.dirty[i][ppage / 8] >> (ppage % 8) & 1 != 0
        {
            return &self.overlay[i];
        }
        &self.files[i]
    }

    /// The un-whitened 12-byte meta of (cs, ppage), None if the page is blank. Reads only the spare.
    pub fn meta(&self, cs: usize, ppage: usize) -> Result<Option<[u8; META]>> {
        if ppage >= self.geo.pages_per_ce {
            return Ok(None);
        }
        let mut m = [0u8; META];
        posio::read_exact_at(self.file(cs, ppage), &mut m, (ppage * self.stride + self.geo.page_size) as u64)
            .map_err(|e| crate::internal!("NAND read: {e}"))?;
        if self.xor_ff {
            m.iter_mut().for_each(|b| *b ^= 0xFF);
        }
        if m.iter().all(|&b| b == 0) || m.iter().all(|&b| b == 0xFF) {
            return Ok(None);
        }
        Ok(Some(if self.plain { m } else { k48nand::whiten(&m, ppage) }))
    }

    /// (data, 12-byte meta, un-whitened unless raw), None if the page is blank.
    pub fn read(&self, cs: usize, ppage: usize, raw: bool) -> Result<Option<(Vec<u8>, [u8; META])>> {
        if ppage >= self.geo.pages_per_ce {
            return Ok(None);
        }
        let mut rec = vec![0u8; self.stride];
        posio::read_exact_at(self.file(cs, ppage), &mut rec, (ppage * self.stride) as u64)
            .map_err(|e| crate::internal!("NAND read: {e}"))?;
        if self.xor_ff {
            k48nand_image::invert(&mut rec);
        }
        let ps = self.geo.page_size;
        let sp = &rec[ps..];
        if sp.iter().all(|&b| b == 0) || (sp.iter().all(|&b| b == 0xFF) && rec[..ps].iter().all(|&b| b == 0xFF)) {
            return Ok(None);
        }
        let mut m = [0u8; META];
        m.copy_from_slice(&sp[..META]);
        let meta = if raw || self.plain { m } else { k48nand::whiten(&m, ppage) };
        rec.truncate(ps);
        Ok(Some((rec, meta)))
    }

    pub fn read_vpn(&self, vpn: usize) -> Result<Option<(Vec<u8>, [u8; META])>> {
        let (cs, p) = self.geo.vpn_to_phys(vpn);
        self.read(cs, p, false)
    }

    /// lpn -> vpn + 1 (0 = unmapped) of the newest copy of every user page (VolumeRebuild.yaftlMap).
    pub fn yaftl_map(&self) -> Result<Vec<u32>> {
        let geo = &self.geo;
        let mut vpn_of = vec![0u32; geo.total_pages];
        let mut usn_of = vec![0u32; geo.total_pages];
        for v in (0..geo.num_blocks).filter(|v| !CTRL_BLOCKS.contains(v)) {
            for j in 0..geo.ppsublk {
                let vpn = v * geo.ppsublk + j;
                let (cs, pp) = geo.vpn_to_phys(vpn);
                // pages of a vblock are programmed in order
                let Some(m) = self.meta(cs, pp)? else { break };
                let typ = m[9];
                if typ & T_USER == 0 || typ & (T_INDEX | T_CLOSED) != 0 {
                    continue;
                }
                let lpn = le32(&m, 0) as usize;
                let usn = le32(&m, 4);
                if lpn >= geo.total_pages {
                    continue;
                }
                // equal USN: the later vpn (a later page of the same block) wins
                if vpn_of[lpn] == 0 || usn > usn_of[lpn] || (usn == usn_of[lpn] && (vpn + 1) as u32 > vpn_of[lpn]) {
                    vpn_of[lpn] = (vpn + 1) as u32;
                    usn_of[lpn] = usn;
                }
            }
        }
        Ok(vpn_of)
    }
}

fn hex(v: impl std::fmt::LowerHex) -> String {
    format!("0x{v:x}")
}

/// Checks a store; `log` gets every "ok"/"FAIL" line. Returns true when nothing failed.
pub fn check(dir: &Path, mbr: Option<&Path>, system: Option<&Path>, log: &dyn Fn(&str)) -> Result<bool> {
    let geo = geometry(dir)?;
    let mut st = StoreReader::open(dir, &geo, None)?;
    let mut r = Report { fails: Vec::new(), log };
    fn bit(bbt: &[u8], b: usize) -> bool {
        bbt[b / 8] >> (b % 8) & 1 != 0
    }
    fn magic(d: &[u8], m: &str) -> bool {
        let mut want = m.as_bytes().to_vec();
        want.resize(16, 0);
        d.len() >= 16 && d[..16] == want[..]
    }

    // special pages: _ReadSpecialBlock's scan window, top block down
    let mut sig_block: Option<usize> = None;
    for cs in 0..geo.num_cs {
        let mut found: Option<(usize, Vec<usize>, Vec<u8>)> = None;
        for blk in (geo.blocks_per_ce - geo.blocks_per_ce / 10..geo.blocks_per_ce).rev() {
            if let Some((d, _)) = st.read(cs, geo.ppage(blk, 0), false)?
                && magic(&d, "DEVICEINFOBBT")
            {
                let n = (le32(&d, 0x34) as usize).min(d.len() - 0x38);
                found =
                    Some((blk, (0..8).map(|k| le32(&d, 0x14 + 4 * k) as usize).collect(), d[0x38..0x38 + n].to_vec()));
                break;
            }
        }
        r.ok(
            found.as_ref().is_some_and(|f| f.2.len() == geo.bbt_len),
            format!("cs{cs} DEVICEINFOBBT at block {}", hex(found.as_ref().map_or(0, |f| f.0))),
        );
        let Some((_, c, bbt)) = found else { continue };
        let in_range = |b: usize| b / 8 < bbt.len();
        r.ok(
            in_range(0) && !bit(&bbt, 0) && geo.cand[cs].iter().all(|&b| in_range(b) && !bit(&bbt, b)),
            format!("cs{cs} BBT marks block 0 and candidates bad"),
        );
        r.ok(in_range(VFL_BLOCKS[0]) && bit(&bbt, VFL_BLOCKS[0]), format!("cs{cs} BBT block 1 good"));
        let d2 = if c[1] < geo.blocks_per_ce { st.read(cs, geo.ppage(c[1], 0), false)? } else { None };
        r.ok(d2.is_some_and(|d| magic(&d.0, "DEVICEINFOBBT")), format!("cs{cs} second BBT copy at {}", hex(c[1])));
        if cs == 0 {
            sig_block = Some(c[4]);
        }
    }
    let sb = sig_block.unwrap_or(0);
    let sig = if sb < geo.blocks_per_ce { st.read(0, geo.ppage(sb, 0), false)? } else { None };
    r.ok(
        sig.as_ref().is_some_and(|s| magic(&s.0, "NANDDRIVERSIGN")),
        format!("NANDDRIVERSIGN at cs0 block {} (BBT hdr+0x24)", hex(sb)),
    );
    if let Some((d, _)) = &sig {
        let ns = le32(d, 0x38);
        let flags = le32(d, 0x3c);
        st.plain = flags & 0x10000 == 0; // 0x5, or a recipe's nand_sig_flags (3.1.x: 4)
        r.ok(
            ns >> 8 == NSIG_BASE >> 8
                && (0x31..=0x39).contains(&(ns & 0xff))
                && (flags == SIG_FLAGS || (st.plain && (4..=5).contains(&flags))),
            format!(
                "signature nSig={ns:08x} flags={flags:08x} (VSVFL, epoch {}, whitening {})",
                ns & 0xf,
                if st.plain { "off" } else { "on" }
            ),
        );
    }

    // VFL contexts
    for cs in 0..geo.num_cs {
        let copies = (0..8).map(|p| st.read(cs, geo.ppage(VFL_BLOCKS[0], p), false)).collect::<Result<Vec<_>>>()?;
        let Some(first) = &copies[0] else {
            r.fails.push(format!("FAIL cs{cs} VFLCxt missing"));
            continue;
        };
        let ctx = first.0[..0x800].to_vec();
        r.ok(
            copies.iter().all(|c| c.as_ref().is_some_and(|c| c.0[..0x800] == ctx[..])),
            format!("cs{cs} VFLCxt 8 identical copies"),
        );
        r.ok(ctx == k48nand::vfl_checksum(&ctx), format!("cs{cs} VFLCxt checksums"));
        r.ok(
            copies.iter().all(|c| c.as_ref().is_some_and(|c| c.1[8] == 0 && c.1[9] == T_VFL)),
            format!("cs{cs} VFLCxt spare type 0x80"),
        );
        let u16_at = |o: usize| ctx[o] as usize | (ctx[o + 1] as usize) << 8;
        let ver = le32(&ctx, 0x7f4);
        let ftl_type = le32(&ctx, 8);
        let usable = u16_at(0x696);
        let pstart = u16_at(0x698);
        let cb = vec![u16_at(0x69a), u16_at(0x69c), u16_at(0x69e)];
        r.ok(
            ver <= 2 && ftl_type == 2 && usable == geo.usable && pstart == geo.usable && cb == CTRL_BLOCKS,
            format!("cs{cs} VFLCxt version {ver} ftl_type {ftl_type} usable {usable} ctrl {cb:?}"),
        );
        let remapped: Vec<usize> = (0..geo.remap.len()).collect();
        r.ok(
            geo.remap.iter().enumerate().all(|(pbn, &(bank, slot))| u16_at(0x26 + 2 * (bank * geo.pool + slot)) == pbn),
            format!("cs{cs} pool map remaps blocks {remapped:?}"),
        );
    }

    // FTL walk: every vblock's page 0 classifies it
    let mut toc_from_index: HashMap<usize, Vec<u32>> = HashMap::new();
    let mut toc_from_user: HashMap<usize, usize> = HashMap::new();
    let (mut nuser, mut nindex, mut nfree) = (0, 0, 0);
    for v in 0..geo.num_blocks {
        let base = v * geo.ppsublk;
        let Some((_, m)) = st.read_vpn(base)? else {
            nfree += 1;
            continue;
        };
        let typ = m[9];
        if CTRL_BLOCKS.contains(&v) {
            r.fails.push(format!("FAIL ctrl vblock {v} is programmed"));
        }
        if typ == T_INDEX {
            nindex += 1;
        } else if typ == T_USER {
            nuser += 1;
        } else {
            r.fails.push(format!("FAIL vblock {v} page 0 has spare type {}", hex(typ)));
            continue;
        }
        let mut lpns: Vec<u32> = Vec::new();
        for j in 0..geo.ppsublk {
            let Some((pd, pm)) = st.read_vpn(base + j)? else { break };
            if pm[9] & T_CLOSED != 0 {
                break;
            }
            let lpn = le32(&pm, 0);
            lpns.push(lpn);
            if typ == T_INDEX {
                toc_from_index.insert(lpn as usize, (0..geo.toc_entries).map(|k| le32(&pd, 4 * k)).collect());
            } else {
                toc_from_user.insert(lpn as usize, base + j);
            }
        }
        if lpns.len() == geo.data_pages {
            let mut table = Vec::new();
            for i in 0..geo.toc {
                if let Some((d, _)) = st.read_vpn(base + geo.data_pages + i)? {
                    table.extend(d);
                }
            }
            let got: Vec<u32> =
                (0..geo.data_pages).map(|k| if 4 * k + 4 <= table.len() { le32(&table, 4 * k) } else { 0 }).collect();
            if v.is_multiple_of(25) || got != lpns {
                r.ok(got == lpns, format!("vblock {v} closed, BTOC matches {} page spares", lpns.len()));
            }
        }
    }
    log(&format!("ok    {nuser} user, {nindex} index, {nfree} free vblocks"));
    let mut rebuilt: HashMap<usize, usize> = HashMap::new();
    for (&t, arr) in &toc_from_index {
        for (i, &vpn) in arr.iter().enumerate() {
            if vpn != UNMAPPED {
                rebuilt.insert(t * geo.toc_entries + i, vpn as usize);
            }
        }
    }
    r.ok(rebuilt == toc_from_user, format!("index pages map exactly the {} user pages", toc_from_user.len()));
    let lpn_data = |lpn: usize| -> Result<Option<Vec<u8>>> {
        match rebuilt.get(&lpn) {
            Some(&vpn) => Ok(st.read_vpn(vpn)?.map(|r| r.0)),
            None => Ok(None),
        }
    };

    let m0 = lpn_data(0)?;
    r.ok(m0.as_ref().is_some_and(|m| m[510] == 0x55 && m[511] == 0xAA), "LBA 0 carries an MBR".into());
    if let Some(m0) = &m0 {
        let parts = k48nand::partitions(m0);
        for (i, p) in parts.iter().enumerate().filter(|(_, p)| p.typ != 0) {
            r.ok(
                p.lba + p.count <= geo.exported_pages,
                format!("partition {} type {:02x} lba {} count {} inside exported size", i + 1, p.typ, p.lba, p.count),
            );
            if p.typ == 0xAF && p.count > 256 {
                let sig = lpn_data(p.lba)?.map(|d| d[1024..1026].to_vec());
                r.ok(
                    sig.as_deref() == Some(b"H+") || sig.as_deref() == Some(b"HX"),
                    format!("partition {} has an HFS+ volume header", i + 1),
                );
            }
        }
        if let Some(mbr) = mbr {
            let src = std::fs::read(mbr).map_err(|e| crate::internal!("{}: {e}", mbr.display()))?;
            let src = &src[..src.len().min(512)];
            let name = mbr.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            r.ok(
                src.len() >= 0x1ce && m0[..0x1be] == src[..0x1be] && parts[0] == k48nand::partitions(&pad(src, 512))[0],
                format!("LBA 0 matches {name} (boot code + partition 1)"),
            );
        }
        if let Some(system) = system {
            let (lba, cnt) = (parts[0].lba, parts[0].count);
            let f = File::open(system).map_err(|e| crate::internal!("{}: {e}", system.display()))?;
            let len = f.metadata().map_err(|e| crate::internal!("{}: {e}", system.display()))?.len() as usize;
            let mut sample: Vec<usize> = (0..64).collect();
            sample.extend((0..cnt).step_by((cnt / 512).max(1)));
            let mut bad = 0;
            let ps = geo.page_size;
            for &n in &sample {
                let mut want = vec![0u8; ps];
                let at = n * ps;
                if at < len {
                    let k = ps.min(len - at);
                    posio::read_exact_at(&f, &mut want[..k], at as u64)
                        .map_err(|e| crate::internal!("{}: {e}", system.display()))?;
                }
                let got = lpn_data(lba + n)?;
                let patched = got.as_ref().is_some_and(|g| g.windows(12).any(|w| w == b"/dev/disk0s2"));
                if got.as_ref() != Some(&want) && !patched {
                    bad += 1;
                }
            }
            let name = system.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            r.ok(bad == 0, format!("{} sampled system pages match {name}", sample.len()));
        }
    }
    let fails = r.fails;
    for f in &fails {
        log(f);
    }
    log(&format!(
        "{}: {}",
        dir.display(),
        if fails.is_empty() { "all checks passed".to_string() } else { format!("FAILED ({})", fails.len()) }
    ));
    Ok(fails.is_empty())
}

/// check's "ok" lines go out as they pass; the "FAIL" lines are kept for the end.
struct Report<'a> {
    fails: Vec<String>,
    log: &'a dyn Fn(&str),
}

impl Report<'_> {
    fn ok(&mut self, cond: bool, what: String) {
        if cond {
            (self.log)(&format!("ok   {what}"));
        } else {
            self.fails.push(format!("FAIL {what}"));
        }
    }
}

fn pad(b: &[u8], n: usize) -> Vec<u8> {
    let mut v = b.to_vec();
    v.resize(v.len().max(n), 0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k48nand::tests::temp;

    fn selfcheck_json() -> Map<String, Value> {
        match serde_json::from_str(&Geometry::selfcheck().json()) {
            Ok(Value::Object(g)) => g,
            _ => panic!("selfcheck geometry.json is an object"),
        }
    }

    fn write_geometry(dir: &Path, g: &Map<String, Value>) {
        std::fs::write(dir.join("geometry.json"), serde_json::to_vec(g).expect("json")).expect("write geometry.json");
    }

    #[test]
    fn physical_and_unknown_formats_refused_before_page_mapping() {
        for format in ["nand-xor-ff-v2", "future-format", ""] {
            let dir = temp("formats");
            let mut g = selfcheck_json();
            g.insert("storage_format".into(), format.into());
            write_geometry(&dir, &g);
            // No page files: refusal must occur before any page is read.
            assert!(geometry(&dir).is_err(), "{format}");
            assert!(StoreReader::open(&dir, &Geometry::selfcheck(), None).is_err(), "{format}");
            assert!(check(&dir, None, None, &|_| {}).is_err(), "{format}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn geometry_rejects_different_physical_contract() {
        for field in ["spare_bytes", "chip_id"] {
            let dir = temp("contract");
            let mut g = selfcheck_json();
            g.insert(field.into(), if field == "chip_id" { "0xFFFFFFFF".into() } else { 64.into() });
            write_geometry(&dir, &g);
            assert!(geometry(&dir).is_err(), "{field}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn legacy_format_and_overlay_compatibility() {
        let dir = temp("legacy");
        let mut g = selfcheck_json();
        write_geometry(&dir, &g);
        assert_eq!(geometry(&dir).map(|g| g.name).ok(), Some("selfcheck"));
        g.insert("storage_format".into(), "legacy-zero-blank-v1".into());
        write_geometry(&dir, &g);
        assert_eq!(geometry(&dir).map(|g| g.name).ok(), Some("selfcheck"));
        let overlay = dir.join("overlay");
        std::fs::create_dir(&overlay).expect("overlay");
        std::fs::write(overlay.join("storage-format"), "nand-xor-ff-v2\n").expect("marker");
        assert!(StoreReader::open(&dir, &Geometry::selfcheck(), Some(&overlay)).is_err());
        assert!(StoreReader::open(&dir, &Geometry::k48_with_16gb(), None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The image store's geometry.json names its format and layout, and reads back as a generated image store.
    #[test]
    fn image_geometry_json_reads_back() {
        let dir = temp("imagejson");
        let geo = Geometry::k48_with_16gb_v1();
        std::fs::write(dir.join("geometry.json"), geo.json_for(StoreFormat::Image)).expect("write");
        assert_eq!(store_format(&dir).ok(), Some(StoreFormat::Image));
        assert_eq!(geometry(&dir).ok(), Some(geo));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

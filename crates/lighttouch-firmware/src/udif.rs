//! UDIF disk images (an IPSW's rootfs or ramdisk .dmg) without hdiutil: the koly trailer locates the plist; each
//! blkx entry's mish table maps runs of sectors to raw, zero-fill, zlib or ADC chunks of the data fork. And the raw
//! HFS+ volume out of a rootfs DMG: the Apple_HFS(X) partition of its Apple Partition Map.
//!
//! ```ignore
//! udif::extract_rootfs(&rootfs_dmg, &raw_volume)?;
//! let image = UdifImage::open(&dmg)?;
//! let head = image.read(0, 64)?;                      // the partition map
//! image.write_to(&file, start..start + count)?;       // a partition, at offset 0 of the file
//! ```
use crate::error::Result;
use crate::posio;
use plist::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Zero,
    Raw,
    Zlib,
    Adc,
}

#[derive(Debug, Clone, Copy)]
struct Chunk {
    kind: Kind,
    /// First output sector.
    sector: u64,
    sectors: u64,
    /// In the file.
    offset: u64,
    length: u64,
}

pub struct UdifImage {
    path: PathBuf,
    pub sector_count: u64,
    /// By output sector.
    chunks: Vec<Chunk>,
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn be64(b: &[u8], o: usize) -> u64 {
    (be32(b, o) as u64) << 32 | be32(b, o + 4) as u64
}

impl UdifImage {
    pub fn open(path: &Path) -> Result<UdifImage> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut f = File::open(path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
        let end = f.seek(SeekFrom::End(0))?;
        if end < 512 {
            return Err(crate::unsupported!("{name}: not a UDIF image"));
        }
        let mut koly = [0u8; 512];
        f.seek(SeekFrom::Start(end - 512))?;
        f.read_exact(&mut koly)?;
        if !koly.starts_with(b"koly") {
            return Err(crate::unsupported!("{name}: no koly trailer (not a UDIF image)"));
        }
        let data_fork = be64(&koly, 0x18);
        let (xml_offset, xml_length) = (be64(&koly, 0xD8), be64(&koly, 0xE0) as usize);
        let sector_count = be64(&koly, 0x1EC);
        let mut xml = vec![0u8; xml_length];
        f.seek(SeekFrom::Start(xml_offset))?;
        f.read_exact(&mut xml).map_err(|_| crate::unsupported!("{name}: no blkx table"))?;
        let no_table = || crate::unsupported!("{name}: no blkx table");
        let plist = Value::from_reader(std::io::Cursor::new(&xml)).map_err(|_| no_table())?;
        let blkx = plist
            .as_dictionary()
            .and_then(|d| d.get("resource-fork"))
            .and_then(Value::as_dictionary)
            .and_then(|d| d.get("blkx"))
            .and_then(Value::as_array)
            .ok_or_else(no_table)?;
        let mut chunks = Vec::new();
        for entry in blkx {
            let Some(mish) = entry.as_dictionary().and_then(|d| d.get("Data")).and_then(Value::as_data) else { continue };
            if mish.len() < 204 || !mish.starts_with(b"mish") {
                return Err(crate::unsupported!("{name}: bad blkx entry"));
            }
            let first = be64(mish, 8);
            let base = be64(mish, 24) + data_fork;
            let n = be32(mish, 200) as usize;
            if mish.len() < 204 + 40 * n {
                return Err(crate::unsupported!("{name}: truncated blkx entry"));
            }
            for i in 0..n {
                let c = 204 + 40 * i;
                let kind = match be32(mish, c) {
                    0x0000_0000 | 0x0000_0002 => Kind::Zero,
                    0x0000_0001 => Kind::Raw,
                    0x8000_0004 => Kind::Adc,
                    0x8000_0005 => Kind::Zlib,
                    0x7FFF_FFFE | 0xFFFF_FFFF => continue, // comment, terminator
                    other => {
                        return Err(crate::unsupported!(
                            "{name}: chunk type 0x{other:x} (bzip2/LZFSE/LZMA) is not supported here"
                        ));
                    }
                };
                let sectors = be64(mish, c + 16);
                if sectors == 0 {
                    continue;
                }
                chunks.push(Chunk {
                    kind,
                    sector: first + be64(mish, c + 8),
                    sectors,
                    offset: base + be64(mish, c + 24),
                    length: be64(mish, c + 32),
                });
            }
        }
        chunks.sort_by_key(|c| c.sector);
        Ok(UdifImage { path: path.to_path_buf(), sector_count, chunks })
    }

    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Sectors `sector..sector + count` (zeros where no chunk covers them).
    pub fn read(&self, sector: u64, count: u64) -> Result<Vec<u8>> {
        let f = File::open(&self.path).map_err(|e| crate::internal!("cannot open {}: {e}", self.path.display()))?;
        let mut out = vec![0u8; (count * 512) as usize];
        for c in self.chunks.iter().filter(|c| c.sector < sector + count && c.sector + c.sectors > sector) {
            let bytes = self.decode(c, &f)?;
            let (from, to) = (c.sector.max(sector), (c.sector + c.sectors).min(sector + count));
            out[((from - sector) * 512) as usize..((to - sector) * 512) as usize]
                .copy_from_slice(&bytes[((from - c.sector) * 512) as usize..((to - c.sector) * 512) as usize]);
        }
        Ok(out)
    }

    /// Writes sectors `range` to `out` from its offset 0 on a few threads; zero runs are left as holes (the caller
    /// sizes the file).
    pub fn write_to(&self, out: &File, range: Range<u64>) -> Result<()> {
        let f = File::open(&self.path).map_err(|e| crate::internal!("cannot open {}: {e}", self.path.display()))?;
        let work: Vec<&Chunk> = self
            .chunks
            .iter()
            .filter(|c| c.kind != Kind::Zero && c.sector < range.end && c.sector + c.sectors > range.start)
            .collect();
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 4);
        let failed = AtomicBool::new(false);
        let error = Mutex::new(None);
        std::thread::scope(|scope| {
            for worker in 0..workers {
                let (work, f, failed, error, range) = (&work, &f, &failed, &error, range.clone());
                scope.spawn(move || {
                    let mut i = worker;
                    while i < work.len() && !failed.load(Ordering::Relaxed) {
                        let c = work[i];
                        let result = self.decode(c, f).and_then(|bytes| {
                            let (from, to) = (c.sector.max(range.start), (c.sector + c.sectors).min(range.end));
                            let slice = &bytes[((from - c.sector) * 512) as usize..((to - c.sector) * 512) as usize];
                            if slice.iter().any(|&b| b != 0) {
                                posio::write_all_at(out, slice, (from - range.start) * 512)?;
                            }
                            Ok(())
                        });
                        if let Err(e) = result {
                            failed.store(true, Ordering::Relaxed);
                            error.lock().unwrap().get_or_insert(e);
                        }
                        i += workers;
                    }
                });
            }
        });
        match error.into_inner().unwrap() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn decode(&self, c: &Chunk, f: &File) -> Result<Vec<u8>> {
        let size = (c.sectors * 512) as usize;
        if c.kind == Kind::Zero {
            return Ok(vec![0; size]);
        }
        let mut input = vec![0u8; c.length as usize];
        posio::read_exact_at(f, &mut input, c.offset).map_err(|_| crate::unsupported!("{}: ends inside a chunk", self.name()))?;
        match c.kind {
            Kind::Raw => {
                input.resize(size, 0);
                Ok(input)
            }
            Kind::Zlib => {
                let mut out = vec![0u8; size];
                let mut z = flate2::Decompress::new(true);
                match z.decompress(&input, &mut out, flate2::FlushDecompress::Finish) {
                    Ok(flate2::Status::StreamEnd) if z.total_out() as usize == size => Ok(out),
                    _ => Err(crate::unsupported!("{}: bad zlib chunk at sector {}", self.name(), c.sector)),
                }
            }
            Kind::Adc => adc(&input, size),
            Kind::Zero => unreachable!(),
        }
    }
}

/// Apple Data Compression: runs of literals, and back-references 3-18 bytes long (2-byte form) or 4-67 long (3-byte
/// form, 16-bit distance).
pub fn adc(input: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(size);
    let mut i = 0;
    while i < input.len() && out.len() < size {
        let b = input[i] as usize;
        if b & 0x80 != 0 {
            let n = (b & 0x7F) + 1;
            if i + 1 + n > input.len() {
                return Err(crate::unsupported!("bad ADC literal run"));
            }
            out.extend_from_slice(&input[i + 1..i + 1 + n]);
            i += 1 + n;
            continue;
        }
        let (length, distance) = if b & 0x40 != 0 {
            if i + 2 >= input.len() {
                return Err(crate::unsupported!("bad ADC match"));
            }
            let r = ((b & 0x3F) + 4, (input[i + 1] as usize) << 8 | input[i + 2] as usize);
            i += 3;
            r
        } else {
            if i + 1 >= input.len() {
                return Err(crate::unsupported!("bad ADC match"));
            }
            let r = ((b >> 2 & 0x0F) + 3, (b & 0x03) << 8 | input[i + 1] as usize);
            i += 2;
            r
        };
        let from = out.len().checked_sub(distance + 1).ok_or_else(|| crate::unsupported!("bad ADC distance"))?;
        for k in 0..length {
            out.push(out[from + k]);
        }
    }
    out.resize(size, 0);
    Ok(out)
}

/// (byte offset, byte length) of the Apple_HFS(X) partition in an Apple Partition Map.
pub fn apm_hfs_slice(r: &[u8]) -> Result<(u64, u64)> {
    if r.len() < 4 || r[0] != 0x45 || r[1] != 0x52 {
        return Err(crate::unsupported!("not an Apple partition map"));
    }
    let bs = (r[2] as usize) << 8 | r[3] as usize;
    if bs == 0 || r.len() < bs + 8 {
        return Err(crate::unsupported!("truncated Apple partition map"));
    }
    let n = be32(r, bs + 4) as usize;
    for i in 1..=n.max(1) {
        if bs * (i + 1) > r.len() {
            continue;
        }
        let e = bs * i;
        let kind = r[e + 48..e + 80].split(|&b| b == 0).next().unwrap_or(&[]);
        if kind == b"Apple_HFSX" || kind == b"Apple_HFS" {
            return Ok((be32(r, e + 8) as u64 * bs as u64, be32(r, e + 12) as u64 * bs as u64));
        }
    }
    Err(crate::unsupported!("no Apple_HFS(X) partition"))
}

/// The raw HFS+ volume out of a rootfs DMG (straight from the UDIF chunks); a source that already is a bare HFS
/// volume is copied as is.
pub fn extract_rootfs(dmg: &Path, out: &Path) -> Result<()> {
    let mut signature = [0u8; 2];
    if let Ok(f) = File::open(dmg) {
        if posio::read_exact_at(&f, &mut signature, 1024).is_ok() && (&signature == b"H+" || &signature == b"HX") {
            let _ = std::fs::remove_file(out);
            std::fs::copy(dmg, out)?;
            return Ok(());
        }
    }
    let image = UdifImage::open(dmg)?;
    let (off, len) = apm_hfs_slice(&image.read(0, 64)?)?;
    let _ = std::fs::remove_file(out);
    let file = posio::create_sparse(out)?;
    file.set_len(len).map_err(|e| crate::internal!("cannot size {}: {e}", out.display()))?;
    image.write_to(&file, off / 512..(off + len) / 512)
}

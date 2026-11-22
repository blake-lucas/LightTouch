//! New N42 PPN media (Swift `A6NAND`). Only factory partition/reservation metadata is supplied;
//! the original restore kernel formats the FTL. Never use this on existing media.
use crate::error::Result;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

pub const PAGE_BYTES: usize = 16_384;
pub const SPARE_BYTES: usize = 64;
pub const PAGES_PER_BLOCK: u32 = 256;
pub const BLOCKS_PER_CE: u32 = 2128;
pub const RECORD_BYTES: usize = PAGE_BYTES + SPARE_BYTES;

fn put(b: &mut [u8], at: usize, value: u32, count: usize) {
    for i in 0..count {
        b[at + i] = ((value >> (8 * i)) & 0xff) as u8;
    }
}

fn tag(b: &mut [u8], value: &str, at: usize) {
    let mut bytes = value.as_bytes().to_vec();
    bytes.resize(4, 0);
    b[at..at + 4].copy_from_slice(&bytes);
}

/// Swift `A6NAND.checksum`: two running sums, folded every 21 bytes, then twice more.
fn checksum(b: &mut [u8], start: usize, size: usize) {
    let mut a: u32 = 255;
    let mut c: u32 = 255;
    for i in 0..size {
        a = a.wrapping_add(u32::from(b[start + i]));
        c = c.wrapping_add(a);
        if (i + 1) % 21 == 0 {
            a = (a & 255) + (a >> 8);
            c = (c & 255) + (c >> 8);
        }
    }
    for _ in 0..2 {
        a = (a & 255) + (a >> 8);
        c = (c & 255) + (c >> 8);
    }
    b[start + size] = (a & 0xff) as u8;
    b[start + size + 1] = (c & 0xff) as u8;
}

/// The 1536-byte factory header (A6NAND.swift:8-68), byte for byte.
pub fn factory_header() -> Vec<u8> {
    let mut b = vec![0u8; 1536];
    tag(&mut b, "ndrG", 0);
    put(&mut b, 8, 1, 4);
    put(&mut b, 12, 9, 4);
    for (i, v) in [
        1u32,
        2,
        BLOCKS_PER_CE,
        PAGES_PER_BLOCK,
        (PAGE_BYTES / 512) as u32,
        SPARE_BYTES as u32,
        6,
        0xdead_cafe,
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut b, 0x200 + 4 * i, v, 4);
    }
    put(&mut b, 0x224, 0xdead_cafe, 4);
    checksum(&mut b, 0x224, 42);

    let parts: [(&str, u32, u32, u32); 8] = [
        ("boot", 0, 1, 2),
        ("plog", 4, 1, 0x108),
        ("nvrm", 2, 3, 0x108),
        ("firm", 2, 1, 0x308),
        ("fsys", 1, BLOCKS_PER_CE - 1, 6),
        ("scfg", 0, 1, 1),
        ("diag", 0, 1, 1),
        ("fbbt", 0, 1, 1),
    ];
    for i in 0..32 {
        let (name, a, c, d) = parts.get(i).copied().unwrap_or(("none", 0, 0, 0));
        let reversed: String = name.chars().rev().collect();
        let at = 0x400 + 16 * i;
        tag(&mut b, &reversed, at);
        put(&mut b, at + 4, a, 4);
        put(&mut b, at + 8, c, 4);
        put(&mut b, at + 12, d, 4);
    }
    // Virtual boot-unit partitions reserve physical blocks 1...6 on both banks.
    tag(&mut b, "rAps", 0x250);
    put(&mut b, 0x254, 12, 2);
    tag(&mut b, "sbus", 0x35c);
    put(&mut b, 0x360, 12, 2);
    put(&mut b, 0x362, 2, 1);
    for i in 0..12u32 {
        put(&mut b, 0x25a + 4 * i as usize, 1 + i / 2, 3);
        put(&mut b, 0x25d + 4 * i as usize, i % 2, 1);
        put(&mut b, 0x366 + i as usize, i + 2, 1);
    }
    checksum(&mut b, 0x250, 0x10a);
    checksum(&mut b, 0x35c, 0x4a);
    b
}

fn geometry_json() -> Vec<u8> {
    let mut m = serde_json::Map::new();
    for (k, v) in [
        ("page_bytes", PAGE_BYTES as u64),
        ("spare_bytes", SPARE_BYTES as u64),
        ("pages_per_block", PAGES_PER_BLOCK as u64),
        ("blocks_per_ce", BLOCKS_PER_CE as u64),
        ("ce_per_bus", 1),
        ("buses", 2),
    ] {
        m.insert(k.to_string(), serde_json::Value::from(v));
    }
    m.insert("chip_id".into(), serde_json::Value::from("0x014E5050"));
    m.insert("storage_format".into(), serde_json::Value::from("nand-xor-ff-v2"));
    m.insert("layout".into(), serde_json::Value::from("ppn-factory"));
    let mut out = serde_json::to_vec_pretty(&serde_json::Value::Object(m)).unwrap_or_default();
    out.push(b'\n');
    out
}

/// `A6NAND.create`: geometry.json + two sparse chip files, each with the header record twice.
pub fn create(dir: &Path) -> Result<()> {
    if dir.exists() {
        return Err(crate::internal!("NAND output already exists"));
    }
    std::fs::create_dir_all(dir)?;
    let geometry = dir.join("geometry.json");
    let mut g = std::fs::OpenOptions::new().write(true).create_new(true).open(&geometry)?;
    g.write_all(&geometry_json())?;
    g.sync_all()?;

    let header = factory_header();
    let mut record = vec![0u8; RECORD_BYTES];
    for (i, v) in header.into_iter().enumerate() {
        record[i] = v ^ 0xff;
    }
    let chip_bytes = u64::from(BLOCKS_PER_CE) * u64::from(PAGES_PER_BLOCK) * RECORD_BYTES as u64;
    for bus in 0..2 {
        let path = dir.join(format!("bus{bus}-ce0.pages"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| crate::internal!("Cannot create NAND bank: {e}"))?;
        file.set_len(chip_bytes)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&record)?;
        file.write_all(&record)?;
        file.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_shape() {
        let h = factory_header();
        assert_eq!(h.len(), 1536);
        assert_eq!(&h[0..4], b"ndrG");
        assert_eq!(&h[0x400..0x404], b"toob");
        assert_eq!(&h[0x410..0x414], b"golp");
        assert_eq!(&h[0x250..0x254], b"rAps");
        assert_eq!(&h[0x35c..0x360], b"sbus");
    }

    #[test]
    fn chip_size() {
        assert_eq!(u64::from(BLOCKS_PER_CE) * u64::from(PAGES_PER_BLOCK) * RECORD_BYTES as u64, 8_960_344_064);
    }
}

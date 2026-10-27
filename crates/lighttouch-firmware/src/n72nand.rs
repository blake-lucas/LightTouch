//! The iPod touch 2G's NAND (4096 data + 64 spare bytes a page) in the generated FTL layout hw/arm/ipod_touch_fmss.c
//! serves: one HFSX volume striped by a closed formula, plus the ~50 bookkeeping pages the formula does not cover,
//! written as an image store (page_nand, Image::n72). Ports imgtools/ftlmap.predict, ipod2g_nand.metadata_pages and
//! build_nand.write_pages (zero blocks are not written: they read blank).
//!
//! ```ignore
//! n72nand::predict(block)                                    // (cs, page) of an allocation block
//! n72nand::metadata_pages(blocks, epoch)                     // {(cs, page): data + spare}
//! let (volume, metadata) = n72nand::write(&img, blocks, epoch, &nand_dir)?;
//! ```
//! `blocks` is the HFS extent in 4 KiB blocks (recipe system_mib * 256), never the backing file's length; `epoch` is
//! Restore.plist's DeviceMap SCEP.
use crate::error::Result;
use crate::page_nand::{Image, Store};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub const PAGE: usize = 4096;
pub const SPARE: usize = 64;
pub const PAGES_PER_BLOCK: usize = 128;
const BBT_PAGE: usize = 4095 * 128;
const MAP_PAGES: usize = 18;
/// fmss_blank_page's spare: what an unwritten page reads as, and what volume pages carry.
pub const BLANK_SPARE: [u8; SPARE] = {
    let mut s = [0u8; SPARE];
    s[8] = 0xFF;
    s[10] = 0xFF;
    s
};
const VFL_SPARE: [u8; SPARE] = {
    let mut s = [0u8; SPARE];
    s[0] = 1;
    s[9] = 0x80;
    s
};
const META_SPARE: [u8; SPARE] = {
    let mut s = [0u8; SPARE];
    s[9] = 0x43;
    s
};
/// The Apple_HFS partition type GUID, as stored.
pub const HFS_TYPE: [u8; 16] =
    [0x00, 0x53, 0x46, 0x48, 0x00, 0x00, 0xaa, 0x11, 0xaa, 0x11, 0x00, 0x30, 0x65, 0x43, 0xec, 0xac];
const FTL_LOG: &[u8] = b"Writing FTL Meta to physical page 255 @ cs 3\nto physical page 130 @ cs 2\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Page {
    pub cs: usize,
    pub page: usize,
}

/// Four consecutive blocks form a row over the chip selects; rows alternate between two erase blocks and erase
/// blocks are used in pairs from 2 on.
pub fn predict(n: usize) -> Page {
    let r = n.div_ceil(4);
    Page { cs: (n + 3) % 4, page: (2 * (r / 256) + 2 + r % 2) * PAGES_PER_BLOCK + (r % 256) / 2 }
}

fn u16s(w: &[u32]) -> Vec<u8> {
    let mut b = vec![0u8; PAGE];
    for (i, &v) in w.iter().enumerate() {
        b[2 * i] = v as u8;
        b[2 * i + 1] = (v >> 8) as u8;
    }
    b
}

/// (cs, page) of the k-th FTL map page: the volume's interleave over erase blocks 0/1.
fn stripe(k: usize) -> Page {
    let m = k + 1;
    let r = m / 4;
    Page { cs: m % 4, page: ((r + 1) % 2) * PAGES_PER_BLOCK + (r + 1) / 2 }
}

pub(crate) fn put(b: &mut [u8], o: usize, v: u64, n: usize) {
    for k in 0..n {
        b[o + k] = (v >> (8 * k)) as u8;
    }
}

/// zlib's CRC-32.
pub(crate) fn crc(b: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(b);
    c.sum()
}

/// The VFL writer's checksum over the 0x7f8-byte context: word sum and word xor, each keyed with 0xaabbccdd.
pub(crate) fn vfl_checksum(p: &mut [u8]) {
    let (mut sum, mut xor) = (0u32, 0u32);
    for w in p[..0x7F8].chunks_exact(4) {
        let v = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        sum = sum.wrapping_add(v);
        xor ^= v;
    }
    put(p, 0x7F8, sum.wrapping_add(0xAABB_CCDD) as u64, 4);
    put(p, 0x7FC, (xor ^ 0xAABB_CCDD) as u64, 4);
}

fn vfl_page(cs: usize, first: bool) -> Vec<u8> {
    let mut w = vec![0u32; 2048];
    w[0] = cs as u32;
    w[6] = 1;
    w[19..839].fill(0xFFF0);
    w[839..848].copy_from_slice(&[0, 1, 2, 3, 0x800, 0x800, 0, 1, 2]);
    w[877] = 0x14;
    w[878] = 0x10;
    w[1018] = 2;
    if first {
        w[1024] = 1;
        w[1028] = 0x8000;
    }
    let mut p = u16s(&w);
    // 4.x iBoot verifies the checksum; 3.x's verifier is a stub
    vfl_checksum(&mut p);
    p
}

fn ftl_context() -> Vec<u8> {
    let mut w = vec![0u32; 2048];
    w[4] = 20;
    for (i, v) in (3..23).enumerate() {
        w[7 + i] = v; // free pool VBNs
    }
    for (i, v) in (5..23).enumerate() {
        w[28 + 2 * i] = v;
    }
    for i in 0..18 {
        w[212 + 10 * i] = 0xFFFF;
    }
    w[1021..1024].copy_from_slice(&[0x4656, 0xFFFF, 0xB9A9]);
    let mut d = u16s(&w);
    d[2048..2048 + FTL_LOG.len()].copy_from_slice(FTL_LOG);
    d
}

/// Device LBA 0-2: protective MBR, GPT header, one Apple_HFS entry sized exactly to the filesystem (older HFS
/// disables alternate-header writes when the partition extends more than one allocation block beyond it).
/// `slack`: recipe 1 (every base before 0b43f26) ended the partition 11 blocks past the volume.
pub fn gpt_pages(blocks: usize, slack: usize) -> [Vec<u8>; 3] {
    let mut mbr = vec![0u8; PAGE];
    mbr[8] = 0xFF;
    mbr[10] = 0xFF; // as generated: a spare pattern in the data area
    mbr[0x1BE + 4] = 0xEE;
    put(&mut mbr, 0x1BE + 8, 3, 4);
    put(&mut mbr, 0x1BE + 12, (blocks + 10) as u64, 4);
    mbr[0x1FE] = 0x55;
    mbr[0x1FF] = 0xAA;
    let mut ent = vec![0u8; PAGE];
    ent[..16].copy_from_slice(&HFS_TYPE);
    put(&mut ent, 0x20, 3, 8);
    put(&mut ent, 0x28, (3 + blocks - 1 + slack) as u64, 8);
    let mut hdr = vec![0u8; PAGE];
    hdr[..8].copy_from_slice(b"EFI PART");
    put(&mut hdr, 8, 0x0001_0000, 4);
    put(&mut hdr, 12, 0x5C, 4);
    put(&mut hdr, 0x48, 2, 8);
    put(&mut hdr, 0x50, 1, 4);
    put(&mut hdr, 0x54, 0x80, 4);
    put(&mut hdr, 0x58, crc(&ent[..0x80]) as u64, 4);
    let c = crc(&hdr[..0x5C]);
    put(&mut hdr, 0x10, c as u64, 4);
    [mbr, hdr, ent]
}

/// NANDDRIVERSIGN: '0' + the NAND epoch (Restore.plist SCEP) | 0x43313100, flags 4.
fn signature(epoch: u32) -> Vec<u8> {
    let mut d = vec![0u8; PAGE];
    d[..14].copy_from_slice(b"NANDDRIVERSIGN");
    put(&mut d, 0x34, 4, 4);
    put(&mut d, 0x38, (0x4331_3100 | (0x30 + epoch)) as u64, 4);
    d
}

fn record(data: Vec<u8>, spare: &[u8; SPARE]) -> Vec<u8> {
    let mut r = data;
    r.extend_from_slice(spare);
    r
}

/// Every page the volume formula does not cover, data + spare.
pub fn metadata_pages(blocks: usize, epoch: u32) -> BTreeMap<Page, Vec<u8>> {
    let zero = [0u8; SPARE];
    let mut pages = BTreeMap::new();
    pages.insert(Page { cs: 0, page: 0 }, record(vec![0u8; PAGE], &META_SPARE));
    pages.insert(Page { cs: 3, page: 255 }, record(ftl_context(), &META_SPARE));
    let maps: Vec<Page> = (0..MAP_PAGES).map(stripe).collect();
    for (k, &p) in maps.iter().enumerate() {
        let words: Vec<u32> = (0..2048).map(|i| (k * 2048 + i + 1) as u32).collect();
        pages.insert(p, record(u16s(&words), &zero));
    }
    let mut bbt = b"DEVICEINFOBBT\0\0\0".to_vec();
    bbt.resize(PAGE, 0xFF);
    for cs in 0..4 {
        for pg in PAGES_PER_BLOCK..PAGES_PER_BLOCK + 8 {
            let p = Page { cs, page: pg };
            if !maps.contains(&p) {
                pages.insert(p, record(vfl_page(cs, cs == 0 && pg > PAGES_PER_BLOCK), &VFL_SPARE));
            }
        }
        pages.insert(Page { cs, page: BBT_PAGE + 1 }, record(bbt.clone(), &zero));
    }
    for (cs, d) in gpt_pages(blocks, 0).into_iter().enumerate() {
        pages.insert(Page { cs, page: 2 * PAGES_PER_BLOCK }, record(d, &BLANK_SPARE));
    }
    pages.insert(Page { cs: 0, page: BBT_PAGE }, record(signature(epoch), &zero));
    pages
}

/// Fills `buf` from `r` as far as the file goes; the rest stays as it was (zeros: past the end reads as zeros).
fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(got)
}

/// The image store at `out` (created) for a flat volume of `blocks` 4 KiB blocks: metadata pages, then every
/// non-zero block at its predicted page. Returns (volume pages, metadata pages) written. The caller supplies the
/// actual HFS extent; a volume file shorter than that reads as zeros past its end.
pub fn write(volume: &Path, blocks: usize, epoch: u32, out: &Path) -> Result<(usize, usize)> {
    let mut f = File::open(volume).map_err(|e| crate::internal!("{}: {e}", volume.display()))?;
    let store = Store::create(out, &Image::n72())?;
    let meta = metadata_pages(blocks, epoch);
    for (p, d) in &meta {
        store.write(p.cs, p.page, d)?;
    }
    const CHUNK: usize = 1024;
    let mut buf = vec![0u8; CHUNK * PAGE];
    let mut rec = vec![0u8; PAGE + SPARE];
    rec[PAGE..].copy_from_slice(&BLANK_SPARE);
    let (mut n, mut written) = (0, 0);
    while n < blocks {
        let want = CHUNK.min(blocks - n);
        let got = read_up_to(&mut f, &mut buf[..want * PAGE])?;
        buf[got..want * PAGE].fill(0);
        for (i, blk) in buf[..want * PAGE].chunks_exact(PAGE).enumerate() {
            if blk.iter().all(|&b| b == 0) {
                continue; // a hole reads as the blank page, zero data
            }
            rec[..PAGE].copy_from_slice(blk);
            let p = predict(n + i);
            store.write(p.cs, p.page, &rec)?;
            written += 1;
        }
        n += want;
    }
    store.sync()?;
    Ok((written, meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn le(d: &[u8], o: usize, n: usize) -> u64 {
        (0..n).fold(0, |a, k| a | (d[o + k] as u64) << (8 * k))
    }

    /// ipod2g_nand.selfcheck.
    #[test]
    fn metadata_selfcheck() {
        let p = metadata_pages(128000, 1);
        assert!(p.len() == 50 && p.values().all(|d| d.len() == PAGE + SPARE));
        let hdr = &p[&Page { cs: 1, page: 256 }];
        let mut h = hdr[..0x5C].to_vec();
        h[0x10..0x14].fill(0);
        assert_eq!(crc(&h) as u64, le(hdr, 0x10, 4));
        assert_eq!(le(&p[&Page { cs: 2, page: 256 }], 0x28, 8), 128002);
        assert_eq!(predict(0), Page { cs: 3, page: 256 });
        assert_eq!(predict(1), Page { cs: 0, page: 384 });
    }

    #[test]
    fn partition_and_filesystem_have_the_same_alternate_header() {
        for blocks in [1, 127, 128, 129, 255, 256, 257, 1023, 1024, 1025, 128000, 1_835_008] {
            let pages = gpt_pages(blocks, 0);
            let (first, last) = (le(&pages[2], 0x20, 8), le(&pages[2], 0x28, 8));
            assert_eq!(first, 3);
            assert_eq!(last - first + 1, blocks as u64);
            assert_eq!(crc(&pages[2][..0x80]) as u64, le(&pages[1], 0x58, 4));
            let mut header = pages[1].clone();
            header[0x10..0x14].fill(0);
            assert_eq!(crc(&header[..0x5C]) as u64, le(&pages[1], 0x10, 4));
            assert_eq!(le(&pages[0], 0x1BE + 8, 4), 3);
            assert_eq!(le(&pages[0], 0x1BE + 12, 4), blocks as u64 + 10);
        }
        assert_eq!(le(&gpt_pages(100, 11)[2], 0x28, 8), 3 + 99 + 11);
    }

    /// The Python reference's 50 metadata pages (LegacyPreparationGoldens.n72Metadata, captured from qemu-ios
    /// 0be1499f24), with the two exact-partition GPT pages replacing recipe 1's.
    const EXACT_GPT: [(&str, &str); 2] = [
        ("1 256", "6c9e22f327f6adbeda01579125ea9d10d844275b0510d47beb32ff47a64c075e"),
        ("2 256", "ed87cbf501e2c060dc212f6f73b661956092494630f16c247093bbf7a2f63688"),
    ];
    const METADATA: [&str; 50] = [
        "0 0 d50120672cd8297fc9f3100f3493a29f33b52788ec5b1eb02d90a10aa83ec62f",
        "0 1 8cf30587585a8540d101c97293d1f6ab8ccd215708433aef7b06a9cca9388c94",
        "0 2 e87d070d5ddeb1e7a44bc5f9423989c6071e8f47738c71caacfc144253cf8349",
        "0 128 71a81e305757abed41179b7923513c210027bb1d30012d0cdd17a7a3912f1981",
        "0 129 a063cc249c5b8ac8b691001715aaabdee1e427a72d2a9c514ee4071c151d81a0",
        "0 130 0f76aa2e8d7f007c14f3e622ac56a7e26e937158beb124515ac7a88cbfde7285",
        "0 131 c71189abfc5ec5d4b06568976ee8838cf8d1d0ed74b5859abe908d8597d3c8cc",
        "0 132 c71189abfc5ec5d4b06568976ee8838cf8d1d0ed74b5859abe908d8597d3c8cc",
        "0 133 c71189abfc5ec5d4b06568976ee8838cf8d1d0ed74b5859abe908d8597d3c8cc",
        "0 134 c71189abfc5ec5d4b06568976ee8838cf8d1d0ed74b5859abe908d8597d3c8cc",
        "0 135 c71189abfc5ec5d4b06568976ee8838cf8d1d0ed74b5859abe908d8597d3c8cc",
        "0 256 196d3b167c13a349183237d9f7d9dccde2b3d4fe3d83134613c8828eeea41572",
        "0 524160 1ce801475bed14101f82e2cb2804fc028adaa269e32c130ec0868c13f3f4cc53",
        "0 524161 5b786fda3624538a14788e8b3bf408f8d7b40a55266ba12bcabbfedad1506210",
        "1 1 f8f86ecd4c30e2a9ae5a4bf0125251fc0084f7416bb28f55f1360b0bcb27d637",
        "1 2 0da8152409a25d564505942d3ede24321e8c24d01053399c6ecd950dc56f9cec",
        "1 128 2770c49d2622c57ae2afdba8af87639d9e19fa9aee15f22f8a834cfe40fab4f8",
        "1 129 dad6bb948017f8fad5cd69ea217800b8ec5f73937e9c6bf1a660531bb1afd63a",
        "1 130 bfb7b932d0656fb9b701131b807754e77b036162161a6a04f058f19968f8f6eb",
        "1 131 5b97e06f9d02e292e4e1338469a8d713fc4dd3af431d3ed8f615ffcbe291f66a",
        "1 132 5b97e06f9d02e292e4e1338469a8d713fc4dd3af431d3ed8f615ffcbe291f66a",
        "1 133 5b97e06f9d02e292e4e1338469a8d713fc4dd3af431d3ed8f615ffcbe291f66a",
        "1 134 5b97e06f9d02e292e4e1338469a8d713fc4dd3af431d3ed8f615ffcbe291f66a",
        "1 135 5b97e06f9d02e292e4e1338469a8d713fc4dd3af431d3ed8f615ffcbe291f66a",
        "1 256 3598ace9921724e91e3d908c0d5253fdb5ebc2d6980b270162f8297f08f4f97a",
        "1 524161 5b786fda3624538a14788e8b3bf408f8d7b40a55266ba12bcabbfedad1506210",
        "2 1 8288ce99b86a9eb0fc2b8c829f60711645ad061310eceb908b821deeba267df9",
        "2 2 cd988d153ceb85bc87ba7ee65a8b58dd2e7da871306d828731e376e49d8ff389",
        "2 128 fcf7d06f4a3ff0a6fc7a46541fc2f791b5b54fe0d09de8ca16e48ca0429e0c1b",
        "2 129 10104e368cfae81d4846c930a4295c21d4f28f2eee1fcbecd491dddf1eec5e0b",
        "2 130 1f075cd161b309aa80f91782e639249a94a80f3dac563793c951e5b244a3eef8",
        "2 131 5df6cf8a56051960a718e043c4fd4c202035bb073144fc833e0bc800c0dcc47f",
        "2 132 5df6cf8a56051960a718e043c4fd4c202035bb073144fc833e0bc800c0dcc47f",
        "2 133 5df6cf8a56051960a718e043c4fd4c202035bb073144fc833e0bc800c0dcc47f",
        "2 134 5df6cf8a56051960a718e043c4fd4c202035bb073144fc833e0bc800c0dcc47f",
        "2 135 5df6cf8a56051960a718e043c4fd4c202035bb073144fc833e0bc800c0dcc47f",
        "2 256 889df1c85e0955774a16c9dc069002e72674e4f19b16e0a2ac48cc982e430d20",
        "2 524161 5b786fda3624538a14788e8b3bf408f8d7b40a55266ba12bcabbfedad1506210",
        "3 1 e51cf4b0662ba923c4be54eff6d1c811aa6e56183e011d26a7ccff7c9775b320",
        "3 2 1f957892fe20cd587f9bfa7ae4680a705c07f1060b8b3eae5018572b26ae1876",
        "3 128 c345b45116e5ab615ad8e65d15782366219de181d12bea009be8e3821a4d3fbc",
        "3 129 b40cf216d0de062a9277fd3a503e4dd51f220593542347b1293f3bfcedf403ae",
        "3 130 6050b8f302ab045bd8cbaaabeba62a1f17508cc31ed4c92e1a6017bbb24f8750",
        "3 131 6050b8f302ab045bd8cbaaabeba62a1f17508cc31ed4c92e1a6017bbb24f8750",
        "3 132 6050b8f302ab045bd8cbaaabeba62a1f17508cc31ed4c92e1a6017bbb24f8750",
        "3 133 6050b8f302ab045bd8cbaaabeba62a1f17508cc31ed4c92e1a6017bbb24f8750",
        "3 134 6050b8f302ab045bd8cbaaabeba62a1f17508cc31ed4c92e1a6017bbb24f8750",
        "3 135 6050b8f302ab045bd8cbaaabeba62a1f17508cc31ed4c92e1a6017bbb24f8750",
        "3 255 43d923800461745b644ebabdde371a25639269a1b851032a88191f8c50af64e6",
        "3 524161 5b786fda3624538a14788e8b3bf408f8d7b40a55266ba12bcabbfedad1506210",
    ];

    #[test]
    fn metadata_preserves_legacy_except_exact_partition_gpt() {
        let want: Vec<String> = METADATA
            .iter()
            .map(|line| {
                let key = line.rsplit_once(' ').map(|(k, _)| k).unwrap_or(line);
                match EXACT_GPT.iter().find(|(k, _)| *k == key) {
                    Some((k, sha)) => format!("{k} {sha}"),
                    None => line.to_string(),
                }
            })
            .collect();
        let got: Vec<String> = metadata_pages(1_835_008, 4)
            .iter()
            .map(|(p, d)| format!("{} {} {}", p.cs, p.page, hex::encode(Sha256::digest(d))))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn write_skips_zero_blocks_and_pads_a_short_volume() {
        let dir = std::env::temp_dir().join(format!("lt-n72nand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut vol = vec![0u8; 10 * PAGE + 100]; // a short last block, zero-padded
        vol[0] = 1;
        vol[3 * PAGE + 7] = 2;
        vol[10 * PAGE + 99] = 3;
        std::fs::write(dir.join("volume.img"), &vol).unwrap();
        let out = dir.join("nand");
        assert_eq!(write(&dir.join("volume.img"), 20, 4, &out).unwrap(), (3, 50));
        let store = Store::open(&out, false).unwrap();
        let read = |n: usize| {
            let p = predict(n);
            store.read(p.cs, p.page).unwrap()
        };
        assert_eq!(read(0)[..PAGE], vol[..PAGE]);
        assert_eq!(read(10)[99], 3);
        assert_eq!(read(10)[PAGE..], BLANK_SPARE);
        assert_eq!(read(1), Image::n72().blank_record);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

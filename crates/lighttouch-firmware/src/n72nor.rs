//! The iPod touch 2G (n72ap) 1 MiB SPI NOR, synthesized from an identity. Port of imgtools/build_nor.py (synth_base +
//! build, --identity): IMG2 superblock, SysCfg (Mod#, Regn, SrNm, Batt), the IPSW's all_flash img3s packed at 0x8000
//! (padded to the 0x40 granularity, fullSize rewritten, SHSH wrapped under the emulated UID so iBoot's in-place unwrap
//! recovers it), and an nvram bank at 0xfc000 (debug-uarts, btaddr, wifiaddr).
//!
//! ```ignore
//! let nor = n72nor::build(&identity, &images, &types, None)?;     // None: wrap every image's SHSH (3.x+)
//! n72nor::ORDER                                                   // the stock image set, in the stock order
//! n72nor::img3_type(&img3)?                                       // "illb", "ibot", ...
//! ```
//! An identity is anything with string fields by key (identity.json's object, a map); see `Identity`.
use crate::error::Result;
use crate::n72nand::{crc, put};
use std::collections::{BTreeMap, HashMap};

pub const SIZE: usize = 0x100000;
const SYSCFG: usize = 0x4000;
pub const NVRAM: usize = 0xFC000;
pub const NVRAM_BANK: usize = 0x2000;
const IMG3_HEADER: usize = 0x14;
/// The image types the stock 5F138 NOR carries, in its order (build_nor.DEFAULT_ORDER).
pub const ORDER: [&str; 10] = ["illb", "ibot", "dtre", "logo", "nsrv", "bat0", "bat1", "recm", "glyC", "glyP"];
/// What iBoot encrypts under the UID key to get the SHSH wrapping key (its first AES request: mode 6, | 1 for
/// encrypt, | 8; the machine's aes-uid=hardware honors that bit).
const KDF_CONST: [u8; 16] =
    [0xdb, 0x1f, 0x5b, 0x33, 0x60, 0x6c, 0x5f, 0x1c, 0x19, 0x34, 0xaa, 0x66, 0x58, 0x9c, 0x06, 0x61];
/// The emulated S5L8720's UID (key_uid in hw/arm/ipod_touch_aes.h).
const UID_KEY: [u8; 16] =
    [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF];

/// A unit identity's string fields by key: model-number, region-info, serial-number, battery-serial, wifi-mac, bt-mac.
pub trait Identity {
    fn field(&self, key: &str) -> Option<&str>;
}

impl Identity for serde_json::Map<String, serde_json::Value> {
    fn field(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }
}

impl Identity for BTreeMap<String, String> {
    fn field(&self, key: &str) -> Option<&str> {
        self.get(key).map(String::as_str)
    }
}

impl<S: std::hash::BuildHasher> Identity for HashMap<String, String, S> {
    fn field(&self, key: &str) -> Option<&str> {
        self.get(key).map(String::as_str)
    }
}

impl Identity for [(&str, &str)] {
    fn field(&self, key: &str) -> Option<&str> {
        self.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
}

pub(crate) fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub(crate) fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// AES-128-CBC encryption without padding, zero IV; None unless `d` is whole blocks.
fn aes128_cbc_encrypt(d: &[u8], key: &[u8; 16]) -> Option<Vec<u8>> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::NoPadding};
    let mut out = d.to_vec();
    let len = out.len();
    let c = cbc::Encryptor::<aes::Aes128>::new(key.into(), &[0u8; 16].into());
    c.encrypt_padded_mut::<NoPadding>(&mut out, len).ok()?;
    Some(out)
}

/// The SHSH wrapping key iBoot derives: KDF_CONST encrypted under the UID.
pub fn wrapping_key() -> [u8; 16] {
    let mut key = [0u8; 16];
    if let Some(k) = aes128_cbc_encrypt(&KDF_CONST, &UID_KEY) {
        key.copy_from_slice(&k); // one whole block in, one out
    }
    key
}

/// An img3's type tag ("illb", ...), as the header stores it reversed at 0x10.
pub fn img3_type(img3: &[u8]) -> Result<String> {
    if img3.len() < IMG3_HEADER || img3[..4] != [0x33, 0x67, 0x6D, 0x49] {
        return Err(crate::unsupported!("not an img3"));
    }
    Ok(img3[16..20].iter().rev().map(|&b| b as char).collect())
}

/// build_nor.build over synth_base(identity): the whole NOR. `images` are the all_flash img3s by type; `types` the
/// ones packed, in order; `wrap_types` None wraps every packed image's SHSH (3.x+), Some(["ibot"]) only iBoot's (2.x).
pub fn build(
    id: &(impl Identity + ?Sized),
    images: &BTreeMap<String, Vec<u8>>,
    types: &[String],
    wrap_types: Option<&[String]>,
) -> Result<Vec<u8>> {
    let mut nor = base(id)?;
    let gran = 0x40;
    let start = gran * 0x200;
    if let Some(w) = wrap_types
        && !w.iter().all(|t| types.contains(t))
    {
        return Err(crate::internal!("SHSH wrap types must belong to the selected image set"));
    }
    let key = wrapping_key();
    let mut off = start;
    for t in types {
        let Some(img) = images.get(t) else { return Err(crate::unsupported!("all_flash has no img3 of type {t}")) };
        let mut img = img.clone();
        if wrap_types.is_none_or(|w| w.contains(t)) && !wrap_shsh(&mut img, &key) {
            return Err(crate::unsupported!("{t}: no SHSH tag to wrap, or its length is not a multiple of 16"));
        }
        let padded = (img.len() + gran - 1) & !(gran - 1);
        img.resize(padded, 0);
        if img.len() < IMG3_HEADER {
            return Err(crate::unsupported!("{t}: not an img3"));
        }
        put32(&mut img, 4, padded as u32);
        if off + padded > NVRAM {
            return Err(crate::unsupported!("NOR image area overflows the nvram partition ({t})"));
        }
        nor[off..off + padded].copy_from_slice(&img);
        off += padded;
    }
    put32(&mut nor, 0x10, off.div_ceil(gran) as u32); // the stock span over-counts by image_start; kept
    let c = crc(&nor[..0x30]);
    put32(&mut nor, 0x30, c);
    Ok(nor)
}

/// `nor` (a build) as `build` would have made it for `id`: the SysCfg block and the nvram bank are the only
/// identity-bearing bytes (the UID that wraps the SHSH is the machine's, not the unit's).
pub fn reidentify(nor: &[u8], id: &(impl Identity + ?Sized)) -> Result<Vec<u8>> {
    if nor.len() != SIZE {
        return Err(crate::unsupported!("nor.bin is {} bytes, not {SIZE}", nor.len()));
    }
    let mut b = nor.to_vec();
    let fresh = base(id)?;
    let image_start = 0x40 * 0x200;
    b[SYSCFG..image_start].copy_from_slice(&fresh[SYSCFG..image_start]);
    b[NVRAM..].copy_from_slice(&fresh[NVRAM..]);
    Ok(b)
}

/// Encrypts the SHSH tag (past sigCheckArea, so no digest covers it) under the UID-derived key. K48IBoot's NOR builder
/// calls this with the S5L8930 wrapping key.
pub(crate) fn wrap_shsh(b: &mut [u8], key: &[u8; 16]) -> bool {
    if b.len() < IMG3_HEADER {
        return false;
    }
    let nopack = le32(b, 8) as usize;
    let mut off = IMG3_HEADER;
    while off + 12 <= IMG3_HEADER + nopack && off + 12 <= b.len() {
        let total = le32(b, off + 4) as usize;
        let dlen = le32(b, off + 8) as usize;
        if total == 0 {
            break;
        }
        if &b[off..off + 4] == b"HSHS" && dlen.is_multiple_of(16) {
            let Some(d) = b.get(off + 12..off + 12 + dlen) else { return false };
            let Some(enc) = aes128_cbc_encrypt(d, key) else { return false };
            b[off + 12..off + 12 + dlen].copy_from_slice(&enc);
            return true;
        }
        off += total;
    }
    false
}

/// synth_base: IMG2 + SysCfg + nvram for the identity, an empty image area.
fn base(id: &(impl Identity + ?Sized)) -> Result<Vec<u8>> {
    let mut nor = vec![0u8; SIZE];
    nor[..4].copy_from_slice(b"2GMI");
    put32(&mut nor, 4, 0x40);
    put32(&mut nor, 8, 0);
    put32(&mut nor, 12, 0x200);
    write_syscfg(&mut nor, id)?;
    let upper = |k: &str| id.field(k).unwrap_or("").to_uppercase();
    let bank = nvram_bank(&[
        ("debug-uarts".into(), "1".into()),
        ("btaddr".into(), upper("bt-mac")),
        ("wifiaddr".into(), upper("wifi-mac")),
    ])?;
    nor[NVRAM..NVRAM + NVRAM_BANK].copy_from_slice(&bank);
    let c = crc(&nor[..0x30]);
    put32(&mut nor, 0x30, c);
    Ok(nor)
}

/// SysCfg at 0x4000 (the 1G's generate_nor.c writes the same block): Mod#, Regn, SrNm, Batt from the identity.
pub(crate) fn write_syscfg(nor: &mut [u8], id: &(impl Identity + ?Sized)) -> Result<()> {
    nor[SYSCFG..SYSCFG + 4].copy_from_slice(b"gfCS");
    for (i, v) in [0xC8u32, 0x2000, 0x0001_0001, 0, 4].into_iter().enumerate() {
        put32(nor, SYSCFG + 4 + 4 * i, v);
    }
    let tags =
        [("Mod#", "model-number"), ("Regn", "region-info"), ("SrNm", "serial-number"), ("Batt", "battery-serial")];
    for (i, (tag, key)) in tags.into_iter().enumerate() {
        let value = id.field(key).unwrap_or("").as_bytes();
        if value.len() > 16 {
            return Err(crate::unsupported!("{key} is longer than a SysCfg value"));
        }
        let o = SYSCFG + 0x18 + i * 0x14;
        let entry = &mut nor[o..o + 20];
        entry.fill(0);
        for (k, &b) in tag.as_bytes().iter().rev().enumerate() {
            entry[k] = b;
        }
        entry[4..4 + value.len()].copy_from_slice(value);
    }
    Ok(())
}

/// A CHRP nvram partition header: signature, checksum, length in 16-byte units, 12-byte name.
fn chrp(sig: u8, length: usize, name: &str) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[0] = sig;
    h[2] = ((length / 16) & 0xFF) as u8;
    h[3] = ((length / 16) >> 8) as u8;
    h[4..4 + name.len()].copy_from_slice(name.as_bytes());
    let mut s: u32 = 0;
    for &b in std::iter::once(&h[0]).chain(&h[2..]) {
        s += b as u32;
        if s > 0xFF {
            s = (s & 0xFF) + 1;
        }
    }
    h[1] = s as u8;
    h
}

/// One 8 KiB bank as iBoot writes it: "nvram" (adler32 of the rest, generation), "common", an empty "APL,OSXPanic"
/// and the free-space partition.
pub(crate) fn nvram_bank(common: &[(String, String)]) -> Result<Vec<u8>> {
    let mut bank = vec![0u8; NVRAM_BANK];
    bank[..16].copy_from_slice(&chrp(0x5A, 0x20, "nvram"));
    put32(&mut bank, 0x14, 0x10);
    let body: Vec<u8> = common.iter().flat_map(|(k, v)| format!("{k}={v}\0").into_bytes()).collect();
    if body.len() > 0x800 - 16 {
        return Err(crate::unsupported!("nvram common partition overflows"));
    }
    bank[0x20..0x30].copy_from_slice(&chrp(0x70, 0x800, "common"));
    bank[0x30..0x30 + body.len()].copy_from_slice(&body);
    bank[0x820..0x830].copy_from_slice(&chrp(0xA1, 0x810, "APL,OSXPanic"));
    bank[0x1030..0x1040].copy_from_slice(&chrp(0x7F, NVRAM_BANK - 0x1030, "wwwwwwwwwwww"));
    let a = crate::lzss::adler32(&bank[0x14..]);
    put(&mut bank, 0x10, a as u64, 4);
    Ok(bank)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// iBoot's first AES request encrypts the seed under the UID; the NOR wraps every SHSH with that key (openssl enc
    /// -aes-128-ecb of KDF_CONST under 0123...EF).
    #[test]
    fn wrapping_key_is_the_uid_encrypt() {
        assert_eq!(hex::encode(wrapping_key()), "435473c386c2c7df0b61d1e6ead8b50e");
    }

    /// A minimal img3: header, then a DATA tag and an SHSH tag of 16 bytes.
    fn img3(kind: &[u8; 4], fill: u8) -> Vec<u8> {
        let mut tags = Vec::new();
        for (tag, len) in [(b"ATAD", 8usize), (b"HSHS", 16)] {
            tags.extend_from_slice(tag);
            tags.extend_from_slice(&((12 + len) as u32).to_le_bytes());
            tags.extend_from_slice(&(len as u32).to_le_bytes());
            tags.extend(std::iter::repeat_n(fill, len));
        }
        let mut d = vec![0x33, 0x67, 0x6D, 0x49];
        d.extend_from_slice(&((IMG3_HEADER + tags.len()) as u32).to_le_bytes());
        d.extend_from_slice(&(tags.len() as u32).to_le_bytes());
        d.extend_from_slice(&[0; 4]);
        d.extend(kind.iter().rev());
        d.extend_from_slice(&tags);
        d
    }

    #[test]
    fn build_packs_wraps_and_identifies() {
        let id: &[(&str, &str)] = &[
            ("model-number", "MB528"),
            ("region-info", "LL/A"),
            ("serial-number", "LVRB8JACTYK"),
            ("wifi-mac", "02:ec:ea:1f:04:1c"),
            ("bt-mac", "02:ec:ea:1f:04:1d"),
        ];
        let images: BTreeMap<String, Vec<u8>> =
            [("illb", img3(b"illb", 1)), ("ibot", img3(b"ibot", 2))].into_iter().map(|(k, v)| (k.into(), v)).collect();
        assert_eq!(img3_type(&images["ibot"]).unwrap(), "ibot");
        let types: Vec<String> = vec!["illb".into(), "ibot".into()];
        let nor = build(id, &images, &types, Some(&["ibot".into()])).unwrap();
        assert_eq!(nor.len(), SIZE);
        // illb packed raw at 0x8000, padded to 0x40 with its fullSize rewritten; ibot after it, SHSH wrapped
        let a = &nor[0x8000..0x8080];
        assert_eq!(le32(a, 4), 0x80); // 0x14 + 20 + 28 bytes, padded
        assert_eq!(a[IMG3_HEADER + 20 + 12..IMG3_HEADER + 20 + 28], [1u8; 16]);
        let b = &nor[0x8080..0x8100];
        assert_eq!(img3_type(b).unwrap(), "ibot");
        assert_eq!(b[IMG3_HEADER + 20 + 12..IMG3_HEADER + 20 + 28], aes128_cbc_encrypt(&[2u8; 16], &wrapping_key()).unwrap()[..]);
        assert_eq!(le32(&nor, 0x30), crc(&nor[..0x30]));
        assert_eq!(&nor[0x4000 + 0x18..0x4000 + 0x18 + 9], b"#doMMB528");
        let text = String::from_utf8_lossy(&nor[NVRAM + 0x30..NVRAM + 0x830]).into_owned();
        assert!(text.starts_with("debug-uarts=1\0btaddr=02:EC:EA:1F:04:1D\0wifiaddr=02:EC:EA:1F:04:1C\0"));
        // an image without an SHSH tag to wrap is refused; reidentify only touches SysCfg and nvram
        let mut bare = images.clone();
        bare.get_mut("illb").unwrap()[IMG3_HEADER + 20..IMG3_HEADER + 24].copy_from_slice(b"XXXX");
        assert!(build(id, &bare, &types, None).is_err());
        assert!(build(id, &bare, &types, Some(&["ibot".into()])).is_ok());
        let other: &[(&str, &str)] = &[("serial-number", "X")];
        let re = reidentify(&nor, other).unwrap();
        assert_eq!(re[..0x4000], nor[..0x4000]);
        assert_eq!(re[0x8000..NVRAM], nor[0x8000..NVRAM]);
        assert_eq!(reidentify(&re, id).unwrap(), nor);
    }
}

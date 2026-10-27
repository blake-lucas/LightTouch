//! IMG3 tags and AES-CBC payload decryption, the iPhone OS 1.x 8900 container and its IMG2 images.
//!
//! ```ignore
//! img3::tags(&data)?["DATA"]                       // (offset, data length) per tag, last wins
//! img3::decrypt(&data, &iv, &key, false)?          // 3.x+: the final partial block is encrypted into the padding
//! img3::decrypt(&data, &iv, &key, true)?           // 2.x: the final partial block is plaintext
//! apple8900::body(&data)?                          // format 3 AES-128-CBC under key 0x837, format 4 plain
//! img2::payload(&body)?                            // "2gmI": the plaintext image
//! ```
//! Which tail convention a firmware uses is decided once, on the kernelcache (see decrypt.rs).
use crate::error::Result;
use std::collections::BTreeMap;

pub(crate) fn u32le(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag {
    pub offset: usize,
    pub data_length: usize,
}

/// Tag magic (as read, e.g. "DATA", "KBAG") -> its header offset and data length.
pub fn tags(d: &[u8]) -> Result<BTreeMap<String, Tag>> {
    if d.len() < 0x14 || d[0..4] != [0x33, 0x67, 0x6D, 0x49] {
        return Err(crate::unsupported!("not an img3"));
    }
    let full = u32le(d, 4) as usize;
    let mut off = 0x14;
    let mut tags = BTreeMap::new();
    while off + 12 <= full && off + 12 <= d.len() {
        let magic: String = d[off..off + 4].iter().rev().map(|&b| b as char).collect();
        let total = u32le(d, off + 4) as usize;
        let dlen = u32le(d, off + 8) as usize;
        if total < 12 {
            break;
        }
        tags.insert(magic, Tag { offset: off, data_length: dlen });
        off += total;
    }
    Ok(tags)
}

fn data_tag(d: &[u8]) -> Result<Tag> {
    tags(d)?.get("DATA").copied().ok_or_else(|| crate::unsupported!("img3 has no DATA tag"))
}

/// The DATA payload as stored (an unencrypted img3's plaintext, else the ciphertext).
pub fn payload(d: &[u8]) -> Result<Vec<u8>> {
    let tag = data_tag(d)?;
    let start = tag.offset + 12;
    if start + tag.data_length > d.len() {
        return Err(crate::unsupported!("img3 DATA runs past the file"));
    }
    Ok(d[start..start + tag.data_length].to_vec())
}

pub fn decrypt(d: &[u8], iv: &[u8], key: &[u8], plain_tail: bool) -> Result<Vec<u8>> {
    let tag = data_tag(d)?;
    let start = tag.offset + 12;
    let dlen = tag.data_length;
    if start + dlen > d.len() {
        return Err(crate::unsupported!("img3 DATA runs past the file"));
    }
    if plain_tail {
        let n = dlen & !15;
        let mut out = aes_cbc_decrypt(&d[start..start + n], iv, key)?;
        out.extend_from_slice(&d[start + n..start + dlen]);
        return Ok(out);
    }
    let n = (dlen + 15) & !15;
    if start + n > d.len() {
        return Err(crate::unsupported!("img3 DATA padding runs past the file"));
    }
    let mut out = aes_cbc_decrypt(&d[start..start + n], iv, key)?;
    out.truncate(dlen);
    Ok(out)
}

/// AES-CBC decryption without padding; `buf` must be whole blocks. 16, 24 or 32-byte keys.
pub fn aes_cbc_decrypt(buf: &[u8], iv: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};
    if iv.len() != 16 || buf.len() % 16 != 0 {
        return Err(crate::unsupported!("AES: key {} B, iv {} B, data {} B", key.len(), iv.len(), buf.len()));
    }
    let mut out = buf.to_vec();
    let result = match key.len() {
        16 => cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv).map(|c| c.decrypt_padded_mut::<NoPadding>(&mut out).map(|_| ())),
        24 => cbc::Decryptor::<aes::Aes192>::new_from_slices(key, iv).map(|c| c.decrypt_padded_mut::<NoPadding>(&mut out).map(|_| ())),
        32 => cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv).map(|c| c.decrypt_padded_mut::<NoPadding>(&mut out).map(|_| ())),
        _ => return Err(crate::unsupported!("AES: key {} B, iv {} B, data {} B", key.len(), iv.len(), buf.len())),
    };
    match result {
        Ok(Ok(())) => Ok(out),
        _ => Err(crate::internal!("AES-CBC decryption failed")),
    }
}

/// iPhone OS 1.x containers: every IPSW component is an 8900 container, and the all_flash images inside are IMG2.
pub mod apple8900 {
    use super::{aes_cbc_decrypt, u32le};
    use crate::error::Result;

    const HEADER_SIZE: usize = 0x800;
    /// The S5L8900's key 0x837, what the bootrom's 8900 engine decrypts format-3 containers with.
    const KEY_837: [u8; 16] = [0x18, 0x84, 0x58, 0xA6, 0xD1, 0x50, 0x34, 0xDF, 0xE3, 0x86, 0xF2, 0x3B, 0x61, 0xD4, 0x37, 0x74];

    pub fn is_container(d: &[u8]) -> bool {
        d.starts_with(b"8900")
    }

    /// The payload, decrypted when the container says so. A final partial block is stored in the clear.
    pub fn body(d: &[u8]) -> Result<Vec<u8>> {
        if d.len() < HEADER_SIZE || !is_container(d) || &d[4..7] != b"1.0" {
            return Err(crate::unsupported!("not an 8900 v1.0 container"));
        }
        let size = u32le(d, 0xC) as usize;
        if HEADER_SIZE + size > d.len() {
            return Err(crate::unsupported!("8900 payload runs past the file"));
        }
        let body = &d[HEADER_SIZE..HEADER_SIZE + size];
        match d[7] {
            4 => Ok(body.to_vec()),
            3 => {
                let n = size & !15;
                let mut out = aes_cbc_decrypt(&body[..n], &[0u8; 16], &KEY_837)?;
                out.extend_from_slice(&body[n..]);
                Ok(out)
            }
            other => Err(crate::unsupported!("8900 format {other} (only 3, encrypted, and 4, plain, are known)")),
        }
    }
}

pub mod img2 {
    use super::u32le;
    use crate::error::Result;

    pub const HEADER_SIZE: usize = 0x400;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Header {
        /// "ibot", "dtre", "logo", ... (stored reversed, as IMG3 types are).
        pub kind: String,
        pub load_address: u32,
        pub padded_length: usize,
        pub data_length: usize,
    }

    pub fn header(body: &[u8]) -> Result<Header> {
        if body.len() < HEADER_SIZE || !body.starts_with(b"2gmI") {
            return Err(crate::unsupported!("not an IMG2 image"));
        }
        let kind: String = body[4..8].iter().rev().map(|&b| b as char).collect();
        let h = Header {
            load_address: u32le(body, 0xC),
            padded_length: u32le(body, 0x10) as usize,
            data_length: u32le(body, 0x14) as usize,
            kind,
        };
        if h.data_length > h.padded_length || HEADER_SIZE + h.padded_length > body.len() {
            return Err(crate::unsupported!("IMG2 {} data runs past the image", h.kind));
        }
        Ok(h)
    }

    pub fn payload(body: &[u8]) -> Result<Vec<u8>> {
        let h = header(body)?;
        Ok(body[HEADER_SIZE..HEADER_SIZE + h.data_length].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_plaintext_payload() {
        // magic, full size, size no tags, ... then one DATA tag with 5 bytes of data.
        let mut d = Vec::new();
        d.extend_from_slice(&[0x33, 0x67, 0x6D, 0x49]);
        d.extend_from_slice(&(0x14u32 + 20).to_le_bytes());
        d.extend_from_slice(&[0; 12]);
        d.extend_from_slice(b"ATAD");
        d.extend_from_slice(&20u32.to_le_bytes());
        d.extend_from_slice(&5u32.to_le_bytes());
        d.extend_from_slice(b"hello\0\0\0");
        let t = tags(&d).unwrap();
        assert_eq!(t["DATA"], Tag { offset: 0x14, data_length: 5 });
        assert_eq!(payload(&d).unwrap(), b"hello");
    }
}

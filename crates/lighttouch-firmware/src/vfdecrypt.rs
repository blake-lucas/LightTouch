//! Decrypts an Apple "encrcdsa" v2 disk image (an iOS rootfs DMG) with its known 36-byte key (AES-128 key +
//! HMAC-SHA1 key); chunk n's IV is HMAC-SHA1(hmac_key, be32(n))[..16]. Ports imgtools/vfdecrypt.c.
//!
//! ```ignore
//! ipsw.stream(&os_image, |reader| vfdecrypt::decrypt(reader, &out, &key36))?;
//! ```
//! The input is read once, front to back (it can come straight out of the zip), in batches of whole chunks that
//! are decrypted on every core: each chunk's IV depends only on its index.
use crate::error::{Code, Error, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

pub fn decrypt(input: &mut dyn Read, output: &Path, key: &[u8]) -> Result<()> {
    if key.len() != 36 {
        return Err(Error::new(Code::KeyMissing, format!("vfdecrypt key must be 36 bytes, got {}", key.len())));
    }
    let (aes_key, hmac_key) = key.split_at(16);
    let mut header = [0u8; 0x100];
    if read_fully(input, &mut header)? != 0x100 || &header[..8] != b"encrcdsa" {
        return Err(crate::unsupported!("not encrcdsa"));
    }
    let be = |o: usize, n: usize| header[o..o + n].iter().fold(0u64, |v, &b| v << 8 | b as u64);
    let bs = be(52, 4) as usize;
    let data_offset = be(64, 8) as usize;
    let mut left = be(56, 8);
    if bs == 0 || bs % 16 != 0 || bs > 1 << 24 {
        return Err(crate::unsupported!("encrcdsa block size {bs}"));
    }
    let per_batch = ((8 << 20) / bs).max(1);
    let mut ct = vec![0u8; per_batch * bs];
    let mut pt = vec![0u8; per_batch * bs];
    // Position at the data: header bytes already read count toward it; skip forward, never back.
    let mut carried = 0;
    if data_offset < 0x100 {
        carried = 0x100 - data_offset;
        ct[..carried].copy_from_slice(&header[data_offset..]);
    } else {
        let mut skip = data_offset - 0x100;
        while skip > 0 {
            let n = read_fully(input, &mut ct[..skip.min(bs)])?;
            if n == 0 {
                return Err(crate::unsupported!("short image"));
            }
            skip -= n;
        }
    }
    let mut out = File::create(output).map_err(|e| crate::internal!("cannot create {}: {e}", output.display()))?;
    let slices = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
    let mut n: u32 = 0;
    while left > 0 {
        let r = carried + read_fully(input, &mut ct[carried..])?;
        carried = 0;
        if r == 0 {
            return Err(crate::unsupported!("short image"));
        }
        if r % 16 != 0 {
            return Err(crate::unsupported!("encrcdsa chunk {n} is {} bytes, not whole AES blocks", r % bs));
        }
        let chunks = r.div_ceil(bs);
        let per = chunks.div_ceil(slices);
        std::thread::scope(|scope| {
            for (s, (c, p)) in ct[..r].chunks(per * bs).zip(pt[..r].chunks_mut(per * bs)).enumerate() {
                let first = n + (s * per) as u32;
                scope.spawn(move || decrypt_chunks(c, p, bs, first, aes_key, hmac_key));
            }
        });
        let take = (r as u64).min(left) as usize;
        out.write_all(&pt[..take])?;
        left -= take as u64;
        n += chunks as u32;
    }
    out.flush()?;
    Ok(())
}

fn decrypt_chunks(ct: &[u8], pt: &mut [u8], bs: usize, first: u32, aes_key: &[u8], hmac_key: &[u8]) {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};
    use hmac::{Hmac, Mac};
    for (k, (c, p)) in ct.chunks(bs).zip(pt.chunks_mut(bs)).enumerate() {
        let index = first + k as u32;
        let mut mac = Hmac::<sha1::Sha1>::new_from_slice(hmac_key).expect("HMAC takes any key length");
        mac.update(&index.to_be_bytes());
        let iv = mac.finalize().into_bytes();
        p.copy_from_slice(c);
        let cipher = cbc::Decryptor::<aes::Aes128>::new_from_slices(aes_key, &iv[..16]).expect("16-byte key and IV");
        let _ = cipher.decrypt_padded_mut::<NoPadding>(p);
    }
}

/// Reads until `buf` is full or EOF; returns the bytes read.
fn read_fully(input: &mut dyn Read, buf: &mut [u8]) -> Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match input.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(crate::unsupported!("reading the encrypted image: {e}")),
        }
    }
    Ok(got)
}

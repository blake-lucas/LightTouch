//! The iPod touch 1G (n45ap) and original iPhone (m68ap) 1 MiB NOR. Port of devos50's qemu-ios-generate-nor (branch
//! ipod_touch_1g, generate_nor.c + aes.c): SysCfg at 0x4000 (n72nor's block), the IMG2 directory header at 0x8400, the
//! IPSW's all_flash IMG2 images from block 1040 on (0x40-byte blocks, five spare blocks after each), each re-signed the
//! way a restore does on the device, and the nvram bank at 0xfc000 (boot-args, wifiaddr, btaddr).
//!
//! ```ignore
//! let nor = n45nor::build(&identity, &images, n45nor::BOOT_ARGS)?;   // IMG2 bodies (8900 payloads) by type
//! ```
//! Re-signing: the header and data hashes are SHA-1s encrypted under the IMG2 verify key, which the device derives
//! from its UID. The emulated S5L8900's UID engine (qemu-ios ipod_touch_aes.c `s5l8900-compat`) keeps devos50's
//! convention: key 0123456789ABCDEF x2, and encryption run with the *decryption* key schedule (OpenSSL
//! AES_set_decrypt_key + AES_cbc_encrypt(..., AES_ENCRYPT)), which no AES library does, so AesCore below carries a
//! small AES core.
use crate::error::Result;
use crate::img3::img2::HEADER_SIZE;
use crate::n72nand::crc;
use crate::n72nor::{Identity, NVRAM_BANK, le32, nvram_bank, put32, write_syscfg};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::sync::LazyLock;

pub const SIZE: usize = 0x100000;
const BLOCK: usize = 0x40;
const DIRECTORY: usize = 0x8400;
const FIRST_BLOCK: usize = 1040;
pub const NVRAM: usize = 0xFC000;
/// generate_nor.c's image order. LLB and iBoot stay out: the machine enters iBoot-204 itself.
pub const ORDER: [&str; 7] = ["dtre", "batC", "logo", "nsrv", "batl", "batL", "recm"];
/// generate_nor.c's nvram boot-args (the kernel's serial console, root on the NAND's first partition).
pub const BOOT_ARGS: &str = "debug=0x8 kextlog=0xfff cpus=1 rd=disk0s1 serial=1 io=0xffff8fff";
const HASH_PADDING: [u8; 44] = [
    0xad, 0x2e, 0xe3, 0x8d, 0x2d, 0x9b, 0xe4, 0x35, 0x99, 0x04, 0x44, 0x33, 0x65, 0x3d, 0xf0, 0x74, 0x98, 0xd8, 0x56,
    0x3b, 0x4f, 0xf9, 0x6a, 0x55, 0x45, 0xce, 0x82, 0xf2, 0x9a, 0x5a, 0xc2, 0xbc, 0x47, 0x61, 0x6d, 0x65, 0x4f, 0x76,
    0x65, 0x72, 0xa6, 0xa0, 0x99, 0x13,
];

/// `images`: IMG2 bodies by type (every ORDER type must be there).
pub fn build(id: &(impl Identity + ?Sized), images: &BTreeMap<String, Vec<u8>>, boot_args: &str) -> Result<Vec<u8>> {
    let mut nor = vec![0u8; SIZE];
    write_syscfg(&mut nor, id)?;
    nor[DIRECTORY..DIRECTORY + 4].copy_from_slice(b"2GMI");
    put32(&mut nor, DIRECTORY + 4, BLOCK as u32);
    put32(&mut nor, DIRECTORY + 8, FIRST_BLOCK as u32);
    put32(&mut nor, DIRECTORY + 16, 512 * 1024);
    let c = crc(&nor[DIRECTORY..DIRECTORY + 0x30]);
    put32(&mut nor, DIRECTORY + 0x30, c);
    let mut off = FIRST_BLOCK * BLOCK;
    for t in ORDER {
        let Some(body) = images.get(t) else { return Err(crate::unsupported!("all_flash has no IMG2 of type {t}")) };
        let img = sign(body)?;
        if off + img.len() > NVRAM {
            return Err(crate::unsupported!("NOR image area overflows the nvram partition ({t})"));
        }
        nor[off..off + img.len()].copy_from_slice(&img);
        off += (img.len() / BLOCK + 5) * BLOCK;
    }
    // iBoot-204 fills arm-io/sdio's local-mac-address from nvram wifiaddr (its SysCfg fallback is a stub returning
    // 0, so without it lockdownd hashes 00:00:00:00:00:00 into the UDID); the iPhone's iBoot fills bluetooth's
    // from btaddr.
    let mut common = vec![("boot-args".to_string(), boot_args.to_string())];
    for (var, key) in [("wifiaddr", "wifi-mac"), ("btaddr", "bt-mac")] {
        if let Some(v) = id.field(key) {
            common.push((var.into(), v.to_uppercase()));
        }
    }
    nor[NVRAM..NVRAM + NVRAM_BANK].copy_from_slice(&nvram_bank(&common)?);
    Ok(nor)
}

/// add_img2: block count, the trusted-write and encrypted flags, data hash, header CRC, extension CRC, header hash.
fn sign(body: &[u8]) -> Result<Vec<u8>> {
    let mut b = body.to_vec();
    if b.len() < HEADER_SIZE {
        return Err(crate::unsupported!("IMG2 shorter than its header"));
    }
    let padded = le32(&b, 0x10) as usize;
    if HEADER_SIZE + padded > b.len() {
        return Err(crate::unsupported!("IMG2 data runs past the image"));
    }
    let blocks = (b.len() / BLOCK + 5) as u32;
    put32(&mut b, 0x18, blocks);
    let flags = le32(&b, 0x1C) | 1 << 24 | 1 << 1;
    put32(&mut b, 0x1C, flags);
    let mut data_hash = Sha1::digest(&b[HEADER_SIZE..HEADER_SIZE + padded]).to_vec();
    data_hash.extend_from_slice(&HASH_PADDING);
    b[0x20..0x60].copy_from_slice(&img2_verify_encrypt(&data_hash));
    let c = crc(&b[..0x64]);
    put32(&mut b, 0x64, c);
    if flags & 1 << 30 != 0 {
        let next = le32(&b, 0x60) as usize;
        if 0x6C + next > HEADER_SIZE {
            return Err(crate::unsupported!("IMG2 extension runs past the header"));
        }
        let c = crc(&b[0x6C..0x6C + next]);
        put32(&mut b, 0x68, c);
    }
    let mut header_hash = Sha1::digest(&b[..0x3E0]).to_vec();
    header_hash.extend_from_slice(&HASH_PADDING[..12]);
    b[0x3E0..0x400].copy_from_slice(&img2_verify_encrypt(&header_hash));
    Ok(b)
}

const UID: [u8; 16] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF];
const VERIFY_SEED: [u8; 16] =
    [0xCD, 0xF3, 0x45, 0xB3, 0x12, 0xE7, 0x48, 0x85, 0x8B, 0xBE, 0x21, 0x47, 0xF0, 0xE5, 0x80, 0x88];
const VERIFY_IV: [u8; 16] =
    [0x11, 0x5D, 0x70, 0x41, 0x82, 0x4B, 0x98, 0x6F, 0xBB, 0x99, 0x6C, 0x9C, 0x69, 0x78, 0xF1, 0xA5];

/// aes_img2verify_encrypt: a 256-bit custom key of 16 zero bytes then the verify key (aes_setup: the seed run
/// through the UID, CBC under VERIFY_IV, each word byte-swapped), zero IV.
static VERIFY: LazyLock<AesCore> = LazyLock::new(|| {
    let k = AesCore::new(&UID).cbc_encrypt(&VERIFY_SEED, &VERIFY_IV).unwrap_or_default(); // one whole block
    let mut key = vec![0u8; 16];
    for w in k.chunks_exact(4) {
        key.extend(w.iter().rev());
    }
    AesCore::new(&key)
});

fn img2_verify_encrypt(d: &[u8]) -> Vec<u8> {
    // callers pass whole blocks (64 and 32 bytes)
    VERIFY.cbc_encrypt(d, &[0; 16]).unwrap_or_default()
}

/// AES encryption rounds over the *decryption* key schedule (OpenSSL's AES_set_decrypt_key: the encryption round keys
/// reversed, InvMixColumns on all but the first and last). Byte-oriented; only NOR signing uses it.
struct AesCore {
    /// Round keys, 16 bytes each, rounds + 1 of them.
    keys: Vec<[u8; 16]>,
}

static SBOX: LazyLock<[u8; 256]> = LazyLock::new(|| {
    let mut s = [0u8; 256];
    for x in 0..256usize {
        let inv = if x == 0 { 0 } else { (1..=255u8).find(|&y| mul(x as u8, y) == 1).unwrap_or(0) };
        let mut r = inv;
        for i in 1..=4 {
            r ^= inv.rotate_left(i);
        }
        s[x] = r ^ 0x63;
    }
    s
});

fn mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = (a << 1) ^ if a & 0x80 != 0 { 0x1B } else { 0 };
        b >>= 1;
    }
    p
}

fn inv_mix(c: &[u8]) -> [u8; 4] {
    std::array::from_fn(|i| mul(c[i], 14) ^ mul(c[(i + 1) % 4], 11) ^ mul(c[(i + 2) % 4], 13) ^ mul(c[(i + 3) % 4], 9))
}

impl AesCore {
    fn new(key: &[u8]) -> AesCore {
        let sbox = &*SBOX;
        let nk = key.len() / 4;
        let rounds = nk + 6;
        let mut w: Vec<[u8; 4]> = key.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let mut rcon = 1u8;
        for i in nk..4 * (rounds + 1) {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [t[1], t[2], t[3], t[0]].map(|b| sbox[b as usize]);
                t[0] ^= rcon;
                rcon = mul(rcon, 2);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|b| sbox[b as usize]);
            }
            let prev = w[i - nk];
            w.push(std::array::from_fn(|k| prev[k] ^ t[k]));
        }
        let enc: Vec<[u8; 16]> =
            (0..=rounds).map(|r| std::array::from_fn(|k| w[4 * r + k / 4][k % 4])).collect();
        let keys = enc
            .iter()
            .rev()
            .enumerate()
            .map(|(r, k)| {
                if r == 0 || r == rounds {
                    *k
                } else {
                    let mut out = [0u8; 16];
                    for c in 0..4 {
                        out[4 * c..4 * c + 4].copy_from_slice(&inv_mix(&k[4 * c..4 * c + 4]));
                    }
                    out
                }
            })
            .collect();
        AesCore { keys }
    }

    fn encrypt_block(&self, input: &[u8; 16]) -> [u8; 16] {
        let sbox = &*SBOX;
        let rounds = self.keys.len() - 1;
        let mut s: [u8; 16] = std::array::from_fn(|i| input[i] ^ self.keys[0][i]);
        for r in 1..=rounds {
            s = s.map(|b| sbox[b as usize]);
            let t = s;
            s = std::array::from_fn(|i| t[(i + 4 * (i % 4)) % 16]); // ShiftRows over column-major state
            if r != rounds {
                let t = s;
                for c in (0..16).step_by(4) {
                    let a = &t[c..c + 4];
                    for i in 0..4 {
                        s[c + i] = mul(a[i], 2) ^ mul(a[(i + 1) % 4], 3) ^ a[(i + 2) % 4] ^ a[(i + 3) % 4];
                    }
                }
            }
            for (b, k) in s.iter_mut().zip(&self.keys[r]) {
                *b ^= k;
            }
        }
        s
    }

    fn cbc_encrypt(&self, d: &[u8], iv: &[u8; 16]) -> Option<Vec<u8>> {
        if !d.len().is_multiple_of(16) {
            return None;
        }
        let mut prev = *iv;
        let mut out = Vec::with_capacity(d.len());
        for block in d.chunks_exact(16) {
            prev = self.encrypt_block(&std::array::from_fn(|i| block[i] ^ prev[i]));
            out.extend_from_slice(&prev);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the encryption key schedule this core is plain AES: FIPS-197's AES-128 and AES-256 vectors check the
    /// round function, so only the schedule swap differs from the standard.
    #[test]
    fn core_matches_fips197_with_the_encryption_schedule() {
        fn standard(key: &[u8]) -> AesCore {
            // undo the swap: reverse back, InvMixColumns' inverse is MixColumns (apply InvMix three times)
            let mut core = AesCore::new(key);
            let rounds = core.keys.len() - 1;
            core.keys.reverse();
            for r in 1..rounds {
                for c in 0..4 {
                    let mut col: [u8; 4] = core.keys[r][4 * c..4 * c + 4].try_into().unwrap();
                    for _ in 0..3 {
                        col = inv_mix(&col);
                    }
                    core.keys[r][4 * c..4 * c + 4].copy_from_slice(&col);
                }
            }
            core
        }
        let pt: [u8; 16] = std::array::from_fn(|i| (i as u8) * 0x11);
        let k128: Vec<u8> = (0..16).collect();
        let k256: Vec<u8> = (0..32).collect();
        assert_eq!(hex::encode(standard(&k128).encrypt_block(&pt)), "69c4e0d86a7b0430d8cdb78070b4c55a");
        assert_eq!(hex::encode(standard(&k256).encrypt_block(&pt)), "8ea2b7ca516745bfeafc49904b496089");
    }

    /// iBoot-204 fills local-mac-address only from nvram wifiaddr: the NOR's nvram carries the identity's Wi-Fi MAC,
    /// and none when the identity has none (devos50's set).
    #[test]
    fn nvram_carries_the_wifi_mac() {
        let mut body = vec![0u8; HEADER_SIZE + 16];
        body[0x10] = 16;
        let images: BTreeMap<String, Vec<u8>> = ORDER.iter().map(|t| (t.to_string(), body.clone())).collect();
        let id: &[(&str, &str)] = &[("model-number", "MA623"), ("wifi-mac", "02:1a:2b:3c:4d:5e")];
        let nvram = |nor: &[u8]| String::from_utf8_lossy(&nor[NVRAM + 0x30..NVRAM + 0x830]).into_owned();
        let text = nvram(&build(id, &images, BOOT_ARGS).unwrap());
        assert!(text.contains("\0wifiaddr=02:1A:2B:3C:4D:5E\0") && !text.contains("btaddr"));
        let devos50: &[(&str, &str)] = &[
            ("model-number", "MA623"),
            ("region-info", "B/LL"),
            ("serial-number", "ABCDEFG"),
            ("battery-serial", "690476146348"),
        ];
        let nor = build(devos50, &images, BOOT_ARGS).unwrap();
        assert!(!nvram(&nor).contains("wifiaddr"));
        // images from block 1040, five spare blocks after each; each signed (block count, flags)
        let first = FIRST_BLOCK * BLOCK;
        assert_eq!(le32(&nor, first + 0x18), ((HEADER_SIZE + 16) / BLOCK + 5) as u32);
        assert_eq!(le32(&nor, first + 0x1C), 1 << 24 | 1 << 1);
        assert_eq!(le32(&nor, first + 0x64), crc(&nor[first..first + 0x64]));
        let second = first + ((HEADER_SIZE + 16) / BLOCK + 5) * BLOCK;
        assert_eq!(nor[second..second + 0x400], nor[first..first + 0x400]);
        assert!(build(devos50, &BTreeMap::new(), BOOT_ARGS).is_err());
    }
}

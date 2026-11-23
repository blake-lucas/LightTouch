//! Decrypts an IPSW's boot chain, kernelcache, ramdisks and rootfs into a directory:
//!   iBSS.bin iBEC.bin iBoot.bin LLB.bin DeviceTree.bin AppleLogo.bin kernelcache.mach
//!   <RestoreRamDisk>-ramdisk.dmg <UpdateRamDisk>-ramdisk.dmg rootfs.dmg (vfdecrypted UDIF)
//!
//! The tail convention (3.x+: the final partial AES block is encrypted into the tag padding; 2.x: plaintext) is
//! decided on the kernelcache, the one component with a checksum: if its complzss length or Adler-32 fails with
//! the padded decrypt, every img3 of the IPSW is decrypted with a plaintext tail.
//!
//! 1.x components are 8900 containers and need no keys: all_flash images come out as their IMG2 payload,
//! everything else as the container's body. Only the rootfs is keyed.
use crate::entry::{Entry, from_hex};
use crate::error::{Code, Error, Result};
use crate::img3::{self, apple8900, img2};
use crate::ipsw::{Ipsw, build_components};
use crate::lzss;
use crate::vfdecrypt;
use std::collections::BTreeMap;
use std::path::Path;

/// Output name -> BuildManifest component.
pub const COMPONENTS: [(&str, &str); 7] = [
    ("iBSS", "iBSS"),
    ("iBEC", "iBEC"),
    ("iBoot", "iBoot"),
    ("LLB", "LLB"),
    ("DeviceTree", "DeviceTree"),
    ("AppleLogo", "AppleLogo"),
    ("Kernelcache", "KernelCache"),
];

pub struct Decrypted {
    pub plain_tail: bool,
    /// Component -> IPSW path.
    pub paths: BTreeMap<String, String>,
    /// Output file names written, in order.
    pub files: Vec<String>,
}

pub fn decrypt(ipsw: &Ipsw, entry: &Entry, dir: &Path, rootfs: bool, log: &dyn Fn(&str)) -> Result<Decrypted> {
    let comp = build_components(ipsw, &entry.board)?;
    std::fs::create_dir_all(dir)?;
    let path = |c: &str| -> Result<String> {
        comp.get(c).cloned().ok_or_else(|| crate::unsupported!("{}: the IPSW names no {c}", entry.id))
    };
    let img3_key = |p: &str| -> Result<(Vec<u8>, Vec<u8>)> {
        let k = entry.key_for_path(p)?;
        match (k.iv.as_deref().and_then(from_hex), from_hex(&k.key)) {
            (Some(iv), Some(key)) => Ok((iv, key)),
            _ => Err(Error::new(Code::KeyMissing, format!("{}: no IV/key for {}", entry.id, k.file))),
        }
    };
    let has_kbag = |raw: &[u8]| img3::tags(raw).map(|t| t.contains_key("KBAG"));
    let dec = |p: &str, plain_tail: bool| -> Result<Vec<u8>> {
        let raw = ipsw.read(p)?;
        if apple8900::is_container(&raw) {
            let body = apple8900::body(&raw)?;
            return if body.starts_with(b"2gmI") { img2::payload(&body) } else { Ok(body) };
        }
        // 2.x DFU stages have no KBAG: the payload is in the clear.
        if !has_kbag(&raw)? {
            return img3::payload(&raw);
        }
        let (iv, key) = img3_key(p)?;
        img3::decrypt(&raw, &iv, &key, plain_tail)
    };

    let kc_path = path("KernelCache")?;
    let kc_raw = ipsw.read(&kc_path)?;
    let mut plain_tail = false;
    let kernel = if apple8900::is_container(&kc_raw) {
        lzss::complzss(&apple8900::body(&kc_raw)?)?
    } else if !has_kbag(&kc_raw)? {
        // Several 10.x kernelcaches carry no KBAG: the payload is the plain complzss image.
        lzss::complzss(&img3::payload(&kc_raw)?)?
    } else {
        let (iv, key) = img3_key(&kc_path)?;
        match img3::decrypt(&kc_raw, &iv, &key, false).and_then(|d| lzss::complzss(&d)) {
            Ok(k) => k,
            Err(_) => {
                plain_tail = true;
                lzss::complzss(&img3::decrypt(&kc_raw, &iv, &key, true)?)?
            }
        }
    };

    // Components no recipe reads after this (the DFU stage, the ramdisks) are skipped without a key rather than
    // failing the build: public key pages lack them for several builds. An 8900 container, or an img3 without a
    // KBAG, needs none. AppleLogo is only the boot splash.
    let keyed = |p: &str| -> bool {
        if entry.key_for_path(p).is_ok() {
            return true;
        }
        match ipsw.read(p) {
            Ok(raw) => apple8900::is_container(&raw) || has_kbag(&raw).map(|k| !k).unwrap_or(false),
            Err(_) => false,
        }
    };
    let mut files = Vec::new();
    for (name, c) in COMPONENTS {
        if c == "KernelCache" {
            std::fs::write(dir.join("kernelcache.mach"), &kernel)?;
            files.push("kernelcache.mach".to_string());
        } else if c == "iBEC" && !comp.contains_key(c) {
            continue; // 1.0: no iBEC (no update path)
        } else {
            let p = path(c)?;
            if ["iBSS", "iBEC", "AppleLogo"].contains(&c) && !keyed(&p) && has_kbag(&ipsw.read(&p)?)? {
                log(&format!("warning: {}: no key for {c} ({p}); skipped", entry.id));
                continue;
            }
            std::fs::write(dir.join(format!("{name}.bin")), dec(&p, plain_tail)?)?;
            files.push(format!("{name}.bin"));
        }
    }
    for c in ["RestoreRamDisk", "UpdateRamDisk"] {
        let Some(p) = comp.get(c) else { continue };
        if !keyed(p) {
            log(&format!("warning: {}: no key for {c} ({p}); skipped", entry.id));
            continue;
        }
        let out = format!("{}-ramdisk.dmg", &p[..p.len() - 4]);
        std::fs::write(dir.join(&out), dec(p, plain_tail)?)?;
        files.push(out);
    }
    if rootfs {
        let os = path("OS")?;
        let out = dir.join("rootfs.dmg");
        // Several 10.x install images ship an unencrypted (plain UDIF) root filesystem even when the
        // catalog carries no VFDecrypt key; an encrypted one starts with "encrcdsa".
        let mut magic = [0u8; 8];
        let mut seen = 0usize;
        ipsw.stream(&os, |reader| {
            seen = std::io::Read::read(reader, &mut magic).map_err(|e| crate::internal!("{os}: {e}"))?;
            Ok(())
        })?;
        let encrypted = seen >= 8 && &magic == b"encrcdsa";
        let key = entry.key_for_path(&os).ok().and_then(|k| from_hex(&k.key)).filter(|k| k.len() == 36);
        if let Some(key) = key {
            ipsw.stream(&os, |reader| vfdecrypt::decrypt(reader, &out, &key))?;
        } else if encrypted {
            return Err(Error::new(Code::KeyMissing, format!("{}: no key for the root filesystem {os}", entry.id)));
        } else {
            let mut writer = std::fs::File::create(&out)?;
            ipsw.stream(&os, |reader| {
                std::io::copy(reader, &mut writer).map_err(|e| crate::internal!("{os}: {e}"))?;
                Ok(())
            })?;
        }
        files.push("rootfs.dmg".to_string());
    }
    Ok(Decrypted { plain_tail, paths: comp, files })
}

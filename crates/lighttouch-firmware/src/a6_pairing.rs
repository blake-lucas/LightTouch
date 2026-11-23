//! The iPhone 5's (14G61) pairing with LightTouch, made at preparation, so the guest trusts the app from its first
//! start and never asks "Trust This Computer?".
//!
//! iOS 10's lockdownd keeps its pairing key in the keychain, but when the keychain has none and
//! `/var/root/Library/Lockdown/device_private_key.pem` exists, it imports that file (`_check_keypair`'s
//! "migration attempt") and deletes both PEMs, keeping the pair records (it destroys them only when it makes a new
//! key). A pair record is `pair_records/<HostID>.plist` beside them, which `StartSession` loads by HostID to make its
//! TLS identity from the record's DeviceCertificate and RootCertificate. But the first boot of a build also "rolls"
//! the keys (`rollme`, once ManagedConfiguration reports in): a new key, every pair record destroyed, then
//! `com.apple.who`/`uptodate` = 2 in data_ark, which later boots read to skip it. The preparation writes that
//! state too (`WHO`), as the guest itself leaves it after its first boot. So the preparation makes the device key and
//! one pairing, as libimobiledevice's `pair_record_generate_keys_and_certs` would after Trust: 2048-bit RSA root and
//! host keys, version 3 certificates with serial 0 and no names, signed by the root with SHA-256, the device's
//! certificate for the device key. The guest gets the key and its side of the record; the host side (usbmuxd's
//! `<UDID>.plist`, with the private keys, and the matching `SystemConfiguration.plist`) goes in the prepared
//! folder's `pairing/`, which the app copies into the record's pairing folder before its USB daemon starts.
use crate::error::Result;
use crate::hfs::{Editor, Volume};
use plist::{Dictionary, Value};
use rsa::pkcs1::{EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding};
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey};
use rsa::{Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Data volume paths (the volume is mounted at /private/var).
pub const LOCKDOWN: &str = "root/Library/Lockdown";
pub const DEVICE_PRIVATE_KEY: &str = "root/Library/Lockdown/device_private_key.pem";
pub const DEVICE_PUBLIC_KEY: &str = "root/Library/Lockdown/device_public_key.pem";
pub const PAIR_RECORDS: &str = "root/Library/Lockdown/pair_records";
/// lockdownd's data_ark after its key roll for this build (14G61's first boot wrote exactly these): `rollme` skips
/// the roll, which would replace the key and destroy the pair records, when `uptodate` is 2.
pub const WHO: [(&str, i64); 1] = [("com.apple.who-uptodate", 2)];
pub const WHO_BUILDS: [&str; 2] = ["com.apple.who-uptodate_build", "com.apple.who-epoch_build"];
/// The prepared folder's host side: usbmuxd's pairing folder contents.
pub const HOST_FOLDER: &str = "pairing";
const KEY_BITS: usize = 2048;
/// 2012-01-01T00:00:00Z: certificates are valid from before any clock the guest may start with.
const NOT_BEFORE: i64 = 1_325_376_000;
const TEN_YEARS: i64 = 10 * 365 * 86_400;

pub struct Pairing {
    pub udid: String,
    pub host_id: String,
    pub system_buid: String,
    pub device_private_key: Vec<u8>,
    pub device_public_key: Vec<u8>,
    pub root_certificate: Vec<u8>,
    pub root_private_key: Vec<u8>,
    pub host_certificate: Vec<u8>,
    pub host_private_key: Vec<u8>,
    pub device_certificate: Vec<u8>,
    pub wifi_mac: String,
}

fn crypto(e: impl std::fmt::Display) -> crate::Error {
    crate::internal!("pairing keys: {e}")
}

/// A new pairing for the guest whose lockdownd UDID is `udid`. `now` (Unix seconds) dates the certificates' end.
pub fn make(udid: &str, wifi_mac: &str, now: i64) -> Result<Pairing> {
    let mut rng = rand_core::OsRng;
    let device = RsaPrivateKey::new(&mut rng, KEY_BITS).map_err(crypto)?;
    let root = RsaPrivateKey::new(&mut rng, KEY_BITS).map_err(crypto)?;
    let host = RsaPrivateKey::new(&mut rng, KEY_BITS).map_err(crypto)?;
    let until = now.max(NOT_BEFORE) + TEN_YEARS;
    let root_certificate = certificate(&root.to_public_key(), &root, Kind::Root, until)?;
    let host_certificate = certificate(&host.to_public_key(), &root, Kind::Host, until)?;
    let device_certificate = certificate(&device.to_public_key(), &root, Kind::Device, until)?;
    let pkcs8 = |k: &RsaPrivateKey| -> Result<Vec<u8>> {
        Ok(k.to_pkcs8_pem(LineEnding::LF).map_err(crypto)?.as_bytes().to_vec())
    };
    Ok(Pairing {
        udid: udid.to_string(),
        host_id: uuid()?,
        system_buid: uuid()?,
        // lockdownd's migration reads PKCS#1 ("RSA PRIVATE KEY"), as its own PEMs were.
        device_private_key: device.to_pkcs1_pem(LineEnding::LF).map_err(crypto)?.as_bytes().to_vec(),
        device_public_key: device.to_public_key().to_pkcs1_pem(LineEnding::LF).map_err(crypto)?.into_bytes(),
        root_certificate: pem("CERTIFICATE", &root_certificate),
        root_private_key: pkcs8(&root)?,
        host_certificate: pem("CERTIFICATE", &host_certificate),
        host_private_key: pkcs8(&host)?,
        device_certificate: pem("CERTIFICATE", &device_certificate),
        wifi_mac: wifi_mac.to_lowercase(),
    })
}

impl Pairing {
    /// lockdownd's record: what a host's `Pair` request carries.
    pub fn guest_record(&self) -> Dictionary {
        Dictionary::from_iter([
            ("DeviceCertificate", Value::Data(self.device_certificate.clone())),
            ("HostCertificate", Value::Data(self.host_certificate.clone())),
            ("HostID", Value::from(self.host_id.clone())),
            ("RootCertificate", Value::Data(self.root_certificate.clone())),
            ("SystemBUID", Value::from(self.system_buid.clone())),
        ])
    }

    /// usbmuxd's record, as libimobiledevice saves it after pairing: lockdownd's, the private keys and the Wi-Fi MAC.
    pub fn host_record(&self) -> Dictionary {
        let mut d = self.guest_record();
        d.insert("HostPrivateKey".into(), Value::Data(self.host_private_key.clone()));
        d.insert("RootPrivateKey".into(), Value::Data(self.root_private_key.clone()));
        d.insert("WiFiMACAddress".into(), Value::from(self.wifi_mac.clone()));
        d
    }

    /// The key PEMs and the pair record into the Data volume image, root's (0:0); folders 0755, files 0600; and
    /// data_ark's key-roll state for `build` (`WHO`).
    pub fn install(&self, data: &Path, build: &str) -> Result<()> {
        let record = format!("{PAIR_RECORDS}/{}.plist", self.host_id);
        let mut ed = Editor::open(data)?;
        let mut folders = Vec::new();
        let mut at = String::new();
        for part in PAIR_RECORDS.split('/') {
            at = if at.is_empty() { part.to_string() } else { format!("{at}/{part}") };
            if !ed.exists(&at) {
                folders.push(at.clone());
            }
        }
        ed.mkdirs(PAIR_RECORDS)?;
        ed.write(DEVICE_PRIVATE_KEY, self.device_private_key.clone(), Some(0o600))?;
        ed.write(DEVICE_PUBLIC_KEY, self.device_public_key.clone(), Some(0o600))?;
        ed.write(&record, xml(&self.guest_record())?, Some(0o600))?;
        crate::plistedit::seed(&mut ed, crate::setup::DATA_ARK, |d| {
            for (k, v) in WHO {
                d.insert(k.into(), Value::Integer(v.into()));
            }
            for k in WHO_BUILDS {
                d.insert(k.into(), Value::from(build));
            }
            Ok(())
        })?;
        ed.commit()?;
        let v = Volume::open(data, true)?;
        let mut paths: Vec<&str> = folders.iter().map(String::as_str).collect();
        paths.extend([DEVICE_PRIVATE_KEY, DEVICE_PUBLIC_KEY, record.as_str()]);
        v.set_owner(&paths, 0, 0, None, None)?;
        Ok(())
    }

    /// The host side into `out/pairing/`: `<UDID>.plist` and `SystemConfiguration.plist`, 0600.
    pub fn write_host(&self, out: &Path) -> Result<()> {
        let dir = out.join(HOST_FOLDER);
        std::fs::create_dir_all(&dir)?;
        let config = Dictionary::from_iter([("SystemBUID", Value::from(self.system_buid.clone()))]);
        for (name, bytes) in
            [(format!("{}.plist", self.udid), binary(&self.host_record())?), ("SystemConfiguration.plist".into(), xml(&config)?)]
        {
            let path = dir.join(name);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut f = options.open(&path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
            std::io::Write::write_all(&mut f, &bytes)?;
        }
        Ok(())
    }
}

fn xml(d: &Dictionary) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    Value::Dictionary(d.clone()).to_writer_xml(&mut out).map_err(|e| crate::internal!("pair record: {e}"))?;
    Ok(out)
}

fn binary(d: &Dictionary) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    Value::Dictionary(d.clone()).to_writer_binary(&mut out).map_err(|e| crate::internal!("pair record: {e}"))?;
    Ok(out)
}

/// A random (version 4) UUID in capitals, the form of a HostID and a SystemBUID.
fn uuid() -> Result<String> {
    let mut b = [0u8; 16];
    rand_core::RngCore::try_fill_bytes(&mut rand_core::OsRng, &mut b).map_err(crypto)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex = hex::encode_upper(b);
    Ok(format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]))
}

fn pem(label: &str, der: &[u8]) -> Vec<u8> {
    use base64_lines::encode;
    format!("-----BEGIN {label}-----\n{}-----END {label}-----\n", encode(der)).into_bytes()
}

mod base64_lines {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    /// Base64 in 64-character lines, each ending in a newline.
    pub fn encode(bytes: &[u8]) -> String {
        let mut s = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |a, (i, &b)| a | (b as u32) << (16 - 8 * i));
            for i in 0..4 {
                s.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        let mut out = String::new();
        for line in s.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(line).unwrap_or_default());
            out.push('\n');
        }
        out
    }
}

enum Kind {
    Root,
    Host,
    Device,
}

// ---- DER, just what these certificates need

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = body.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let bytes: Vec<u8> = n.to_be_bytes().iter().copied().skip_while(|&b| b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(body);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn oid(der_body: &[u8]) -> Vec<u8> {
    tlv(0x06, der_body)
}

const SHA256_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const SUBJECT_KEY_ID: &[u8] = &[0x55, 0x1d, 0x0e];

fn algorithm() -> Vec<u8> {
    seq(&[oid(SHA256_WITH_RSA), vec![0x05, 0x00]])
}

fn extension(id: &[u8], critical: bool, value: Vec<u8>) -> Vec<u8> {
    let mut parts = vec![oid(id)];
    if critical {
        parts.push(vec![0x01, 0x01, 0xff]);
    }
    parts.push(tlv(0x04, &value));
    seq(&parts)
}

/// UTCTime (YYMMDDHHMMSSZ), as OpenSSL writes dates before 2050; GeneralizedTime after.
fn time(unix: i64) -> Vec<u8> {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // days since 1970-01-01 -> civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let hms = format!("{:02}{:02}{:02}{:02}{:02}Z", m, d, secs / 3600, secs / 60 % 60, secs % 60);
    if y < 2050 {
        tlv(0x17, format!("{:02}{hms}", y % 100).as_bytes())
    } else {
        tlv(0x18, format!("{y:04}{hms}").as_bytes())
    }
}

fn certificate(key: &RsaPublicKey, signer: &RsaPrivateKey, kind: Kind, until: i64) -> Result<Vec<u8>> {
    let spki = key.to_public_key_der().map_err(crypto)?.as_bytes().to_vec();
    // keyUsage digitalSignature | keyEncipherment: bits 0 and 2 of a 3-bit string.
    let usage = || vec![0x03, 0x02, 0x05, 0xa0];
    let ca = seq(&[vec![0x01, 0x01, 0xff]]);
    let not_ca = seq(&[]);
    let extensions = match kind {
        Kind::Root => vec![extension(BASIC_CONSTRAINTS, true, ca)],
        Kind::Host => vec![extension(BASIC_CONSTRAINTS, true, not_ca), extension(KEY_USAGE, true, usage())],
        Kind::Device => {
            // OpenSSL's "hash": SHA-1 of the subjectPublicKey bits (the PKCS#1 RSAPublicKey).
            let bits = key.to_pkcs1_der().map_err(crypto)?.as_bytes().to_vec();
            let id = tlv(0x04, &Sha1::digest(&bits));
            vec![
                extension(BASIC_CONSTRAINTS, true, not_ca),
                extension(SUBJECT_KEY_ID, false, id),
                extension(KEY_USAGE, true, usage()),
            ]
        }
    };
    let tbs = seq(&[
        tlv(0xa0, &[0x02, 0x01, 0x02]), // version 3
        vec![0x02, 0x01, 0x00],         // serial 0
        algorithm(),
        seq(&[]), // issuer: no name
        seq(&[time(NOT_BEFORE), time(until)]),
        seq(&[]), // subject: no name
        spki,
        tlv(0xa3, &seq(&extensions)),
    ]);
    let signature =
        signer.sign(Pkcs1v15Sign::new::<Sha256>(), &Sha256::digest(&tbs)).map_err(crypto)?;
    let mut bits = vec![0u8];
    bits.extend(signature);
    Ok(seq(&[tbs, algorithm(), tlv(0x03, &bits)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openssl::x509::X509;

    #[test]
    fn certificates_chain_to_the_root_and_carry_the_keys() {
        let p = make("0123456789abcdef0123456789abcdef01234567", "02:08:F1:00:00:01", 1_700_000_000).unwrap();
        let root = X509::from_pem(&p.root_certificate).unwrap();
        let root_key = root.public_key().unwrap();
        assert!(root.verify(&root_key).unwrap());
        for pem in [&p.host_certificate, &p.device_certificate] {
            assert!(X509::from_pem(pem).unwrap().verify(&root_key).unwrap());
        }
        let device = openssl::rsa::Rsa::private_key_from_pem(&p.device_private_key).unwrap();
        let issued = X509::from_pem(&p.device_certificate).unwrap();
        assert_eq!(issued.public_key().unwrap().rsa().unwrap().n(), device.n());
        assert!(String::from_utf8_lossy(&p.device_private_key).starts_with("-----BEGIN RSA PRIVATE KEY-----\n"));
        let host = openssl::pkey::PKey::private_key_from_pem(&p.host_private_key).unwrap();
        assert!(X509::from_pem(&p.host_certificate).unwrap().public_key().unwrap().public_eq(&host));
        assert_eq!(issued.not_before().to_string(), "Jan  1 00:00:00 2012 GMT");
        assert_eq!(p.host_id.len(), 36);
        assert_eq!(p.wifi_mac, "02:08:f1:00:00:01");
        let host_record = p.host_record();
        for k in ["DeviceCertificate", "HostCertificate", "HostID", "RootCertificate", "SystemBUID", "HostPrivateKey",
            "RootPrivateKey"]
        {
            assert!(host_record.contains_key(k), "{k}");
        }
    }

    #[test]
    fn installs_into_data() {
        let dir = std::env::temp_dir().join(format!("a6-pairing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("data.img");
        Editor::format(&data, 64 << 20, "Data", true, 1_700_000_000).unwrap();
        let p = make("0123456789abcdef0123456789abcdef01234567", "02:08:f1:00:00:01", 1_700_000_000).unwrap();
        p.install(&data, "14G61").unwrap();
        p.write_host(&dir).unwrap();
        let ed = Editor::open_read_only(&data).unwrap();
        assert_eq!(ed.read(DEVICE_PRIVATE_KEY).unwrap(), p.device_private_key);
        let record = ed.read(&format!("{PAIR_RECORDS}/{}.plist", p.host_id)).unwrap();
        let record = Value::from_reader(std::io::Cursor::new(record)).unwrap().into_dictionary().unwrap();
        assert_eq!(record, p.guest_record());
        let ark = crate::plistedit::read(&ed, crate::setup::DATA_ARK).unwrap().unwrap();
        assert_eq!(ark.get("com.apple.who-uptodate").and_then(Value::as_signed_integer), Some(2));
        assert_eq!(ark.get("com.apple.who-epoch_build").and_then(Value::as_string), Some("14G61"));
        let v = Volume::open(&data, false).unwrap();
        for path in [LOCKDOWN, PAIR_RECORDS, DEVICE_PRIVATE_KEY] {
            assert_eq!(v.record(path).map(|r| (r.uid, r.gid)).unwrap(), (0, 0), "{path}");
        }
        let host = dir.join(HOST_FOLDER).join(format!("{}.plist", p.udid));
        let host = Value::from_file(host).unwrap().into_dictionary().unwrap();
        assert_eq!(host.get("HostID").and_then(Value::as_string), Some(p.host_id.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dates() {
        assert_eq!(time(NOT_BEFORE), tlv(0x17, b"120101000000Z"));
        assert_eq!(time(1_563_796_800), tlv(0x17, b"190722120000Z"));
        assert_eq!(time(2_524_607_999), tlv(0x17, b"491231235959Z"));
        assert_eq!(time(2_524_608_000), tlv(0x18, b"20500101000000Z"));
    }
}

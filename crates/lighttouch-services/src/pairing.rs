//! The keys and certificates of a new pairing, made as libimobiledevice's `pair_record_generate_keys_and_certs`
//! makes them: 2048-bit RSA root and host keys, version 3 certificates with serial 0, no names and ten years'
//! validity, signed by the root with SHA-1 before iOS 4 and SHA-256 from it.
use crate::{Error, Result};
use openssl::asn1::Asn1Time;
use openssl::bn::BigNum;
use openssl::hash::MessageDigest;
use openssl::pkey::{HasPublic, PKey, PKeyRef, Private};
use openssl::rsa::Rsa;
use openssl::x509::extension::{BasicConstraints, KeyUsage, SubjectKeyIdentifier};
use openssl::x509::{X509, X509Builder};
use std::sync::Mutex;

/// PEM, as the pairing record and lockdownd carry them.
#[derive(Clone)]
pub struct Keys {
    pub root_certificate: Vec<u8>,
    pub root_private_key: Vec<u8>,
    pub host_certificate: Vec<u8>,
    pub host_private_key: Vec<u8>,
    pub device_certificate: Vec<u8>,
}

/// `device_public_key` is lockdownd's `DevicePublicKey` (a PKCS#1 PEM).
pub fn generate(device_public_key: &[u8], ios: (u32, u32, u32)) -> Result<Keys> {
    let digest = if ios.0 < 4 { MessageDigest::sha1() } else { MessageDigest::sha256() };
    let root = PKey::from_rsa(Rsa::generate(2048)?)?;
    let host = PKey::from_rsa(Rsa::generate(2048)?)?;
    let device = match Rsa::public_key_from_pem_pkcs1(device_public_key) {
        Ok(rsa) => PKey::from_rsa(rsa)?,
        Err(_) => PKey::public_key_from_pem(device_public_key)
            .map_err(|_| Error("the guest's public key couldn't be read".into()))?,
    };
    let root_certificate = certificate(&root, &root, Kind::Root, digest)?;
    let host_certificate = certificate(&host, &root, Kind::Host, digest)?;
    let device_certificate = certificate(&device, &root, Kind::Device, digest)?;
    Ok(Keys {
        root_certificate: root_certificate.to_pem()?,
        root_private_key: root.private_key_to_pem_pkcs8()?,
        host_certificate: host_certificate.to_pem()?,
        host_private_key: host.private_key_to_pem_pkcs8()?,
        device_certificate: device_certificate.to_pem()?,
    })
}

enum Kind {
    Root,
    Host,
    Device,
}

fn certificate<T: HasPublic>(
    key: &PKeyRef<T>,
    signer: &PKeyRef<Private>,
    kind: Kind,
    digest: MessageDigest,
) -> Result<X509> {
    let mut builder = X509Builder::new()?;
    let serial = BigNum::from_u32(0)?.to_asn1_integer()?;
    builder.set_serial_number(&serial)?;
    builder.set_version(2)?;
    let usage = || KeyUsage::new().critical().digital_signature().key_encipherment().build();
    match kind {
        Kind::Root => builder.append_extension(BasicConstraints::new().critical().ca().build()?)?,
        Kind::Host => {
            builder.append_extension(BasicConstraints::new().critical().build()?)?;
            builder.append_extension(usage()?)?;
        }
        Kind::Device => builder.append_extension(BasicConstraints::new().critical().build()?)?,
    }
    let (from, until) = (Asn1Time::days_from_now(0)?, Asn1Time::days_from_now(365 * 10)?);
    builder.set_not_before(&from)?;
    builder.set_not_after(&until)?;
    builder.set_pubkey(key)?;
    if let Kind::Device = kind {
        let identifier = SubjectKeyIdentifier::new().build(&builder.x509v3_context(None, None))?;
        builder.append_extension(identifier)?;
        builder.append_extension(usage()?)?;
    }
    builder.sign(signer, digest)?;
    Ok(builder.build())
}

/// Identities offered to guests that haven't answered yet, by UDID: (HostID, keys).
static PENDING: Mutex<Vec<(String, String, Keys)>> = Mutex::new(Vec::new());

/// The identity to pair `udid` with: the one already offered to it, or a new one.
pub fn pending(udid: &str, device_public_key: &[u8], ios: (u32, u32, u32)) -> Result<(String, Keys)> {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, host_id, keys)) = pending.iter().find(|(u, _, _)| u == udid) {
        return Ok((host_id.clone(), keys.clone()));
    }
    let made = (uuid(), generate(device_public_key, ios)?);
    pending.push((udid.to_string(), made.0.clone(), made.1.clone()));
    Ok(made)
}

/// Paired: the next pairing with `udid` starts afresh.
pub fn forget(udid: &str) {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).retain(|(u, _, _)| u != udid);
}

/// A random (version 4) UUID in capitals, the form of a pairing's HostID.
pub fn uuid() -> String {
    let mut b = [0u8; 16];
    let _ = openssl::rand::rand_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_the_device_key_with_the_root() {
        let device = Rsa::generate(1024).unwrap();
        let pem = device.public_key_to_pem_pkcs1().unwrap();
        let keys = generate(&pem, (3, 1, 3)).unwrap();
        let root = X509::from_pem(&keys.root_certificate).unwrap();
        let issued = X509::from_pem(&keys.device_certificate).unwrap();
        assert!(issued.verify(&root.public_key().unwrap()).unwrap());
        assert!(X509::from_pem(&keys.host_certificate).unwrap().verify(&root.public_key().unwrap()).unwrap());
        assert_eq!(issued.public_key().unwrap().rsa().unwrap().n(), device.n());
        assert!(PKey::private_key_from_pem(&keys.host_private_key).is_ok());
        assert_eq!(uuid().len(), 36);
    }
}

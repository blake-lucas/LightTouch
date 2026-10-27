//! iPhone OS 1.x's certificate anchors (N45/TrustStore1x.swift). 1.0's SecTrust asks only the system anchor source,
//! the trust store in Security.framework (table tsettings, the schema 2.x/3.x keep for their user store); the user
//! store (~/Library/Keychains/TrustStore.sqlite3) is never consulted. A row there makes a certificate an anchor: sha1
//! of its DER, subj the subject Name's content as 1.x normalizes it (PrintableString values uppercased, every other
//! string type as issued, read off the stock rows), tset the stock rows' empty-array plist, data the DER. Checked on
//! M68 1A543a: with the row, SecTrustEvaluate gives 4 (unspecified) for a WebProxyCA-shaped chain and Safari opens
//! HTTPS through a TLS 1.0 relay; without it, 3 (deny).
//!
//! This is the in-volume half of Swift's `TrustStore1x.trust(device:certificate:)`: the stopped edit around it (the
//! device record, the overlay stamp and the device directory's marker) belongs to the host's device store, which calls
//! `add_anchor` on the edit's editor. SQLite is the bundled one (rusqlite): the store is copied out of the volume to a
//! host temporary file, edited there and written back in place.
use crate::error::Result;
use crate::hfs::Editor;
use sha1::{Digest, Sha1};

pub const PATH: &str = "System/Library/Frameworks/Security.framework/TrustStore.sqlite3";

/// The stock rows' tset: an empty trust-settings array.
pub const EMPTY_SETTINGS: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple Computer//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<array/>\n\
</plist>\n";

/// A DER element at `o`: (tag, header length, content length).
fn element(d: &[u8], o: usize) -> Result<(u8, usize, usize)> {
    if o + 1 >= d.len() {
        return Err(crate::unsupported!("certificate truncated"));
    }
    let mut length = d[o + 1] as usize;
    let mut header = 2;
    if length & 0x80 != 0 {
        let n = length & 0x7f;
        if n > 3 || o + 2 + n > d.len() {
            return Err(crate::unsupported!("certificate length"));
        }
        length = d[o + 2..o + 2 + n].iter().fold(0, |a, &b| a << 8 | b as usize);
        header = 2 + n;
    }
    if o + header + length > d.len() {
        return Err(crate::unsupported!("certificate truncated"));
    }
    Ok((d[o], header, length))
}

/// The certificate's subject Name content, normalized as 1.x stores it.
pub fn normalized_subject(certificate: &[u8]) -> Result<Vec<u8>> {
    let d = certificate;
    let mut o = element(d, 0)?.1; // Certificate
    o += element(d, o)?.1; // TBSCertificate
    if d.get(o) == Some(&0xa0) {
        let (_, h, l) = element(d, o)?;
        o += h + l;
    } // version
    for _ in 0..4 {
        let (_, h, l) = element(d, o)?;
        o += h + l;
    } // serial, signature, issuer, validity
    let (_, nh, nl) = element(d, o)?;
    let mut out = d[o + nh..o + nh + nl].to_vec();
    // Walk the RDNs: SET { SEQUENCE { OID, value } }; uppercase PrintableString (0x13) values in place.
    let mut p = 0;
    while p < out.len() {
        let (_, sh, sl) = element(&out, p)?;
        let mut q = p + sh;
        while q < p + sh + sl {
            let (_, ah, al) = element(&out, q)?;
            let (_, oh, ol) = element(&out, q + ah)?;
            let v = q + ah + oh + ol;
            let (tag, vh, vl) = element(&out, v)?;
            if tag == 0x13 {
                out[v + vh..v + vh + vl].make_ascii_uppercase();
            }
            q += ah + al;
        }
        p += sh + sl;
    }
    Ok(out)
}

/// Adds (or replaces) `certificate` as an anchor in the SQLite trust store at `store`, a host file.
pub fn add_anchor_to_file(certificate: &[u8], store: &std::path::Path) -> Result<()> {
    use rusqlite::{Connection, OpenFlags};
    let subject = normalized_subject(certificate)?;
    let name = store.display().to_string();
    let db = Connection::open_with_flags(store, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| crate::unsupported!("{name}: not a trust store"))?;
    let mut stmt = db
        .prepare("INSERT OR REPLACE INTO tsettings(sha1,subj,tset,data) VALUES(?,?,?,?)")
        .map_err(|_| crate::unsupported!("{name}: no tsettings table"))?;
    let sha1 = Sha1::digest(certificate).to_vec();
    stmt.execute(rusqlite::params![sha1, subject, EMPTY_SETTINGS, certificate])
        .map_err(|e| crate::unsupported!("{name}: {e}"))?;
    Ok(())
}

/// Makes `certificate` an anchor in a stopped 1.x device's system volume: the row added to its Security.framework
/// trust store, the file rewritten in place (its record, owner and mode kept).
pub fn add_anchor(system: &mut Editor, certificate: &[u8]) -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    normalized_subject(certificate)?; // a certificate this can parse, before any work
    let bytes = system.read(PATH)?;
    let n = N.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("lt-truststore-{}-{n}.sqlite3", std::process::id()));
    let journal = tmp.with_extension("sqlite3-journal");
    let result = std::fs::write(&tmp, &bytes)
        .map_err(|e| crate::internal!("{}: {e}", tmp.display()))
        .and_then(|()| add_anchor_to_file(certificate, &tmp))
        .and_then(|()| std::fs::read(&tmp).map_err(|e| crate::internal!("{}: {e}", tmp.display())));
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&journal);
    system.write(PATH, result?, None)
}

/// The certificate's SHA-1, hex: the anchor's key and the device marker's.
pub fn sha1_hex(certificate: &[u8]) -> String {
    hex::encode(Sha1::digest(certificate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::Scratch;

    fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![tag, body.len() as u8];
        v.extend_from_slice(body);
        v
    }

    /// The subject: C=us (PrintableString), CN=Light Touch (UTF8String).
    fn subject_content() -> Vec<u8> {
        let mut c = tlv(0x31, &tlv(0x30, &[tlv(0x06, &[0x55, 0x04, 0x06]), tlv(0x13, b"us")].concat()));
        c.extend(tlv(0x31, &tlv(0x30, &[tlv(0x06, &[0x55, 0x04, 0x03]), tlv(0x0c, b"Light Touch")].concat())));
        c
    }

    /// A certificate skeleton: version, serial, signature, issuer, validity, subject; nothing after the subject
    /// matters to the normalization.
    fn certificate() -> Vec<u8> {
        let tbs = [
            tlv(0xa0, &tlv(0x02, &[2])),
            tlv(0x02, &[1]),
            tlv(0x30, &tlv(0x06, &[0x2a])),
            tlv(0x30, &[]),
            tlv(0x30, &[]),
            tlv(0x30, &subject_content()),
        ]
        .concat();
        tlv(0x30, &tlv(0x30, &tbs))
    }

    /// normalizesPrintableStringsOnly: 1.x stores PrintableString values uppercased and every other string type as
    /// issued.
    #[test]
    fn normalizes_printable_strings_only() {
        let normalized = normalized_subject(&certificate()).unwrap();
        let mut expected = subject_content();
        let us = expected.iter().position(|&b| b == b'u').unwrap();
        expected[us] = b'U';
        expected[us + 1] = b'S';
        assert_eq!(normalized, expected);
        assert!(String::from_utf8_lossy(&normalized).contains("Light Touch"));
        assert!(normalized_subject(&certificate()[..10]).is_err());
    }

    #[test]
    fn the_empty_settings_are_the_stock_rows() {
        let text = std::str::from_utf8(EMPTY_SETTINGS).unwrap();
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple Computer//"));
        assert!(text.ends_with("<array/>\n</plist>\n"));
        assert_eq!(plist::Value::from_reader(std::io::Cursor::new(EMPTY_SETTINGS)).unwrap(), plist::Value::Array(vec![]));
    }

    /// addsAnAnchorRowAsTheStockRowsAre, inside a volume: one row per certificate, the stock rows' columns.
    #[test]
    fn adds_an_anchor_row_as_the_stock_rows_are() {
        let host = std::env::temp_dir().join(format!("lt-ts-test-{}.sqlite3", std::process::id()));
        let _ = std::fs::remove_file(&host);
        {
            let db = rusqlite::Connection::open(&host).unwrap();
            db.execute_batch(
                "CREATE TABLE tsettings(sha1 BLOB NOT NULL DEFAULT '',subj BLOB NOT NULL DEFAULT '',tset BLOB,data BLOB,PRIMARY KEY(sha1));",
            )
            .unwrap();
        }
        let (_s, mut ed) = Scratch::new("truststore");
        ed.mkdirs("System/Library/Frameworks/Security.framework").unwrap();
        ed.write(PATH, std::fs::read(&host).unwrap(), Some(0o644)).unwrap();
        let cert = certificate();
        add_anchor(&mut ed, &cert).unwrap();
        add_anchor(&mut ed, &cert).unwrap(); // idempotent: one row per certificate
        std::fs::write(&host, ed.read(PATH).unwrap()).unwrap();
        let db = rusqlite::Connection::open(&host).unwrap();
        let rows: Vec<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)> = db
            .prepare("SELECT sha1,subj,tset,data FROM tsettings")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        drop(db);
        let _ = std::fs::remove_file(&host);
        assert_eq!(rows.len(), 1);
        let (sha1, subj, tset, data) = &rows[0];
        assert_eq!(*sha1, Sha1::digest(&cert).to_vec());
        assert_eq!(*subj, normalized_subject(&cert).unwrap());
        assert_eq!(tset.as_slice(), EMPTY_SETTINGS);
        assert_eq!(*data, cert);
        assert_eq!(sha1_hex(&cert).len(), 40);
    }

    #[test]
    fn a_volume_without_the_store_is_refused() {
        let (_s, mut ed) = Scratch::new("truststore");
        assert!(add_anchor(&mut ed, &certificate()).is_err());
        ed.mkdirs("System/Library/Frameworks/Security.framework").unwrap();
        ed.write(PATH, b"not a database".to_vec(), None).unwrap();
        assert!(add_anchor(&mut ed, &certificate()).is_err());
    }
}

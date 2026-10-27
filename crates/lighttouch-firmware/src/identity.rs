//! A synthetic unit identity (serial, MLB, ECID, die-id, MACs) as a pure function of a seed, and lockdownd's UDID.
//! Ports ipad1_kboot.synth_identity/udid and ipod2g_device.identity (FirmwareKit's UnitIdentity, with HostRuntime's
//! IPhoneIdentity and UnitSeed, which it leans on).
//!
//! ```ignore
//! let id = UnitIdentity::synthesize("ipad1-7B500-default", "16g", None)?;                 // iPad 1 (k48ap)
//! let pod = UnitIdentity::synthesize_ipod(seed, "MC086", "LL/A", true)?;                  // n72ap
//! id.get("udid"), id.udid()                // SHA1(serial + Wi-Fi MAC + BT MAC), MACs lowercase
//! id.adding_imei(seed)                     // n90ap/n88ap/m68ap: imei + SHA1(serial + IMEI + Wi-Fi MAC + BT MAC)
//! id.json()                                // byte-identical to Python's json.dump(ident, f, indent=1)
//! id.write(path)?                          // create-new, mode 600
//! UnitIdentity::load(path)?                // an identity.json (keys sorted), e.g. to build kboot.bin
//! ```
//! Fields keep Python's insertion order, which is what makes `json` match. A value is a string or a list of strings
//! (die-id).
use crate::error::Result;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnitIdentity {
    pub fields: Vec<(String, Value)>,
}

/// Apple serials leave out I and O.
const SERIAL_CHARS: &[u8] = b"0123456789ABCDEFGHJKLMNPQRSTUVWXYZ";
pub const IPAD_REGION: &str = "LL/A";
/// A type allocation code no manufacturer was ever assigned, so a generated IMEI can't collide with a real phone's.
pub const SYNTHETIC_TAC: &str = "00000000";

/// Wi-Fi iPad 1 model numbers by storage (the only NAND geometry modeled is 16 GB).
pub fn ipad_model(storage: &str) -> Option<&'static str> {
    match storage {
        "16g" => Some("MB292"),
        _ => None,
    }
}

fn sha256(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

fn sha1_hex(text: &str) -> String {
    hex::encode(Sha1::digest(text.as_bytes()))
}

/// The 40-bit ECID: SHA-256(seed) bytes 20..<25, odd (HostRuntime's UnitSeed, which DeviceLock shares).
pub fn ecid(seed: &str) -> u64 {
    sha256(seed)[20..25].iter().fold(0u64, |a, &b| a << 8 | b as u64) | 1
}

/// The ECID as identity.json and the machine's ecid option write it.
pub fn ecid_string(seed: &str) -> String {
    format!("0x{:010x}", ecid(seed))
}

/// lockdownd's UDID: SHA1(serial + Wi-Fi MAC + BT MAC), MACs lowercase.
pub fn udid(serial: &str, wifi_mac: &str, bt_mac: &str) -> String {
    sha1_hex(&format!("{serial}{}{}", wifi_mac.to_lowercase(), bt_mac.to_lowercase()))
}

/// An iPhone's UDID, which lockdownd (1.x-4.x) and MobileGestalt (5.x on) hash the modem's IMEI into.
pub fn iphone_udid(serial: &str, imei: &str, wifi_mac: &str, bt_mac: &str) -> String {
    sha1_hex(&format!("{serial}{imei}{}{}", wifi_mac.to_lowercase(), bt_mac.to_lowercase()))
}

/// The IMEI: the synthetic TAC, six digits from the seed, and the Luhn digit.
pub fn imei(seed: &str) -> String {
    let h = sha256(&format!("imei:{seed}"));
    let body: String = SYNTHETIC_TAC.to_string() + &h[..6].iter().map(|b| (b % 10).to_string()).collect::<String>();
    let check = luhn(&body);
    format!("{body}{check}")
}

/// The Luhn check digit of a digit string (an IMEI's fifteenth).
pub fn luhn(digits: &str) -> u32 {
    let sum: u32 = digits
        .chars()
        .rev()
        .enumerate()
        .map(|(i, c)| {
            let d = c.to_digit(10).unwrap_or(0);
            if i % 2 == 0 {
                if d * 2 > 9 { d * 2 - 9 } else { d * 2 }
            } else {
                d
            }
        })
        .sum();
    (10 - sum % 10) % 10
}

fn mac(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
}

fn s(k: &str, v: impl Into<String>) -> (String, Value) {
    (k.to_string(), Value::Str(v.into()))
}

impl UnitIdentity {
    pub fn new(fields: Vec<(String, Value)>) -> UnitIdentity {
        UnitIdentity { fields }
    }

    /// A string field.
    pub fn get(&self, key: &str) -> Option<&str> {
        match self.fields.iter().find(|(k, _)| k == key) {
            Some((_, Value::Str(s))) => Some(s),
            _ => None,
        }
    }

    pub fn die_id(&self) -> Option<&[String]> {
        match self.fields.iter().find(|(k, _)| k == "die-id") {
            Some((_, Value::List(l))) => Some(l),
            _ => None,
        }
    }

    pub fn udid(&self) -> Option<&str> {
        self.get("udid")
    }

    /// A field every synthesized identity has.
    fn synthesized(&self, key: &str) -> String {
        self.get(key).unwrap_or_default().to_string()
    }

    /// A made-up but well-formed iPad 1 identity: 11-character serial, 13-character MLB, 40-bit ECID, two die-id
    /// words, a locally administered Wi-Fi MAC (02:...) and Bluetooth = Wi-Fi + 1. `model_number`: another A4 board's
    /// (the iPod touch 4G's MC540), in place of the iPad's by storage.
    pub fn synthesize(seed: &str, storage: &str, model_number: Option<&str>) -> Result<UnitIdentity> {
        let model = model_number
            .or_else(|| ipad_model(storage))
            .ok_or_else(|| crate::unsupported!("no iPad 1 model for storage {storage}"))?;
        let h = sha256(seed);
        let chars = |from: usize, n: usize| -> String {
            h[from..from + n].iter().map(|&b| SERIAL_CHARS[b as usize % SERIAL_CHARS.len()] as char).collect()
        };
        // even last byte: BT = +1 never carries
        let wifi = [0x02, h[28], h[29], h[30], h[31], h[27] & 0xFE];
        let mut bt = wifi;
        bt[5] += 1;
        let ecid = ecid(seed);
        // SecureROM builds ECID from CHIPID words 2/3 and CPRV from bits 10..15 of word 3 (revision 0x11); derive the
        // die-id words from the ECID so the ROM-advertised identity agrees (ipad1_kboot.synth_identity, qemu-ios
        // ff331e1ef9). die-id[1]'s high 16 bits keep the original h[2:6] value.
        let die_lo = h[2..6].iter().fold(0u64, |a, &b| a << 8 | b as u64);
        let word2 = ((ecid >> 21) & 0x1FFFFF) | (((ecid >> 16) & 31) << 21) | (((ecid >> 2) & 63) << 26);
        let word3 = (die_lo & 0xFFFF_0000) | 0x2400 | (((ecid >> 8) & 255) << 2) | (ecid & 3);
        let mut id = UnitIdentity::new(vec![
            s("serial-number", chars(0, 11)),
            s("mlb-serial-number", chars(11, 13)),
            s("unique-chip-id", format!("0x{ecid:010x}")),
            ("die-id".into(), Value::List(vec![format!("0x{word2:08x}"), format!("0x{word3:08x}")])),
            s("wifi-mac", mac(&wifi)),
            s("bt-mac", mac(&bt)),
            s("model-number", model),
            s("region-info", IPAD_REGION),
            s("seed", seed),
        ]);
        let u = udid(&id.synthesized("serial-number"), &id.synthesized("wifi-mac"), &id.synthesized("bt-mac"));
        id.fields.push(s("udid", u));
        Ok(id)
    }

    /// The iPod touch 2G identity: the iPad's serial and MACs plus a 12-digit battery serial; model and region are the
    /// recipe's. `bluetooth: false` is the 1G, which has no Bluetooth: no bt-mac, lockdownd's UDID hashes an empty BT
    /// address (SHA1(serial + Wi-Fi MAC)), and its serial reads as made in 2008 (`one_g_serial`).
    pub fn synthesize_ipod(seed: &str, model_number: &str, region_info: &str, bluetooth: bool) -> Result<UnitIdentity> {
        let base = Self::synthesize(seed, "16g", None)?;
        let h = sha256(&format!("battery:{seed}"));
        let serial = if bluetooth {
            base.synthesized("serial-number")
        } else {
            one_g_serial(&base.synthesized("serial-number"), h[12])
        };
        let mut fields = vec![s("serial-number", serial.clone()), s("wifi-mac", base.synthesized("wifi-mac"))];
        if bluetooth {
            fields.push(s("bt-mac", base.synthesized("bt-mac")));
        }
        fields.push(s("battery-serial", h[..12].iter().map(|b| (b % 10).to_string()).collect::<String>()));
        fields.push(s("model-number", model_number));
        fields.push(s("region-info", region_info));
        fields.push(s("seed", seed));
        let mut id = UnitIdentity::new(fields);
        if bluetooth {
            id.fields.push(s("unique-chip-id", base.synthesized("unique-chip-id")));
        }
        let u = if bluetooth { base.synthesized("udid") } else { udid(&serial, &base.synthesized("wifi-mac"), "") };
        id.fields.push(s("udid", u));
        Ok(id)
    }

    /// The original iPhone (m68ap): the iPod's fields plus a 15-digit IMEI, which the baseband reports and lockdownd
    /// hashes into the UDID: SHA1(serial + IMEI + Wi-Fi MAC + BT MAC).
    pub fn synthesize_iphone(seed: &str, model_number: &str, region_info: &str) -> Result<UnitIdentity> {
        Ok(Self::synthesize_ipod(seed, model_number, region_info, true)?.adding_imei(seed))
    }

    /// This identity with an iPhone's IMEI after the serial, and the UDID the baseband's IMEI makes.
    pub fn adding_imei(&self, seed: &str) -> UnitIdentity {
        let mut id = self.clone();
        let imei = imei(seed);
        id.fields.retain(|(k, _)| k != "udid" && k != "imei");
        id.fields.insert(1, s("imei", imei.clone()));
        let u = iphone_udid(&id.synthesized("serial-number"), &imei, &id.synthesized("wifi-mac"), &id.synthesized("bt-mac"));
        id.fields.push(s("udid", u));
        id
    }

    /// Python's json.dump(obj, f, indent=1) (ensure_ascii, no trailing newline).
    pub fn json(&self) -> Vec<u8> {
        fn q(s: &str) -> String {
            let mut o = String::from("\"");
            for u in s.encode_utf16() {
                match u {
                    0x22 => o.push_str("\\\""),
                    0x5C => o.push_str("\\\\"),
                    0x0A => o.push_str("\\n"),
                    0x0D => o.push_str("\\r"),
                    0x09 => o.push_str("\\t"),
                    0x08 => o.push_str("\\b"),
                    0x0C => o.push_str("\\f"),
                    0x20..=0x7E => o.push(u as u8 as char),
                    _ => o.push_str(&format!("\\u{u:04x}")),
                }
            }
            o.push('"');
            o
        }
        if self.fields.is_empty() {
            return b"{}".to_vec();
        }
        let body: Vec<String> = self
            .fields
            .iter()
            .map(|(k, v)| match v {
                Value::Str(s) => format!(" {}: {}", q(k), q(s)),
                Value::List(l) if l.is_empty() => format!(" {}: []", q(k)),
                Value::List(l) => format!(
                    " {}: [\n{}\n ]",
                    q(k),
                    l.iter().map(|x| format!("  {}", q(x))).collect::<Vec<_>>().join(",\n")
                ),
            })
            .collect();
        format!("{{\n{}\n}}", body.join(",\n")).into_bytes()
    }

    /// Creates `path` (never over an existing file), private to the user.
    pub fn write(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(path).map_err(|e| crate::internal!("create {}: {e}", path.display()))?;
        f.write_all(&self.json())?;
        Ok(())
    }

    /// An identity.json (any key order; strings and string lists only), its fields sorted by key.
    pub fn load(path: &Path) -> Result<UnitIdentity> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self::parse(&std::fs::read(path)?, &name)
    }

    pub fn parse(data: &[u8], name: &str) -> Result<UnitIdentity> {
        let obj: serde_json::Value =
            serde_json::from_slice(data).map_err(|_| crate::unsupported!("{name} is not a JSON object"))?;
        let serde_json::Value::Object(obj) = obj else { return Err(crate::unsupported!("{name} is not a JSON object")) };
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        let mut fields = Vec::new();
        for k in keys {
            let bad = || crate::unsupported!("{name}: {k} is not a string or a list of strings");
            let v = match &obj[k] {
                serde_json::Value::String(s) => Value::Str(s.clone()),
                serde_json::Value::Array(a) => {
                    Value::List(a.iter().map(|x| x.as_str().map(str::to_string).ok_or_else(bad)).collect::<Result<_>>()?)
                }
                _ => return Err(bad()),
            };
            fields.push((k.clone(), v));
        }
        Ok(UnitIdentity::new(fields))
    }
}

/// `serial` with its year and week of manufacture (PPYWW…) set to a week of 2008. iPhone OS 1.1.3 to 1.1.5's lockdownd
/// (load_nikita) turns on the January apps (Mail, Maps, Stocks, Weather, Notes) for an iPod touch made from week 52 of
/// 2007, which shipped with them, and otherwise only for the paid upgrade's receipt, signed by Apple; without either,
/// SpringBoard leaves the five off the Home screen.
pub fn one_g_serial(serial: &str, byte: u8) -> String {
    let c: Vec<char> = serial.chars().collect();
    let head: String = c.iter().take(2).collect();
    let tail: String = c.iter().skip(5).collect();
    format!("{head}8{:02}{tail}", 1 + byte as u32 % 36)
}

impl crate::n72nor::Identity for UnitIdentity {
    fn field(&self, key: &str) -> Option<&str> {
        self.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(d: &[u8]) -> String {
        hex::encode(Sha256::digest(d))
    }

    /// sha256 of json.dumps(ipad1_kboot.synth_identity(seed), indent=1).
    #[test]
    fn json_matches_recorded_digests() {
        for (seed, want) in [
            ("ipad1-7B500-default", "4d379949478173c02768a25adbd7e91c02f63daccd0e65c0e6506b81da61e861"),
            ("x", "40db97bd11bf40b3734a6d5a9fde47e7c0ad135178b25c48bb28553a19d4e2b4"),
            ("y", "0a0ef7113474b96bf46f646f4fb74c0fa01260f800a3f6c791170f796ff4101f"),
            ("", "e78717179783745bef5ecf24f96391e07bb9a9b1ccd97ccc31b231889d04005f"),
            ("caf\u{e9} \u{2713} \"q\"\\\n\t\u{7f}\u{1F600}", "bd4f2ab2194d18d4c25738444a4ba831213ffa0b362bbac56e1bffaedab49dc7"),
        ] {
            assert_eq!(sha(&UnitIdentity::synthesize(seed, "16g", None).unwrap().json()), want, "{seed:?}");
        }
    }

    #[test]
    fn default_seed() {
        let id = UnitIdentity::synthesize("ipad1-7B500-default", "16g", None).unwrap();
        assert_eq!(
            String::from_utf8(id.json()).unwrap(),
            r#"{
 "serial-number": "0Y2ZETGRCJB",
 "mlb-serial-number": "83FGZ84AP5CMG",
 "unique-chip-id": "0x6bb6bf76e7",
 "die-id": [
  "0xe7e35db5",
  "0x686525db"
 ],
 "wifi-mac": "02:ea:75:42:31:de",
 "bt-mac": "02:ea:75:42:31:df",
 "model-number": "MB292",
 "region-info": "LL/A",
 "seed": "ipad1-7B500-default",
 "udid": "144707f35769dad502241e7df06a757d8eeea130"
}"#
        );
        assert_eq!(id.udid().unwrap(), udid("0Y2ZETGRCJB", "02:EA:75:42:31:DE", "02:ea:75:42:31:df"));
        assert!(UnitIdentity::synthesize("s", "64g", None).is_err());
    }

    /// ipod2g_device.identity(seed, {"model_number": "MC086", "region_info": "LL/A"}).
    #[test]
    fn ipod() {
        let id = UnitIdentity::synthesize_ipod("ipod2g-8C148-default", "MC086", "LL/A", true).unwrap();
        let legacy = UnitIdentity::new(id.fields.iter().filter(|(k, _)| k != "unique-chip-id").cloned().collect());
        assert_eq!(sha(&legacy.json()), "cdd30a95a8297be0b8cd336e28311f604e64409d60849323ced09695165c34d0");
        let ipad = UnitIdentity::synthesize("ipod2g-8C148-default", "16g", None).unwrap();
        assert_eq!(id.get("unique-chip-id"), ipad.get("unique-chip-id"));
        assert_eq!(id.get("battery-serial"), Some("142503116299"));
        assert_eq!(id.udid(), Some("0129500823c3921495fbea0555169adc11312b63"));
    }

    #[test]
    fn ipod_1g() {
        let pod = UnitIdentity::synthesize_ipod("ipod1g-test", "MA623", "LL/A", true).unwrap();
        let id = UnitIdentity::synthesize_ipod("ipod1g-test", "MA623", "LL/A", false).unwrap();
        assert!(id.get("unique-chip-id").is_none() && id.get("bt-mac").is_none());
        assert_eq!(id.get("wifi-mac"), pod.get("wifi-mac"));
        let serial = id.get("serial-number").unwrap();
        let other = pod.get("serial-number").unwrap();
        assert_eq!(serial[..2], other[..2]);
        assert_eq!(serial[5..], other[5..]);
        assert_eq!(&serial[2..3], "8");
        assert!((1..=36).contains(&serial[3..5].parse::<u32>().unwrap()));
        assert_eq!(id.udid().unwrap(), sha1_hex(&format!("{serial}{}", id.get("wifi-mac").unwrap())));
        assert_ne!(id.udid(), pod.udid());
    }

    #[test]
    fn iphone() {
        let id = UnitIdentity::synthesize_iphone("iphone2g-test", "MA501", "LL/A").unwrap();
        let imei = id.get("imei").unwrap();
        assert!(imei.len() == 15 && imei.starts_with("00000000"));
        assert_eq!(luhn(&imei[..14]), imei[14..].parse::<u32>().unwrap());
        assert_eq!(luhn("49015420323751"), 8); // the classic example IMEI 490154203237518
        let want = sha1_hex(&format!(
            "{}{imei}{}{}",
            id.get("serial-number").unwrap(),
            id.get("wifi-mac").unwrap(),
            id.get("bt-mac").unwrap()
        ));
        assert_eq!(id.udid(), Some(want.as_str()));
        assert_eq!(id.fields.iter().filter(|(k, _)| k == "udid").count(), 1);
    }

    #[test]
    fn a4_iphone_identity_carries_imei() {
        let base = UnitIdentity::synthesize("iphone4-8C148-default", "16g", Some("MC603")).unwrap();
        let id = base.adding_imei("iphone4-8C148-default");
        assert_eq!(id.fields[1].0, "imei");
        assert_eq!(id.fields.last().unwrap().0, "udid");
        assert_eq!(id.fields.len(), base.fields.len() + 1);
        let keys = |i: &UnitIdentity| -> Vec<String> {
            i.fields.iter().map(|(k, _)| k.clone()).filter(|k| k != "imei" && k != "udid").collect()
        };
        assert_eq!(keys(&id), keys(&base));
        assert_ne!(id.udid(), base.udid());
    }

    #[test]
    fn write_is_exclusive_and_private() {
        let dir = std::env::temp_dir().join(format!("lt-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let id = UnitIdentity::synthesize("x", "16g", None).unwrap();
        let path = dir.join("identity.json");
        id.write(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(id.write(&path).is_err());
        let back = UnitIdentity::load(&path).unwrap();
        assert_eq!(back.udid(), id.udid());
        assert_eq!(back.die_id(), id.die_id());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The Swift creates' identity.json files (fwkit-oracle n81-a, n72-a), byte for byte.
    #[test]
    fn oracle_identities() {
        let n81 = UnitIdentity::synthesize("ipod4-10B500-default", "16g", Some("MC540")).unwrap();
        assert_eq!(sha(&n81.json()), "9047efe84a5f5704bb8d11692ec6ab7443f38437195fc609ac24644333ef4a2d");
        assert_eq!(n81.udid(), Some("531fee7032b3e5c64c566d6e0094a8834a6fa400"));
        let n72 = UnitIdentity::synthesize_ipod("ipod2g-7E18-default", "MB528", "LL/A", true).unwrap();
        assert_eq!(sha(&n72.json()), "5e44d95cb55eab99ae5994a6174de251305baeff8d5fc7d097996b2844181396");
    }
}

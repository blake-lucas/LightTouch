//! What an IPSW the user chose is, measured against the iOS version they picked (`firmwarekit identify`), so both
//! apps decide and word it the same way. Only the zip's directory and Restore.plist are read; the apps hash the file
//! themselves (Android while copying it in) and pass the SHA-1.
//!
//! Which mismatches may be prepared anyway ("Use anyway"): preparation takes the keys and recipe from the chosen
//! entry and the components from the IPSW's identity for the chosen board. So a file that holds the chosen board can
//! be tried (it may not decrypt or boot, and then preparation fails with a readable error); one without it, or no
//! IPSW at all, cannot.
use crate::ipsw::{Ipsw, RestoreInfo};
use lighttouch_catalog::catalog::{Catalog, Firmware, short_product_name};
use serde::Serialize;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Apple's file for the chosen version.
    Match,
    /// The IPSW of another version the catalog lists (this device's or another's): offer to switch to it.
    Other,
    /// The chosen device and build, but not Apple's file byte for byte (repacked, modified or damaged inside).
    Modified,
    /// The chosen device in a build the catalog doesn't list.
    UnknownVersion,
    /// A device LightTouch emulates, but not the chosen one, in a build the catalog doesn't list.
    OtherDevice,
    /// A device LightTouch doesn't emulate.
    Unsupported,
    /// Not an IPSW: empty, not a zip, or a zip without a usable Restore.plist.
    NotIpsw,
    /// A zip that can't be read: cut short or corrupt.
    Damaged,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub verdict: Verdict,
    /// Whether the chosen version may be prepared from this file anyway.
    pub bypass: bool,
    /// One or two short sentences for the prepare screen; empty for a match.
    pub message: String,
    /// [Verdict::Other]: the catalog entry this file is, and the button that switches to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_label: Option<String>,
    /// What the file says it is (empty when it can't be read).
    pub product_type: String,
    pub version: String,
    pub build: String,
}

impl Check {
    fn new(verdict: Verdict, message: impl Into<String>) -> Self {
        Self {
            verdict,
            bypass: matches!(verdict, Verdict::Modified | Verdict::UnknownVersion),
            message: message.into(),
            firmware: None,
            switch_label: None,
            product_type: String::new(),
            version: String::new(),
            build: String::new(),
        }
    }

    pub fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"verdict":"damaged","bypass":false}"#.into())
    }
}

/// The largest Restore.plist read (real ones are a few KiB): a member claiming more is not an IPSW's.
const PLIST_LIMIT: u64 = 4 << 20;

/// Check `path` against `chosen`. `sha1` is the file's, when the caller hashed it; without one a file of the
/// catalog's exact size counts as Apple's (preparation hashes it again).
pub fn check(path: &Path, chosen: &Firmware, sha1: Option<&str>) -> Check {
    // Nothing in here may take the app down: a panic on a strange file is a damaged file.
    std::panic::catch_unwind(|| check_inner(path, chosen, sha1))
        .unwrap_or_else(|_| Check::new(Verdict::Damaged, "That IPSW is damaged or incomplete. Download it again."))
}

fn check_inner(path: &Path, chosen: &Firmware, sha1: Option<&str>) -> Check {
    let not_ipsw = || Check::new(Verdict::NotIpsw, "That file isn't an IPSW.");
    let damaged = || Check::new(Verdict::Damaged, "That IPSW is damaged or incomplete. Download it again.");
    let size = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m.len(),
        _ => return Check::new(Verdict::NotIpsw, "LightTouch can't read that file."),
    };
    if size == 0 {
        return Check::new(Verdict::NotIpsw, "That file is empty.");
    }
    // A zip starts with a local file header ("PK\3\4"); anything else is not an IPSW, however it is named.
    let mut magic = [0u8; 4];
    let read = std::fs::File::open(path).and_then(|mut f| f.read(&mut magic)).unwrap_or(0);
    if read < 4 || &magic != b"PK\x03\x04" {
        return not_ipsw();
    }
    let Ok(ipsw) = Ipsw::open(path) else { return damaged() };
    match ipsw.contains("Restore.plist") {
        Ok(true) => {}
        Ok(false) => return not_ipsw(),
        Err(_) => return damaged(),
    }
    let data = match read_limited(&ipsw, "Restore.plist", PLIST_LIMIT) {
        Some(Ok(data)) => data,
        Some(Err(())) => return damaged(),
        None => return not_ipsw(),
    };
    let Ok(restore) = RestoreInfo::parse(&data) else { return not_ipsw() };
    classify(&restore, chosen, size, sha1)
}

/// `member`'s bytes when it inflates cleanly within `limit`; Err when it is corrupt, None when it is too large.
fn read_limited(ipsw: &Ipsw, member: &str, limit: u64) -> Option<Result<Vec<u8>, ()>> {
    let mut out = Vec::new();
    let result =
        ipsw.stream(member, |r| r.take(limit + 1).read_to_end(&mut out).map(|_| ()).map_err(crate::error::Error::from));
    match result {
        Err(_) => Some(Err(())),
        Ok(()) if out.len() as u64 > limit => None,
        Ok(()) => Some(Ok(out)),
    }
}

/// The decision itself, from what Restore.plist says (separate for the tests).
pub fn classify(restore: &RestoreInfo, chosen: &Firmware, size: u64, sha1: Option<&str>) -> Check {
    let catalog = Catalog::get();
    // The 1.1.2 beta names the board ("N45AP") as its ProductType.
    let mut boards: Vec<String> = restore.boards.iter().map(|b| b.to_ascii_lowercase()).collect();
    if boards.is_empty() {
        boards.push(restore.board_config.to_ascii_lowercase());
    }
    let build = restore.product_build_version.as_str();
    let version = restore.product_version.as_str();
    let with_file = |mut c: Check| {
        c.product_type = restore.product_type.clone();
        c.version = version.to_string();
        c.build = build.to_string();
        c
    };
    let switch = |f: &Firmware| {
        let same_device = f.product_type == chosen.product_type;
        let mut c = if same_device {
            Check::new(Verdict::Other, format!("That's the iOS {} IPSW.", f.label()))
        } else {
            Check::new(Verdict::Other, format!("That's the {} iOS {} IPSW.", f.device_name(), f.label()))
        };
        c.switch_label = Some(if same_device {
            format!("Use iOS {}", f.label())
        } else {
            format!("Use {} iOS {}", f.device_name(), f.label())
        });
        c.firmware = Some(f.id.clone());
        c
    };
    if boards.iter().any(|b| *b == chosen.board) {
        if build == chosen.build {
            let source = chosen.source.as_ref();
            let original = match (sha1, source) {
                (Some(sha1), Some(s)) => sha1.eq_ignore_ascii_case(&s.sha1),
                (None, Some(s)) => size == s.bytes,
                (_, None) => true,
            };
            return with_file(if original {
                Check::new(Verdict::Match, "")
            } else {
                Check::new(
                    Verdict::Modified,
                    format!("This isn't Apple's original iOS {} file, so it may not work.", chosen.label()),
                )
            });
        }
        if let Some(other) = catalog.find_build(&chosen.board, build) {
            return with_file(switch(other));
        }
        return with_file(Check::new(
            Verdict::UnknownVersion,
            format!(
                "LightTouch doesn't know iOS {version} ({build}) for the {}. It can try it as iOS {}, but it may not \
                 work.",
                chosen.device_name(),
                chosen.label()
            ),
        ));
    }
    // Another device's: the catalog's entry for it when there is one, else what it is.
    if let Some(other) = catalog.firmwares().iter().find(|f| f.build == build && boards.contains(&f.board)) {
        return with_file(switch(other));
    }
    if let Some(known) = catalog.firmwares().iter().find(|f| boards.contains(&f.board)) {
        let name = known.device_name();
        return with_file(Check::new(
            Verdict::OtherDevice,
            format!("That IPSW is for the {name} (iOS {version}). Choose the {name} to try it."),
        ));
    }
    let what = if short_product_name(&restore.product_type) == "iOS device" {
        restore.product_type.clone()
    } else {
        short_product_name(&restore.product_type).to_string()
    };
    with_file(Check::new(
        Verdict::Unsupported,
        format!("That IPSW is for a device LightTouch can't emulate ({what}, iOS {version})."),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore(product: &str, version: &str, build: &str, boards: &[&str]) -> RestoreInfo {
        RestoreInfo {
            product_type: product.into(),
            product_version: version.into(),
            product_build_version: build.into(),
            board_config: boards[0].into(),
            boards: boards.iter().map(|b| b.to_string()).collect(),
            ..Default::default()
        }
    }

    fn chosen(id: &str) -> Firmware {
        Catalog::get().find(id).unwrap().clone()
    }

    #[test]
    fn apples_file_matches() {
        let f = chosen("n72ap-7E18");
        let s = f.source.as_ref().unwrap();
        let r = restore("iPod2,1", "3.1.3", "7E18", &["n72ap"]);
        assert_eq!(classify(&r, &f, s.bytes, Some(&s.sha1.to_uppercase())).verdict, Verdict::Match);
        assert_eq!(classify(&r, &f, s.bytes, None).verdict, Verdict::Match);
    }

    #[test]
    fn a_repacked_file_may_be_tried() {
        let f = chosen("n72ap-7E18");
        let r = restore("iPod2,1", "3.1.3", "7E18", &["n72ap"]);
        let c = classify(&r, &f, 1, Some("00"));
        assert_eq!(c.verdict, Verdict::Modified);
        assert!(c.bypass);
    }

    #[test]
    fn another_listed_version_offers_a_switch() {
        let f = chosen("n72ap-7E18");
        let c = classify(&restore("iPod2,1", "4.2.1", "8C148", &["n72ap"]), &f, 1, None);
        assert_eq!(c.verdict, Verdict::Other);
        assert_eq!(c.firmware.as_deref(), Some("n72ap-8C148"));
        assert_eq!(c.switch_label.as_deref(), Some("Use iOS 4.2.1"));
        assert!(!c.bypass);
    }

    #[test]
    fn another_devices_listed_version_offers_that_device() {
        let f = chosen("n72ap-7E18");
        let c = classify(&restore("iPod4,1", "6.1.6", "10B500", &["n81ap"]), &f, 1, None);
        assert_eq!(c.verdict, Verdict::Other);
        assert_eq!(c.firmware.as_deref(), Some("n81ap-10B500"));
    }

    #[test]
    fn an_unlisted_version_may_be_tried() {
        let f = chosen("n72ap-7E18");
        let c = classify(&restore("iPod2,1", "9.9", "99Z1", &["n72ap"]), &f, 1, None);
        assert_eq!(c.verdict, Verdict::UnknownVersion);
        assert!(c.bypass);
    }

    #[test]
    fn other_and_unknown_devices_are_refused() {
        let f = chosen("n72ap-7E18");
        let c = classify(&restore("iPod4,1", "7.1.2", "11D257", &["n81ap"]), &f, 1, None);
        assert_eq!(c.verdict, Verdict::OtherDevice);
        assert!(!c.bypass);
        let c = classify(&restore("iPhone7,2", "12.0", "16A366", &["n61ap"]), &f, 1, None);
        assert_eq!(c.verdict, Verdict::Unsupported);
        assert!(!c.bypass);
        assert!(c.message.contains("iPhone7,2"));
    }

    #[test]
    fn a_board_config_that_is_not_ascii_parses() {
        // "a\u{e9}b": dropping its last two bytes would split the é (a panic before this was by characters).
        let plist = "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>ProductType</key><string>iPod9,9</string>\
            <key>ProductBuildVersion</key><string>1A1</string><key>ProductVersion</key><string>1.0</string>\
            <key>DeviceMap</key><array><dict><key>BoardConfig</key><string>a\u{e9}b</string></dict></array>\
            <key>SystemPartitionPadding</key><dict><key>a</key><dict><key>8</key><integer>1</integer></dict></dict>\
            </dict></plist>";
        let r = RestoreInfo::parse(plist.as_bytes()).unwrap();
        assert_eq!(classify(&r, &chosen("n72ap-7E18"), 1, None).verdict, Verdict::Unsupported);
    }

    #[test]
    fn the_iphone_5_restore_serves_every_model() {
        let f = chosen("n42ap-14G61");
        let r = restore("iPhone5,1", "10.3.4", "14G61", &["n41ap", "n42ap", "n48ap", "n49ap"]);
        assert_eq!(classify(&r, &f, f.source.as_ref().unwrap().bytes, None).verdict, Verdict::Match);
    }
}

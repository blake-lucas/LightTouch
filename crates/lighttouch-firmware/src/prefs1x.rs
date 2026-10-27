//! iPhone OS 1.x runs no guest agent, so Tweaks' preference keys go into the stopped device (N45/Preferences1x.swift):
//! its system volume (1.x keeps /private/var on it), in the Preferences of the user SpringBoard runs as (root's to
//! 1.1.2, mobile's from 1.1.3), where SpringBoard and MobileSafari read them as they start.
//!
//! This is the in-volume half of Swift's `Preferences1x.write(device:changes:)`: the stopped edit around it (the
//! device record, the overlay and its stamp, begin/commit/discard) belongs to the host's device store, which calls
//! `apply` on the edit's editor and keeps `digest` as the overlay's stamp (STAMP). A new plist should take its
//! folder's owner, as the agent's chown does on 2.x and later: `Applied::created` names them for the caller.
use crate::error::Result;
use crate::hfs::Editor;
use crate::plistedit::{self, Format};
use plist::Value;
use sha2::{Digest, Sha256};

/// The overlay's record of the keys it holds (the hash of the set ones): Erase empties the overlay, and the next start
/// writes them again.
pub const STAMP: &str = ".preferences";

/// One preference change: `value` set on `key` in `domain`, or the key removed when there is no value.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub domain: String,
    pub key: String,
    pub value: Option<Value>,
}

/// What `apply` did: a log line per domain and the plists it created (to give their folder's owner).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub lines: Vec<String>,
    pub created: Vec<String>,
}

pub const ROOT_LIBRARY: &str = "private/var/root/Library";
pub const MOBILE_LIBRARY: &str = "private/var/mobile/Library";

/// Whose Library SpringBoard reads its preferences from: mobile (501) when its launchd job says so (1.1.3 on), else
/// root (N45Board.springBoardUser).
pub fn springboard_user(system: &Editor) -> (&'static str, u32) {
    let job = plistedit::read(system, crate::cydia::SPRINGBOARD_JOB).ok().flatten();
    match job.as_ref().and_then(|j| j.get("UserName")).and_then(Value::as_string) {
        Some("mobile") => (MOBILE_LIBRARY, 501),
        _ => (ROOT_LIBRARY, 0),
    }
}

/// Applies `changes` to the system volume of a stopped 1.x device: each domain's plist in SpringBoard's user's
/// Preferences, created (XML) where missing, the other keys, owner and mode of an existing one kept.
pub fn apply(system: &mut Editor, changes: &[Change]) -> Result<Applied> {
    let dir = format!("{}/Preferences", springboard_user(system).0);
    // the domains in the order they first appear
    let mut domains: Vec<&str> = Vec::new();
    for c in changes {
        if c.domain.is_empty() || c.domain.contains('/') {
            return Err(crate::internal!("a preference change without a domain"));
        }
        if !domains.contains(&c.domain.as_str()) {
            domains.push(&c.domain);
        }
    }
    let mut applied = Applied::default();
    for domain in domains {
        let keys: Vec<&Change> = changes.iter().filter(|c| c.domain == domain).collect();
        let path = format!("{dir}/{domain}.plist");
        if !system.exists(&path) {
            applied.created.push(path.clone());
        }
        plistedit::seed(system, &path, |d| {
            for c in &keys {
                match &c.value {
                    Some(v) => {
                        d.insert(c.key.clone(), v.clone());
                    }
                    None => {
                        d.remove(&c.key);
                    }
                }
            }
            Ok(())
        })?;
        let names: Vec<&str> = keys.iter().map(|c| c.key.as_str()).collect();
        applied.lines.push(format!("/{path}: {}", names.join(", ")));
    }
    Ok(applied)
}

/// The keys `changes` set, in order: the same set gives the same stamp whatever was removed with it. The hash is of
/// a binary plist of [domain, key, value] triples; this plist writer's bytes may differ from Foundation's, so a stamp
/// Swift wrote matches none of these and the keys are written once more, which changes nothing.
pub fn digest(changes: &[Change]) -> Result<String> {
    let mut set: Vec<&Change> = changes.iter().filter(|c| c.value.is_some()).collect();
    set.sort_by_key(|c| format!("{} {}", c.domain, c.key));
    let list = Value::Array(
        set.iter()
            .map(|c| {
                Value::Array(vec![
                    Value::String(c.domain.clone()),
                    Value::String(c.key.clone()),
                    c.value.clone().unwrap_or(Value::String(String::new())),
                ])
            })
            .collect(),
    );
    Ok(hex::encode(Sha256::digest(plistedit::serialize(&list, Format::Binary)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::{Scratch, get_plist, put_plist};

    fn change(domain: &str, key: &str, value: Option<Value>) -> Change {
        Change { domain: domain.into(), key: key.into(), value }
    }

    /// writesIntoTheUserSpringBoardRunsAs, its volume part: the keys go into that user's Preferences, the existing
    /// SpringBoard plist keeps its other keys and format, the same keys again change nothing, and a removal takes its
    /// key out.
    #[test]
    fn writes_into_the_user_springboard_runs_as() {
        for mobile in [false, true] {
            let (_s, mut ed) = Scratch::new("prefs1x");
            let home = if mobile { MOBILE_LIBRARY } else { ROOT_LIBRARY };
            let mut job = plistedit::dictionary([("Label", "com.apple.SpringBoard".into())]);
            if mobile {
                job.insert("UserName".into(), "mobile".into());
            }
            put_plist(&mut ed, crate::cydia::SPRINGBOARD_JOB, job, Format::Xml);
            for other in [ROOT_LIBRARY, MOBILE_LIBRARY] {
                ed.mkdirs(&format!("{other}/Preferences")).unwrap();
            }
            let sb = format!("{home}/Preferences/com.apple.springboard.plist");
            put_plist(&mut ed, &sb, plistedit::dictionary([("SBDisableIdleSleep", true.into())]), Format::Binary);
            assert_eq!(springboard_user(&ed), (home, if mobile { 501 } else { 0 }));

            let set = vec![
                change("com.apple.springboard", "SBFakeTime", Some(true.into())),
                change("com.apple.springboard", "SBFakeTimeString", Some("9:41 AM".into())),
                change("com.apple.mobilesafari", "ConsoleEnabled", Some(true.into())),
            ];
            let applied = apply(&mut ed, &set).unwrap();
            let safari = format!("{home}/Preferences/com.apple.mobilesafari.plist");
            assert_eq!(applied.created, [safari.clone()]);
            assert_eq!(applied.lines[0], format!("/{sb}: SBFakeTime, SBFakeTimeString"));
            let d = get_plist(&ed, &sb);
            assert_eq!(d.get("SBDisableIdleSleep").and_then(Value::as_boolean), Some(true), "its other keys kept");
            assert_eq!(d.get("SBFakeTimeString").and_then(Value::as_string), Some("9:41 AM"));
            assert_eq!(plistedit::format_of(&ed.read(&sb).unwrap()), Format::Binary);
            assert_eq!(get_plist(&ed, &safari).get("ConsoleEnabled").and_then(Value::as_boolean), Some(true));
            assert!(apply(&mut ed, &set).unwrap().created.is_empty());

            apply(&mut ed, &[change("com.apple.springboard", "SBFakeTime", None)]).unwrap();
            assert!(get_plist(&ed, &sb).get("SBFakeTime").is_none(), "a removal takes its key out");
            assert!(apply(&mut ed, &[change("", "x", None)]).is_err());
        }
    }

    #[test]
    fn the_stamp_is_of_the_set_keys_only() {
        let a = vec![
            change("com.apple.springboard", "SBFakeTime", Some(true.into())),
            change("com.apple.mobilesafari", "ConsoleEnabled", Some(true.into())),
        ];
        let mut b = a.clone();
        b.reverse();
        b.push(change("com.apple.springboard", "Gone", None));
        assert_eq!(digest(&a).unwrap(), digest(&b).unwrap());
        assert_eq!(digest(&a).unwrap().len(), 64);
        assert_ne!(digest(&a).unwrap(), digest(&a[..1]).unwrap());
    }
}

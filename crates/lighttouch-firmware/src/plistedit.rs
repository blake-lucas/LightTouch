//! Property-list edits inside a volume (SystemEdits' rewritePlist / seedPlist and the dictionary helpers they use):
//! a plist is read from the editor, edited as a dictionary and written back in place in the format it had.
//!
//! ```ignore
//! plistedit::rewrite(&mut ed, "System/Library/LaunchDaemons/com.apple.BTServer.plist", |d| {
//!     d.insert("Disabled".into(), true.into());
//!     Ok(())
//! })?;
//! plistedit::seed(&mut data, "preferences/SystemConfiguration/preferences.plist", wifi_proxy_prefs)?;
//! ```
use crate::error::Result;
use crate::hfs::Editor;
use plist::{Dictionary, Value};
use std::io::Cursor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Xml,
    Binary,
}

/// A plist's format by its first bytes ("bplist" is binary; anything else is read as XML).
pub fn format_of(bytes: &[u8]) -> Format {
    if bytes.starts_with(b"bplist") { Format::Binary } else { Format::Xml }
}

pub fn parse(bytes: &[u8]) -> Result<Value> {
    Value::from_reader(Cursor::new(bytes)).map_err(|e| crate::unsupported!("not a property list: {e}"))
}

pub fn serialize(value: &Value, format: Format) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match format {
        Format::Xml => sorted(value).to_writer_xml(&mut out),
        Format::Binary => value.to_writer_binary(&mut out),
    }
    .map_err(|e| crate::internal!("write a property list: {e}"))?;
    if format == Format::Xml && out.last() != Some(&b'\n') {
        out.push(b'\n'); // as Foundation ends one
    }
    Ok(out)
}

/// `value` with every dictionary's keys in CFPropertyList's XML order (UTF-16 code units, as CFStringCompare with
/// no options orders them), so an XML plist comes out byte for byte as Foundation writes it.
fn sorted(value: &Value) -> Value {
    match value {
        Value::Dictionary(d) => {
            let mut keys: Vec<&String> = d.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            let mut out = Dictionary::new();
            for k in keys {
                out.insert(k.clone(), sorted(&d[k.as_str()]));
            }
            Value::Dictionary(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        v => v.clone(),
    }
}

/// The dictionary plist at `path` (through symlinks); None when there is no file.
pub fn read(ed: &Editor, path: &str) -> Result<Option<Dictionary>> {
    if !ed.exists(path) {
        return Ok(None);
    }
    match parse(&ed.read(path)?)? {
        Value::Dictionary(d) => Ok(Some(d)),
        _ => Err(crate::unsupported!("{path} is not a dictionary plist")),
    }
}

/// Applies `edit` to the dictionary plist at `path` in place, keeping its format (binary or XML) and its record.
pub fn rewrite(ed: &mut Editor, path: &str, edit: impl FnOnce(&mut Dictionary) -> Result<()>) -> Result<()> {
    let bytes = ed.read(path)?;
    let format = format_of(&bytes);
    let mut d = match parse(&bytes) {
        Ok(Value::Dictionary(d)) => d,
        _ => return Err(crate::unsupported!("{} is not a dictionary plist", leaf(path))),
    };
    edit(&mut d)?;
    ed.write(path, serialize(&Value::Dictionary(d), format)?, None)
}

/// `rewrite` when the plist exists, else creates it (XML, as configd writes, with any missing folders) from an empty
/// dictionary.
pub fn seed(ed: &mut Editor, path: &str, edit: impl FnOnce(&mut Dictionary) -> Result<()>) -> Result<()> {
    if ed.exists(path) {
        return rewrite(ed, path, edit);
    }
    if let Some((dir, _)) = path.rsplit_once('/') {
        ed.mkdirs(dir)?;
    }
    let mut d = Dictionary::new();
    edit(&mut d)?;
    ed.write(path, serialize(&Value::Dictionary(d), Format::Xml)?, None)
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `d[key]` as a dictionary, inserting an empty one when absent or not a dictionary (Python's setdefault).
pub fn dict<'a>(d: &'a mut Dictionary, key: &str) -> &'a mut Dictionary {
    dict_or(d, key, Dictionary::new)
}

/// `dict` with `default()` inserted when the key is absent.
pub fn dict_or<'a>(d: &'a mut Dictionary, key: &str, default: impl FnOnce() -> Dictionary) -> &'a mut Dictionary {
    if !matches!(d.get(key), Some(Value::Dictionary(_))) {
        d.insert(key.to_string(), Value::Dictionary(default()));
    }
    match d.get_mut(key) {
        Some(Value::Dictionary(inner)) => inner,
        _ => unreachable!("just inserted"),
    }
}

/// `d[key]` as an array with `item` first (removed from wherever else it was).
pub fn move_first(d: &mut Dictionary, key: &str, item: &str) {
    let mut list: Vec<Value> = match d.get(key) {
        Some(Value::Array(a)) => a.iter().filter(|v| v.as_string() != Some(item)).cloned().collect(),
        _ => Vec::new(),
    };
    list.insert(0, Value::String(item.to_string()));
    d.insert(key.to_string(), Value::Array(list));
}

/// Adds `lib` to a launchd job's EnvironmentVariables DYLD_INSERT_LIBRARIES (once).
pub fn dyld_insert(job: &mut Dictionary, lib: &str) {
    let env = dict(job, "EnvironmentVariables");
    let mut libs: Vec<String> = env
        .get("DYLD_INSERT_LIBRARIES")
        .and_then(Value::as_string)
        .unwrap_or("")
        .split(':')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if !libs.iter().any(|l| l == lib) {
        libs.push(lib.to_string());
    }
    env.insert("DYLD_INSERT_LIBRARIES".into(), Value::String(libs.join(":")));
}

/// A string array value.
pub fn strings<S: AsRef<str>>(items: &[S]) -> Value {
    Value::Array(items.iter().map(|s| Value::String(s.as_ref().to_string())).collect())
}

/// A dictionary value from (key, value) pairs, in order.
pub fn dictionary<const N: usize>(pairs: [(&str, Value); N]) -> Dictionary {
    let mut d = Dictionary::new();
    for (k, v) in pairs {
        d.insert(k.to_string(), v);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setdefault_and_order() {
        let mut d = Dictionary::new();
        dict(&mut d, "Sets").insert("x".into(), 1.into());
        dict(&mut d, "Sets").insert("y".into(), 2.into());
        assert_eq!(d.get("Sets").and_then(Value::as_dictionary).map(|s| s.len()), Some(2));
        d.insert("ServiceOrder".into(), strings(&["a", "b", "c"]));
        move_first(&mut d, "ServiceOrder", "c");
        assert_eq!(d.get("ServiceOrder"), Some(&strings(&["c", "a", "b"])));
        let mut job = Dictionary::new();
        dyld_insert(&mut job, "/a.dylib");
        dyld_insert(&mut job, "/b.dylib");
        dyld_insert(&mut job, "/a.dylib");
        let env = job.get("EnvironmentVariables").and_then(Value::as_dictionary).unwrap();
        assert_eq!(env.get("DYLD_INSERT_LIBRARIES").and_then(Value::as_string), Some("/a.dylib:/b.dylib"));
    }

    #[test]
    fn formats_round_trip() {
        let v = Value::Dictionary(dictionary([("Label", "com.apple.SpringBoard".into()), ("Disabled", true.into())]));
        for f in [Format::Xml, Format::Binary] {
            let bytes = serialize(&v, f).unwrap();
            assert_eq!(format_of(&bytes), f);
            assert_eq!(parse(&bytes).unwrap(), v);
        }
    }
}

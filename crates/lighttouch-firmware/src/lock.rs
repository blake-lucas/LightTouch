//! The device lock and the JSON the Swift tool wrote: device.lock.json pretty-printed with sorted keys as Foundation's
//! JSONSerialization prints it, and the entry's compact sorted encoding (Swift's JSONEncoder, slashes escaped) whose
//! sha256 the lock records.
use serde_json::{Map, Value};

pub const FILE_NAME: &str = "device.lock.json";

/// The serial of the last preparation change this tool carries (FirmwareSchema PreparationChanges.serial): a base
/// records it so the app can tell which changes it predates.
pub const PREPARATION_SERIAL: i64 = 15;

/// `value` with every object's keys sorted.
pub fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), sorted(&m[k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        v => v.clone(),
    }
}

/// `extras` over `base`; objects merge recursively.
pub fn merged(base: Value, extras: Value) -> Value {
    match (base, extras) {
        (Value::Object(mut a), Value::Object(b)) => {
            for (k, v) in b {
                let merged_value = match a.remove(&k) {
                    Some(old @ Value::Object(_)) if v.is_object() => merged(old, v),
                    _ => v,
                };
                a.insert(k, merged_value);
            }
            Value::Object(a)
        }
        (_, extras) => extras,
    }
}

fn string(s: &str, escape_slashes: bool) -> String {
    let json = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    if escape_slashes { json.replace('/', "\\/") } else { json }
}

fn number(n: &serde_json::Number) -> String {
    if let Some(f) = n.as_f64().filter(|_| n.is_f64()) {
        if f.fract() == 0.0 && f.abs() < 1e15 {
            return format!("{}", f as i64);
        }
    }
    n.to_string()
}

/// Compact JSON with sorted keys and escaped slashes, as Swift's JSONEncoder([.sortedKeys]) writes an entry.
pub fn swift_compact(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number(n),
        Value::String(s) => string(s, true),
        Value::Array(a) => format!("[{}]", a.iter().map(swift_compact).collect::<Vec<_>>().join(",")),
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let body: Vec<String> = keys.iter().map(|k| format!("{}:{}", string(k, true), swift_compact(&m[*k]))).collect();
            format!("{{{}}}", body.join(","))
        }
    }
}

/// The key order of JSONSerialization's .sortedKeys: case-insensitive, digit runs by value, ties broken by the plain
/// order (Foundation's caseInsensitive, numeric, forcedOrdering comparison), so "iBoot" sorts before "iBSS".
pub fn foundation_order(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (mut i, mut j) = (0, 0);
    while i < x.len() && j < y.len() {
        if x[i].is_ascii_digit() && y[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < x.len() && x[i].is_ascii_digit() {
                i += 1;
            }
            while j < y.len() && y[j].is_ascii_digit() {
                j += 1;
            }
            let (p, q): (String, String) = (x[si..i].iter().collect(), y[sj..j].iter().collect());
            let (p, q) = (p.trim_start_matches('0'), q.trim_start_matches('0'));
            let o = p.len().cmp(&q.len()).then_with(|| p.cmp(q));
            if o != Ordering::Equal {
                return o;
            }
            continue;
        }
        let o = x[i].to_lowercase().cmp(y[j].to_lowercase());
        if o != Ordering::Equal {
            return o;
        }
        i += 1;
        j += 1;
    }
    (x.len() - i).cmp(&(y.len() - j)).then_with(|| a.cmp(b))
}

/// JSONSerialization's [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]: two-space indents, "key" : value.
pub fn pretty(value: &Value) -> String {
    fn write(v: &Value, depth: usize, out: &mut String) {
        let pad = |d: usize| "  ".repeat(d);
        match v {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(&b.to_string()),
            Value::Number(n) => out.push_str(&number(n)),
            Value::String(s) => out.push_str(&string(s, false)),
            Value::Array(a) => {
                if a.is_empty() {
                    out.push_str("[\n\n");
                    out.push_str(&pad(depth));
                    out.push(']');
                    return;
                }
                out.push_str("[\n");
                for (i, item) in a.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    write(item, depth + 1, out);
                    out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            Value::Object(m) => {
                if m.is_empty() {
                    out.push_str("{\n\n");
                    out.push_str(&pad(depth));
                    out.push('}');
                    return;
                }
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort_by(|a, b| foundation_order(a, b));
                out.push_str("{\n");
                for (i, k) in keys.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    out.push_str(&string(k, false));
                    out.push_str(" : ");
                    write(&m[*k], depth + 1, out);
                    out.push_str(if i + 1 < keys.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    write(value, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats() {
        let v = json!({"b": {"y": 1, "x": "a/b"}, "a": [true, null], "c": 0.5});
        assert_eq!(swift_compact(&v), r#"{"a":[true,null],"b":{"x":"a\/b","y":1},"c":0.5}"#);
        assert_eq!(
            pretty(&v),
            "{\n  \"a\" : [\n    true,\n    null\n  ],\n  \"b\" : {\n    \"x\" : \"a/b\",\n    \"y\" : 1\n  },\n  \"c\" : 0.5\n}"
        );
        let mut keys = vec!["iBSS", "LLB", "iBoot", "kernelcache", "ce10", "ce2", "OpenGLES", "libappsync.dylib"];
        keys.sort_by(|a, b| foundation_order(a, b));
        assert_eq!(keys, ["ce2", "ce10", "iBoot", "iBSS", "kernelcache", "libappsync.dylib", "LLB", "OpenGLES"]);
        let m = merged(json!({"tool": {"name": "firmwarekit", "built": {"a": 1}}}), json!({"tool": {"built": {"b": 2}}}));
        assert_eq!(m, json!({"tool": {"name": "firmwarekit", "built": {"a": 1, "b": 2}}}));
    }
}

//! Apple's flattened device tree, parsed, edited in place and serialized (ipad1_kboot.DeviceTree).
//!
//! ```ignore
//! let mut dt = DeviceTree::new(blob)?;
//! dt.contains("arm-io/sdio"); dt.props["chosen"]["root-matching"]          // Slot { offset, length }
//! dt.set("chosen", "firmware-version", Value::Str("iBoot-817.29".into()))?; // into the existing slot, zero-padded
//! dt.add("arm-io/usb-complex", "hsic-enabled", &[])?;                       // appends a property; the blob grows
//! dt.rename("chosen/memory-map", "MemoryMapReserved-0", "Kernel-__TEXT")?;
//! dt.add_node("arm-io/spi0", "nor-flash", &[("compatible", Value::Str("nor-flash,spi".into()))])?;
//! dt.value("chosen", "secure-root-prefix")                                  // a property's bytes
//! dt.data()                                                                 // the serialized tree
//! ```
//! Node paths join the "name" properties below the root with "/" ("" is the root, "arm-io/sdio"); a later node with
//! the same path shadows an earlier one, as in the Python. iBoot's DT reserves every slot it fills, so `set` never
//! resizes: a value longer than its slot is an error.
use crate::error::Result;
use std::collections::BTreeMap;

/// A property value as it is encoded into its slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// UTF-8 and a NUL.
    Str(String),
    U32(u32),
    /// Little-endian u32s.
    Words(Vec<u32>),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn encoded(&self) -> Vec<u8> {
        match self {
            Value::Str(s) => {
                let mut v = s.as_bytes().to_vec();
                v.push(0);
                v
            }
            Value::U32(v) => le(&[*v]),
            Value::Words(w) => le(w),
            Value::Bytes(d) => d.clone(),
        }
    }
}

/// Little-endian u32s.
pub fn le(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// A property record: name at +0, length at +32, value at +36.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub offset: usize,
    pub length: usize,
}

/// A node's header offset and the offset just past its properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub start: usize,
    pub props_end: usize,
}

#[derive(Debug, Clone)]
pub struct DeviceTree {
    data: Vec<u8>,
    /// Node path -> property name -> its record.
    pub props: BTreeMap<String, BTreeMap<String, Slot>>,
    pub nodes: BTreeMap<String, Node>,
    /// Node path -> offset just past its last descendant (where `add_node` inserts).
    ends: BTreeMap<String, usize>,
}

/// A property name in its 32-byte field (cut at 32, zero-padded).
pub fn name32(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.as_bytes().iter().copied().take(32).collect();
    v.resize(32, 0);
    v
}

fn u32le(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

impl DeviceTree {
    pub fn new(blob: &[u8]) -> Result<DeviceTree> {
        let mut dt = DeviceTree { data: blob.to_vec(), props: BTreeMap::new(), nodes: BTreeMap::new(), ends: BTreeMap::new() };
        dt.reparse()?;
        Ok(dt)
    }

    /// The serialized tree.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    pub fn contains(&self, path: &str) -> bool {
        self.props.contains_key(path)
    }

    pub fn slot(&self, path: &str, prop: &str) -> Option<Slot> {
        self.props.get(path)?.get(prop).copied()
    }

    /// The property names of the node at `path`, sorted; empty for no such node.
    pub fn prop_names(&self, path: &str) -> Vec<String> {
        self.props.get(path).map(|p| p.keys().cloned().collect()).unwrap_or_default()
    }

    pub fn has(&self, path: &str, prop: &str) -> bool {
        self.slot(path, prop).is_some()
    }

    pub fn value(&self, path: &str, prop: &str) -> Option<Vec<u8>> {
        let s = self.slot(path, prop)?;
        Some(self.data[s.offset + 36..s.offset + 36 + s.length].to_vec())
    }

    /// Writes into the existing slot, zero-padding the rest of it.
    pub fn set(&mut self, path: &str, prop: &str, value: Value) -> Result<()> {
        let s = self.slot(path, prop).ok_or_else(|| crate::unsupported!("DeviceTree: no {path}:{prop}"))?;
        let v = value.encoded();
        if v.len() > s.length {
            return Err(crate::unsupported!("DeviceTree: {path}:{prop} holds {} bytes, got {}", s.length, v.len()));
        }
        let at = s.offset + 36;
        self.data[at..at + v.len()].copy_from_slice(&v);
        self.data[at + v.len()..at + s.length].fill(0);
        Ok(())
    }

    /// Appends a property to a node; the blob grows, so lay memory out after the last add.
    pub fn add(&mut self, path: &str, prop: &str, value: &[u8]) -> Result<()> {
        let node = *self.nodes.get(path).ok_or_else(|| crate::unsupported!("DeviceTree: no node {path}"))?;
        let mut rec = name32(prop);
        rec.extend_from_slice(&le(&[value.len() as u32]));
        rec.extend_from_slice(value);
        rec.resize(rec.len() + pad4(value.len()) - value.len(), 0);
        self.data.splice(node.props_end..node.props_end, rec);
        let n = u32le(&self.data, node.start) + 1;
        self.data[node.start..node.start + 4].copy_from_slice(&n.to_le_bytes());
        self.reparse()
    }

    /// Appends a child node (its name first, then `props`) to `parent`; the blob grows, as `add`.
    pub fn add_node(&mut self, parent: &str, name: &str, props: &[(&str, Value)]) -> Result<()> {
        let (Some(node), Some(&end)) = (self.nodes.get(parent).copied(), self.ends.get(parent)) else {
            return Err(crate::unsupported!("DeviceTree: no node {parent}"));
        };
        let mut rec = le(&[1 + props.len() as u32, 0]);
        let named = std::iter::once(("name", Value::Str(name.to_string()))).chain(props.iter().map(|(k, v)| (*k, v.clone())));
        for (k, v) in named {
            let e = v.encoded();
            rec.extend_from_slice(&name32(k));
            rec.extend_from_slice(&le(&[e.len() as u32]));
            rec.extend_from_slice(&e);
            rec.resize(rec.len() + pad4(e.len()) - e.len(), 0);
        }
        self.data.splice(end..end, rec);
        let n = u32le(&self.data, node.start + 4) + 1;
        self.data[node.start + 4..node.start + 8].copy_from_slice(&n.to_le_bytes());
        self.reparse()
    }

    pub fn rename(&mut self, path: &str, old: &str, new: &str) -> Result<()> {
        let s = self
            .props
            .get_mut(path)
            .and_then(|p| p.remove(old))
            .ok_or_else(|| crate::unsupported!("DeviceTree: no {path}:{old}"))?;
        self.data[s.offset..s.offset + 32].copy_from_slice(&name32(new));
        self.props.entry(path.to_string()).or_default().insert(new.to_string(), s);
        Ok(())
    }

    fn reparse(&mut self) -> Result<()> {
        self.props.clear();
        self.nodes.clear();
        self.ends.clear();
        let end = self.node(0, None)?;
        if end != self.data.len() {
            return Err(crate::unsupported!("DeviceTree: trailing bytes after the device tree"));
        }
        Ok(())
    }

    fn node(&mut self, start: usize, parent: Option<&str>) -> Result<usize> {
        let truncated = || crate::unsupported!("DeviceTree: truncated");
        if start + 8 > self.data.len() {
            return Err(truncated());
        }
        let nprops = u32le(&self.data, start) as usize;
        let nchildren = u32le(&self.data, start + 4) as usize;
        let mut off = start + 8;
        let mut mine: BTreeMap<String, Slot> = BTreeMap::new();
        for _ in 0..nprops {
            if off + 36 > self.data.len() {
                return Err(truncated());
            }
            let name = cstr(&self.data[off..off + 32]);
            let len = (u32le(&self.data, off + 32) & 0x7FFF_FFFF) as usize;
            if off + 36 + len > self.data.len() {
                return Err(truncated());
            }
            mine.insert(name, Slot { offset: off, length: len });
            off += 36 + pad4(len);
        }
        let n = *mine.get("name").ok_or_else(|| crate::unsupported!("DeviceTree: node without a name"))?;
        let name = cstr(&self.data[n.offset + 36..n.offset + 36 + n.length]);
        let path = match parent {
            None => String::new(),
            Some(p) => format!("{p}/{name}").trim_start_matches('/').to_string(),
        };
        self.props.insert(path.clone(), mine);
        self.nodes.insert(path.clone(), Node { start, props_end: off });
        for _ in 0..nchildren {
            off = self.node(off, Some(&path))?;
        }
        self.ends.insert(path, off);
        Ok(off)
    }
}

/// The bytes up to the first NUL, as text.
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn prop(name: &str, value: &[u8]) -> Vec<u8> {
        let mut v = name32(name);
        v.extend_from_slice(&le(&[value.len() as u32]));
        v.extend_from_slice(value);
        v.resize(v.len() + pad4(value.len()) - value.len(), 0);
        v
    }

    pub fn node(props: &[(&str, Vec<u8>)], children: &[Vec<u8>]) -> Vec<u8> {
        let mut v = le(&[props.len() as u32, children.len() as u32]);
        for (k, d) in props {
            v.extend(prop(k, d));
        }
        for c in children {
            v.extend_from_slice(c);
        }
        v
    }

    pub fn n(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    fn small() -> Vec<u8> {
        node(
            &[("name", n("device-tree")), ("model", vec![0; 8])],
            &[node(
                &[("name", n("chosen")), ("firmware-version", vec![0; 16]), ("board-id", vec![0; 4])],
                &[node(&[("name", n("memory-map")), ("MemoryMapReserved-0", vec![0; 8])], &[])],
            )],
        )
    }

    #[test]
    fn edits() {
        let blob = small();
        let mut dt = DeviceTree::new(&blob).unwrap();
        assert_eq!(dt.data(), &blob[..]);
        assert!(dt.set("chosen", "board-id", Value::Words(vec![1, 2])).is_err());
        dt.set("chosen", "firmware-version", Value::Str("iBoot-1".into())).unwrap();
        assert_eq!(dt.value("chosen", "firmware-version").unwrap()[..8], *b"iBoot-1\0");
        dt.add("", "added", &[1, 2, 3]).unwrap();
        assert_eq!(dt.value("", "added").unwrap(), vec![1, 2, 3]);
        assert_eq!(dt.data().len(), blob.len() + 40);
        dt.rename("chosen/memory-map", "MemoryMapReserved-0", "X").unwrap();
        assert!(dt.has("chosen/memory-map", "X") && !dt.has("chosen/memory-map", "MemoryMapReserved-0"));
        assert_eq!(DeviceTree::new(dt.data()).unwrap().value("chosen/memory-map", "X").unwrap(), vec![0; 8]);
        let mut trailing = blob.clone();
        trailing.extend_from_slice(&[0; 4]);
        assert!(DeviceTree::new(&trailing).is_err());
        dt.add_node("chosen", "child", &[("compatible", Value::Str("x".into()))]).unwrap();
        assert_eq!(dt.value("chosen/child", "compatible").unwrap(), b"x\0");
        assert!(dt.contains("chosen/memory-map"));
    }
}

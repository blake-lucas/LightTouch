//! The guest-package formats the .itpack carries (HostRuntime/GuestPack.swift): the ITPACK01 archive reader, one
//! package's manifest.json, and the `ltpkg 1` offer text it_boot reads. FirmwareKit seeds a package at preparation;
//! the app offers one each boot.
use crate::error::Result;
use flate2::read::ZlibDecoder;
use serde::Deserialize;
use std::io::Read;
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"ITPACK01";
/// The offer wire this side writes (it_boot's `ltpkg 1`).
pub const PACKAGE_PROTOCOL: i64 = 1;

/// The GL targets' stock paths (mkpkg GL_TARGETS): their hooks exist only where the preparer installed the front end.
pub fn gl_targets() -> std::collections::HashSet<String> {
    [
        "/System/Library/Frameworks/OpenGLES.framework/MBXGLEngine.bundle/MBXGLEngine",
        "/System/Library/Frameworks/OpenGLES.framework/OpenGLES",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Requires {
    #[serde(default)]
    pub boards: Vec<String>,
    #[serde(default)]
    pub builds: Vec<String>,
    /// "legacy": a legacy-linked family, which takes the arch's legacy loader (mkpkg.LEGACY_LOADER).
    #[serde(default)]
    pub link: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub mode: String,
    pub size: i64,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Hook {
    pub file: String,
    pub target: String,
    pub respring: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub serial: i64,
    pub version: String,
    pub family: String,
    pub arch: String,
    #[serde(default)]
    pub stub: Option<bool>,
    pub requires: Requires,
    pub files: Vec<File>,
    pub jobs: Vec<String>,
    pub hooks: Vec<Hook>,
}

impl Manifest {
    /// Drop hooks (and their payload files) by file name.
    pub fn drop_hooks(&mut self, files: &std::collections::HashSet<String>) {
        self.hooks.retain(|h| !files.contains(&h.file));
        self.files.retain(|f| !files.contains(&f.name));
    }
}

#[derive(Deserialize)]
struct IndexEntry {
    name: String,
    size: i64,
}

#[derive(Deserialize)]
struct Index {
    entries: Vec<IndexEntry>,
}

/// One package the reader found: its family directory, manifest, and payloads by package-relative path.
pub struct Package {
    pub family: String,
    pub manifest: Manifest,
    pub payloads: std::collections::HashMap<String, Vec<u8>>,
}

/// "ITPACK01", a little-endian u32 index length, the JSON index, then one zlib stream; entries in index order.
pub fn read(path: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let blob = std::fs::read(path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let invalid = |why: &str| crate::unsupported!("{name}: {why}");
    if blob.len() < 12 || &blob[..8] != MAGIC {
        return Err(invalid("not an .itpack"));
    }
    let n = u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]) as usize;
    if blob.len() < 12 + n + 2 {
        return Err(invalid("truncated"));
    }
    let index: Index = serde_json::from_slice(&blob[12..12 + n]).map_err(|e| invalid(&format!("bad index: {e}")))?;
    // skip zlib's 2-byte header: the stream is raw deflate (the adler trailer is ignored).
    let mut stream = Vec::new();
    ZlibDecoder::new(&blob[12 + n..]).read_to_end(&mut stream).map_err(|e| invalid(&format!("inflate: {e}")))?;
    let mut entries = Vec::new();
    let mut offset = 0usize;
    for e in &index.entries {
        if e.name.starts_with('/') || e.name.split('/').any(|p| p == "..") || e.size < 0 {
            return Err(invalid(&format!("bad entry {}", e.name)));
        }
        let size = e.size as usize;
        if offset + size > stream.len() {
            return Err(invalid(&format!("bad entry {}", e.name)));
        }
        entries.push((e.name.clone(), stream[offset..offset + size].to_vec()));
        offset += size;
    }
    if offset != stream.len() {
        return Err(invalid("the index does not cover the stream"));
    }
    Ok(entries)
}

/// mkpkg's requires.builds: an exact build id, or "<major>*" for every build of that iOS major.
pub fn build_matches(builds: &[String], build: &str) -> bool {
    let major: String = build.chars().take_while(|c| c.is_ascii_digit()).collect();
    builds.iter().any(|b| b == build || (b.ends_with('*') && b[..b.len() - 1] == major))
}

/// The packages in `entries` whose builds take `build` (and `board`, when given), with their family directory and
/// payloads; stubs only with `stubs`.
pub fn packages(
    entries: &[(String, Vec<u8>)],
    board: Option<&str>,
    build: &str,
    stubs: bool,
) -> Result<Vec<Package>> {
    let mut found = Vec::new();
    for (name, data) in entries {
        if !name.ends_with("/manifest.json") {
            continue;
        }
        let manifest: Manifest =
            serde_json::from_slice(data).map_err(|e| crate::unsupported!("{name}: bad manifest: {e}"))?;
        let board_ok = board.map(|b| manifest.requires.boards.iter().any(|x| x == b)).unwrap_or(true);
        if !board_ok || !build_matches(&manifest.requires.builds, build) || (!stubs && manifest.stub == Some(true)) {
            continue;
        }
        let prefix = &name[..name.len() - "manifest.json".len()];
        let mut payloads = std::collections::HashMap::new();
        for (entry, bytes) in entries {
            if entry.starts_with(prefix) && entry != name {
                payloads.insert(entry[prefix.len()..].to_string(), bytes.clone());
            }
        }
        found.push(Package { family: prefix.trim_end_matches('/').to_string(), manifest, payloads });
    }
    Ok(found)
}

/// The offer text it_boot reads (mkpkg.py offer_text): payload lines indexed in manifest order, after the verdicts.
/// `serial` other than the manifest's (0: the built-in package) carries no payload lines.
pub fn offer_text(m: &Manifest, build: &str, serial: Option<i64>, good: &[i64], bad: &[i64]) -> String {
    let ser = serial.unwrap_or(m.serial);
    let mut lines = vec![format!("ltpkg {PACKAGE_PROTOCOL}"), format!("build {build}"), format!("serial {ser} {}", m.version)];
    lines.extend(good.iter().map(|g| format!("verdict good {g}")));
    lines.extend(bad.iter().map(|b| format!("verdict bad {b}")));
    if serial.is_none() || serial == Some(m.serial) {
        let hooks: std::collections::HashMap<&str, &Hook> = m.hooks.iter().map(|h| (h.file.as_str(), h)).collect();
        let jobs: std::collections::HashSet<&str> = m.jobs.iter().map(String::as_str).collect();
        for (i, f) in m.files.iter().enumerate() {
            let kind = if hooks.contains_key(f.name.as_str()) {
                "hook"
            } else if jobs.contains(f.name.as_str()) {
                "job"
            } else {
                "file"
            };
            let mut line = format!("{kind} {i} {} {} {} {}", f.name, f.mode, f.size, f.sha256);
            if let Some(h) = hooks.get(f.name.as_str()) {
                line += &format!(" {}", h.target);
                if h.respring {
                    line += " respring";
                }
            }
            lines.push(line);
        }
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_match_rules() {
        assert!(build_matches(&["10B500".into()], "10B500"));
        assert!(build_matches(&["10*".into()], "10B500"));
        assert!(!build_matches(&["8*".into()], "10B500"));
        assert!(!build_matches(&["10B501".into()], "10B500"));
    }
}

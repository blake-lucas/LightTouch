//! The open source notices bundled in `resources/licenses/<group>/`, as the Android app's licence screen reads its
//! `assets/licenses`: each group's `manifest.json` (`{"schema": 1, "source_reference", "files": [{"path",
//! "sha256"}]}`) lists its files, and a file is only shown when its SHA-256 matches.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct NoticeFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoticeGroup {
    pub folder: String,
    /// "Firmware preparation", or the folder name.
    pub title: String,
    pub files: Vec<NoticeFile>,
    /// Where the group's source code is.
    pub source: String,
    dir: PathBuf,
}

/// A path the manifests may name: `/`-separated parts of `[A-Za-z0-9._-]`, none empty, `.` or `..`.
pub fn checked_notice_path(path: &str) -> bool {
    path.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    })
}

fn title(folder: &str) -> String {
    match folder {
        "firmware" => "Firmware preparation",
        "engine" => "Engine",
        "frontend" => "Desktop interface",
        "usb" => "App installs (USB bridge)",
        other => other,
    }
    .to_string()
}

/// The order groups are listed in; others follow by name.
const ORDER: [&str; 4] = ["frontend", "engine", "firmware", "usb"];

/// Every group under `licenses`. Fails on the first malformed manifest.
pub fn load(licenses: &Path) -> Result<Vec<NoticeGroup>, String> {
    let entries = std::fs::read_dir(licenses).map_err(|e| format!("Licenses could not be loaded: {e}"))?;
    let mut folders: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    folders.sort_by_key(|f| (ORDER.iter().position(|o| o == f).unwrap_or(ORDER.len()), f.clone()));
    folders
        .iter()
        .map(|folder| group(licenses, folder).map_err(|e| format!("Licenses could not be loaded: {e}")))
        .collect()
}

fn group(licenses: &Path, folder: &str) -> Result<NoticeGroup, String> {
    if !checked_notice_path(folder) || folder.contains('/') {
        return Err(format!("unusable folder {folder}"));
    }
    let dir = licenses.join(folder);
    let text = std::fs::read_to_string(dir.join("manifest.json")).map_err(|e| format!("{folder}: {e}"))?;
    let manifest: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{folder}: {e}"))?;
    if manifest["schema"].as_i64() != Some(1) {
        return Err(format!("{folder}: unknown manifest schema"));
    }
    let source = manifest["source_reference"].as_str().ok_or(format!("{folder}: no source reference"))?.to_string();
    let files = manifest["files"].as_array().ok_or(format!("{folder}: no files"))?;
    let files = files
        .iter()
        .map(|entry| {
            let path = entry["path"].as_str().unwrap_or_default();
            let sha256 = entry["sha256"].as_str().unwrap_or_default();
            let hex = sha256.len() == 64 && sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            if checked_notice_path(path) && hex {
                Ok(NoticeFile { path: path.to_string(), sha256: sha256.to_string() })
            } else {
                Err(format!("{folder}: unusable entry {path}"))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NoticeGroup { folder: folder.to_string(), title: title(folder), files, source, dir })
}

impl NoticeGroup {
    /// The file's text once its digest matches the manifest.
    pub fn read(&self, file: &NoticeFile) -> Result<String, String> {
        let bytes = std::fs::read(self.dir.join(&file.path)).map_err(|e| format!("License could not be read: {e}"))?;
        let digest: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if digest != file.sha256 {
            return Err("License could not be read: Notice integrity check failed".to_string());
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(checked_notice_path("egui-0.37.0-LICENSE"));
        assert!(checked_notice_path("qemu/COPYING"));
        assert!(!checked_notice_path("../etc/passwd"));
        assert!(!checked_notice_path("a//b"));
        assert!(!checked_notice_path("a b"));
    }

    #[test]
    fn manifest_and_digest() {
        let root = std::env::temp_dir().join(format!("lt-notices-{}", std::process::id()));
        let dir = root.join("firmware");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("NOTICE.txt"), "hello\n").unwrap();
        let hash: String = Sha256::digest(b"hello\n").iter().map(|b| format!("{b:02x}")).collect();
        let manifest = serde_json::json!({"schema": 1, "source_reference": "https://example.org",
            "files": [{"path": "NOTICE.txt", "sha256": hash}]});
        std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
        let groups = load(&root).unwrap();
        assert_eq!(groups[0].title, "Firmware preparation");
        assert_eq!(groups[0].read(&groups[0].files[0]).unwrap(), "hello\n");
        std::fs::write(dir.join("NOTICE.txt"), "changed\n").unwrap();
        assert!(groups[0].read(&groups[0].files[0]).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

//! Apps installed while the device is prepared (create --ipa): each IPA's Payload/<name>.app goes into a container of
//! its own on the data volume, /private/var/mobile/Applications/<UUID>/ with Documents, Library/Preferences,
//! Library/Caches and tmp beside it, as installd lays one out. Nothing registers them here: the preparation's first
//! start runs before any installation cache exists, and MobileInstallation builds that cache by scanning the
//! containers it finds, so SpringBoard shows the apps from the first boot on. Benchmark devices get their games in
//! seconds this way instead of minutes over the USB installer.
//!
//! The archive's members are staged as host files under `scratch` and the volume reads them at commit, so a large
//! game never sits in memory. Modes: folders 0755, files 0644 unless the archive marks them executable, the bundle's
//! executable always 0755 (old archives often store no modes, and SpringBoard can't launch a 0644 executable).
use crate::error::{Code, Error, Result};
use crate::hfs::Editor;
use std::io::Read;
use std::path::Path;

/// SpringBoard's tip for new Home screen icons ("Edit Home Screen ... Dismiss"), marked as already shown: it comes up
/// the first time SpringBoard finds an app the user added, over whatever is in front, and the app under it stops
/// drawing until it is dismissed. Returns the preferences file (mobile's).
pub fn skip_reorder_tip(ed: &mut Editor, var: &str) -> Result<String> {
    let prefs = if var.is_empty() { SPRINGBOARD_PREFS.to_string() } else { format!("{var}/{SPRINGBOARD_PREFS}") };
    crate::plistedit::seed(ed, &prefs, |d| {
        d.insert("SBDidShowReorderText".into(), true.into());
        Ok(())
    })?;
    Ok(prefs)
}

const SPRINGBOARD_PREFS: &str = "mobile/Library/Preferences/com.apple.springboard.plist";

/// What one IPA became: its bundle ID, its container (a volume path) and every path written, for their owner.
pub struct Preinstalled {
    pub bundle_id: String,
    pub name: String,
    pub container: String,
    pub paths: Vec<String>,
}

const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const S_IFDIR: u32 = 0o040000;

fn bad(ipa: &Path, what: impl std::fmt::Display) -> Error {
    Error::new(Code::Unsupported, format!("{}: {what}", ipa.display()))
}

/// The container's folder name: a UUID made from the bundle ID, so a device prepared again gets the same path.
pub fn container_uuid(bundle_id: &str) -> String {
    let h = crate::files::sha256(format!("lighttouch preinstalled app {bundle_id}").as_bytes()).to_uppercase();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Install `ipa` into `ed`, whose /private/var is at `var` ("" for a data volume of its own, "private/var" for a
/// single-volume device). `scratch` holds the staged members until the volume is committed.
pub fn install(ed: &mut Editor, var: &str, ipa: &Path, scratch: &Path) -> Result<Preinstalled> {
    let file = std::fs::File::open(ipa).map_err(|e| bad(ipa, e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| bad(ipa, e))?;
    // Payload/<name>.app/Info.plist names the bundle and its executable
    let mut app_dir = None;
    for i in 0..zip.len() {
        let name = zip.by_index_raw(i).map_err(|e| bad(ipa, e))?.name().to_string();
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 3 && parts[0] == "Payload" && parts[1].ends_with(".app") && parts[2] == "Info.plist" {
            if app_dir.is_some() {
                return Err(bad(ipa, "more than one app in Payload"));
            }
            app_dir = Some(parts[1].to_string());
        }
    }
    let app_dir = app_dir.ok_or_else(|| bad(ipa, "no Payload/<name>.app/Info.plist"))?;
    let info = {
        let mut entry = zip.by_name(&format!("Payload/{app_dir}/Info.plist")).map_err(|e| bad(ipa, e))?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| bad(ipa, e))?;
        plist::Value::from_reader(std::io::Cursor::new(bytes))
            .ok()
            .and_then(plist::Value::into_dictionary)
            .ok_or_else(|| bad(ipa, "the app's Info.plist is damaged"))?
    };
    let text = |k: &str| info.get(k).and_then(plist::Value::as_string).filter(|s| !s.is_empty()).map(str::to_string);
    let bundle_id = text("CFBundleIdentifier").ok_or_else(|| bad(ipa, "no CFBundleIdentifier"))?;
    let executable = text("CFBundleExecutable").ok_or_else(|| bad(ipa, "no CFBundleExecutable"))?;
    let name = text("CFBundleDisplayName").or_else(|| text("CFBundleName")).unwrap_or_else(|| bundle_id.clone());

    let join = |a: &str, b: &str| if a.is_empty() { b.to_string() } else { format!("{a}/{b}") };
    let container = join(var, &format!("mobile/Applications/{}", container_uuid(&bundle_id)));
    if ed.lstat(&container)?.is_some() {
        return Err(bad(ipa, format!("{bundle_id} is already installed")));
    }
    let stage = scratch.join(container_uuid(&bundle_id));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| crate::internal!("{}: {e}", stage.display()))?;

    ed.mkdirs(&container)?;
    for dir in ["Documents", "Library", "Library/Preferences", "Library/Caches", "tmp"] {
        ed.mkdir(&format!("{container}/{dir}"), Some(0o755))?;
    }
    let prefix = format!("Payload/{app_dir}/");
    let main = format!("{app_dir}/{executable}");
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| bad(ipa, e))?;
        let raw = entry.name().to_string();
        // the app itself, and iTunesMetadata.plist beside it in the container as installd keeps it
        let rel = if let Some(rest) = raw.strip_prefix(&prefix) {
            format!("{app_dir}/{rest}")
        } else if raw == "iTunesMetadata.plist" {
            raw.clone()
        } else {
            continue;
        };
        let rel = rel.trim_end_matches('/').to_string();
        if rel.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            return Err(bad(ipa, format!("unsafe member {raw}")));
        }
        let path = format!("{container}/{rel}");
        let mode = entry.unix_mode().unwrap_or(0);
        if let Some((dir, _)) = path.rsplit_once('/') {
            if !ed.exists(dir) {
                ed.mkdirs(dir)?;
            }
        }
        if entry.is_dir() || mode & S_IFMT == S_IFDIR {
            if !ed.exists(&path) {
                ed.mkdir(&path, Some(0o755))?;
            }
            continue;
        }
        let mut bytes = Vec::new();
        if mode & S_IFMT == S_IFLNK {
            entry.read_to_end(&mut bytes).map_err(|e| bad(ipa, e))?;
            let target = String::from_utf8(bytes).map_err(|_| bad(ipa, format!("{raw}: a link that isn't text")))?;
            if target.starts_with('/') || target.contains('\0') {
                return Err(bad(ipa, format!("{raw}: a link outside the app")));
            }
            ed.symlink(&path, &target)?;
            continue;
        }
        let staged = stage.join(format!("{i}"));
        let mut out = std::fs::File::create(&staged).map_err(|e| crate::internal!("{}: {e}", staged.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| bad(ipa, format!("{raw}: {e}")))?;
        drop(out);
        let perm = if rel == main || mode & 0o111 != 0 { 0o755 } else { 0o644 };
        ed.write_from(&path, &staged, Some(perm))?;
    }
    if !ed.exists(&format!("{container}/{main}")) {
        return Err(bad(ipa, format!("no executable {main}")));
    }
    let paths = std::iter::once(container.clone())
        .chain(ed.walk(&container)?.into_iter().map(|rel| format!("{container}/{rel}")))
        .collect();
    Ok(Preinstalled { bundle_id, name, container, paths })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_names_are_uuids_from_the_bundle_id() {
        let a = container_uuid("com.halfbrick.jetpackjoyride");
        assert_eq!(a, container_uuid("com.halfbrick.jetpackjoyride"));
        assert_ne!(a, container_uuid("com.limasky.doodlejump"));
        let parts: Vec<usize> = a.split('-').map(str::len).collect();
        assert_eq!(parts, [8, 4, 4, 4, 12]);
        assert!(a.chars().all(|c| c == '-' || c.is_ascii_digit() || c.is_ascii_uppercase()));
    }
}

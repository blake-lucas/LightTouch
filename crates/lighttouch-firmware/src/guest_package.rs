//! The guest package: the it_boot loader and the arch's seed package from <arch>.itpack baked into the system
//! volume, and the hook backups. Every Mach-O it bakes (the loader, the package's
//! binaries and hooks) is first proven to load on this firmware (FitCheck.loads), except the GL engines' and
//! AppSync's hooks, which their own installers check.
use crate::error::{Code, Error, Result};
use crate::fitcheck;
use crate::guest_archive::{self as ga, Manifest};
use crate::hfs::Editor;
use crate::hfs::editor::Kind;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const ROOT: &str = "usr/local/lighttouch";
const LOADER_BIN: &str = "usr/local/bin/it_boot";
const LOADER_JOB: &str = "System/Library/LaunchDaemons/com.qemu.it-boot.plist";
const SYSTEM_VERSION: &str = "System/Library/CoreServices/SystemVersion.plist";
const APPSYNC_PATH: &str = "usr/lib/libappsync.dylib";

/// What was baked: device.lock.json's guest_package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub family: String,
    pub seed: i64,
    pub version: String,
    pub gles: bool,
    pub itpack_path: String,
    pub itpack_sha256: String,
    pub hooks: Vec<String>,
    pub jobs: Vec<String>,
}

impl Record {
    pub fn object(&self) -> serde_json::Value {
        serde_json::json!({
            "family": self.family, "seed": self.seed, "version": self.version, "gles": self.gles,
            "itpack": {"path": self.itpack_path, "sha256": self.itpack_sha256},
            "hooks": self.hooks, "jobs": self.jobs,
        })
    }
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Preserves `target`'s original (or its absence) before an installer changes it; returns the path the caller makes
/// root's. Existing provenance is immutable.
pub fn preserve_hook(volume: &mut Editor, target: &str) -> Result<String> {
    let baked = format!("{target}.baked");
    let absent = format!("{target}.baked-absent");
    let have_baked = volume.lstat(&baked)?;
    let have_absent = volume.lstat(&absent)?;
    if have_baked.is_some() && have_absent.is_some() {
        return Err(crate::internal!("conflicting hook provenance for {target}"));
    }
    if let Some(marker) = have_absent {
        if marker.kind != Kind::File || marker.size != 0 {
            return Err(crate::internal!("invalid absent hook marker for {target}"));
        }
        return Ok(absent);
    }
    if have_baked.is_some() {
        return Ok(baked);
    }
    if let Some(m) = volume.lstat(target)? {
        // <target>.baked keeps what the volume had, with its current permissions.
        let perm = m.perm;
        let bytes =
            if m.kind == Kind::Symlink { volume.read_link(target)?.into_bytes() } else { volume.read(target)? };
        if let Some((dir, _)) = baked.rsplit_once('/') {
            volume.mkdirs(dir)?;
        }
        volume.write(&baked, bytes, Some(perm))?;
        return Ok(baked);
    }
    if let Some((dir, _)) = absent.rsplit_once('/') {
        volume.mkdirs(dir)?;
    }
    volume.write(&absent, Vec::new(), Some(0o644))?;
    Ok(absent)
}

/// `put` with the parent folders created (0755) and recorded, as mkpkg does; an existing file keeps its catalog
/// record.
fn put(volume: &mut Editor, written: &mut Vec<String>, rel: &str, data: Vec<u8>, mode: u16) -> Result<()> {
    let mut missing: Vec<String> = Vec::new();
    if let Some((parent, _)) = rel.rsplit_once('/') {
        let mut at = String::new();
        for part in parent.split('/').filter(|p| !p.is_empty()) {
            at = if at.is_empty() { part.to_string() } else { format!("{at}/{part}") };
            if !volume.is_folder(&at) {
                missing.push(at.clone());
            }
        }
    }
    for d in &missing {
        volume.mkdir(d, None)?;
    }
    volume.write(rel, data, Some(mode))?;
    written.extend(missing);
    written.push(rel.to_string());
    Ok(())
}

fn octal(s: &str) -> u16 {
    u16::from_str_radix(s.trim(), 8).unwrap_or(0o755)
}

/// The loader and the seed package of `itpack` into `volume`: (the paths written, which the caller makes root's;
/// the record). A hook whose target is not on the volume is dropped; unless left out on purpose (`omitted`) or a GL
/// engine's, the drop is a recorded misfit.
pub fn seed(
    volume: &mut Editor,
    itpack: &Path,
    gles: bool,
    omitted: &HashSet<String>,
    fit: &fitcheck::Log,
) -> Result<(Vec<String>, Record)> {
    let itpack_bytes = std::fs::read(itpack).map_err(|e| crate::internal!("{}: {e}", itpack.display()))?;
    let name = itpack.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let list = ga::read(itpack)?;
    let mut entries: HashMap<String, Vec<u8>> = HashMap::new();
    for (n, d) in &list {
        entries.entry(n.clone()).or_insert_with(|| d.clone()); // first wins
    }

    let build = plist_string(volume, SYSTEM_VERSION, "ProductBuildVersion")
        .ok_or_else(|| crate::unsupported!("no ProductBuildVersion in /{SYSTEM_VERSION}"))?;
    let families = ga::packages(&list, None, &build, true)?;
    if families.len() != 1 {
        return Err(crate::unsupported!("{name}: {} packages for build {build}", families.len()));
    }
    let family = families[0].family.clone();
    let mut man: Manifest = families[0].manifest.clone();

    let gl_targets = ga::gl_targets();
    let on_volume = |volume: &Editor, target: &str| volume.exists(target.trim_start_matches('/'));
    let hooks: Vec<ga::Hook> = man
        .hooks
        .iter()
        .filter(|h| (gles || !gl_targets.contains(&h.target)) && on_volume(volume, &h.target))
        .cloned()
        .collect();
    let needs_absence = hooks.iter().any(|h| {
        volume
            .lstat(&format!("{}.baked-absent", h.target.trim_start_matches('/')))
            .map(|m| m.is_some())
            .unwrap_or(false)
    });
    if needs_absence && entries.get("loader/hook-provenance").map(Vec::as_slice) != Some(b"file-or-absence 1\n") {
        return Err(crate::unsupported!(
            "guest loader cannot restore absent hook originals; rebuild the guest exports"
        ));
    }
    let kept: HashSet<&str> = hooks.iter().map(|h| h.file.as_str()).collect();
    let dropped: HashSet<String> =
        man.hooks.iter().map(|h| h.file.clone()).filter(|f| !kept.contains(f.as_str())).collect();
    for h in &man.hooks {
        if !dropped.contains(&h.file) {
            continue;
        }
        if gl_targets.contains(&h.target) || omitted.contains(&h.target) {
            continue;
        }
        fit.check(
            fitcheck::Fit::new(
                format!("{family}/{} (hook)", h.file),
                false,
                format!("its target {} is not on this firmware: the hook is dropped", h.target),
            ),
            false,
            "left out",
        )?;
    }
    man.drop_hooks(&dropped);
    let files = man.files.clone();

    // the loader and the package's own binaries must load on this firmware's dyld, with this firmware's images
    let arch = itpack.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let fw = fitcheck::Firmware::new(volume.snapshot()?, &arch, None);
    let legacy_loader = man.requires.link.as_deref() == Some("legacy") && entries.contains_key("loader/it_boot-legacy");
    let loader_name = if legacy_loader { "loader/it_boot-legacy" } else { "loader/it_boot" };
    let loader_bytes = entries
        .get(loader_name)
        .cloned()
        .ok_or_else(|| Error::new(Code::Internal, format!("{name}: no {loader_name}")))?;
    fit.check(fitcheck::loads("it_boot (guest-package loader)", &loader_bytes, &fw, None), true, "left out")?;
    let hook_targets: HashMap<&str, &str> = hooks.iter().map(|h| (h.file.as_str(), h.target.as_str())).collect();
    for f in &files {
        let target = hook_targets.get(f.name.as_str()).copied();
        let Some(bytes) = entries.get(&format!("{family}/{}", f.name)) else { continue };
        if !fitcheck::is_mach_o(bytes) {
            continue;
        }
        let is_gl_or_appsync =
            target.map(|t| gl_targets.contains(t) || t == format!("/{APPSYNC_PATH}")).unwrap_or(false);
        if is_gl_or_appsync {
            continue;
        }
        let host = target.and_then(|t| fitcheck::host(t, &fw));
        fit.check(fitcheck::loads(&format!("{family}/{}", f.name), bytes, &fw, host.as_deref()), true, "left out")?;
    }
    drop(fw);

    let payload = |entries: &HashMap<String, Vec<u8>>, n: &str| -> Result<Vec<u8>> {
        entries.get(n).cloned().ok_or_else(|| Error::new(Code::Internal, format!("{name}: no {n}")))
    };

    let mut written: Vec<String> = Vec::new();
    put(volume, &mut written, LOADER_BIN, loader_bytes, 0o755)?;
    put(volume, &mut written, LOADER_JOB, payload(&entries, "loader/com.qemu.it-boot.plist")?, 0o644)?;
    let serial = man.serial;
    let pkg = format!("{ROOT}/pkgs/{serial}");
    for f in &files {
        put(
            volume,
            &mut written,
            &format!("{pkg}/{}", f.name),
            payload(&entries, &format!("{family}/{}", f.name))?,
            octal(&f.mode),
        )?;
    }
    put(
        volume,
        &mut written,
        &format!("{pkg}/offer"),
        ga::offer_text(&man, &build, None, &[], &[]).into_bytes(),
        0o644,
    )?;
    volume.symlink(&format!("{ROOT}/current"), &format!("pkgs/{serial}"))?;
    let mut state = format!("seed {serial}\n");
    put(volume, &mut written, &format!("{ROOT}/state"), state.clone().into_bytes(), 0o644)?;
    written.push(format!("{ROOT}/current"));
    let modes: HashMap<&str, u16> = files.iter().map(|f| (f.name.as_str(), octal(&f.mode))).collect();
    for h in &hooks {
        let target = h.target.trim_start_matches('/').to_string();
        let backup = preserve_hook(volume, &target)?;
        if !written.contains(&backup) {
            written.push(backup);
        }
        put(
            volume,
            &mut written,
            &target,
            payload(&entries, &format!("{family}/{}", h.file))?,
            *modes.get(h.file.as_str()).unwrap_or(&0o755),
        )?;
        state += &format!("hook {} {}\n", if h.respring { 1 } else { 0 }, h.target);
        put(volume, &mut written, &format!("{ROOT}/state"), state.clone().into_bytes(), 0o644)?;
    }
    let jobs: Vec<String> = man.jobs.iter().map(|j| last_component(j).to_string()).collect();
    for j in &jobs {
        let rel = format!("System/Library/LaunchDaemons/{j}");
        if volume.lstat(&rel)?.is_some() {
            volume.remove(&rel)?;
        }
    }
    Ok((
        written,
        Record {
            family,
            seed: serial,
            version: man.version.clone(),
            gles,
            itpack_path: itpack.to_string_lossy().into_owned(),
            itpack_sha256: sha256_hex(&itpack_bytes),
            hooks: hooks.iter().map(|h| h.target.clone()).collect(),
            jobs,
        },
    ))
}

fn last_component(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// A string value under `key` of a dictionary plist on the volume.
fn plist_string(volume: &Editor, rel: &str, key: &str) -> Option<String> {
    let d = volume.read(rel).ok()?;
    match plist::Value::from_reader(std::io::Cursor::new(d)) {
        Ok(plist::Value::Dictionary(d)) => d.get(key).and_then(plist::Value::as_string).map(str::to_string),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hook's original (or its absence) is preserved once and never overwritten (HookProvenanceTests).
    #[test]
    fn original_and_absence_are_immutable() {
        let dir = std::env::temp_dir().join(format!("lt-hook-provenance-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("v.hfs");
        Editor::format(&image, 8 << 20, "Test", false, 0).unwrap();
        let mut ed = Editor::open(&image).unwrap();
        ed.mkdirs("framework").unwrap();
        let target = "framework/Engine";
        ed.write(target, b"original".to_vec(), Some(0o751)).unwrap();
        let backup = preserve_hook(&mut ed, target).unwrap();
        assert_eq!(backup, format!("{target}.baked"));
        ed.write(target, b"frontend".to_vec(), Some(0o755)).unwrap();
        assert_eq!(preserve_hook(&mut ed, target).unwrap(), backup);
        assert_eq!(ed.read(&backup).unwrap(), b"original");
        assert_eq!(ed.stat(&backup).unwrap().unwrap().perm, 0o751);

        let missing = "framework/Missing";
        let marker = preserve_hook(&mut ed, missing).unwrap();
        assert_eq!(marker, format!("{missing}.baked-absent"));
        ed.write(missing, b"override".to_vec(), Some(0o755)).unwrap();
        assert_eq!(preserve_hook(&mut ed, missing).unwrap(), marker);
        assert!(ed.read(&marker).unwrap().is_empty());
        assert!(!ed.exists(&format!("{missing}.baked")));
        ed.write(&format!("{missing}.baked"), b"conflict".to_vec(), Some(0o755)).unwrap();
        assert!(preserve_hook(&mut ed, missing).is_err());
        assert_eq!(ed.read(missing).unwrap(), b"override");
        drop(ed);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

//! Resources carried inside the executable (the Windows single-file build).
//!
//! `package-desktop.py --onefile` appends a zip of the resources folder to `lighttouch.exe`, then a trailer: the
//! payload's offset and length, its SHA-256, and a magic. The first start unpacks it into `Runtime/<hash>/` beside the
//! data folder; every later start, and every worker, finds it there. A new version carries a new payload, so it gets a
//! folder of its own and the old ones are removed; a folder whose files went missing or changed size is unpacked again.
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"LTRES01\0";
const TRAILER: u64 = 8 + 8 + 32 + 8;
/// Written last into an unpacked folder: one `size<TAB>path` line per file.
const MARKER: &str = ".complete";

/// The payload in this executable.
#[derive(Debug, Clone)]
pub struct Embedded {
    exe: PathBuf,
    offset: u64,
    len: u64,
    sha256: [u8; 32],
}

impl Embedded {
    /// The payload appended to the running executable, if it has one.
    pub fn current() -> Option<Embedded> {
        Self::read(&std::env::current_exe().ok()?)
    }

    fn read(exe: &Path) -> Option<Embedded> {
        let mut f = File::open(exe).ok()?;
        let size = f.metadata().ok()?.len();
        if size < TRAILER {
            return None;
        }
        f.seek(SeekFrom::Start(size - TRAILER)).ok()?;
        let mut t = [0u8; TRAILER as usize];
        f.read_exact(&mut t).ok()?;
        if &t[48..56] != MAGIC {
            return None;
        }
        let offset = u64::from_le_bytes(t[0..8].try_into().ok()?);
        let len = u64::from_le_bytes(t[8..16].try_into().ok()?);
        if offset.checked_add(len)? != size - TRAILER {
            return None;
        }
        Some(Embedded { exe: exe.to_path_buf(), offset, len, sha256: t[16..48].try_into().ok()? })
    }

    /// Where this payload unpacks: named by its hash, so each version has its own.
    pub fn dir(&self) -> PathBuf {
        root().join(hex(&self.sha256[..8]))
    }

    /// Unpacked already, with every file the size it was unpacked at.
    pub fn ready(&self) -> bool {
        complete(&self.dir())
    }

    /// The payload's size, for progress.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Unpack into [`Embedded::dir`] (a no-op when it is ready), then remove other versions' folders. `progress`
    /// gets the payload bytes read so far and the total.
    pub fn unpack(&self, progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf, String> {
        let dir = self.dir();
        if complete(&dir) {
            return Ok(dir);
        }
        let root = root();
        std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
        // Read and check the whole payload first: a damaged download stops here, before anything is written.
        let mut f = File::open(&self.exe).map_err(|e| format!("{}: {e}", self.exe.display()))?;
        f.seek(SeekFrom::Start(self.offset)).map_err(|e| e.to_string())?;
        let mut payload = Vec::with_capacity(self.len as usize);
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut left = self.len;
        while left > 0 {
            let want = left.min(buf.len() as u64) as usize;
            let n = f.read(&mut buf[..want]).map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("LightTouch's executable is cut short; download it again".into());
            }
            hash.update(&buf[..n]);
            payload.extend_from_slice(&buf[..n]);
            left -= n as u64;
            progress((self.len - left) / 2, self.len);
        }
        if hash.finalize().as_slice() != self.sha256 {
            return Err("LightTouch's executable is damaged; download it again".into());
        }
        let stage = root.join(format!("{}.partial-{}", hex(&self.sha256[..8]), std::process::id()));
        let _ = std::fs::remove_dir_all(&stage);
        let result = extract(&payload, &stage, &mut |done| progress(self.len / 2 + done / 2, self.len));
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(e);
        }
        // Another process may have finished the same version meanwhile; its copy is as good as this one.
        if complete(&dir) {
            let _ = std::fs::remove_dir_all(&stage);
        } else {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::rename(&stage, &dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        progress(self.len, self.len);
        prune(&root, &dir);
        Ok(dir)
    }
}

/// `Runtime/` beside the data folder (Windows: `%LOCALAPPDATA%\LightTouch\Runtime`).
fn root() -> PathBuf {
    let data = crate::paths::data_root();
    match data.parent() {
        Some(parent) if std::env::var_os("LIGHTTOUCH_DATA").is_none() => parent.join("Runtime"),
        _ => data.join("runtime"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn complete(dir: &Path) -> bool {
    let Ok(list) = std::fs::read_to_string(dir.join(MARKER)) else { return false };
    let mut any = false;
    for line in list.lines() {
        let Some((size, name)) = line.split_once('\t') else { return false };
        let Ok(meta) = std::fs::metadata(dir.join(name)) else { return false };
        if size.parse::<u64>().ok() != Some(meta.len()) {
            return false;
        }
        any = true;
    }
    any
}

/// Unpack the zip into `stage` and write the marker last. `progress` gets the uncompressed bytes written so far,
/// scaled to the compressed total.
fn extract(payload: &[u8], stage: &Path, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(payload)).map_err(|e| format!("payload: {e}"))?;
    let total: u64 = (0..zip.len()).filter_map(|i| zip.by_index_raw(i).ok().map(|f| f.size())).sum::<u64>().max(1);
    let mut written = 0u64;
    let mut marker = String::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("payload: {e}"))?;
        let Some(name) = entry.enclosed_name() else { return Err(format!("payload: unsafe name {}", entry.name())) };
        let path = stage.join(&name);
        if entry.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut out = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = entry.read(&mut buf).map_err(|e| format!("payload {}: {e}", name.display()))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", path.display()))?;
            written += n as u64;
            progress(written * payload.len() as u64 / total);
        }
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = out.set_permissions(std::fs::Permissions::from_mode(mode & 0o777));
        }
        out.sync_all().map_err(|e| format!("{}: {e}", path.display()))?;
        marker.push_str(&format!("{}\t{}\n", entry.size(), name.to_string_lossy().replace('\\', "/")));
    }
    std::fs::write(stage.join(MARKER), marker).map_err(|e| format!("{}: {e}", stage.display()))
}

/// Remove other versions' folders and abandoned partial unpacks (an hour old: a younger one may be another
/// LightTouch's first start). A folder a running older LightTouch still has open fails on Windows and goes next time.
fn prune(root: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let partial = e.file_name().to_string_lossy().contains(".partial-");
        let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if path != keep && path.is_dir() && (!partial || age.is_some_and(|a| a.as_secs() > 3600)) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload built as package-desktop.py builds it unpacks, is found ready, and is unpacked again when a file
    /// goes missing; a flipped byte is refused.
    #[test]
    fn unpacks_and_heals() {
        let tmp = std::env::temp_dir().join(format!("lt-bundle-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mut zipped = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut zipped);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("libqemu-arm.dll", o).unwrap();
            z.write_all(&[7u8; 5000]).unwrap();
            z.start_file("guest-tools/armv6.itpack", o).unwrap();
            z.write_all(b"ITPACK01").unwrap();
            z.finish().unwrap();
        }
        let payload = zipped.into_inner();
        let exe = tmp.join("lighttouch.exe");
        let mut bytes = b"MZ fake program".to_vec();
        let offset = bytes.len() as u64;
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&Sha256::digest(&payload));
        bytes.extend_from_slice(MAGIC);
        std::fs::write(&exe, &bytes).unwrap();

        let e = Embedded::read(&exe).unwrap();
        assert_eq!(e.len(), payload.len() as u64);
        let stage = tmp.join("stage");
        extract(&payload, &stage, &mut |_| {}).unwrap();
        assert!(complete(&stage));
        assert_eq!(std::fs::read(stage.join("guest-tools/armv6.itpack")).unwrap(), b"ITPACK01");
        std::fs::remove_file(stage.join("libqemu-arm.dll")).unwrap();
        assert!(!complete(&stage));

        let mut damaged = bytes.clone();
        damaged[offset as usize + 40] ^= 1;
        std::fs::write(&exe, &damaged).unwrap();
        let bad = Embedded::read(&exe).unwrap();
        // SAFETY of the test: LIGHTTOUCH_DATA points root() into the test folder.
        unsafe { std::env::set_var("LIGHTTOUCH_DATA", tmp.join("data")) };
        assert!(bad.unpack(&mut |_, _| {}).unwrap_err().contains("damaged"));
        std::fs::write(&exe, &bytes).unwrap();
        let good = Embedded::read(&exe).unwrap();
        let out = good.unpack(&mut |_, _| {}).unwrap();
        assert!(complete(&out) && out.starts_with(tmp.join("data")));
        assert!(good.ready() && good.dir() == out);
        // A file gone: unpacked again. Another version's folder: removed.
        std::fs::remove_file(out.join("libqemu-arm.dll")).unwrap();
        std::fs::create_dir_all(out.parent().unwrap().join("0123456789abcdef")).unwrap();
        assert!(!good.ready());
        let again = good.unpack(&mut |_, _| {}).unwrap();
        assert!(complete(&again) && !out.parent().unwrap().join("0123456789abcdef").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

//! The preparation's file chores: digests with progress, sparse-aware hashing of the NAND page files, the store's
//! listing, free space and the decrypt cache's lease (Swift's Preparer.digest/nandListing, SparseFile,
//! StorageCapacity, FirmwareCache).
use crate::error::Result;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The digest of `path` with `D`, hex; `count` hears every chunk's size.
pub fn digest<D: Digest>(path: &Path, count: &dyn Fn(u64)) -> Result<String> {
    let mut file = File::open(path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    let mut hasher = D::new();
    let mut buffer = vec![0u8; 1 << 22];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        count(n as u64);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn sha1_file(path: &Path, count: &dyn Fn(u64)) -> Result<String> {
    digest::<Sha1>(path, count)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    digest::<Sha256>(path, &|_| {})
}

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The SHA-256 of a sparse file, reading only its data: a hole hashes as the zeros it reads as, from memory. A NAND
/// page file is 2.2 GB with a tenth of it written; reading it all would move every byte.
pub fn sparse_sha256(path: &Path, count: &dyn Fn(u64)) -> Result<String> {
    let file = File::open(path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    let len = file.metadata()?.len();
    let extents = crate::k48nand_image::data_extents(&file).unwrap_or_else(|_| vec![(0, len)]);
    let mut hasher = Sha256::new();
    let zeros = vec![0u8; 1 << 22];
    let mut buffer = vec![0u8; 1 << 22];
    let mut at = 0u64;
    let hole = |hasher: &mut Sha256, mut n: u64| {
        while n > 0 {
            let k = n.min(zeros.len() as u64) as usize;
            hasher.update(&zeros[..k]);
            count(k as u64);
            n -= k as u64;
        }
    };
    for (start, end) in extents {
        let (start, end) = (start.max(at), end.min(len));
        if end <= start {
            continue;
        }
        hole(&mut hasher, start - at);
        let mut p = start;
        while p < end {
            let k = (end - p).min(buffer.len() as u64) as usize;
            crate::posio::read_exact_at(&file, &mut buffer[..k], p)?;
            hasher.update(&buffer[..k]);
            count(k as u64);
            p += k as u64;
        }
        at = end;
    }
    hole(&mut hasher, len - at);
    Ok(hex::encode(hasher.finalize()))
}

/// Every regular file under `dir` by its relative path ("/"-separated), sorted: the store's listing order.
pub fn tree_files(dir: &Path) -> Result<Vec<String>> {
    fn walk(base: &Path, rel: &str, out: &mut Vec<String>) -> Result<()> {
        for entry in std::fs::read_dir(base.join(rel))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(base, &path, out)?;
            } else if kind.is_file() {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if dir.is_dir() {
        walk(dir, "", &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// The sha256 of each of `files` (relative to `nand`, in listing order) and the listing's sha256 over "path sha256\n"
/// lines: what identifies a store. One thread a file.
pub fn nand_listing(nand: &Path, files: &[String], count: &(dyn Fn(u64) + Sync)) -> Result<(Vec<(String, String)>, String)> {
    let results: Vec<Result<String>> = std::thread::scope(|s| {
        let handles: Vec<_> = files.iter().map(|f| s.spawn(move || sparse_sha256(&nand.join(f), count))).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(crate::internal!("a hashing thread failed")))).collect()
    });
    let mut hashes = Vec::new();
    let mut listing = Sha256::new();
    for (f, r) in files.iter().zip(results) {
        let h = r?;
        listing.update(format!("{f} {h}\n").as_bytes());
        hashes.push((f.clone(), h));
    }
    Ok((hashes, hex::encode(listing.finalize())))
}

/// The bytes free for this user on the filesystem holding `dir`; None where it can't tell.
pub fn available(dir: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        Some(s.f_bavail as u64 * s.f_frsize as u64)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(dir: *const u16, free: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
        }
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
        (ok != 0).then_some(free)
    }
}

/// Fails with disk_full when `dir`'s filesystem has less than `bytes` free.
pub fn require_space(bytes: i64, dir: &Path) -> Result<()> {
    if bytes <= 0 {
        return Ok(());
    }
    if let Some(free) = available(dir) {
        if (free as i64) < bytes {
            return Err(crate::error::Error {
                code: crate::error::Code::DiskFull,
                message: format!("needs {bytes} bytes, {free} available"),
                piece: None,
            });
        }
    }
    Ok(())
}

/// A shared hold on the decrypt cache's root (its .lease file, never removed), so a prune waits for every preparation
/// using it.
pub struct Lease {
    _file: File,
}

pub fn consume_cache(root: &Path) -> Result<Lease> {
    std::fs::create_dir_all(root)?;
    let file = File::options().read(true).write(true).create(true).truncate(false).open(root.join(".lease"))?;
    file.lock_shared().map_err(|e| crate::internal!("firmware cache lease: {e}"))?;
    Ok(Lease { _file: file })
}

/// Removes a folder tree.
pub fn remove_tree(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

/// `path` with symlinks resolved (an IPSW imported from another volume is a link to the user's file).
pub fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

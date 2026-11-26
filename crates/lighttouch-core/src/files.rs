//! Copying a folder to another drive, for moving the device library (Settings → Storage). Sparse files stay sparse:
//! devices prepared before the qcow2 NAND keep their chips as page files that are mostly holes (an iPod touch 4G
//! store is 17.7 GB long, ~1.8 GB stored), and a dense copy would write all of it.
use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

/// The bytes `path` (a file, or a folder and everything below it) takes on disk, holes left out where the system
/// says. Symbolic links are not followed.
pub fn stored_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if meta.is_dir() {
        std::fs::read_dir(path).into_iter().flatten().flatten().map(|e| stored_bytes(&e.path())).sum()
    } else if meta.is_file() {
        allocated_at(path, &meta)
    } else {
        0
    }
}

#[cfg(unix)]
fn allocated(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(any(unix, windows)))]
fn allocated(meta: &Metadata) -> u64 {
    meta.len()
}

/// The bytes a file takes on an NTFS drive, holes left out (`GetCompressedFileSizeW`), else its length.
#[cfg(windows)]
fn allocated_at(path: &Path, meta: &Metadata) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut high = 0u32;
    // SAFETY: a NUL-terminated path and an out-parameter for the high half.
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    if low == u32::MAX && std::io::Error::last_os_error().raw_os_error().unwrap_or(0) != 0 {
        return meta.len();
    }
    (u64::from(high) << 32) | u64::from(low)
}

#[cfg(not(windows))]
fn allocated_at(_path: &Path, meta: &Metadata) -> u64 {
    allocated(meta)
}

/// The volume `path` (which exists) is on: `C:\`, or a mount folder's path.
#[cfg(windows)]
pub fn volume(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut out = vec![0u16; 1024];
    // SAFETY: a NUL-terminated path and a buffer of the length passed.
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
    let end = out.iter().position(|&c| c == 0)?;
    (ok != 0).then(|| String::from_utf16_lossy(&out[..end]))
}

/// Names [`copy_tree`] leaves out.
pub type Skip<'a> = &'a dyn Fn(&std::ffi::OsStr) -> bool;

/// Copy `src` (a folder or a file) to `dst`, which must not exist yet, with every file synced to disk, leaving out
/// entries whose names `skip` picks. `progress` gets the bytes copied so far, adding to `done`, after each piece
/// and returns `false` to stop; that, or any failure, leaves `dst` partly written for the caller to remove. Symbolic
/// links are refused (a device library has none).
pub fn copy_tree(
    src: &Path,
    dst: &Path,
    skip: Skip,
    done: &mut u64,
    progress: &mut dyn FnMut(u64) -> bool,
) -> io::Result<()> {
    if std::fs::symlink_metadata(src)?.is_file() {
        return copy_file(src, dst, done, progress).map_err(|e| at(src, e));
    }
    copy_dir(src, dst, skip, done, progress)
}

fn copy_dir(
    src: &Path,
    dst: &Path,
    skip: Skip,
    done: &mut u64,
    progress: &mut dyn FnMut(u64) -> bool,
) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(std::fs::symlink_metadata(src)?.permissions().mode() & 0o777);
    }
    builder.create(dst).map_err(|e| at(dst, e))?;
    for entry in std::fs::read_dir(src).map_err(|e| at(src, e))? {
        let entry = entry?;
        if skip(&entry.file_name()) {
            continue;
        }
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&from, &to, skip, done, progress)?;
        } else if kind.is_file() {
            copy_file(&from, &to, done, progress).map_err(|e| at(&from, e))?;
        } else {
            return Err(at(&from, io::Error::new(io::ErrorKind::Unsupported, "not a file or folder")));
        }
    }
    Ok(())
}

fn at(path: &Path, e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::Interrupted {
        return e;
    }
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

const CHUNK: usize = 1 << 20;

fn copy_file(src: &Path, dst: &Path, done: &mut u64, progress: &mut dyn FnMut(u64) -> bool) -> io::Result<()> {
    let input = File::open(src)?;
    let meta = input.metadata()?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(meta.permissions().mode() & 0o777);
    }
    let output = options.open(dst)?;
    mark_sparse(&output);
    let len = meta.len();
    output.set_len(len)?;
    let mut buffer = vec![0u8; CHUNK];
    for (start, end) in data_ranges(&input, len) {
        let mut offset = start;
        while offset < end {
            let n = ((end - offset) as usize).min(CHUNK);
            read_exact_at(&input, &mut buffer[..n], offset)?;
            // Zeros are left as holes: the file's length is already set, and a hole reads back as zeros.
            if buffer[..n].iter().any(|&b| b != 0) {
                write_all_at(&output, &buffer[..n], offset)?;
            }
            offset += n as u64;
            *done += n as u64;
            if !progress(*done) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
        }
    }
    output.sync_all()?;
    if let Ok(modified) = meta.modified() {
        let _ = output.set_modified(modified);
    }
    Ok(())
}

/// Where `file`'s data is, as `[start, end)` ranges in order. All of it where the system can't say.
#[cfg(unix)]
fn data_ranges(file: &File, len: u64) -> Vec<(u64, u64)> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    let mut ranges = Vec::new();
    let mut at = 0u64;
    while at < len {
        // SAFETY: lseek on an open descriptor this function borrows; positional reads don't use the offset.
        let start = unsafe { libc::lseek(fd, at as libc::off_t, libc::SEEK_DATA) };
        if start < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::ENXIO) {
                break; // only a hole remains
            }
            // The filesystem can't say (ENOTSUP on macOS's App Translocation mount): copy the rest.
            ranges.push((at, len));
            break;
        }
        // SAFETY: as above.
        let end = unsafe { libc::lseek(fd, start, libc::SEEK_HOLE) };
        let end = if end <= start { len } else { (end as u64).min(len) };
        ranges.push((start as u64, end));
        at = end;
    }
    ranges
}

#[cfg(not(unix))]
fn data_ranges(_file: &File, len: u64) -> Vec<(u64, u64)> {
    if len == 0 { Vec::new() } else { vec![(0, len)] }
}

/// NTFS writes zeros into a gap unless the file is marked sparse. Best effort: FAT and exFAT have no sparse files,
/// and the copy is still right without.
#[cfg(windows)]
fn mark_sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    let mut returned = 0u32;
    // SAFETY: a handle this function borrows, no input or output buffers.
    unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn mark_sparse(_file: &File) {}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(unix)]
fn write_all_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, offset)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            n => {
                buf = &buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

/// Free bytes on the drive that holds `path` (or its nearest existing parent).
pub fn free_bytes(path: &Path) -> Option<u64> {
    crate::host::free_bytes(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_copy_keeps_contents_and_holes() {
        let root = std::env::temp_dir().join(format!("lt-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(src.join("records/a/base")).unwrap();
        std::fs::write(src.join("selected"), b"a").unwrap();
        // 64 MiB long, two small runs of data.
        let mut chip = File::create(src.join("records/a/base/chip0")).unwrap();
        chip.set_len(64 << 20).unwrap();
        write_all_at(&chip, b"head", 0).unwrap();
        write_all_at(&chip, &[7u8; 5000], 40 << 20).unwrap();
        chip.flush().unwrap();
        drop(chip);
        std::fs::write(src.join("records/a/.lease"), b"").unwrap();
        let skip = |name: &std::ffi::OsStr| name == ".lease";
        let (mut seen, mut done) = (0, 0);
        copy_tree(&src, &root.join("dst"), &skip, &mut done, &mut |done| {
            seen = done;
            true
        })
        .unwrap();
        assert!(!root.join("dst/records/a/.lease").exists());
        let copied = std::fs::read(root.join("dst/records/a/base/chip0")).unwrap();
        assert_eq!(copied.len(), 64 << 20);
        assert_eq!(&copied[..4], b"head");
        assert!(copied[40 << 20..(40 << 20) + 5000].iter().all(|&b| b == 7));
        assert_eq!(
            copied.iter().map(|&b| b as u64).sum::<u64>(),
            b"head".iter().map(|&b| b as u64).sum::<u64>() + 7 * 5000
        );
        assert_eq!(std::fs::read(root.join("dst/selected")).unwrap(), b"a");
        #[cfg(target_os = "linux")]
        {
            // Holes stay holes (where the filesystem has them, as tmpfs and ext4 do).
            assert!(stored_bytes(&root.join("dst/records/a/base/chip0")) < 8 << 20);
            assert!(seen < 8 << 20, "only stored data is read: {seen}");
        }
        assert!(seen > 0);
        // A stop leaves a partial copy and says so.
        let stopped = copy_tree(&src, &root.join("dst2"), &skip, &mut 0, &mut |_| false).unwrap_err();
        assert_eq!(stopped.kind(), io::ErrorKind::Interrupted);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

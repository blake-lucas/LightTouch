//! Positional reads and writes on a shared `File` (pread/pwrite on Unix, overlapped seek_read/seek_write on
//! Windows), and sparse files where the filesystem has them.
use std::fs::File;
use std::io;
use std::path::Path;

pub fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_at(f, buf, offset)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::FileExt::seek_read(f, buf, offset)
    }
}

pub fn write_at(f: &File, buf: &[u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::write_at(f, buf, offset)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::FileExt::seek_write(f, buf, offset)
    }
}

pub fn read_exact_at(f: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match read_at(f, buf, offset) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read")),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn write_all_at(f: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match write_at(f, buf, offset) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "short write")),
            Ok(n) => {
                buf = &buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A new, empty read-write file that leaves unwritten ranges as holes (marked sparse on NTFS; Unix filesystems do
/// that by themselves).
pub fn create_sparse(path: &Path) -> io::Result<File> {
    let f = File::options().read(true).write(true).create(true).truncate(true).open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        const FSCTL_SET_SPARSE: u32 = 0x000900C4;
        unsafe extern "system" {
            fn DeviceIoControl(
                device: *mut std::ffi::c_void,
                code: u32,
                input: *const std::ffi::c_void,
                input_size: u32,
                output: *mut std::ffi::c_void,
                output_size: u32,
                returned: *mut u32,
                overlapped: *mut std::ffi::c_void,
            ) -> i32;
        }
        let mut returned = 0u32;
        // Best effort: FAT has no sparse files, and the file is still correct without.
        unsafe {
            DeviceIoControl(
                f.as_raw_handle() as _,
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
    }
    Ok(f)
}

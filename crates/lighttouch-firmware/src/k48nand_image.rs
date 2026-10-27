//! The record-level helpers of FirmwareKit's K48NANDImage that the K48 store writer and reader share: a chip
//! select's file name, XOR 0xFF inversion (the image store format, nand-xor-ff-v2), and the data extents of a
//! sparse file (which pages of a volume image were ever written).
//!
//! The legacy-to-image conversion and its proof (K48NANDImage.convert/verify/compact, the format 2 migration) are
//! boot admission, not preparation, and are not ported.
//!
//! ```ignore
//! let path = dir.join(k48nand_image::name(cs, &geo, "pages"));     // bus<b>-ce<c>.pages
//! k48nand_image::invert(&mut record);                               // as stored <-> as programmed
//! for (start, end) in k48nand_image::data_extents(&file)? { ... }   // byte ranges holding data
//! ```
use crate::k48nand::Geometry;
use std::fs::File;
use std::io;

/// One chip select's file name in a store or overlay.
pub fn name(i: usize, geo: &Geometry, ext: &str) -> String {
    format!("bus{}-ce{}.{ext}", i / geo.ce_per_bus, i % geo.ce_per_bus)
}

/// `buffer` XOR 0xFF, a word at a time (records are multiples of 8 bytes; a tail is done bytewise).
pub fn invert(buffer: &mut [u8]) {
    let (words, tail) = buffer.as_chunks_mut::<8>();
    for w in words {
        *w = (!u64::from_ne_bytes(*w)).to_ne_bytes();
    }
    for b in tail {
        *b ^= 0xFF;
    }
}

/// `count` bytes of `a` at `o` equal `b`'s at `p`.
pub fn same(a: &[u8], o: usize, b: &[u8], p: usize, count: usize) -> bool {
    a[o..o + count] == b[p..p + count]
}

/// The byte ranges `[start, end)` of `file` that hold data, in order (SEEK_DATA/SEEK_HOLE on Unix,
/// FSCTL_QUERY_ALLOCATED_RANGES on Windows). A filesystem without sparse files reports the whole file. These are
/// allocation units, so they may include zeros; the writer lays out every page in them, as the Swift tool does.
pub fn data_extents(file: &File) -> io::Result<Vec<(u64, u64)>> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(Vec::new());
    }
    extents(file, len)
}

#[cfg(unix)]
fn extents(file: &File, _len: u64) -> io::Result<Vec<(u64, u64)>> {
    use std::os::unix::io::AsRawFd;
    // Declared here rather than through the libc crate: two calls, and the crate stays dependency-light.
    // macOS swapped the two whence values relative to Linux and the BSDs.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const SEEK_DATA: i32 = 4;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const SEEK_HOLE: i32 = 3;
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    const SEEK_DATA: i32 = 3;
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    const SEEK_HOLE: i32 = 4;
    // lseek64 on Linux and Android: off_t is 32 bits on their 32-bit targets.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe extern "C" {
        #[link_name = "lseek64"]
        fn lseek(fd: i32, offset: i64, whence: i32) -> i64;
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    unsafe extern "C" {
        fn lseek(fd: i32, offset: i64, whence: i32) -> i64;
    }
    let fd = file.as_raw_fd();
    let mut out = Vec::new();
    let mut offset = 0i64;
    loop {
        // ENXIO past the last data is the loop's end; any other failure ends it too, as in FirmwareKit.
        let start = unsafe { lseek(fd, offset, SEEK_DATA) };
        if start < 0 {
            break;
        }
        let end = unsafe { lseek(fd, start, SEEK_HOLE) };
        if end <= start {
            break;
        }
        out.push((start as u64, end as u64));
        offset = end;
    }
    Ok(out)
}

#[cfg(windows)]
fn extents(file: &File, len: u64) -> io::Result<Vec<(u64, u64)>> {
    use std::os::windows::io::AsRawHandle;
    const FSCTL_QUERY_ALLOCATED_RANGES: u32 = 0x000940CF;
    const ERROR_MORE_DATA: i32 = 234;
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
    // FILE_ALLOCATED_RANGE_BUFFER: { FileOffset, Length }, both LARGE_INTEGER
    let mut out = Vec::new();
    let mut from = 0u64;
    let mut ranges = vec![[0i64; 2]; 1024];
    while from < len {
        let query = [from as i64, (len - from) as i64];
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle() as _,
                FSCTL_QUERY_ALLOCATED_RANGES,
                query.as_ptr() as _,
                16,
                ranges.as_mut_ptr() as _,
                (ranges.len() * 16) as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        let more = ok == 0 && io::Error::last_os_error().raw_os_error() == Some(ERROR_MORE_DATA);
        if ok == 0 && !more {
            // FAT and other filesystems without sparse files: every byte is data.
            return Ok(vec![(0, len)]);
        }
        let n = returned as usize / 16;
        for r in &ranges[..n] {
            out.push((r[0] as u64, (r[0] + r[1]) as u64));
        }
        match (more, out.last()) {
            (true, Some(&(_, end))) if end > from => from = end,
            _ => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invert_is_xor_ff_with_a_tail() {
        let mut b: Vec<u8> = (0..21).collect();
        invert(&mut b);
        assert!(b.iter().enumerate().all(|(i, &v)| v == !(i as u8)));
    }

    #[test]
    fn extents_cover_written_pages() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("k48img-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("sparse.img");
        let f = crate::posio::create_sparse(&path)?;
        f.set_len(64 << 20)?;
        crate::posio::write_all_at(&f, &[1u8; 4096], 32 << 20)?;
        let ext = data_extents(&f)?;
        std::fs::remove_dir_all(&dir)?;
        assert!(ext.iter().any(|&(s, e)| s <= 32 << 20 && e >= (32 << 20) + 4096), "{ext:?}");
        // a sparse-capable filesystem reports far less than the file
        let total: u64 = ext.iter().map(|(s, e)| e - s).sum();
        assert!(total <= 64 << 20);
        Ok(())
    }
}

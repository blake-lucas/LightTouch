//! Guest frames from the worker to the UI through named shared memory (POSIX `shm_open` on Linux/macOS, a
//! pagefile-backed mapping on Windows). The UI creates the segment for a session and the worker writes into it.
//! A generation counter acts as a seqlock: odd while the worker writes, even and unchanged around a valid read.
//! Latest frame wins; a slow reader just skips frames.
use std::io;
use std::sync::atomic::{AtomicU64, Ordering, fence};

const MAGIC: u32 = 0x4c54_4652; // "LTFR"
const VERSION: u32 = 1;
const HEADER: usize = 64;
/// Room for the largest supported screen (iPad, 1024x768) with margin.
pub const CAPACITY: usize = 16 << 20;

#[repr(C)]
struct Header {
    magic: u32,
    version: u32,
    generation: AtomicU64,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    sequence: u64,
    timestamp_ns: u64,
    published_ns: u64,
    capacity: u64,
}
const _: () = assert!(std::mem::size_of::<Header>() <= HEADER);

/// What the UI gets with each frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameMeta {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub sequence: u64,
    pub timestamp_ns: u64,
    /// Host monotonic time the worker finished copying it.
    pub published_ns: u64,
}

/// A unique segment name for one session.
pub fn segment_name() -> String {
    use std::sync::atomic::AtomicU32;
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let salt = crate::clock::monotonic_ns() as u32;
    // macOS limits POSIX shm names to 31 bytes.
    if cfg!(windows) {
        format!("Local\\lighttouch-{}-{n}-{salt:08x}", std::process::id())
    } else {
        format!("/lt-{}-{n}-{salt:08x}", std::process::id())
    }
}

/// The UI's side: owns the segment.
pub struct FrameReader {
    map: Mapping,
    name: String,
}

impl FrameReader {
    pub fn create(name: &str) -> io::Result<Self> {
        let map = Mapping::create(name, HEADER + CAPACITY)?;
        let header = map.header();
        unsafe {
            (*header).magic = MAGIC;
            (*header).version = VERSION;
            (*header).capacity = CAPACITY as u64;
        }
        Ok(Self { map, name: name.to_string() })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Copy the newest frame into `pixels` (BGRA, `stride` bytes per row) if it is newer than `last_sequence`.
    pub fn read(&self, last_sequence: u64, pixels: &mut Vec<u8>) -> Option<FrameMeta> {
        let header = self.map.header();
        for _ in 0..4 {
            let generation = unsafe { &(*header).generation }.load(Ordering::Acquire);
            if generation & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let meta = unsafe {
                FrameMeta {
                    width: std::ptr::read_volatile(&(*header).width),
                    height: std::ptr::read_volatile(&(*header).height),
                    stride: std::ptr::read_volatile(&(*header).stride),
                    sequence: std::ptr::read_volatile(&(*header).sequence),
                    timestamp_ns: std::ptr::read_volatile(&(*header).timestamp_ns),
                    published_ns: std::ptr::read_volatile(&(*header).published_ns),
                }
            };
            if meta.sequence == 0 || meta.sequence == last_sequence {
                return None;
            }
            let bytes = meta.stride as usize * meta.height as usize;
            if bytes == 0 || bytes > CAPACITY {
                return None;
            }
            pixels.resize(bytes, 0);
            unsafe { std::ptr::copy_nonoverlapping(self.map.pixels(), pixels.as_mut_ptr(), bytes) };
            fence(Ordering::Acquire);
            if unsafe { &(*header).generation }.load(Ordering::Relaxed) == generation {
                return Some(meta);
            }
        }
        None
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        Mapping::unlink(&self.name);
    }
}

/// The worker's side.
pub struct FrameWriter {
    map: Mapping,
}

// The writer is used from one thread at a time (the worker's frame thread).
unsafe impl Send for FrameWriter {}

impl FrameWriter {
    pub fn open(name: &str) -> io::Result<Self> {
        let map = Mapping::open(name, HEADER + CAPACITY)?;
        let header = map.header();
        let ok = unsafe { (*header).magic == MAGIC && (*header).version == VERSION };
        if !ok {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame segment has the wrong format"));
        }
        Ok(Self { map })
    }

    /// Fill the pixel area with `write`, which returns the frame's metadata, or `None` if it wrote nothing new.
    pub fn publish(&mut self, write: impl FnOnce(&mut [u8]) -> Option<FrameMeta>) {
        let header = self.map.header();
        let generation = unsafe { &(*header).generation };
        generation.fetch_add(1, Ordering::AcqRel);
        fence(Ordering::Release);
        let pixels = unsafe { std::slice::from_raw_parts_mut(self.map.pixels(), CAPACITY) };
        if let Some(meta) = write(pixels) {
            unsafe {
                std::ptr::write_volatile(&mut (*header).width, meta.width);
                std::ptr::write_volatile(&mut (*header).height, meta.height);
                std::ptr::write_volatile(&mut (*header).stride, meta.stride);
                std::ptr::write_volatile(&mut (*header).format, 1);
                std::ptr::write_volatile(&mut (*header).timestamp_ns, meta.timestamp_ns);
                std::ptr::write_volatile(&mut (*header).published_ns, crate::clock::monotonic_ns());
                std::ptr::write_volatile(&mut (*header).sequence, meta.sequence);
            }
        }
        generation.fetch_add(1, Ordering::Release);
    }
}

// --- platform mappings -------------------------------------------------------------------------------------------

struct Mapping {
    base: *mut u8,
    len: usize,
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    fn header(&self) -> *mut Header {
        self.base as *mut Header
    }

    fn pixels(&self) -> *mut u8 {
        unsafe { self.base.add(HEADER) }
    }
}

#[cfg(unix)]
impl Mapping {
    fn create(name: &str, len: usize) -> io::Result<Self> {
        Self::map(name, len, true)
    }

    fn open(name: &str, len: usize) -> io::Result<Self> {
        Self::map(name, len, false)
    }

    fn map(name: &str, len: usize, create: bool) -> io::Result<Self> {
        let cname = std::ffi::CString::new(name).map_err(|_| io::ErrorKind::InvalidInput)?;
        let flags = if create { libc::O_CREAT | libc::O_EXCL | libc::O_RDWR } else { libc::O_RDWR };
        // macOS declares shm_open variadic, so its mode goes as an unsigned int there.
        #[cfg(target_vendor = "apple")]
        let fd = unsafe { libc::shm_open(cname.as_ptr(), flags, 0o600 as libc::c_uint) };
        #[cfg(not(target_vendor = "apple"))]
        let fd = unsafe { libc::shm_open(cname.as_ptr(), flags, 0o600 as libc::mode_t) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            if create && unsafe { libc::ftruncate(fd, len as libc::off_t) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let base = unsafe {
                libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0)
            };
            if base == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { base: base as *mut u8, len })
        })();
        unsafe { libc::close(fd) };
        if result.is_err() && create {
            Self::unlink(name);
        }
        result
    }

    fn unlink(name: &str) {
        if let Ok(cname) = std::ffi::CString::new(name) {
            unsafe { libc::shm_unlink(cname.as_ptr()) };
        }
    }
}

#[cfg(unix)]
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.base as *mut libc::c_void, self.len) };
    }
}

#[cfg(windows)]
impl Mapping {
    fn wide(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn create(name: &str, len: usize) -> io::Result<Self> {
        use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, PAGE_READWRITE};
        let wide = Self::wide(name);
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                (len as u64 >> 32) as u32,
                len as u32,
                wide.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        Self::view(handle, len)
    }

    fn open(name: &str, len: usize) -> io::Result<Self> {
        use windows_sys::Win32::System::Memory::{FILE_MAP_ALL_ACCESS, OpenFileMappingW};
        let wide = Self::wide(name);
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, wide.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Self::view(handle, len)
    }

    fn view(handle: windows_sys::Win32::Foundation::HANDLE, len: usize) -> io::Result<Self> {
        use windows_sys::Win32::System::Memory::{FILE_MAP_ALL_ACCESS, MapViewOfFile};
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, len) };
        if view.Value.is_null() {
            let error = io::Error::last_os_error();
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(error);
        }
        Ok(Self { base: view.Value as *mut u8, len, handle })
    }

    fn unlink(_name: &str) {
        // A pagefile-backed mapping disappears with its last handle.
    }
}

#[cfg(windows)]
impl Drop for Mapping {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.base as *mut _ });
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
        let _ = self.len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_cross_the_segment() {
        let name = segment_name();
        let reader = FrameReader::create(&name).unwrap();
        let mut writer = FrameWriter::open(&name).unwrap();
        let mut pixels = Vec::new();
        assert!(reader.read(0, &mut pixels).is_none());
        writer.publish(|buffer| {
            buffer[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
            Some(FrameMeta { width: 2, height: 1, stride: 8, sequence: 7, timestamp_ns: 5, published_ns: 0 })
        });
        let meta = reader.read(0, &mut pixels).unwrap();
        assert_eq!((meta.width, meta.height, meta.sequence), (2, 1, 7));
        assert_eq!(pixels, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(reader.read(7, &mut pixels).is_none());
    }
}

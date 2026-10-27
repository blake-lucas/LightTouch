//! The device library: saved devices, each with its own immutable base, writable state and assets
//! (`lt_library_*`). The same on-disk format Android uses.
use crate::ffi::{Error, Result, chars, check, cpath, cstr_lossy, cstring, opt_cpath, ptr};
use lighttouch_sys as sys;
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};

/// One saved device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub board: String,
    pub build: String,
    pub base_sha256: String,
    pub base_path: PathBuf,
    pub state_path: PathBuf,
    pub bootrom_path: Option<PathBuf>,
    pub guest_package_path: Option<PathBuf>,
    pub selected: bool,
    pub dirty: bool,
}

impl Record {
    fn from_info(info: &sys::lt_record_info) -> Self {
        let path = |value: &[c_char]| {
            let text = chars(value);
            if text.is_empty() { None } else { Some(PathBuf::from(text)) }
        };
        Self {
            id: chars(&info.id),
            name: chars(&info.display_name),
            board: chars(&info.board),
            build: chars(&info.build),
            base_sha256: chars(&info.base_sha256),
            base_path: PathBuf::from(chars(&info.base_path)),
            state_path: PathBuf::from(chars(&info.state_path)),
            bootrom_path: path(&info.bootrom_path).filter(|p| p.is_file()),
            guest_package_path: path(&info.guest_package_path).filter(|p| p.is_file()),
            selected: info.selected != 0,
            dirty: info.dirty != 0,
        }
    }
}

/// Progress of a long library operation: a phase name ("hashing", "extracting", "verifying", ...) with bytes done
/// and total for that phase. Return `false` to cancel.
pub type PhaseFn<'a> = dyn FnMut(&str, u64, u64) -> bool + 'a;

/// A preparer's output to add as a new record.
#[derive(Debug, Clone, Default)]
pub struct ImportSource {
    /// The prepared folder, with its `device.lock.json`.
    pub path: PathBuf,
    pub name: Option<String>,
    pub bootrom: Option<PathBuf>,
    pub guest_package: Option<PathBuf>,
}

pub struct Library {
    raw: *mut sys::lt_library,
    root: PathBuf,
}

// The core serializes library mutations itself (flock); a handle may move between threads.
unsafe impl Send for Library {}

impl Library {
    /// Open (creating if needed) the library at `root`.
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root).map_err(|e| Error::new(sys::LT_IO, format!("{}: {e}", root.display())))?;
        let path = cpath(root)?;
        let mut config = sys::lt_library_config::new();
        config.root_path = path.as_ptr();
        let mut raw = std::ptr::null_mut();
        let mut error = sys::lt_error::new();
        check(unsafe { sys::lt_library_open(&config, &mut raw, &mut error) }, &error)?;
        Ok(Self { raw, root: root.to_path_buf() })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn records(&self) -> Result<Vec<Record>> {
        let mut count = 0u32;
        let mut error = sys::lt_error::new();
        check(unsafe { sys::lt_library_count(self.raw, &mut count, &mut error) }, &error)?;
        let mut records = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut info = Box::new(sys::lt_record_info::new());
            check(unsafe { sys::lt_library_record_at(self.raw, index, &mut *info, &mut error) }, &error)?;
            records.push(Record::from_info(&info));
        }
        Ok(records)
    }

    pub fn record(&self, id: &str) -> Result<Record> {
        self.records()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| Error::invalid(format!("no saved device with id {id}")))
    }

    pub fn select(&self, id: &str) -> Result<()> {
        let id = cstring(id)?;
        let mut error = sys::lt_error::new();
        check(unsafe { sys::lt_library_select(self.raw, id.as_ptr(), &mut error) }, &error)
    }

    pub fn rename(&self, id: &str, name: &str) -> Result<()> {
        let (id, name) = (cstring(id)?, cstring(name)?);
        let mut error = sys::lt_error::new();
        check(unsafe { sys::lt_library_rename(self.raw, id.as_ptr(), name.as_ptr(), &mut error) }, &error)
    }

    /// Permanently delete a stopped record (`LT_BUSY` while it runs).
    pub fn remove(&self, id: &str) -> Result<()> {
        let id = cstring(id)?;
        let mut error = sys::lt_error::new();
        check(unsafe { sys::lt_library_remove(self.raw, id.as_ptr(), &mut error) }, &error)
    }

    /// Replace a stopped record's guest tools; the guest installs them at its next start.
    pub fn set_guest_package(&self, id: &str, package: &Path) -> Result<Record> {
        let (id, package) = (cstring(id)?, cpath(package)?);
        let mut info = Box::new(sys::lt_record_info::new());
        let mut error = sys::lt_error::new();
        check(
            unsafe {
                sys::lt_library_set_guest_package(self.raw, id.as_ptr(), package.as_ptr(), &mut *info, &mut error)
            },
            &error,
        )?;
        Ok(Record::from_info(&info))
    }

    /// Move a preparer's output folder (on the library's own filesystem) in as a new record without copying it.
    /// On failure the folder is handed back where it was.
    pub fn adopt_prepared(&self, source: &ImportSource, progress: &mut PhaseFn<'_>) -> Result<Record> {
        let path = cpath(&source.path)?;
        let name = source.name.as_deref().map(cstring).transpose()?;
        let rom = opt_cpath(source.bootrom.as_deref())?;
        let package = opt_cpath(source.guest_package.as_deref())?;
        let mut config = sys::lt_library_import_config::new();
        config.source_path = path.as_ptr();
        config.display_name = ptr(&name);
        config.bootrom_path = ptr(&rom);
        config.guest_package_path = ptr(&package);
        let mut info = Box::new(sys::lt_record_info::new());
        let mut error = sys::lt_error::new();
        let mut observer: &mut PhaseFn<'_> = progress;
        let opaque = &mut observer as *mut &mut PhaseFn<'_> as *mut c_void;
        let status = unsafe {
            sys::lt_library_adopt_prepared(self.raw, &config, Some(phase_trampoline), opaque, &mut *info, &mut error)
        };
        check(status, &error)?;
        Ok(Record::from_info(&info))
    }

    pub(crate) fn raw(&self) -> *mut sys::lt_library {
        self.raw
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        unsafe { sys::lt_library_close(self.raw) }
    }
}

unsafe extern "C" fn phase_trampoline(opaque: *mut c_void, phase: *const c_char, done: u64, total: u64) -> c_int {
    let observer = unsafe { &mut *(opaque as *mut &mut PhaseFn<'_>) };
    let phase = cstr_lossy(phase);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(&phase, done, total))) {
        Ok(true) => 0,
        _ => 1,
    }
}

//! Small helpers shared by the safe wrappers: errors, C strings and fixed-size char arrays.
use lighttouch_sys as sys;
use std::ffi::{CStr, CString, c_char};
use std::fmt;
use std::path::Path;

/// A failed core call: the `lt_status` code and the core's own message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub status: sys::lt_status,
    pub message: String,
}

impl Error {
    pub fn new(status: sys::lt_status, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(sys::LT_INVALID, message)
    }
    pub fn is(&self, status: sys::lt_status) -> bool {
        self.status == status
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() { write!(f, "{}", status_name(self.status)) } else { f.write_str(&self.message) }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub fn status_name(status: sys::lt_status) -> &'static str {
    match status {
        sys::LT_OK => "ok",
        sys::LT_INVALID => "invalid request",
        sys::LT_IO => "storage error",
        sys::LT_BUSY => "busy",
        sys::LT_UNSUPPORTED => "not supported",
        sys::LT_ENGINE => "engine error",
        sys::LT_DIRTY => "the device was not shut down cleanly",
        sys::LT_CANCELLED => "cancelled",
        sys::LT_AGAIN => "try again",
        _ => "unknown error",
    }
}

/// Turn a status and the error the core filled in into a `Result`.
pub(crate) fn check(status: sys::lt_status, error: &sys::lt_error) -> Result<()> {
    if status == sys::LT_OK { Ok(()) } else { Err(Error::new(status, chars(&error.message))) }
}

pub(crate) fn chars(value: &[c_char]) -> String {
    let bytes: Vec<u8> = value.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

pub(crate) fn cstring(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| Error::invalid("text contains a NUL byte"))
}

pub(crate) fn cpath(path: &Path) -> Result<CString> {
    let text = path.to_str().ok_or_else(|| Error::invalid(format!("path is not UTF-8: {}", path.display())))?;
    cstring(text)
}

pub(crate) fn opt_cpath(path: Option<&Path>) -> Result<Option<CString>> {
    path.map(cpath).transpose()
}

pub(crate) fn ptr(value: &Option<CString>) -> *const c_char {
    value.as_ref().map_or(std::ptr::null(), |v| v.as_ptr())
}

pub(crate) fn cstr_lossy(value: *const c_char) -> String {
    if value.is_null() { String::new() } else { unsafe { CStr::from_ptr(value) }.to_string_lossy().into_owned() }
}

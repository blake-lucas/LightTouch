//! The one error every module returns. `code` is the preparer contract's (the `code` of firmwarekit's error event).
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    KeyMissing,
    ShaMismatch,
    Unsupported,
    ActivationFailed,
    OneshotFailed,
    DiskFull,
    Internal,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::KeyMissing => "key_missing",
            Code::ShaMismatch => "sha_mismatch",
            Code::Unsupported => "unsupported",
            Code::ActivationFailed => "activation_failed",
            Code::OneshotFailed => "oneshot_failed",
            Code::DiskFull => "disk_full",
            Code::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Error {
    pub code: Code,
    pub message: String,
    /// The fit check's piece when a required one doesn't fit: the app names it plainly.
    pub piece: Option<String>,
}

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), piece: None }
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(Code::Unsupported, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(Code::Internal, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        let code = if e.raw_os_error() == Some(28) { Code::DiskFull } else { Code::Internal };
        Error::new(code, e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `Err(Error::unsupported(...))` with format arguments.
#[macro_export]
macro_rules! unsupported {
    ($($arg:tt)*) => { $crate::error::Error::unsupported(format!($($arg)*)) };
}

/// `Err(Error::internal(...))` with format arguments.
#[macro_export]
macro_rules! internal {
    ($($arg:tt)*) => { $crate::error::Error::internal(format!($($arg)*)) };
}

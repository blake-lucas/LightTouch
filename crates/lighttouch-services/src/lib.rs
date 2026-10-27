//! Guest device services over the emulated USB cable.
//!
//! The engine dials the QEMU-only usbmuxd (`bridge`), which serves usbmux clients on a private socket. Over a
//! usbmux connection (`usbmux`) this crate speaks lockdown (`lockdown`, with the TLS session old iOS needs), and the
//! services lockdown starts: AFC (`afc`) and the installation proxy (`instproxy`).
pub mod afc;
pub mod apps;
pub mod bridge;
pub mod instproxy;
pub mod lockdown;
mod pairing;
pub mod usbmux;

use std::fmt;
use std::io::{Read, Write};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}

impl From<plist::Error> for Error {
    fn from(e: plist::Error) -> Self {
        Self(format!("unreadable reply ({e})"))
    }
}

impl From<openssl::error::ErrorStack> for Error {
    fn from(e: openssl::error::ErrorStack) -> Self {
        Self(format!("TLS: {e}"))
    }
}

impl From<String> for Error {
    fn from(e: String) -> Self {
        Self(e)
    }
}

impl From<&str> for Error {
    fn from(e: &str) -> Self {
        Self(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A byte stream to the guest: a usbmux connection, possibly wrapped in TLS.
pub trait Io: Read + Write + Send {}
impl<T: Read + Write + Send> Io for T {}

/// Read exactly `len` bytes, or fail if the stream ends first.
fn read_exact_vec(io: &mut dyn Read, len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf)?;
    Ok(buf)
}

/// A string field of a plist dictionary.
fn string(dict: &plist::Dictionary, key: &str) -> Option<String> {
    dict.get(key).and_then(|v| v.as_string()).map(str::to_string)
}

/// The XML form of a plist value.
fn to_xml(value: &plist::Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    value.to_writer_xml(&mut out)?;
    Ok(out)
}

/// Parse XML or binary plist bytes into a dictionary.
fn dictionary(bytes: &[u8]) -> Result<plist::Dictionary> {
    plist::Value::from_reader(std::io::Cursor::new(bytes))?
        .into_dictionary()
        .ok_or_else(|| Error("the guest replied with something other than a dictionary".into()))
}

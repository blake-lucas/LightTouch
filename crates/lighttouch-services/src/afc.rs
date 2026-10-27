//! AFC, the device's file service (`com.apple.afc` for the media area, `com.apple.crashreportcopymobile` for crash
//! reports). Each packet is a 40-byte little-endian header ("CFA6LPAA", total length, header length, number,
//! operation), the operation's arguments, then any payload.
use crate::lockdown::Conn;
use crate::{Error, Result, read_exact_vec};
use std::io::Write;

const MAGIC: &[u8; 8] = b"CFA6LPAA";
const HEADER: usize = 40;
const REPLY_LIMIT: usize = 64 << 20;

const OP_STATUS: u64 = 0x01;
const OP_DATA: u64 = 0x02;
const OP_READ_DIR: u64 = 0x03;
const OP_REMOVE_PATH: u64 = 0x08;
const OP_MAKE_DIR: u64 = 0x09;
const OP_GET_DEVINFO: u64 = 0x0B;
const OP_FILE_OPEN: u64 = 0x0D;
const OP_FILE_OPEN_RES: u64 = 0x0E;
const OP_FILE_READ: u64 = 0x0F;
const OP_FILE_WRITE: u64 = 0x10;
const OP_FILE_CLOSE: u64 = 0x14;

/// `FileRefOpen` modes.
#[derive(Debug, Clone, Copy)]
#[repr(u64)]
pub enum Mode {
    ReadOnly = 1,
    /// Create or truncate, write only.
    WriteOnly = 3,
}

pub struct Afc {
    conn: Conn,
    packet: u64,
}

struct Reply {
    operation: u64,
    args: Vec<u8>,
    payload: Vec<u8>,
}

fn status_text(code: u64) -> &'static str {
    match code {
        7 => "invalid argument",
        8 => "no such file",
        9 => "is a directory",
        10 => "permission denied",
        15 => "not enough space on the guest",
        16 => "the guest's file service is busy",
        19 => "the file already exists",
        _ => "the guest's file service reported an error",
    }
}

impl Afc {
    pub fn new(conn: Conn) -> Self {
        Self { conn, packet: 0 }
    }

    fn send(&mut self, operation: u64, args: &[u8], payload: &[u8]) -> Result<()> {
        let this_length = (HEADER + args.len()) as u64;
        let mut packet = Vec::with_capacity(HEADER + args.len() + payload.len());
        packet.extend_from_slice(MAGIC);
        packet.extend_from_slice(&(this_length + payload.len() as u64).to_le_bytes());
        packet.extend_from_slice(&this_length.to_le_bytes());
        packet.extend_from_slice(&self.packet.to_le_bytes());
        packet.extend_from_slice(&operation.to_le_bytes());
        packet.extend_from_slice(args);
        packet.extend_from_slice(payload);
        self.packet += 1;
        self.conn.write_all(&packet)?;
        self.conn.flush()?;
        Ok(())
    }

    fn receive(&mut self) -> Result<Reply> {
        let head = read_exact_vec(&mut self.conn, HEADER)?;
        if &head[0..8] != MAGIC {
            return Err(Error("the guest's file service sent an invalid reply".into()));
        }
        let word = |i: usize| u64::from_le_bytes(head[i..i + 8].try_into().unwrap()) as usize;
        let (entire, this) = (word(8), word(16));
        if entire < HEADER || this < HEADER || this > entire || entire > REPLY_LIMIT {
            return Err(Error("the guest's file service sent an invalid reply".into()));
        }
        let args = read_exact_vec(&mut self.conn, this - HEADER)?;
        let payload = read_exact_vec(&mut self.conn, entire - this)?;
        let reply = Reply { operation: word(32) as u64, args, payload };
        if reply.operation == OP_STATUS {
            let code = reply.args.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
            if code != 0 {
                return Err(Error(format!("{} ({code})", status_text(code))));
            }
        }
        Ok(reply)
    }

    fn call(&mut self, operation: u64, args: &[u8], payload: &[u8]) -> Result<Reply> {
        self.send(operation, args, payload)?;
        self.receive()
    }

    fn path_arg(path: &str) -> Vec<u8> {
        let mut arg = path.as_bytes().to_vec();
        arg.push(0);
        arg
    }

    /// NUL-separated strings of a data reply.
    fn strings(reply: &Reply) -> Vec<String> {
        let data = if reply.payload.is_empty() { &reply.args } else { &reply.payload };
        data.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
    }

    pub fn read_dir(&mut self, path: &str) -> Result<Vec<String>> {
        let reply = self.call(OP_READ_DIR, &Self::path_arg(path), &[])?;
        Ok(Self::strings(&reply).into_iter().filter(|n| n != "." && n != "..").collect())
    }

    /// A device-info value such as `FSFreeBytes`.
    pub fn device_info(&mut self, key: &str) -> Result<Option<String>> {
        let reply = self.call(OP_GET_DEVINFO, &[], &[])?;
        let list = Self::strings(&reply);
        Ok(list.chunks(2).find(|pair| pair[0] == key).and_then(|pair| pair.get(1).cloned()))
    }

    pub fn free_bytes(&mut self) -> Result<u64> {
        self.device_info("FSFreeBytes")?
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| Error("the guest didn't report its free space".into()))
    }

    /// Create a directory; an existing one is fine.
    pub fn make_dir(&mut self, path: &str) -> Result<()> {
        match self.call(OP_MAKE_DIR, &Self::path_arg(path), &[]) {
            Ok(_) => Ok(()),
            Err(e) if e.0.contains("(19)") => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn remove(&mut self, path: &str) -> Result<()> {
        self.call(OP_REMOVE_PATH, &Self::path_arg(path), &[]).map(|_| ())
    }

    pub fn open(&mut self, path: &str, mode: Mode) -> Result<u64> {
        let mut args = (mode as u64).to_le_bytes().to_vec();
        args.extend_from_slice(&Self::path_arg(path));
        let reply = self.call(OP_FILE_OPEN, &args, &[])?;
        if reply.operation != OP_FILE_OPEN_RES || reply.args.len() < 8 {
            return Err(Error("the guest didn't open the file".into()));
        }
        Ok(u64::from_le_bytes(reply.args[..8].try_into().unwrap()))
    }

    pub fn write(&mut self, handle: u64, data: &[u8]) -> Result<()> {
        self.call(OP_FILE_WRITE, &handle.to_le_bytes(), data).map(|_| ())
    }

    /// Up to `length` bytes; empty at the end of the file.
    pub fn read(&mut self, handle: u64, length: u64) -> Result<Vec<u8>> {
        let mut args = handle.to_le_bytes().to_vec();
        args.extend_from_slice(&length.to_le_bytes());
        let reply = self.call(OP_FILE_READ, &args, &[])?;
        if reply.operation != OP_DATA {
            return Ok(Vec::new());
        }
        Ok(if reply.payload.is_empty() { reply.args } else { reply.payload })
    }

    pub fn close(&mut self, handle: u64) -> Result<()> {
        self.call(OP_FILE_CLOSE, &handle.to_le_bytes(), &[]).map(|_| ())
    }
}

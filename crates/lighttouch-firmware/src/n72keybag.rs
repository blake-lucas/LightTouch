//! The iPod touch 2G's 4.x data-protection one-shot (qemu-ios imgtools/ipod2g_keybag.py; Swift's N72Keybag).
//! Effaceable storage (in NOR, through nor-rw) and the system keybag (on disk0s1, in the NAND) are made the way a
//! restore makes them: the IPSW's own Update ramdisk with it_keybag as restored_external, booted as md0.
//!
//! Everything iBoot loads is a signed img3, so the ramdisk cannot come through iBoot. iBoot boots the device
//! normally, and at the kernel's entry (LC_UNIXTHREAD pc, MMU off) this stages what iBoot's restore path would
//! have, over the emulator's gdbstub: the ramdisk at topOfKernelData, a chosen/memory-map RAMDisk (pa, len) in a spare
//! MemoryMapReserved slot, an empty chosen/root-matching, and topOfKernelData past the ramdisk and the one-shot
//! command line. The 4.x DeviceTree's secure-root-prefix "md" makes md0 a SecureRoot. The guest writes nand/ in place
//! and the written NOR replaces nor.bin. The boot uses the recipe's aes-uid, as the device's own boots do (the
//! keybag's keys are UID-derived, and iBoot unwraps the NOR's SHSH with a UID-derived key).
use crate::boot::{self, Board as Hardware};
use crate::devicetree::{DeviceTree, Value};
use crate::error::{Code, Error, Result};
use crate::k48_recipe::{ramdisk_with_helper, KEYBAG_DONE};
use crate::oneshot::{self, Helper};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The command line the host stages with the ramdisk at the paused kernel handoff.
pub const BOOT_ARGS: &str = "rd=md0 serial=3 debug=0x8 -v amfi_allow_any_signature=1 cs_enforcement_disable=1";
pub const PHYS_BASE: u32 = 0x0800_0000;
const CMDLINE_OFFSET: usize = 0x38;
const CMDLINE_LENGTH: usize = 256;

fn failed(message: impl Into<String>) -> Error {
    Error { code: Code::OneshotFailed, message: message.into(), piece: None }
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| crate::internal!("{}: {e}", path.display()))
}

/// `out` holds nand/, nor.bin, iBoot.bin and gid-blobs.bin (writable); `dec` the decrypt cache (the kernelcache);
/// `ramdisk` the decrypted restore ramdisk to boot. `machine` the recipe's -machine options (aes-uid ...).
#[allow(clippy::too_many_arguments)]
pub fn run(
    out: &Path,
    dec: &Path,
    ramdisk: &Path,
    it_keybag: &Path,
    bootrom: &Path,
    helper: &Helper,
    machine: &BTreeMap<String, String>,
    work: &Path,
    log: &dyn Fn(&str),
) -> Result<()> {
    let rd = ramdisk_with_helper(ramdisk, it_keybag, work)?;
    let nor = work.join("nor.rw");
    let serial = work.join("keybag.log");
    std::fs::copy(out.join("nor.bin"), &nor)?;
    let port = free_port()?;
    let hardware = helper.hardware(Hardware::N72)?;
    let file = |n: &str| boot::escape(&out.join(n).to_string_lossy());
    let m = format!(
        "{},bootrom={},nand={},nor={},nor-rw={},direct-iboot={},gid-blobs={},boot-args={}",
        hardware.machine,
        boot::escape(&bootrom.to_string_lossy()),
        file("nand"),
        file("nor.bin"),
        boot::escape(&nor.to_string_lossy()),
        file("iBoot.bin"),
        file("gid-blobs.bin"),
        boot::options(machine),
    );
    let argv: Vec<String> = [
        "LightTouchDevice",
        "-M",
        &m,
        "-m",
        "128M",
        "-display",
        "none",
        "-audio",
        "driver=none",
        "-monitor",
        "none",
        "-serial",
        &format!("file:{}", serial.display()),
        "-gdb",
        &format!("tcp:127.0.0.1:{port}"),
        "-S",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let kernelcache = read(&dec.join("kernelcache.mach"))?;
    let image = read(&rd)?;
    let note = Arc::new(Mutex::new(String::new()));
    let during = {
        let note = note.clone();
        Box::new(move || -> Result<()> {
            let text = handoff(&mut GdbRemote::connect(port)?, &kernelcache, &image)?;
            *note.lock().unwrap_or_else(|e| e.into_inner()) = text;
            Ok(())
        })
    };
    let (r, text) = oneshot::run(helper, &argv, &serial, Some("panic("), None, 300.0, log, Some(during))?;
    log(&note.lock().unwrap_or_else(|e| e.into_inner()));
    for line in text.lines().filter(|l| l.contains("it_keybag:")) {
        log(line.trim());
    }
    if !text.contains(KEYBAG_DONE) {
        let why = text
            .lines()
            .find(|l| l.contains("panic("))
            .map(|l| l.chars().take(160).collect::<String>())
            .unwrap_or_else(|| if r.exited { "halted without the keybag" } else { "no halt" }.to_string());
        return Err(failed(format!("keybag boot: {why} after {} s", r.seconds as i64)));
    }
    let written = read(&nor)?;
    let stock = out.join("nor.bin");
    if written == read(&stock)? {
        return Err(failed("keybag boot: NOR unchanged, effaceable was not written"));
    }
    std::fs::write(&stock, written)?;
    for p in [&rd, &nor, &serial] {
        let _ = std::fs::remove_file(p);
    }
    log("keybag boot: effaceable in nor.bin, the system keybag in nand/");
    Ok(())
}

/// (entry pc, link base): LC_UNIXTHREAD's pc and __TEXT's vmaddr top nibble.
pub fn kernel_entry(b: &[u8]) -> Result<(u32, u32)> {
    let u32_at = |o: usize| -> Option<u32> { b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])) };
    let none = || crate::unsupported!("no LC_UNIXTHREAD / __TEXT in the kernelcache");
    let mut off = 28;
    let (mut pc, mut base) = (None, None);
    for _ in 0..u32_at(16).ok_or_else(none)? {
        let (cmd, size) = (u32_at(off).ok_or_else(none)?, u32_at(off + 4).ok_or_else(none)? as usize);
        if cmd == 5 {
            pc = u32_at(off + 16 + 15 * 4);
        }
        if cmd == 1 {
            let name = b.get(off + 8..off + 24).ok_or_else(none)?;
            let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(16)];
            if name == b"__TEXT" {
                base = u32_at(off + 24).map(|v| v & 0xF000_0000);
            }
        }
        if size < 8 {
            break;
        }
        off += size;
    }
    match (pc, base) {
        (Some(pc), Some(base)) => Ok((pc, base)),
        _ => Err(none()),
    }
}

/// ipod2g_keybag.add_ramdisk: a RAMDisk memory-map entry in the first spare reserved slot, no root-matching.
pub fn add_ramdisk(blob: &[u8], pa: u32, length: u32) -> Result<Vec<u8>> {
    let mut dt = DeviceTree::new(blob)?;
    let spare = dt
        .prop_names("chosen/memory-map")
        .into_iter()
        .find(|k| k.starts_with("MemoryMapReserved-"))
        .ok_or_else(|| crate::unsupported!("no spare chosen/memory-map slot"))?;
    dt.rename("chosen/memory-map", &spare, "RAMDisk")?;
    dt.set("chosen/memory-map", "RAMDisk", Value::Words(vec![pa, length]))?;
    dt.set("chosen", "root-matching", Value::Bytes(Vec::new()))?;
    if dt.data().len() != blob.len() {
        return Err(crate::internal!("DeviceTree changed size"));
    }
    Ok(dt.into_data())
}

/// Stop at the kernel's entry and give it the ramdisk the way iBoot's restore path does.
pub fn handoff(gdb: &mut GdbRemote, kernelcache: &[u8], ramdisk: &[u8]) -> Result<String> {
    let (pc, base) = kernel_entry(kernelcache)?;
    let entry = pc.wrapping_sub(base).wrapping_add(PHYS_BASE);
    if gdb.command(&format!("Z0,{entry:x},4"))? != "OK" {
        return Err(failed("gdbstub refused the breakpoint"));
    }
    let stop = gdb.command("c")?;
    let regs = gdb.registers()?;
    if !stop.starts_with('T') || regs[15] != entry {
        return Err(failed(format!("keybag boot did not stop at the kernel entry: {stop} pc={:x}", regs[15])));
    }
    gdb.command(&format!("z0,{entry:x},4"))?;
    let ba = regs[0];
    let mut args = gdb.read(ba, 0x138)?;
    let u32_at = |a: &[u8], o: usize| u32::from_le_bytes([a[o], a[o + 1], a[o + 2], a[o + 3]]);
    let rev = u16::from_le_bytes([args[0], args[1]]);
    let (virt, phys, mem, top) = (u32_at(&args, 4), u32_at(&args, 8), u32_at(&args, 12), u32_at(&args, 16));
    let (dtp, dtlen) = (u32_at(&args, 0x30), u32_at(&args, 0x34));
    if rev != 1 || phys != PHYS_BASE || virt != base {
        return Err(failed(format!("unexpected boot_args at 0x{ba:x}")));
    }
    let rd_pa = (top + 0xFFF) & !0xFFF;
    let new_top = (rd_pa + ramdisk.len() as u32 + 0x3FFF) & !0x3FFF;
    if new_top >= phys + mem {
        return Err(failed("the ramdisk does not fit below memSize"));
    }
    let dt_pa = dtp - virt + phys;
    let dt = gdb.read(dt_pa, dtlen as usize)?;
    gdb.write(dt_pa, &add_ramdisk(&dt, rd_pa, ramdisk.len() as u32)?)?;
    gdb.write(rd_pa, ramdisk)?;
    let mut command = BOOT_ARGS.as_bytes().to_vec();
    if command.len() >= CMDLINE_LENGTH {
        return Err(crate::internal!("keybag command line exceeds boot_args capacity"));
    }
    command.resize(CMDLINE_LENGTH, 0);
    args[CMDLINE_OFFSET..CMDLINE_OFFSET + CMDLINE_LENGTH].copy_from_slice(&command);
    args[0x10..0x14].copy_from_slice(&new_top.to_le_bytes());
    gdb.write(ba, &args)?;
    gdb.send("c")?;
    Ok(format!(
        "keybag boot: ramdisk {} bytes at 0x{rd_pa:08x}, topOfKernelData 0x{top:08x} -> 0x{new_top:08x}, [{BOOT_ARGS}]",
        ramdisk.len()
    ))
}

/// A loopback port nothing listens on now.
pub fn free_port() -> Result<u16> {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| crate::internal!("no free TCP port: {e}"))?;
    Ok(l.local_addr().map_err(|e| crate::internal!("no free TCP port: {e}"))?.port())
}

/// Just enough of the gdb remote protocol (QEMU's gdbstub): breakpoints, registers, memory, continue.
pub struct GdbRemote {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl GdbRemote {
    /// Connects, retrying for up to 10 s while the helper starts QEMU.
    pub fn connect(port: u16) -> Result<GdbRemote> {
        for _ in 0..100 {
            if let Ok(stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, port)) {
                // `c` waits for iBoot to reach the kernel
                let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
                // Each packet is answered, then acked by a separate "+": without TCP_NODELAY, Nagle and delayed ACKs
                // cost ~40 ms a packet, minutes for a 16 MB ramdisk.
                let _ = stream.set_nodelay(true);
                return Ok(GdbRemote { stream, buffer: Vec::new() });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(failed(format!("no gdbstub on port {port}")))
    }

    pub fn send(&mut self, text: &str) -> Result<()> {
        self.send_bytes(text.as_bytes())
    }

    fn send_bytes(&mut self, body: &[u8]) -> Result<()> {
        let sum = body.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        let mut packet = Vec::with_capacity(body.len() + 4);
        packet.push(b'$');
        packet.extend_from_slice(body);
        packet.extend_from_slice(format!("#{sum:02x}").as_bytes());
        self.stream.write_all(&packet).map_err(|e| failed(format!("gdbstub send failed: {e}")))
    }

    pub fn receive(&mut self) -> Result<String> {
        loop {
            if let Some(i) = self.buffer.iter().position(|&b| b == b'$') {
                if let Some(j) = self.buffer[i..].iter().position(|&b| b == b'#').map(|j| i + j) {
                    if self.buffer.len() >= j + 3 {
                        let data = String::from_utf8_lossy(&self.buffer[i + 1..j]).into_owned();
                        self.buffer.drain(..j + 3);
                        let _ = self.stream.write_all(b"+");
                        return Ok(data);
                    }
                }
            }
            let mut chunk = vec![0u8; 65536];
            let n = self.stream.read(&mut chunk).map_err(|e| failed(format!("gdbstub: {e}")))?;
            if n == 0 {
                return Err(failed("gdbstub closed"));
            }
            self.buffer.extend_from_slice(&chunk[..n]);
        }
    }

    pub fn command(&mut self, text: &str) -> Result<String> {
        self.send(text)?;
        self.receive()
    }

    pub fn registers(&mut self) -> Result<Vec<u32>> {
        let g = self.command("g")?.into_bytes();
        if g.len() < 128 {
            return Err(failed("short register reply"));
        }
        Ok((0..16).map(|i| { let b = unhex(&g[i * 8..i * 8 + 8]); u32::from_le_bytes([b[0], b[1], b[2], b[3]]) }).collect())
    }

    pub fn read(&mut self, address: u32, count: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(count);
        while out.len() < count {
            let k = (count - out.len()).min(0x800);
            let r = self.command(&format!("m{:x},{k:x}", address + out.len() as u32))?.into_bytes();
            if r.len() != 2 * k {
                return Err(failed(format!("gdb read at 0x{address:x}: {}", String::from_utf8_lossy(&r[..r.len().min(8)]))));
            }
            out.extend(unhex(&r));
        }
        Ok(out)
    }

    /// In 1 KiB packets: QEMU's packet buffer is 4 KiB of hex.
    pub fn write(&mut self, address: u32, data: &[u8]) -> Result<()> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        for (n, part) in data.chunks(0x400).enumerate() {
            let at = address + (n * 0x400) as u32;
            let mut packet = format!("M{at:x},{:x}:", part.len()).into_bytes();
            for &x in part {
                packet.push(DIGITS[(x >> 4) as usize]);
                packet.push(DIGITS[(x & 15) as usize]);
            }
            self.send_bytes(&packet)?;
            let r = self.receive()?;
            if r != "OK" {
                return Err(failed(format!("gdb write at 0x{at:x}: {r}")));
            }
        }
        Ok(())
    }
}

fn unhex(s: &[u8]) -> Vec<u8> {
    let v = |c: u8| if c >= b'a' { c - 87 } else if c >= b'A' { c - 55 } else { c - 48 };
    s.chunks_exact(2).map(|p| v(p[0]) << 4 | v(p[1])).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex() {
        assert_eq!(super::unhex(b"00ff1A"), [0, 255, 26]);
    }
}

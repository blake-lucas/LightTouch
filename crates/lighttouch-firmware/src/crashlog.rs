//! A fatal signal in `firmwarekit oneshot` (the engine running one of a preparation's boots) is written to stderr,
//! which the app copies into the prepare log: the signal and fault address, the registers, what the GL bridge was
//! doing (the engine's qemu_ios_crash_note), and a backtrace whose frames read as Android's tombstones do
//! (`#00 pc <offset>  <module> (BuildId: ...)`, so ndk-stack and llvm-addr2line take them). Then the handler that was
//! there before runs: on Android, debuggerd's, which still writes its tombstone.
//!
//! Everything in the handler is async-signal-safe: raw write(2), fixed buffers, /proc/self/maps read with read(2), and
//! memory peeked through a pipe (write(2) fails with EFAULT instead of faulting), so a bad frame pointer ends the walk
//! rather than the report. The frame-record walk comes first; the unwind-table walk (_Unwind_Backtrace, which can take
//! the loader's lock) only after everything else is out.
#![cfg(any(target_os = "linux", target_os = "android"))]

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

const SIGNALS: [i32; 6] = [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT, libc::SIGSYS];
const FRAMES: usize = 40;
const MODULES: usize = 16;
const PATH: usize = 200;

/// The handlers there before ours, in SIGNALS order.
static mut PREVIOUS: [std::mem::MaybeUninit<libc::sigaction>; 6] = [const { std::mem::MaybeUninit::uninit() }; 6];
/// The thread writing a report (0: none).
static OWNER: AtomicI32 = AtomicI32::new(0);
/// The probe pipe: memory is read by writing it into [1] and reading it back from [0].
static PROBE: [AtomicI32; 2] = [AtomicI32::new(-1), AtomicI32::new(-1)];
/// The engine's crash note (qemu_ios_crash_note) and its slot-name function, when the engine has them.
static NOTE: AtomicUsize = AtomicUsize::new(0);
static NOTE_NAME: AtomicUsize = AtomicUsize::new(0);

/// The report's buffers, used only by the thread holding OWNER: signal stacks are small (glibc's and Rust's are a few
/// KiB; bionic's 16 KiB, which debuggerd's handler needs after ours), so the handler's own frames stay small.
struct Scratch {
    out: Out,
    frames: [u64; FRAMES],
    unwound: Unwound,
    modules: [Module; MODULES],
    which: [usize; FRAMES],
    fileoff: [u64; FRAMES],
    chunk: [u8; 2048],
    line: [u8; 512],
}

static mut SCRATCH: Scratch = Scratch {
    out: Out { buf: [0; 600], len: 0 },
    frames: [0; FRAMES],
    unwound: Unwound { frames: [0; FRAMES], n: 0, seen_pc: false, pc: 0 },
    modules: [NO_MODULE; MODULES],
    which: [usize::MAX; FRAMES],
    fileoff: [0; FRAMES],
    chunk: [0; 2048],
    line: [0; 512],
};

/// qemu-ios hw/arm/gles-host.c QemuIosCrashNote: version 1 is the newest call; version 2 adds the newest draw and a
/// ring of the newest calls.
#[repr(C)]
struct CrashNote {
    version: u32,
    size: u32,
    gl_slot: u32,
    gl_ctx: u32,
    gl_api: u32,
    gl_stage: u32,
    gl_calls: u64,
    // version 2
    ring_size: u32,
    draw_mode: u32,
    draw_count: u32,
    draw_guest_fb: u32,
    draw_host_fb: u32,
    draw_fb_status: u32,
    draw_guest_program: u32,
    draw_host_program: u32,
    draw_attribs: u32,
    draw_fetch: u32,
    draw_call: u64,
    ring: [CrashCall; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CrashCall {
    slot: u32,
    ctx: u32,
    a: [u32; 4],
}

/// Size of the version 1 note.
const NOTE_V1: usize = 32;

/// Installs the handlers (once, before the engine starts). The calling thread gets an alternate signal stack if it has
/// none; bionic gives every thread one, glibc none (a stack overflow on a desktop engine thread goes unreported).
pub fn install() {
    unsafe {
        let mut fds = [-1i32; 2];
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) == 0 {
            PROBE[0].store(fds[0], Ordering::Relaxed);
            PROBE[1].store(fds[1], Ordering::Relaxed);
        }
        let mut old: libc::stack_t = std::mem::zeroed();
        if libc::sigaltstack(std::ptr::null(), &mut old) == 0 && old.ss_flags & libc::SS_DISABLE != 0 {
            let size = 64 << 10;
            let stack = libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if stack != libc::MAP_FAILED {
                let new = libc::stack_t { ss_sp: stack, ss_flags: 0, ss_size: size };
                libc::sigaltstack(&new, std::ptr::null_mut());
            }
        }
        for (i, &sig) in SIGNALS.iter().enumerate() {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_fatal as extern "C" fn(i32, *mut libc::siginfo_t, *mut c_void) as usize;
            // SA_NODEFER: a fault inside the report re-enters, which hands it to the previous handler.
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER;
            libc::sigemptyset(&mut sa.sa_mask);
            let previous = (*std::ptr::addr_of_mut!(PREVIOUS))[i].as_mut_ptr();
            libc::sigaction(sig, &sa, previous);
        }
    }
}

/// The engine's crash note, when the loaded engine exports one (engines before dev/pvr-a6 don't).
pub fn attach_engine(lib: &libloading::Library) {
    unsafe {
        if let Ok(note) = lib.get::<unsafe extern "C" fn() -> *const c_void>(b"qemu_ios_crash_note") {
            let p = note() as *const CrashNote;
            let fits = |v: u32, size: u32| match v {
                1 => size as usize >= NOTE_V1,
                2 => size as usize >= std::mem::size_of::<CrashNote>(),
                _ => false,
            };
            if !p.is_null() && fits((*p).version, (*p).size) {
                NOTE.store(p as usize, Ordering::Relaxed);
            }
        }
        if let Ok(name) = lib.get::<unsafe extern "C" fn(u32) -> *const std::ffi::c_char>(b"qemu_ios_crash_gl_name") {
            NOTE_NAME.store(*name as usize, Ordering::Relaxed);
        }
    }
}

// ---- output: a line buffer flushed with write(2)

struct Out {
    buf: [u8; 600],
    len: usize,
}

impl Out {
    fn s(&mut self, text: &str) -> &mut Out {
        self.bytes(text.as_bytes())
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Out {
        for &c in b {
            if self.len == self.buf.len() {
                self.flush();
            }
            self.buf[self.len] = c;
            self.len += 1;
        }
        self
    }
    /// `0x`-less hex, at least `width` digits.
    fn hex(&mut self, v: u64, width: usize) -> &mut Out {
        let mut digits = [0u8; 16];
        let mut n = 0;
        let mut x = v;
        while x != 0 || n < width.max(1) {
            digits[n] = b"0123456789abcdef"[(x & 15) as usize];
            x >>= 4;
            n += 1;
            if n == 16 {
                break;
            }
        }
        while n > 0 {
            n -= 1;
            self.bytes(&[digits[n]]);
        }
        self
    }
    fn x(&mut self, v: u64) -> &mut Out {
        self.s("0x").hex(v, 1)
    }
    fn dec(&mut self, v: i64) -> &mut Out {
        if v < 0 {
            self.s("-");
        }
        let mut digits = [0u8; 20];
        let mut n = 0;
        let mut x = v.unsigned_abs();
        loop {
            digits[n] = b'0' + (x % 10) as u8;
            x /= 10;
            n += 1;
            if x == 0 {
                break;
            }
        }
        while n > 0 {
            n -= 1;
            self.bytes(&[digits[n]]);
        }
        self
    }
    fn cstr(&mut self, p: *const u8, max: usize) -> &mut Out {
        if !p.is_null() {
            for i in 0..max {
                let c = unsafe { *p.add(i) };
                if c == 0 {
                    break;
                }
                self.bytes(&[c]);
            }
        }
        self
    }
    fn line(&mut self) {
        self.bytes(b"\n");
        self.flush();
    }
    fn flush(&mut self) {
        let mut at = 0;
        while at < self.len {
            let n = unsafe { libc::write(2, self.buf[at..].as_ptr() as *const c_void, self.len - at) };
            if n <= 0 {
                break;
            }
            at += n as usize;
        }
        self.len = 0;
    }
}

// ---- safe memory reads

/// `out.len()` bytes at `addr`, or false when any of them is unreadable.
fn peek(addr: usize, out: &mut [u8]) -> bool {
    let (r, w) = (PROBE[0].load(Ordering::Relaxed), PROBE[1].load(Ordering::Relaxed));
    if r < 0 || addr == 0 || addr.checked_add(out.len()).is_none() {
        return false;
    }
    let mut done = 0;
    while done < out.len() {
        let chunk = (out.len() - done).min(4096);
        let n = unsafe { libc::write(w, (addr + done) as *const c_void, chunk) };
        if n <= 0 {
            return false;
        }
        let n = n as usize;
        let mut got = 0;
        while got < n {
            let m = unsafe { libc::read(r, out[done + got..].as_mut_ptr() as *mut c_void, n - got) };
            if m <= 0 {
                return false;
            }
            got += m as usize;
        }
        if n < chunk {
            return false;
        }
        done += n;
    }
    true
}

fn peek_u64(addr: usize) -> Option<u64> {
    let mut b = [0u8; 8];
    peek(addr, &mut b).then(|| u64::from_le_bytes(b))
}

/// A code address without its pointer-authentication bits (arm64 return addresses saved by signed prologues, as
/// bionic's and vendor drivers' are on Armv8.3+ cores). XPACLRI knows the address space's width (39 bits on the Poco,
/// not 48), and is a NOP on cores without PAC.
#[cfg(target_arch = "aarch64")]
fn strip(addr: u64) -> u64 {
    let mut x = addr;
    unsafe {
        std::arch::asm!("mov x30, {0}", "hint #7", "mov {0}, x30", inout(reg) x, out("x30") _,
            options(nomem, nostack, preserves_flags));
    }
    x
}

#[cfg(not(target_arch = "aarch64"))]
fn strip(addr: u64) -> u64 {
    addr
}

// ---- modules: the mapping each address falls in, and where that is in its ELF file (offset, build ID)

#[derive(Clone, Copy)]
struct Module {
    /// The first mapping of the file's run (or the unnamed mapping itself): what tells modules apart.
    base: u64,
    /// In memory: `base` minus the first PT_LOAD's page, when an ELF header is at `base`.
    bias: u64,
    path: [u8; PATH],
    path_len: usize,
    build_id: [u8; 32],
    build_id_len: usize,
    /// An ELF header was found at `base` in memory.
    elf: bool,
    /// From the file itself (read with pread): each PT_LOAD's file offset, size in the file and address, so a frame's
    /// file offset gives its address in the ELF whatever the mappings look like.
    loads: [[u64; 3]; 8],
    nloads: usize,
    /// A file (a path), not unnamed or [bracketed] memory.
    file: bool,
}

const NO_MODULE: Module = Module {
    base: 0,
    bias: 0,
    path: [0; PATH],
    path_len: 0,
    build_id: [0; 32],
    build_id_len: 0,
    elf: false,
    loads: [[0; 3]; 8],
    nloads: 0,
    file: false,
};

/// One /proc/self/maps line: start, end, file offset and path (empty for unnamed memory).
fn parse_map_line(l: &[u8]) -> Option<(u64, u64, u64, &[u8])> {
    let (start, rest) = parse_hex(l)?;
    let (end, rest) = parse_hex(rest.get(1..)?)?;
    let rest = rest.get(1..)?;
    let perms = rest.iter().position(|&c| c == b' ')?;
    let (offset, _) = parse_hex(rest.get(perms + 1..)?)?;
    Some((start, end, offset, map_path(l)))
}

/// Unnamed memory and [bracketed] names ([anon:.bss], the PROT_NONE gaps of a library aligned to 16 KiB pages on a
/// 4 KiB-page kernel, a page-size compatibility region) don't end a file's run of mappings.
fn transparent(path: &[u8]) -> bool {
    path.is_empty() || path[0] == b'['
}

/// The scan's state between maps lines.
struct MapScan {
    run_path: [u8; PATH],
    run_len: usize,
    run_start: u64,
}

impl MapScan {
    fn new() -> MapScan {
        MapScan { run_path: [0; PATH], run_len: usize::MAX, run_start: 0 }
    }

    /// One maps line: each frame inside it gets its module and its file offset (`fileoff`).
    #[allow(clippy::too_many_arguments)]
    fn line(
        &mut self,
        l: &[u8],
        frames: &[u64],
        modules: &mut [Module; MODULES],
        count: &mut usize,
        which: &mut [usize; FRAMES],
        fileoff: &mut [u64; FRAMES],
    ) {
        let Some((start, end, offset, path)) = parse_map_line(l) else { return };
        let len = path.len().min(PATH);
        let skip = transparent(path);
        if !skip && !(self.run_len == len && self.run_path[..len] == path[..len]) {
            self.run_len = len;
            self.run_path[..len].copy_from_slice(&path[..len]);
            self.run_start = start;
        }
        // A frame in unnamed or bracketed memory (TCG's generated code, the vdso) is a module of its own.
        let (base, name): (u64, &[u8]) =
            if skip { (start, &path[..len]) } else { (self.run_start, &self.run_path[..self.run_len]) };
        for (i, &f) in frames.iter().enumerate() {
            if f < start || f >= end || which[i] != usize::MAX {
                continue;
            }
            fileoff[i] = f - start + offset;
            let found = modules[..*count].iter().position(|m| m.base == base);
            which[i] = match found {
                Some(m) => m,
                None if *count < MODULES => {
                    let m = &mut modules[*count];
                    *m = NO_MODULE;
                    m.base = base;
                    m.path_len = name.len();
                    m.path[..name.len()].copy_from_slice(name);
                    m.file = !skip;
                    if !(m.file && file_facts(m)) {
                        elf_facts(m);
                    }
                    *count += 1;
                    *count - 1
                }
                None => usize::MAX,
            };
        }
    }
}

/// Module index and file offset per frame (usize::MAX: none), from one pass over /proc/self/maps.
#[allow(clippy::too_many_arguments)]
fn resolve(
    frames: &[u64],
    modules: &mut [Module; MODULES],
    count: &mut usize,
    which: &mut [usize; FRAMES],
    fileoff: &mut [u64; FRAMES],
    chunk: &mut [u8; 2048],
    line: &mut [u8; 512],
) {
    for w in which.iter_mut() {
        *w = usize::MAX;
    }
    let fd = unsafe { libc::open(c"/proc/self/maps".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return;
    }
    let mut scan = MapScan::new();
    let mut line_len = 0;
    loop {
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut c_void, chunk.len()) };
        if n <= 0 {
            break;
        }
        for &c in &chunk[..n as usize] {
            if c != b'\n' {
                if line_len < line.len() {
                    line[line_len] = c;
                    line_len += 1;
                }
                continue;
            }
            scan.line(&line[..line_len], frames, modules, count, which, fileoff);
            line_len = 0;
        }
    }
    unsafe { libc::close(fd) };
}

fn parse_hex(s: &[u8]) -> Option<(u64, &[u8])> {
    let mut v = 0u64;
    let mut n = 0;
    while n < s.len() {
        let d = match s[n] {
            b'0'..=b'9' => s[n] - b'0',
            b'a'..=b'f' => s[n] - b'a' + 10,
            _ => break,
        };
        v = (v << 4) | d as u64;
        n += 1;
    }
    (n > 0 && n < s.len()).then(|| (v, &s[n..]))
}

/// The path column (after five fields), or the empty slice.
fn map_path(l: &[u8]) -> &[u8] {
    let mut field = 0;
    let mut i = 0;
    while i < l.len() && field < 5 {
        while i < l.len() && l[i] != b' ' {
            i += 1;
        }
        while i < l.len() && l[i] == b' ' {
            i += 1;
        }
        field += 1;
    }
    &l[i..]
}

/// The PT_LOADs and the GNU build ID from the module's file, read with pread (async-signal-safe), so a frame's place
/// in the ELF doesn't depend on how the loader laid the mappings out (Android 17's vendor libraries and libc printed
/// as "<not a module>" when it was read from the first mapping in memory). False when there is no such ELF file.
fn file_facts(m: &mut Module) -> bool {
    let mut cpath = [0u8; PATH + 1];
    if m.path_len == 0 || m.path[0] != b'/' {
        return false;
    }
    cpath[..m.path_len].copy_from_slice(&m.path[..m.path_len]);
    let fd = unsafe { libc::open(cpath.as_ptr() as *const libc::c_char, libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return false;
    }
    let pread = |buf: &mut [u8], at: u64| -> bool {
        let n = unsafe { libc::pread(fd, buf.as_mut_ptr() as *mut c_void, buf.len(), at as libc::off_t) };
        n == buf.len() as isize
    };
    let mut eh = [0u8; 64];
    let ok = pread(&mut eh, 0) && &eh[..4] == b"\x7fELF" && eh[4] == 2;
    let (phoff, phentsize, phnum) = (
        u64::from_le_bytes(eh[32..40].try_into().unwrap()),
        u16::from_le_bytes([eh[54], eh[55]]) as u64,
        u16::from_le_bytes([eh[56], eh[57]]) as u64,
    );
    let mut notes: [(u64, u64); 4] = [(0, 0); 4];
    let mut note_count = 0;
    if ok && phentsize >= 56 && phnum <= 64 {
        for i in 0..phnum {
            let mut ph = [0u8; 56];
            if !pread(&mut ph, phoff + i * phentsize) {
                break;
            }
            let kind = u32::from_le_bytes(ph[0..4].try_into().unwrap());
            let offset = u64::from_le_bytes(ph[8..16].try_into().unwrap());
            let vaddr = u64::from_le_bytes(ph[16..24].try_into().unwrap());
            let filesz = u64::from_le_bytes(ph[32..40].try_into().unwrap());
            if kind == 1 && m.nloads < m.loads.len() {
                m.loads[m.nloads] = [offset, filesz, vaddr];
                m.nloads += 1;
            } else if kind == 4 && note_count < notes.len() {
                notes[note_count] = (offset, filesz);
                note_count += 1;
            }
        }
    }
    'notes: for &(offset, size) in &notes[..note_count] {
        let mut at = offset;
        let end = offset.saturating_add(size.min(4096));
        while at + 12 <= end {
            let mut h = [0u8; 12];
            if !pread(&mut h, at) {
                break;
            }
            let namesz = u32::from_le_bytes(h[0..4].try_into().unwrap()) as u64;
            let descsz = u32::from_le_bytes(h[4..8].try_into().unwrap()) as u64;
            let kind = u32::from_le_bytes(h[8..12].try_into().unwrap());
            let desc_at = at + 12 + namesz.div_ceil(4) * 4;
            if kind == 3 && namesz == 4 && descsz > 0 && descsz <= 32 {
                let mut name = [0u8; 4];
                if pread(&mut name, at + 12) && &name == b"GNU\0" {
                    let n = descsz as usize;
                    if pread(&mut m.build_id[..n], desc_at) {
                        m.build_id_len = n;
                    }
                    break 'notes;
                }
            }
            at = desc_at + descsz.div_ceil(4) * 4;
        }
    }
    unsafe { libc::close(fd) };
    m.nloads > 0
}

/// A frame's address in its module's ELF, from its file offset and the file's PT_LOADs.
fn file_address(m: &Module, fileoff: u64) -> Option<u64> {
    m.loads[..m.nloads]
        .iter()
        .find(|l| fileoff >= l[0] && fileoff < l[0].saturating_add(l[1]))
        .map(|l| fileoff - l[0] + l[2])
}

/// The load bias (base minus the first PT_LOAD's page) and the GNU build ID, from the ELF image in memory: the
/// fallback for a module whose file can't be read (an APK-embedded library).
fn elf_facts(m: &mut Module) {
    m.bias = m.base;
    let mut eh = [0u8; 64];
    if !peek(m.base as usize, &mut eh) || &eh[..4] != b"\x7fELF" || eh[4] != 2 {
        return;
    }
    let phoff = u64::from_le_bytes(eh[32..40].try_into().unwrap());
    let phentsize = u16::from_le_bytes([eh[54], eh[55]]) as u64;
    let phnum = u16::from_le_bytes([eh[56], eh[57]]) as u64;
    if phentsize < 56 || phnum > 64 {
        return;
    }
    m.elf = true;
    let mut first_load: Option<u64> = None;
    let mut notes: [(u64, u64); 4] = [(0, 0); 4];
    let mut note_count = 0;
    for i in 0..phnum {
        let mut ph = [0u8; 56];
        if !peek((m.base + phoff + i * phentsize) as usize, &mut ph) {
            return;
        }
        let kind = u32::from_le_bytes(ph[0..4].try_into().unwrap());
        let vaddr = u64::from_le_bytes(ph[16..24].try_into().unwrap());
        let memsz = u64::from_le_bytes(ph[40..48].try_into().unwrap());
        if kind == 1 && first_load.is_none() {
            first_load = Some(vaddr);
        } else if kind == 4 && note_count < notes.len() {
            notes[note_count] = (vaddr, memsz);
            note_count += 1;
        }
    }
    let page = 4096u64;
    m.bias = m.base.wrapping_sub(first_load.unwrap_or(0) & !(page - 1));
    for &(vaddr, size) in &notes[..note_count] {
        let mut at = m.bias.wrapping_add(vaddr);
        let end = at.saturating_add(size.min(4096));
        while at + 12 <= end {
            let mut h = [0u8; 12];
            if !peek(at as usize, &mut h) {
                break;
            }
            let namesz = u32::from_le_bytes(h[0..4].try_into().unwrap()) as u64;
            let descsz = u32::from_le_bytes(h[4..8].try_into().unwrap()) as u64;
            let kind = u32::from_le_bytes(h[8..12].try_into().unwrap());
            let name_at = at + 12;
            let desc_at = name_at + namesz.div_ceil(4) * 4;
            if kind == 3 && namesz == 4 && descsz > 0 && descsz <= 32 {
                let mut name = [0u8; 4];
                if peek(name_at as usize, &mut name) && &name == b"GNU\0" {
                    let n = descsz as usize;
                    if peek(desc_at as usize, &mut m.build_id[..n]) {
                        m.build_id_len = n;
                    }
                    return;
                }
            }
            at = desc_at + descsz.div_ceil(4) * 4;
        }
    }
}

// ---- the handler

/// Registers of interest from the signal's ucontext: pc, sp, frame pointer, link register (arm64; 0 elsewhere).
fn registers(uctx: *mut c_void) -> (u64, u64, u64, u64) {
    if uctx.is_null() {
        return (0, 0, 0, 0);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let mc = &(*(uctx as *const libc::ucontext_t)).uc_mcontext;
        (mc.pc, mc.sp, mc.regs[29], mc.regs[30])
    }
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let g = &(*(uctx as *const libc::ucontext_t)).uc_mcontext.gregs;
        (g[libc::REG_RIP as usize] as u64, g[libc::REG_RSP as usize] as u64, g[libc::REG_RBP as usize] as u64, 0)
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        (0, 0, 0, 0)
    }
}

fn signal_name(sig: i32) -> &'static str {
    match sig {
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGBUS => "SIGBUS",
        libc::SIGILL => "SIGILL",
        libc::SIGFPE => "SIGFPE",
        libc::SIGABRT => "SIGABRT",
        libc::SIGSYS => "SIGSYS",
        _ => "signal",
    }
}

fn index_of(sig: i32) -> usize {
    SIGNALS.iter().position(|&s| s == sig).unwrap_or(0)
}

/// Puts the previous handler back and runs it: a fault re-executes into it on return; a sent signal (abort's
/// tgkill) or a seccomp trap (which returns past the system call) is handed over here.
unsafe fn chain(sig: i32, info: *mut libc::siginfo_t, uctx: *mut c_void) {
    unsafe {
        let previous = (*std::ptr::addr_of!(PREVIOUS))[index_of(sig)].assume_init_ref();
        libc::sigaction(sig, previous, std::ptr::null_mut());
        let code = if info.is_null() { 0 } else { (*info).si_code };
        if code > 0 && sig != libc::SIGSYS {
            return;
        }
        let handler = previous.sa_sigaction;
        if handler == libc::SIG_IGN {
            return;
        }
        if handler == libc::SIG_DFL {
            libc::syscall(libc::SYS_tgkill, libc::getpid(), libc::gettid(), sig);
            return;
        }
        if previous.sa_flags & libc::SA_SIGINFO != 0 {
            let f: extern "C" fn(i32, *mut libc::siginfo_t, *mut c_void) = std::mem::transmute(handler);
            f(sig, info, uctx);
        } else {
            let f: extern "C" fn(i32) = std::mem::transmute(handler);
            f(sig);
        }
    }
}

extern "C" fn on_fatal(sig: i32, info: *mut libc::siginfo_t, uctx: *mut c_void) {
    let tid = unsafe { libc::gettid() };
    match OWNER.compare_exchange(0, tid, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {}
        Err(owner) if owner == tid => {
            // A fault inside the report itself.
            unsafe { chain(sig, info, uctx) };
            return;
        }
        Err(_) => {
            // Another thread is reporting: let it finish (up to 3 s) before this one goes to the previous handler.
            for _ in 0..300 {
                if OWNER.load(Ordering::Acquire) == 0 {
                    break;
                }
                let pause = libc::timespec { tv_sec: 0, tv_nsec: 10_000_000 };
                unsafe { libc::nanosleep(&pause, std::ptr::null_mut()) };
            }
            unsafe { chain(sig, info, uctx) };
            return;
        }
    }
    report(sig, info, uctx, tid);
    OWNER.store(0, Ordering::Release);
    unsafe { chain(sig, info, uctx) };
}

fn report(sig: i32, info: *mut libc::siginfo_t, uctx: *mut c_void, tid: i32) {
    let sc = unsafe { &mut *std::ptr::addr_of_mut!(SCRATCH) };
    let o = &mut sc.out;
    o.len = 0;
    let (code, addr) = if info.is_null() {
        (0, 0)
    } else {
        unsafe { ((*info).si_code, (*info).si_addr() as u64) }
    };
    let mut name = [0u8; 17];
    unsafe { libc::prctl(libc::PR_GET_NAME, name.as_mut_ptr() as libc::c_ulong, 0, 0, 0) };
    o.line();
    o.s("*** firmwarekit oneshot: the engine crashed: ").s(signal_name(sig)).s(" (").dec(sig as i64).s("), code ");
    o.dec(code as i64).s(", fault address ").x(addr).s(", pid ").dec(unsafe { libc::getpid() } as i64);
    o.s(", tid ").dec(tid as i64).s(" \"").cstr(name.as_ptr(), 16).s("\"").line();
    let (pc, sp, fp, lr) = registers(uctx);
    o.s("    pc ").x(pc).s("  sp ").x(sp).s("  fp ").x(fp);
    if cfg!(target_arch = "aarch64") {
        o.s("  lr ").x(lr);
    }
    o.line();
    #[cfg(target_arch = "aarch64")]
    if !uctx.is_null() {
        let regs = unsafe { &(*(uctx as *const libc::ucontext_t)).uc_mcontext.regs };
        for row in 0..8 {
            o.s("    ");
            for i in row * 4..(row * 4 + 4).min(29) {
                o.s("x").dec(i as i64).s(if i < 10 { "  " } else { " " }).hex(regs[i], 16).s("  ");
            }
            o.line();
        }
    }
    note(o);

    // Frame records: pc, the link register (the caller, when the fault is in a leaf or a call through NULL), then the
    // saved (fp, return address) pairs up the stack.
    let frames = &mut sc.frames;
    frames[0] = pc; // 0 for a call through NULL
    let mut n = 1;
    let push = |frames: &mut [u64; FRAMES], n: &mut usize, a: u64| {
        if *n < FRAMES && a != 0 && frames[*n - 1] != a {
            frames[*n] = a;
            *n += 1;
        }
    };
    if cfg!(target_arch = "aarch64") {
        push(frames, &mut n, strip(lr));
    } else if pc == 0 {
        // x86-64 call through NULL: the return address the call pushed is on top of the stack.
        if let Some(ret) = peek_u64(sp as usize) {
            push(frames, &mut n, ret);
        }
    }
    let mut at = fp;
    for _ in 0..FRAMES {
        if at == 0 || at % 8 != 0 {
            break;
        }
        let (Some(next), Some(ret)) = (peek_u64(at as usize), peek_u64(at as usize + 8)) else { break };
        let ret = strip(ret);
        if ret == 0 {
            break;
        }
        push(frames, &mut n, ret);
        if next <= at || next - at > (8 << 20) {
            break;
        }
        at = next;
    }
    let mut count = 0;
    o.s("backtrace (frame records):").line();
    print_frames(sc, n, false, &mut count);

    // The unwind tables, which also cover code built without frame pointers (a desktop engine); this can take the
    // loader's lock, so it goes last.
    sc.unwound = Unwound { frames: [0; FRAMES], n: 0, seen_pc: false, pc };
    unsafe { _Unwind_Backtrace(trace_one, std::ptr::addr_of_mut!(sc.unwound) as *mut c_void) };
    if sc.unwound.n > 0 {
        sc.out
            .s(if sc.unwound.seen_pc {
                "backtrace (unwind tables, from the faulting frame):"
            } else {
                "backtrace (unwind tables, from the signal handler):"
            })
            .line();
        let n = sc.unwound.n;
        print_frames(sc, n, true, &mut count);
    }
    sc.out.s("*** end of crash report").line();
}

fn note(o: &mut Out) {
    let p = NOTE.load(Ordering::Relaxed) as *const CrashNote;
    if p.is_null() {
        return;
    }
    let (slot, ctx, api, stage, calls) = unsafe {
        (
            std::ptr::read_volatile(&(*p).gl_slot),
            std::ptr::read_volatile(&(*p).gl_ctx),
            std::ptr::read_volatile(&(*p).gl_api),
            std::ptr::read_volatile(&(*p).gl_stage),
            std::ptr::read_volatile(&(*p).gl_calls),
        )
    };
    if calls == 0 {
        o.s("    GL bridge: no guest GL call yet").line();
        return;
    }
    o.s("    GL bridge: ").s(match stage {
        1 => "setting up the context for",
        2 => "inside",
        _ => "returned from",
    });
    o.s(" guest call ");
    let name = NOTE_NAME.load(Ordering::Relaxed);
    if name != 0 {
        let f: unsafe extern "C" fn(u32) -> *const u8 = unsafe { std::mem::transmute(name) };
        o.cstr(unsafe { f(slot) }, 64).s(" ");
    }
    o.s("(id ").dec(slot as i64).s("), context ").x(ctx as u64).s(", guest API ").dec(api as i64);
    o.s(", call #").dec(calls as i64).line();
    if unsafe { (*p).version } < 2 {
        return;
    }
    // Read in place: a copy would put the ring (~800 bytes) on a signal stack of a few KiB.
    let n: &CrashNote = unsafe { &*p };
    if n.draw_call != 0 {
        o.s("    newest draw (call #").dec(n.draw_call as i64).s("): mode ").x(n.draw_mode as u64);
        o.s(" count ").dec(n.draw_count as i64).s(", framebuffer ").dec(n.draw_guest_fb as i64);
        o.s(" (host ").dec(n.draw_host_fb as i64).s(", status ");
        if n.draw_fb_status == 0 { o.s("unchecked") } else { o.x(n.draw_fb_status as u64) };
        o.s("), program ").dec(n.draw_guest_program as i64).s(" (host ").dec(n.draw_host_program as i64);
        o.s(if n.draw_fetch != 0 { ", reads gl_LastFragData" } else { "" }).s("), attribute arrays ");
        o.x(n.draw_attribs as u64).line();
    }
    let size = (n.ring_size as u64).clamp(1, 32);
    let first = calls.saturating_sub(size);
    o.s("    newest guest GL calls (call #, name, context, first arguments):").line();
    for k in first..calls {
        let c = n.ring[(k % size) as usize];
        o.s("      #").dec(k as i64 + 1).s(" ");
        if name != 0 {
            let f: unsafe extern "C" fn(u32) -> *const u8 = unsafe { std::mem::transmute(name) };
            o.cstr(unsafe { f(c.slot) }, 64);
        }
        o.s(" (").dec(c.slot as i64).s(") ").x(c.ctx as u64);
        for v in c.a {
            o.s(" ").x(v as u64);
        }
        o.line();
    }
}

/// The first `n` frame-record frames, or unwound ones.
fn print_frames(sc: &mut Scratch, n: usize, unwound: bool, count: &mut usize) {
    let frames: &[u64] = if unwound { &sc.unwound.frames[..n] } else { &sc.frames[..n] };
    resolve(frames, &mut sc.modules, count, &mut sc.which, &mut sc.fileoff, &mut sc.chunk, &mut sc.line);
    let (o, modules, which, fileoff) = (&mut sc.out, &sc.modules, &sc.which, &sc.fileoff);
    for (i, &f) in frames.iter().enumerate() {
        o.s("      #").s(if i < 10 { "0" } else { "" }).dec(i as i64).s(" pc ");
        let m = (which[i] != usize::MAX).then(|| &modules[which[i]]);
        // The address in the ELF: from the file's PT_LOADs, else from the header in memory.
        let address = m.and_then(|m| file_address(m, fileoff[i]).or(m.elf.then(|| f.wrapping_sub(m.bias))));
        match (m, address) {
            (None, _) => {
                o.hex(f, 16).s("  <unknown>");
            }
            (Some(m), None) if m.file => {
                // A file, but not one whose ELF could be read: its offset still places the frame.
                o.hex(f, 16).s("  <").bytes(&m.path[..m.path_len]).s(" at file offset ");
                o.x(fileoff[i]).s(">");
            }
            (Some(m), None) => {
                o.hex(f, 16).s("  <not a module: ");
                o.bytes(if m.path_len > 0 { &m.path[..m.path_len] } else { b"anonymous memory" }).s(">");
            }
            (Some(m), Some(address)) => {
                o.hex(address, 16).s("  ");
                o.bytes(&m.path[..m.path_len]);
                if m.build_id_len > 0 {
                    o.s(" (BuildId: ");
                    for &b in &m.build_id[..m.build_id_len] {
                        o.hex(b as u64, 2);
                    }
                    o.s(")");
                }
            }
        }
        o.line();
    }
}

struct Unwound {
    frames: [u64; FRAMES],
    n: usize,
    seen_pc: bool,
    pc: u64,
}

#[repr(C)]
struct UnwindContext {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn _Unwind_Backtrace(
        trace: extern "C" fn(*mut UnwindContext, *mut c_void) -> i32,
        arg: *mut c_void,
    ) -> i32;
    fn _Unwind_GetIP(ctx: *mut UnwindContext) -> usize;
}

const URC_NO_REASON: i32 = 0;
const URC_END_OF_STACK: i32 = 5;

extern "C" fn trace_one(ctx: *mut UnwindContext, arg: *mut c_void) -> i32 {
    let u = unsafe { &mut *(arg as *mut Unwound) };
    let ip = unsafe { _Unwind_GetIP(ctx) } as u64;
    if !u.seen_pc && ip == u.pc && ip != 0 {
        // Frames so far were the handler's own: start at the faulting frame.
        u.seen_pc = true;
        u.n = 0;
    }
    if u.n < FRAMES && ip != 0 {
        u.frames[u.n] = ip;
        u.n += 1;
    }
    if u.n >= FRAMES { URC_END_OF_STACK } else { URC_NO_REASON }
}

/// The debuggable builds' crash test (LT_ONESHOT_CRASH=SECONDS[:null|write|abort], the `crash-test` feature only): a
/// thread that, that many seconds into the boot, calls through NULL (the default), writes to address 8, or aborts.
#[cfg(feature = "crash-test")]
pub fn crash_test() {
    let Ok(spec) = std::env::var("LT_ONESHOT_CRASH") else { return };
    let (seconds, kind) = spec.split_once(':').unwrap_or((spec.as_str(), "null"));
    let seconds: u64 = seconds.trim().parse().unwrap_or(30);
    let kind = kind.trim().to_string();
    eprintln!("firmwarekit oneshot: crash test armed ({kind} in {seconds} s)");
    std::thread::Builder::new()
        .name("lt-crash-test".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(seconds));
            eprintln!("firmwarekit oneshot: crash test: {kind} now");
            crash_now(&kind);
        })
        .ok();
}

#[cfg(feature = "crash-test")]
#[inline(never)]
fn crash_now(kind: &str) {
    match kind {
        "abort" => std::process::abort(),
        "write" => unsafe { std::ptr::write_volatile(std::hint::black_box(8usize) as *mut u64, 1) },
        _ => {
            let f: Option<extern "C" fn()> = std::hint::black_box(None);
            #[allow(clippy::missing_transmute_annotations)]
            let f: extern "C" fn() = unsafe { std::mem::transmute(f) };
            f();
        }
    }
}

#[cfg(not(feature = "crash-test"))]
pub fn crash_test() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames against maps lines; returns (module index, file offset) per frame and the modules.
    fn scan(lines: &[&str], frames: &[u64]) -> (Vec<(usize, u64)>, Vec<Module>) {
        let mut modules = [NO_MODULE; MODULES];
        let (mut count, mut which, mut fileoff) = (0, [usize::MAX; FRAMES], [0u64; FRAMES]);
        let mut s = MapScan::new();
        for l in lines {
            s.line(l.as_bytes(), frames, &mut modules, &mut count, &mut which, &mut fileoff);
        }
        ((0..frames.len()).map(|i| (which[i], fileoff[i])).collect(), modules[..count].to_vec())
    }

    #[test]
    fn library_with_unnamed_and_bracketed_gaps_is_one_module() {
        let lines = [
            "720e60b000-720eb6a000 r--p 00000000 fc:29 783387                         /data/app/x/lib/arm64/libqemu-arm.so",
            "720eb6a000-720eb6d000 ---p 00000000 00:00 0 ",
            "720eb6d000-720f4e9000 r-xp 0055e000 fc:29 783387                         /data/app/x/lib/arm64/libqemu-arm.so",
            "720f4e9000-720f4ec000 ---p 00000000 00:00 0                              [page size compat]",
            "720f4ec000-720f5a9000 r--p 00ed9000 fc:29 783387                         /data/app/x/lib/arm64/libqemu-arm.so",
            "720f7cb000-720fcf5000 rw-p 00000000 00:00 0                              [anon:.bss]",
            "7300000000-7300100000 rwxp 00000000 00:00 0 ",
        ];
        let (frames, modules) = scan(&lines, &[0x720eb6d010, 0x720f4ec020, 0x720f7cb100, 0x7300000040]);
        assert_eq!(frames[0], (0, 0x55e010));
        assert_eq!(frames[1], (0, 0xed9020), "a segment after gaps stays in the library's module");
        assert_eq!(modules[0].base, 0x720e60b000);
        assert!(modules[0].file);
        assert_eq!(&modules[0].path[..modules[0].path_len], b"/data/app/x/lib/arm64/libqemu-arm.so");
        let bss = &modules[frames[2].0];
        assert_eq!(&bss.path[..bss.path_len], b"[anon:.bss]");
        assert!(!bss.file);
        let tcg = &modules[frames[3].0];
        assert_eq!(tcg.path_len, 0, "unnamed memory is a module of its own");
        assert_eq!(tcg.base, 0x7300000000);
        assert!(!tcg.file);
    }

    #[test]
    fn file_address_uses_the_loads() {
        let mut m = NO_MODULE;
        m.loads[0] = [0, 0x55e000, 0];
        m.loads[1] = [0x55e000, 0x97b000, 0x562000];
        m.nloads = 2;
        assert_eq!(file_address(&m, 0x100), Some(0x100));
        assert_eq!(file_address(&m, 0x55e010), Some(0x562010));
        assert_eq!(file_address(&m, 0xf00000), None);
    }

    #[test]
    fn map_line_fields() {
        let l = b"720eb6d000-720f4e9000 r-xp 0055e000 fc:29 783387   /vendor/lib64/egl/libGLESv2_powervr.so";
        let (start, end, offset, path) = parse_map_line(l).unwrap();
        assert_eq!((start, end, offset), (0x720eb6d000, 0x720f4e9000, 0x55e000));
        assert_eq!(path, b"/vendor/lib64/egl/libGLESv2_powervr.so");
    }

    #[test]
    fn file_facts_read_this_test_binary() {
        let exe = std::env::current_exe().unwrap();
        let path = exe.to_str().unwrap().as_bytes();
        let mut m = NO_MODULE;
        m.path[..path.len()].copy_from_slice(path);
        m.path_len = path.len();
        assert!(file_facts(&mut m));
        assert!(m.nloads >= 2);
    }
}

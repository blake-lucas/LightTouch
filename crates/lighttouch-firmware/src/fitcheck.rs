//! FitCheck: proof, read off the firmware itself at prepare time, that a guest-side piece fits the firmware it is
//! injected into (ported from the Swift FirmwareKit's FitCheck, GLIDispatchCheck and MachOHeaders).
//! Every check returns a Fit: fits with the proof, or does not fit with why. "Could not tell" is never a fit. No
//! per-build tables: only symbols, strings and structure.
//!
//! ```ignore
//! let fw = Firmware::new(ed.snapshot()?, "armv7", kernel);
//! let f = fitcheck::loads("it_pbd", &helper_bytes, &fw, None);   // f.fits, f.proof
//! log.check(f, true, "left out")?;
//! ```
use crate::error::Result;
use crate::hfs::Editor;
use crate::hfs::editor::Kind;
use crate::macho::{
    self, LC_DYLD_INFO, LC_DYLD_INFO_ONLY as LC_DIO, LC_REEXPORT_DYLIB, MachO32, REQ_DYLD, hex, is_export,
    last_component, le32, trie_reexports,
};
use crate::sandbox_patch;
use crate::shared_cache::DyldSharedCache;
use plist::Value;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Fit {
    pub piece: String,
    pub fits: bool,
    pub proof: String,
}

impl Fit {
    pub fn new(piece: impl Into<String>, fits: bool, proof: impl Into<String>) -> Fit {
        Fit { piece: piece.into(), fits, proof: proof.into() }
    }
    /// The warning event's text: the misfit and what the preparer did about it.
    pub fn warning(&self, outcome: &str) -> String {
        format!("{} does not fit this firmware ({outcome}): {}", self.piece, self.proof)
    }
}

/// The prepare's record of every check (the lock's "fit"). A required piece that does not fit fails; an optional
/// one is recorded and warned about, and the caller leaves it out.
pub struct Log {
    pub fits: RefCell<Vec<Fit>>,
    warn: Box<dyn Fn(&str)>,
}

impl Default for Log {
    fn default() -> Log {
        Log::new(|_| {})
    }
}

impl Log {
    pub fn new(warn: impl Fn(&str) + 'static) -> Log {
        Log { fits: RefCell::new(Vec::new()), warn: Box::new(warn) }
    }
    /// `outcome`: what the preparer does with an optional piece that does not fit ("left out", "kept: ...").
    pub fn check(&self, f: Fit, required: bool, outcome: &str) -> Result<bool> {
        self.fits.borrow_mut().push(f.clone());
        if f.fits {
            return Ok(true);
        }
        if required {
            let mut e = crate::unsupported!("{} does not fit this firmware: {}", f.piece, f.proof);
            e.piece = Some(f.piece.clone());
            return Err(e);
        }
        (self.warn)(&f.warning(outcome));
        Ok(false)
    }
    /// One optional piece that goes wherever it fits: every place is recorded, and only when it fits none is each
    /// misfit warned about. Returns the places' fits.
    pub fn check_any(&self, places: Vec<Fit>, outcome: &str) -> Vec<bool> {
        let any = places.iter().any(|f| f.fits);
        if !any {
            for f in &places {
                (self.warn)(&f.warning(outcome));
            }
        }
        let out = places.iter().map(|f| f.fits).collect();
        self.fits.borrow_mut().extend(places);
        out
    }
    /// A piece the preparer did not install (nothing proven, so not a fit; no warning).
    pub fn not_installed(&self, piece: &str, why: &str) {
        self.fits.borrow_mut().push(Fit::new(piece, false, format!("not installed ({why})")));
    }
    pub fn object(&self) -> serde_json::Value {
        serde_json::to_value(&*self.fits.borrow()).unwrap_or_default()
    }
}

/// LC_DYLD_INFO_ONLY: a firmware whose dyld has no precedent for it takes the legacy-linked helpers.
pub const LC_DYLD_INFO_ONLY: u32 = LC_DIO;
/// OpenGLES.framework/OpenGLES, volume-relative (FitCheck.openGLES).
pub const OPENGLES: &str = "System/Library/Frameworks/OpenGLES.framework/OpenGLES";
const QUARTZCORE: &str = "System/Library/Frameworks/QuartzCore.framework/QuartzCore";
const COREIMAGE: &str = "System/Library/Frameworks/CoreImage.framework/CoreImage";
const IOSURFACE: &str = "System/Library/PrivateFrameworks/IOSurface.framework/IOSurface";
const CORE_SURFACES: [&str; 2] = [
    "System/Library/Frameworks/CoreSurface.framework/CoreSurface",
    "System/Library/PrivateFrameworks/CoreSurface.framework/CoreSurface",
];
const IO_MOBILE_FRAMEBUFFER: &str =
    "System/Library/PrivateFrameworks/IOMobileFramebuffer.framework/IOMobileFramebuffer";
const SGX_ENGINE: &str = "System/Library/Frameworks/OpenGLES.framework/GLEngine.bundle";
const SURFACE_CALLS: [&str; 7] =
    ["GetBaseAddress", "GetBytesPerRow", "GetWidth", "GetHeight", "GetPixelFormat", "Lock", "Unlock"];
const DAEMONS: &str = "System/Library/LaunchDaemons";
const FRAMEWORK_DIRS: [&str; 2] = ["System/Library/Frameworks", "System/Library/PrivateFrameworks"];
const APPSYNC_PATH: &str = "usr/lib/libappsync.dylib";
const APPSYNC_DYLIB: &str = "libappsync.dylib";
const APPSYNC_LAUNCHER: &str = "appsync-launch";

/// it_prefs' settings: (domain key, the binary that reads it) (FitCheck.itPrefs).
pub const IT_PREFS: &[(&str, &str)] = &[
    ("SBDidShowReorderText", "System/Library/CoreServices/SpringBoard.app/SpringBoard"),
    ("AppleLocationServer", "usr/libexec/locationd"),
    ("AppleLocationServerRequiresCert", "usr/libexec/locationd"),
];

static PRECEDENT_BINARIES: [&str; 3] =
    ["sbin/launchd", "System/Library/CoreServices/SpringBoard.app/SpringBoard", "usr/libexec/lockdownd"];

fn dyld_cache_rel(arch: &str) -> String {
    format!("System/Library/Caches/com.apple.dyld/dyld_shared_cache_{arch}")
}

/// The C string `s` as it sits in a binary's string section: NUL on both sides.
fn c_string(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 2);
    v.push(0);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

/// Does `haystack` contain `needle`? (memmem over mapped bytes: the shared cache is hundreds of MB.)
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(haystack, needle).is_some()
}

/// The pristine firmware as the checks read it: a read-only view of the system volume (symlinks resolved inside
/// it), its dyld shared cache, and the decrypted kernelcache when the recipe has one.
pub struct Firmware {
    pub root: Editor,
    pub arch: String,
    pub kernelcache: Option<Vec<u8>>,
    cache: std::cell::OnceCell<Option<DyldSharedCache>>,
    precedent: std::cell::OnceCell<HashMap<u32, String>>,
    exports_memo: RefCell<HashMap<String, Option<HashSet<String>>>>,
    imports_memo: RefCell<HashMap<String, Option<HashSet<String>>>>,
    links_memo: RefCell<HashMap<String, Vec<String>>>,
}

impl Firmware {
    pub fn new(root: Editor, arch: &str, kernelcache: Option<Vec<u8>>) -> Firmware {
        Firmware {
            root,
            arch: arch.to_string(),
            kernelcache,
            cache: std::cell::OnceCell::new(),
            precedent: std::cell::OnceCell::new(),
            exports_memo: RefCell::new(HashMap::new()),
            imports_memo: RefCell::new(HashMap::new()),
            links_memo: RefCell::new(HashMap::new()),
        }
    }

    /// The file at `rel` (with or without a leading "/"), symlinks followed inside the volume.
    pub fn data(&self, rel: &str) -> Option<Vec<u8>> {
        let (path, exists) = self.follow(rel);
        if exists { self.root.read(&path).ok() } else { None }
    }

    /// Does the file at `rel` contain `bytes`?
    fn file_contains(&self, rel: &str, bytes: &[u8]) -> bool {
        self.data(rel).map(|d| contains(&d, bytes)).unwrap_or(false)
    }

    /// Whether `rel` resolves to something that exists on the volume.
    fn resolve_exists(&self, rel: &str) -> bool {
        self.follow(rel).1
    }

    /// `rel` with every symlink followed inside the volume: (the followed path, whether it exists). Mirrors Swift's
    /// FitCheck.Firmware.follow (components, "."/"..", absolute targets reset to the root, 32 hops).
    fn follow(&self, rel: &str) -> (String, bool) {
        let mut parts: Vec<String> = rel.split('/').filter(|p| !p.is_empty()).map(str::to_string).collect();
        parts.reverse(); // use as a stack, popping the front
        let mut out: Vec<String> = Vec::new();
        let mut hops = 0;
        while let Some(p) = parts.pop() {
            if p == "." {
                continue;
            }
            if p == ".." {
                out.pop();
                continue;
            }
            let mut here = out.clone();
            here.push(p.clone());
            let here = here.join("/");
            if let Ok(Some(m)) = self.root.lstat(&here) {
                if m.kind == Kind::Symlink {
                    if let Ok(dest) = self.root.read_link(&here) {
                        hops += 1;
                        if hops >= 32 {
                            return (rel.to_string(), false);
                        }
                        if dest.starts_with('/') {
                            out.clear();
                        }
                        let mut more: Vec<String> =
                            dest.split('/').filter(|p| !p.is_empty()).map(str::to_string).collect();
                        more.reverse();
                        // the remaining parts come after the symlink's target
                        let mut rest = std::mem::take(&mut parts);
                        parts = more;
                        parts.splice(0..0, std::mem::take(&mut rest));
                        continue;
                    }
                }
            }
            out.push(p);
        }
        let path = out.join("/");
        let exists = self.root.lstat(&path).map(|m| m.is_some()).unwrap_or(false);
        (path, exists)
    }

    /// The volume's dyld shared cache for `arch`, if any.
    pub fn cache(&self) -> Option<&DyldSharedCache> {
        self.cache
            .get_or_init(|| self.data(&dyld_cache_rel(&self.arch)).and_then(|d| DyldSharedCache::parse(d).ok()))
            .as_ref()
    }

    /// Which of the firmware's own executables carries each dyld-required load command (LC_REQ_DYLD set): a command
    /// this firmware's dyld is known to take, by precedent.
    pub fn precedent(&self) -> &HashMap<u32, String> {
        self.precedent.get_or_init(|| {
            let mut out: HashMap<u32, String> = HashMap::new();
            for rel in PRECEDENT_BINARIES {
                let Some(d) = self.data(rel) else { continue };
                let Some((m, _)) = MachO32::slice(&d, &self.arch) else { continue };
                for c in &m.commands {
                    if c.cmd & REQ_DYLD != 0 {
                        out.entry(c.cmd).or_insert_with(|| last_component(rel).to_string());
                    }
                }
            }
            out
        })
    }

    /// The cache image installed as `install` (its path, or the followed path with a leading "/").
    fn cache_image<'c>(
        &self,
        cache: &'c DyldSharedCache,
        install: &str,
        followed: &str,
    ) -> Option<&'c crate::shared_cache::Image> {
        cache.image(install).or_else(|| cache.image(&format!("/{followed}")))
    }

    /// The symbols the image installed as `install` exports (its re-exports' included); None when this firmware has
    /// no such image (on disk or in the shared cache).
    fn exports(&self, install: &str) -> Option<HashSet<String>> {
        if let Some(hit) = self.exports_memo.borrow().get(install) {
            return hit.clone();
        }
        self.exports_memo.borrow_mut().insert(install.to_string(), None); // a re-export cycle ends here
        let found = self.load_exports(install);
        self.exports_memo.borrow_mut().insert(install.to_string(), found.clone());
        found
    }

    fn load_exports(&self, install: &str) -> Option<HashSet<String>> {
        let (followed, on_disk) = self.follow(install);
        if let Some(cache) = self.cache() {
            if let Some(img) = self.cache_image(cache, install, &followed) {
                let mut out = HashSet::new();
                cache.for_each_symbol(img, |s| {
                    if is_export(s.kind) {
                        out.insert(s.name);
                    }
                    true
                });
                let mut reexports = Vec::new();
                for (cmd, off) in cache.load_commands(img) {
                    if cmd == LC_REEXPORT_DYLIB {
                        reexports.push(cache.command_string(off));
                    }
                    // a cache image's export trie offset is a cache file offset
                    if cmd == LC_DYLD_INFO || cmd == LC_DIO {
                        let at = le32(&cache.data, off + 40) as usize;
                        let size = le32(&cache.data, off + 44) as usize;
                        out.extend(trie_reexports(&cache.data, at, size));
                    }
                }
                for r in reexports {
                    if let Some(e) = self.exports(&r) {
                        out.extend(e);
                    }
                }
                return Some(out);
            }
        }
        if !on_disk {
            return None;
        }
        let d = self.data(&followed)?;
        let (m, _) = MachO32::slice(&d, &self.arch)?;
        let mut out: HashSet<String> = m.exports();
        for r in m.reexported() {
            if let Some(e) = self.exports(&r) {
                out.extend(e);
            }
        }
        if let Some(info) = m.commands.iter().find(|c| c.cmd == LC_DYLD_INFO || c.cmd == LC_DIO) {
            let at = le32(&m.b, info.off + 40) as usize;
            let size = le32(&m.b, info.off + 44) as usize;
            out.extend(trie_reexports(&m.b, at, size));
        }
        Some(out)
    }

    /// The images `install` links (weak ones included); [] when it is not on this firmware.
    fn links(&self, install: &str) -> Vec<String> {
        if let Some(hit) = self.links_memo.borrow().get(install) {
            return hit.clone();
        }
        let (followed, on_disk) = self.follow(install);
        let mut out = Vec::new();
        if let Some(cache) = self.cache() {
            if let Some(img) = self.cache_image(cache, install, &followed) {
                for (cmd, off) in cache.load_commands(img) {
                    if macho::is_link_command(cmd) {
                        out.push(cache.command_string(off));
                    }
                }
                self.links_memo.borrow_mut().insert(install.to_string(), out.clone());
                return out;
            }
        }
        if on_disk {
            if let Some(d) = self.data(&followed) {
                if let Some((m, _)) = MachO32::slice(&d, &self.arch) {
                    out = m.dylibs().into_iter().map(|d| d.name).collect();
                }
            }
        }
        self.links_memo.borrow_mut().insert(install.to_string(), out.clone());
        out
    }

    /// Every image a process of `executable` has loaded before anything is inserted: it and its links, transitively.
    fn loaded(&self, executable: &str) -> Vec<String> {
        let start = format!("/{}", executable.trim_start_matches('/'));
        let mut seen = vec![start.clone()];
        let mut queue = vec![start];
        while let Some(next) = queue.pop() {
            for l in self.links(&next) {
                if !seen.contains(&l) {
                    seen.push(l.clone());
                    queue.push(l);
                }
            }
        }
        seen
    }

    /// The undefined external symbols (prebound ones included: 2.x/3.0) the image installed as `install` imports;
    /// None when this firmware has no such image.
    fn imports(&self, install: &str) -> Option<HashSet<String>> {
        if let Some(hit) = self.imports_memo.borrow().get(install) {
            return hit.clone();
        }
        let (followed, on_disk) = self.follow(install);
        let mut found: Option<HashSet<String>> = None;
        if let Some(cache) = self.cache() {
            if let Some(img) = self.cache_image(cache, install, &followed) {
                let mut out = HashSet::new();
                cache.for_each_symbol(img, |s| {
                    if macho::is_import(s.kind) {
                        out.insert(s.name);
                    }
                    true
                });
                found = Some(out);
            }
        }
        if found.is_none() && on_disk {
            if let Some(d) = self.data(&followed) {
                if let Some((m, _)) = MachO32::slice(&d, &self.arch) {
                    found = Some(m.imports());
                }
            }
        }
        self.imports_memo.borrow_mut().insert(install.to_string(), found.clone());
        found
    }

    /// Every framework binary on the volume, the shared cache and SpringBoard: the images that could read an
    /// environment variable SpringBoard's job sets. (label, bytes), cache first.
    fn readers(&self) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some(c) = self.cache() {
            out.push(("the shared cache".to_string(), c.data.clone()));
        }
        for dir in FRAMEWORK_DIRS {
            let mut names = self.root.list(dir).unwrap_or_default();
            names.sort();
            for f in names {
                if !f.ends_with(".framework") {
                    continue;
                }
                let stem = &f[..f.len() - ".framework".len()];
                let rel = format!("{dir}/{f}/{stem}");
                if let Some(d) = self.data(&rel) {
                    out.push((last_component(&rel).to_string(), d));
                }
            }
        }
        if let Some(sb) = self.data(IT_PREFS[0].1) {
            out.push(("SpringBoard".to_string(), sb));
        }
        out
    }
}

/// Is `d` a Mach-O (thin 32-bit or fat)?
pub fn is_mach_o(d: &[u8]) -> bool {
    d.len() >= 4 && (d[..4] == [0xCE, 0xFA, 0xED, 0xFE] || d[..4] == [0xCA, 0xFE, 0xBA, 0xBE])
}

/// The stock executable whose launchd job inserts `dylib` (DYLD_INSERT_LIBRARIES), as the volume's jobs say now.
pub fn host(dylib: &str, fw: &Firmware) -> Option<String> {
    let mut names = fw.root.list(DAEMONS).unwrap_or_default();
    names.sort();
    for n in names {
        if !n.ends_with(".plist") {
            continue;
        }
        let Some(d) = plist_dict(fw, &format!("{DAEMONS}/{n}")) else { continue };
        let inserted = d
            .get("EnvironmentVariables")
            .and_then(Value::as_dictionary)
            .and_then(|e| e.get("DYLD_INSERT_LIBRARIES"))
            .and_then(Value::as_string);
        if let Some(inserted) = inserted {
            if inserted.split(':').any(|s| s == dylib) {
                return program(&d);
            }
        }
    }
    None
}

/// A dictionary plist read off the volume (through symlinks).
fn plist_dict(fw: &Firmware, rel: &str) -> Option<plist::Dictionary> {
    let d = fw.data(rel)?;
    match plist::Value::from_reader(std::io::Cursor::new(d)) {
        Ok(Value::Dictionary(d)) => Some(d),
        _ => None,
    }
}

/// A launchd job's program: ProgramArguments[0] else Program.
fn program(d: &plist::Dictionary) -> Option<String> {
    d.get("ProgramArguments")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_string)
        .map(str::to_string)
        .or_else(|| d.get("Program").and_then(Value::as_string).map(str::to_string))
}

/// Does the guest Mach-O `bin` (thin or fat) load on this firmware? Proven, not assumed.
pub fn loads(piece: &str, bin: &[u8], fw: &Firmware, host: Option<&str>) -> Fit {
    let Some((m, slice_arch)) = MachO32::slice(bin, &fw.arch) else {
        return Fit::new(
            piece,
            false,
            format!("no {} slice this CPU runs (not a 32-bit ARM Mach-O for this board)", fw.arch),
        );
    };
    let mut why: Vec<String> = Vec::new();
    let required: Vec<u32> = m.commands.iter().map(|c| c.cmd).filter(|c| c & REQ_DYLD != 0).collect();
    let precedent = fw.precedent();
    let unknown: BTreeSet<u32> = required.iter().copied().filter(|c| !precedent.contains_key(c)).collect();
    if !unknown.is_empty() {
        let list = unknown.iter().map(|c| hex(*c)).collect::<Vec<_>>().join(", ");
        why.push(format!(
            "load command {list} is in none of this firmware's own executables (its dyld refuses what it does not know)"
        ));
    }
    let linked = m.dylibs();
    let mut lost: Vec<String> = Vec::new();
    for l in &linked {
        if fw.exports(&l.name).is_none() && !l.weak {
            lost.push(l.name.clone());
        }
    }
    if !lost.is_empty() {
        why.push(format!("links {}, which this firmware does not have", lost.join(", ")));
    }
    let mut missing: Vec<String> = Vec::new();
    let mut resolved = 0usize;
    let two_level = m.flags & 0x80 != 0;
    for s in m.symbols() {
        // N_UNDF external, not a stub type
        if !(s.kind & 0xE0 == 0 && s.kind & 0x01 != 0 && s.kind & 0x0E == 0 && !s.name.is_empty()) {
            continue;
        }
        if s.desc & 0x40 != 0 {
            continue; // N_WEAK_REF: dyld leaves it NULL
        }
        let ordinal = (s.desc >> 8) as usize;
        let candidates: Vec<String> = if two_level && ordinal >= 1 && ordinal <= linked.len() {
            vec![linked[ordinal - 1].name.clone()]
        } else if two_level && ordinal == 0 {
            continue; // SELF_LIBRARY
        } else if two_level && ordinal == 0xFF {
            host.map(|h| vec![format!("/{}", h.trim_start_matches('/'))]).unwrap_or_default()
        } else {
            // flat namespace, or DYNAMIC_LOOKUP
            let mut c: Vec<String> = linked.iter().map(|l| l.name.clone()).collect();
            if let Some(h) = host {
                c.extend(fw.loaded(h));
            }
            c
        };
        if candidates.iter().any(|c| fw.exports(c).map(|e| e.contains(&s.name)).unwrap_or(false)) {
            resolved += 1;
        } else {
            missing.push(s.name);
        }
    }
    if !missing.is_empty() {
        let shown = missing.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
        let extra = if missing.len() > 6 { format!(" (+{})", missing.len() - 6) } else { String::new() };
        why.push(format!("imports {shown}{extra} that no linked image of this firmware exports"));
    }
    if !why.is_empty() {
        return Fit::new(piece, false, why.join("; "));
    }
    // every one has a precedent: an unknown command returned above
    let mut req_sorted: Vec<u32> = required.iter().copied().collect::<BTreeSet<u32>>().into_iter().collect();
    req_sorted.sort();
    let cmds: Vec<String> =
        req_sorted.iter().filter_map(|c| precedent.get(c).map(|n| format!("{} as {n}", hex(*c)))).collect();
    let in_host = host.map(|h| format!(" in {}", last_component(h))).unwrap_or_default();
    let cmds_text = if cmds.is_empty() {
        "no dyld-required load commands".to_string()
    } else {
        format!("load commands {}", cmds.join(", "))
    };
    Fit::new(
        piece,
        true,
        format!(
            "{slice_arch} slice{in_host}; {cmds_text}; {} linked images present; {resolved} imports resolved",
            linked.len()
        ),
    )
}

// ---- it_msmquiet

const MSM_KEYS: [&str; 2] = ["UNSUPPORTED_FAILURE", "UNSUPPORTED_FAILURE_BODY"];
const MSM_CALLS: [&str; 2] = ["_CFUserNotificationDisplayNotice", "_CFUserNotificationCreate"];

/// it_msmquiet fits when the mounter (`program`, the stock job's) raises the notice it recognizes.
pub fn msm_quiet(fw: &Firmware, program: &str, dylib: &[u8]) -> Fit {
    let name = last_component(program).to_string();
    let piece = format!("it_msmquiet ({name}'s USB \"not supported\" notice)");
    let Some(bin) = fw.data(program) else {
        return Fit::new(piece, false, format!("no {program} on this firmware"));
    };
    let Some((m, _)) = MachO32::slice(&bin, &fw.arch) else {
        return Fit::new(piece, false, format!("no {program} on this firmware"));
    };
    let keys: Vec<&str> = MSM_KEYS.iter().copied().filter(|k| contains(&bin, &c_string(k))).collect();
    let imports = m.imports();
    let calls: Vec<&str> = MSM_CALLS.iter().copied().filter(|c| imports.contains(*c)).collect();
    if keys.is_empty() {
        return Fit::new(
            piece,
            false,
            format!(
                "{name} names neither {}: whatever raises this firmware's notice, it_msmquiet cannot recognize it",
                MSM_KEYS.join(" nor ")
            ),
        );
    }
    if calls.is_empty() {
        return Fit::new(
            piece,
            false,
            format!("{name} imports neither {}, the calls it_msmquiet interposes", MSM_CALLS.join(" nor ")),
        );
    }
    let l = loads(&piece, dylib, fw, Some(program));
    if !l.fits {
        return l;
    }
    let calls_text = calls.iter().map(|c| &c[1..]).collect::<Vec<_>>().join(", ");
    Fit::new(piece, true, format!("{name} raises {} through {}; {}", keys.join(", "), calls_text, l.proof))
}

// ---- it_prefs

/// One Fit per setting: the reader names the key, by it_prefs' own rule (the key and its NUL anywhere in the file).
pub fn prefs(fw: &Firmware, settings: &[(&str, &str)]) -> Vec<Fit> {
    settings
        .iter()
        .map(|&(key, reader)| {
            let piece = format!("it_prefs {key}");
            let name = last_component(reader);
            if !fw.resolve_exists(reader) {
                return Fit::new(piece, false, format!("no {reader} on this firmware"));
            }
            let mut needle = key.as_bytes().to_vec();
            needle.push(0);
            if fw.file_contains(reader, &needle) {
                Fit::new(piece, true, format!("{name} names {key}"))
            } else {
                Fit::new(piece, false, format!("{name} does not name {key}: the setting reaches nothing"))
            }
        })
        .collect()
}

// ---- SpringBoard's environment and the web proxy

const PROXY_KEYS: [&str; 4] = ["ProxyAutoConfigEnable", "ProxyAutoConfigURLString", "ExceptionsList", "FTPPassive"];

/// SpringBoard's environment edits fit when every switch is read by an image of this firmware (readers) or by a
/// binary the bake injects with it (`also`).
pub fn environment(fw: &Firmware, switches: &[Vec<String>], also: &[(String, Vec<u8>)]) -> Fit {
    named("SpringBoard environment", fw, switches, also)
}

/// The web proxy's PAC fits when the firmware names every key the edit sets.
pub fn web_proxy(fw: &Firmware) -> Fit {
    let switches: Vec<Vec<String>> = PROXY_KEYS.iter().map(|k| vec![k.to_string()]).collect();
    named("web proxy PAC", fw, &switches, &[])
}

/// `piece (a, b/c, ...)` fits when each group of names is named as a C string by an image of this firmware or `also`.
fn named(label: &str, fw: &Firmware, switches: &[Vec<String>], also: &[(String, Vec<u8>)]) -> Fit {
    let label_switches = switches.iter().map(|g| g.join("/")).collect::<Vec<_>>().join(", ");
    let piece = format!("{label} ({label_switches})");
    let mut images = fw.readers();
    images.extend(also.iter().cloned());
    let mut read: Vec<String> = Vec::new();
    let mut unread: Vec<String> = Vec::new();
    for names in switches {
        let hit = names.iter().find_map(|n| {
            images.iter().find(|img| contains(&img.1, &c_string(n))).map(|img| format!("{n} by {}", img.0))
        });
        match hit {
            Some(h) => read.push(h),
            None => unread.push(names.join("/")),
        }
    }
    if !unread.is_empty() {
        return Fit::new(
            piece,
            false,
            format!("nothing in this firmware reads {}: the firmware's default decides", unread.join(", ")),
        );
    }
    Fit::new(piece, true, format!("read: {}", read.join("; ")))
}

// ---- boot-args

const AMFI_ARGS: [&str; 2] = ["amfi_allow_any_signature", "cs_enforcement_disable"];
const REQUIRED_ARGS: [&str; 1] = ["amfi_allow_any_signature"];

/// One Fit per boot-arg in `args` (flags like -v aside): fits when the kernel names it.
pub fn boot_args(kernel: Option<&[u8]>, args: &str) -> Vec<Fit> {
    args.split(' ')
        .filter(|s| !s.is_empty())
        .map(|a| a.split('=').next().unwrap_or(a))
        .filter(|name| !name.starts_with('-'))
        .map(|name| {
            let piece = format!("boot-arg {name}");
            let Some(kernel) = kernel else {
                return Fit::new(piece, false, "no decrypted kernelcache to check it against");
            };
            if contains(kernel, &c_string(name)) {
                return Fit::new(piece, true, "read by the kernel");
            }
            if AMFI_ARGS.contains(&name) {
                Fit::new(piece, false, "the kernel does not read it as a boot-arg")
            } else {
                Fit::new(piece, true, "not read by this kernel: no effect here")
            }
        })
        .collect()
}

/// A DeviceTree property the boot chain adds: read when the kernel names it, else inert there. Recorded, never required.
pub fn device_tree_property(kernel: Option<&[u8]>, node: &str, name: &str) -> Fit {
    let piece = format!("DeviceTree {node}/{name}");
    let Some(kernel) = kernel else {
        return Fit::new(piece, false, "no decrypted kernelcache to check it against");
    };
    if contains(kernel, &c_string(name)) {
        Fit::new(piece, true, "read by the kernel")
    } else {
        Fit::new(piece, true, "not read by this kernel: no effect here")
    }
}

/// The jailbreak sandbox patch (SandboxPatch): sb_evaluate located by the "bad opcode" string it reports. Fits when
/// the patch site is found; "no effect here" on iOS before the prelinked Seatbelt kext (nothing to patch).
pub fn sandbox_patch(kernel: Option<&[u8]>) -> Fit {
    let piece = "sandbox patch (sb_evaluate)";
    let Some(kernel) = kernel else {
        return Fit::new(piece, false, "no decrypted kernelcache to check it against");
    };
    match sandbox_patch::locate(kernel) {
        Ok(f) => Fit::new(piece, true, format!("sb_evaluate at 0x{:x} ({})", f.address, macho::hex_bytes(&f.original))),
        Err(sandbox_patch::PatchError::NoSandboxKext | sandbox_patch::PatchError::NoString) => {
            Fit::new(piece, true, "no Seatbelt sandbox kext on this kernel: nothing to patch")
        }
        Err(e) => Fit::new(piece, false, e.to_string()),
    }
}

/// bootArgs, recorded in `log`: amfi_allow_any_signature is required; an unread cs_enforcement_disable is a warning.
pub fn check_boot_args(log: &Log, kernel: Option<&[u8]>, args: &str) -> Result<()> {
    for f in boot_args(kernel, args) {
        let name = f.piece.strip_prefix("boot-arg ").unwrap_or(&f.piece).to_string();
        log.check(f, REQUIRED_ARGS.contains(&name.as_str()), "kept: inert here")?;
    }
    Ok(())
}

// ---- AppSync

/// What libappsync hooks in its host (contrib/appsync appsync.c). Each group: alternatives.
const APPSYNC_CALLS: [&[&str]; 5] = [
    &["_MISValidateSignatureAndCopyInfo", "_MISValidateSignature"],
    &["_SecCertificateCreateWithData"],
    &["_SecCertificateCopySubjectSummary"],
    &["_kMISValidationInfoSignerCertificate"],
    &["_kMISValidationInfoValidatedByProfile"],
];

/// libappsync fits the installation service `host` when the host's process imports what the dylib hooks, the dylib's
/// getprogname gate names the host's program, and the dylib loads in it.
pub fn app_sync(fw: &Firmware, host: &str, dylib: &[u8]) -> Fit {
    let name = last_component(host).to_string();
    let piece = format!("{APPSYNC_DYLIB} (in {name})");
    if !fw.resolve_exists(host) {
        return Fit::new(piece, false, format!("no {host} on this firmware"));
    }
    let mut why: Vec<String> = Vec::new();
    if !contains(dylib, &c_string(&name)) {
        why.push(format!(
            "its getprogname gate does not name {name}: the Security interposes would pass through there"
        ));
    }
    let images = fw.loaded(host);
    let mut hooked: Vec<String> = Vec::new();
    let mut unhooked: Vec<String> = Vec::new();
    for group in APPSYNC_CALLS {
        let hit = group.iter().find_map(|s| {
            images
                .iter()
                .find(|img| fw.imports(img).map(|i| i.contains(*s)).unwrap_or(false))
                .map(|img| format!("{} by {}", &s[1..], last_component(img)))
        });
        match hit {
            Some(h) => hooked.push(h),
            None => unhooked.push(group.iter().map(|s| &s[1..]).collect::<Vec<_>>().join(" or ")),
        }
    }
    if !unhooked.is_empty() {
        why.push(format!("nothing in {name}'s process imports {}, which the dylib hooks", unhooked.join(", ")));
    }
    let l = loads(&piece, dylib, fw, Some(host));
    if !l.fits {
        why.push(l.proof.clone());
    }
    if !why.is_empty() {
        return Fit::new(piece, false, why.join("; "));
    }
    Fit::new(piece, true, format!("hooks {}; gate names {name}; {}", hooked.join(", "), l.proof))
}

/// appsync-launch fits when it loads, names the dylib where the bake puts it, and the service `program` it execs is
/// a Mach-O this CPU runs.
pub fn app_sync_launcher(fw: &Firmware, program: &str, launcher: &[u8]) -> Fit {
    let piece = format!("{APPSYNC_LAUNCHER} (Lockbot's installation_proxy service)");
    let runs = fw.data(program).map(|bin| MachO32::slice(&bin, &fw.arch).is_some()).unwrap_or(false);
    if !runs {
        return Fit::new(
            piece,
            false,
            format!("the service it execs, {program}, is not a Mach-O this CPU runs on this firmware"),
        );
    }
    if !contains(launcher, &c_string(&format!("/{APPSYNC_PATH}"))) {
        return Fit::new(piece, false, format!("it does not insert /{APPSYNC_PATH}, where the bake puts the dylib"));
    }
    let l = loads(&piece, launcher, fw, None);
    if !l.fits {
        return l;
    }
    Fit::new(piece, true, format!("execs {} with /{APPSYNC_PATH} inserted; {}", last_component(program), l.proof))
}

/// AppSync's pieces checked into `log`: libappsync in installd's job's program, else (2.x) in Lockbot's
/// installation_proxy service with appsync-launch. `name` is the dylib's file name in `helpers` (the legacy build on
/// an old dyld); the piece string stays libappsync.dylib's.
pub fn check_app_sync(log: &Log, fw: &Firmware, helpers: &Path, name: &str) -> Result<()> {
    let read = |n: &str| -> Result<Vec<u8>> {
        let u = helpers.join(n);
        std::fs::read(&u).map_err(|_| crate::internal!("guest helper {n} missing from {}", helpers.display()))
    };
    let dylib = read(name)?;
    let installd = ["com.apple.mobile.installd.plist", "com.apple.installd.plist"]
        .into_iter()
        .map(|j| format!("{DAEMONS}/{j}"))
        .find(|j| fw.resolve_exists(j));
    if let Some(job) = installd {
        let Some(host) = plist_dict(fw, &job).and_then(|d| program(&d)) else {
            return log
                .check(Fit::new(APPSYNC_DYLIB, false, format!("{job} names no program")), true, "left out")
                .map(|_| ());
        };
        log.check(app_sync(fw, &host, &dylib), true, "left out")?;
        return Ok(());
    }
    let services = plist_dict(fw, "System/Library/Lockdown/Services.plist");
    let host = services
        .as_ref()
        .and_then(|s| s.get("com.apple.mobile.installation_proxy"))
        .and_then(Value::as_dictionary)
        .and_then(program);
    let Some(host) = host else {
        return log
            .check(
                Fit::new(APPSYNC_DYLIB, false, "no installd job and no Lockbot installation_proxy service"),
                true,
                "left out",
            )
            .map(|_| ());
    };
    log.check(app_sync(fw, &host, &dylib), true, "left out")?;
    log.check(app_sync_launcher(fw, &host, &read(APPSYNC_LAUNCHER)?), true, "left out")?;
    Ok(())
}

/// Historical cache-patch inspection, retained for the oracle/research tests. Preparation uses process-local hooks.
pub fn app_sync_cache(fw: &Firmware) -> Option<Fit> {
    let cache = fw.cache()?;
    let piece = format!("AppSync shared-cache patch ({})", crate::shared_cache::app_sync_cache_patch::TARGET);
    Some(match crate::shared_cache::app_sync_cache_patch::locate(cache) {
        Ok((va, _, word)) => Fit::new(piece, true, format!("entry {} at {}", macho::hex_bytes(&word), hex(va))),
        Err(e) => Fit::new(piece, false, e.to_string()),
    })
}

// ---- The GL front end

/// Does the image installed as `install` name the C string `s`? Cached images: their own __cstring /
/// __objc_methname / __objc_methtype sections; files: anywhere in the file.
fn gles_names(fw: &Firmware, install: &str, s: &str) -> bool {
    if let Some(cache) = fw.cache() {
        if let Some(img) = cache.image(&format!("/{install}")) {
            return ["__objc_methname", "__cstring", "__objc_methtype"]
                .into_iter()
                .any(|sec| cache.c_strings(img, sec).iter().any(|c| c == s));
        }
    }
    fw.data(install).map(|d| contains(&d, &c_string(s))).unwrap_or(false)
}

/// The text from `prefix` to the end of the first C string holding it.
fn gles_string(fw: &Firmware, install: &str, prefix: &str) -> Option<String> {
    if let Some(cache) = fw.cache() {
        if let Some(img) = cache.image(&format!("/{install}")) {
            for sec in ["__cstring", "__objc_methtype"] {
                if let Some(hit) = cache.c_strings(img, sec).into_iter().find(|c| c.contains(prefix)) {
                    if let Some(at) = hit.find(prefix) {
                        return Some(hit[at..].to_string());
                    }
                }
            }
            return None;
        }
    }
    let d = fw.data(install)?;
    let at = memchr::memmem::find(&d, prefix.as_bytes())?;
    let end = d[at..].iter().position(|&c| c == 0).map(|i| at + i).unwrap_or(d.len());
    Some(String::from_utf8_lossy(&d[at..end]).into_owned())
}

/// The defined external symbols of a Mach-O's slice for this firmware's CPU.
fn exported(bin: &[u8], arch: &str) -> HashSet<String> {
    MachO32::slice(bin, arch).map(|(m, _)| m.exports()).unwrap_or_default()
}

/// The one OpenGLES replacement fits when everything it looks up at run time is in this (pristine) firmware.
pub fn gles_front_end(fw: &Firmware, binary: &[u8], names_table: &str) -> Fit {
    let piece = "OpenGLES front end (contrib/gles-public)";
    let l = loads(piece, binary, fw, None);
    if !l.fits {
        return l;
    }
    let Some(stock) = fw.exports(&format!("/{OPENGLES}")) else {
        return Fit::new(piece, false, "this firmware has no OpenGLES.framework");
    };
    let fe = exported(binary, &fw.arch);
    let mut lost: Vec<String> = stock.difference(&fe).cloned().collect();
    lost.sort();
    if !lost.is_empty() {
        let shown = lost.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
        let extra = if lost.len() > 6 { format!(" (+{})", lost.len() - 6) } else { String::new() };
        return Fit::new(
            piece,
            false,
            format!("the stock OpenGLES exports {shown}{extra} that the front end does not"),
        );
    }
    let mut proof = vec![format!("exports all {} of the stock OpenGLES's names", stock.len())];
    for sel in ["nativeWindow", "drawableProperties"] {
        if !gles_names(fw, QUARTZCORE, sel) {
            return Fit::new(piece, false, format!("QuartzCore does not name {sel}: no CAEAGLLayer drawable to bind"));
        }
    }
    let tag = "{_EAGLNativeWindowObject=";
    let Some(window) = [QUARTZCORE, OPENGLES].iter().find_map(|i| gles_string(fw, i, tag)) else {
        return Fit::new(piece, false, "no _EAGLNativeWindowObject @encode in QuartzCore or OpenGLES");
    };
    let body: String = window[tag.len()..].chars().take_while(|&c| c != '}').collect();
    let callbacks = body.matches("^?").count();
    if !(body.starts_with("\"version\"i") || body.starts_with('i')) || callbacks < 5 {
        return Fit::new(piece, false, format!("the native window is {window}, not a version word and five callbacks"));
    }
    proof.push(format!("native window: version + {callbacks} callbacks"));
    let calls: &str;
    if fw
        .exports(&format!("/{IOSURFACE}"))
        .map(|s| SURFACE_CALLS.iter().all(|c| s.contains(&format!("_IOSurface{c}"))))
        .unwrap_or(false)
    {
        calls = "IOSurface";
    } else if let Some(lib) = CORE_SURFACES.iter().find(|c| fw.exports(&format!("/{c}")).is_some()) {
        let s = fw.exports(&format!("/{lib}")).unwrap_or_default();
        let ok = SURFACE_CALLS.iter().all(|c| {
            let nm = if *c == "GetPixelFormat" { "GetPixelFormatType".to_string() } else { c.to_string() };
            s.contains(&format!("_CoreSurfaceBuffer{nm}"))
        });
        if ok {
            calls = "CoreSurface";
        } else {
            return Fit::new(
                piece,
                false,
                format!("neither IOSurface nor CoreSurface exports the surface calls ({})", SURFACE_CALLS.join(", ")),
            );
        }
    } else {
        return Fit::new(
            piece,
            false,
            format!("neither IOSurface nor CoreSurface exports the surface calls ({})", SURFACE_CALLS.join(", ")),
        );
    }
    proof.push(format!("surfaces through {calls}"));
    let fb = fw.exports(&format!("/{IO_MOBILE_FRAMEBUFFER}")).unwrap_or_default();
    for (sel, want) in [
        ("swapNotification:forTransaction:onLayer:", vec!["_IOMobileFramebufferSwapSignal"]),
        (
            "sendNotification:forTransaction:onLayer:",
            vec!["_IOMobileFramebufferSwapSignal", "_IOMobileFramebufferGetMainDisplay", "_IOMobileFramebufferGetID"],
        ),
    ] {
        if !gles_names(fw, QUARTZCORE, sel) {
            continue;
        }
        let missing: Vec<&str> = want.iter().copied().filter(|w| !fb.contains(*w)).collect();
        if !missing.is_empty() {
            return Fit::new(
                piece,
                false,
                format!("QuartzCore sends {sel} and IOMobileFramebuffer lacks {}", missing.join(", ")),
            );
        }
        let short: String = sel.chars().take_while(|&c| c != ':').collect();
        proof.push(format!("{short} signals through IOMobileFramebufferSwapSignal"));
    }
    if gles_names(fw, QUARTZCORE, "GetMacroContextPrivate") || gles_names(fw, COREIMAGE, "GetMacroContextPrivate") {
        let tag = "{__GLIFunctionDispatchRec=";
        // read from the shared cache at run time, so a firmware without one has no macro context to give
        let enc = fw.cache().and(gles_string(fw, OPENGLES, tag));
        let fields = enc.as_ref().and_then(|e| gli_fields(format!("{e}}}").as_bytes()));
        let (Some(enc), Some(fields)) = (enc, fields) else {
            return Fit::new(
                piece,
                false,
                "QuartzCore or CoreImage asks for a macro context and the shared cache's OpenGLES carries no __GLIFunctionDispatchRec @encode",
            );
        };
        if fields.is_empty() {
            // 6.0 beta 1's: count the slots and name them from the exports
            let slots_body: String = enc[tag.len()..].chars().take_while(|&c| c != '}').collect();
            let slots = slots_body.matches("^?").count();
            if slots == 0 {
                return Fit::new(piece, false, "the macro context's dispatch @encode has neither fields nor slots");
            }
            proof
                .push(format!("macro context: {slots} unnamed dispatch slots, named by the stock exports at run time"));
        }
        let known: HashSet<String> = gles_names_table(names_table);
        let unknown: Vec<String> = fields.iter().filter(|f| !known.contains(*f)).cloned().collect();
        if !unknown.is_empty() {
            let shown = unknown.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
            let extra = if unknown.len() > 6 { format!(" (+{})", unknown.len() - 6) } else { String::new() };
            return Fit::new(
                piece,
                false,
                format!("the macro context's dispatch fields {shown}{extra} are no rows of gles-names.h"),
            );
        }
        if !fields.is_empty() {
            proof.push(format!("macro context: {} dispatch fields, all named", fields.len()));
        }
    }
    proof.push(if fw.resolve_exists(SGX_ENGINE) {
        "ES 1.1 + 2.0 (GLEngine.bundle)".to_string()
    } else {
        "ES 1.1 only (no GLEngine.bundle)".to_string()
    });
    proof.push(l.proof);
    Fit::new(piece, true, proof.join("; "))
}

/// The GLES_FN rows of gles-names.h: `GLES_FN(<anything>, <field>, ...)`; the captured second argument is the name.
fn gles_names_table(table: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in table.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("GLES_FN(") else { continue };
        let mut it = rest.splitn(2, ',');
        let (Some(_first), Some(after)) = (it.next(), it.next()) else { continue };
        let field: String = after.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !field.is_empty() {
            // the row is GLES_FN(fn, field, ...): a comma must follow the field
            if after.trim_start()[field.len()..].trim_start().starts_with(',') {
                out.insert(field);
            }
        }
    }
    out
}

/// The dispatch_field list from the first `{__GLIFunctionDispatchRec=...}` @encode in `data` (GLIDispatch.fields).
pub fn gli_fields(data: &[u8]) -> Option<Vec<String>> {
    let needle = b"{__GLIFunctionDispatchRec=";
    let hit = memchr::memmem::find(data, needle)?;
    let mut i = hit + needle.len();
    let mut end = i;
    while end < data.len() && data[end] != b'}' {
        end += 1;
    }
    if end >= data.len() {
        return None;
    }
    let mut out = Vec::new();
    while i < end {
        if data[i] != b'"' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < end && data[j] != b'"' {
            j += 1;
        }
        if j >= end {
            break;
        }
        if j > i + 1 {
            out.push(String::from_utf8_lossy(&data[i + 1..j]).into_owned());
            i = j + 1;
        } else {
            i += 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One optional piece for whichever place takes it: every place is recorded, and the misfits are warned about
    /// only when no place fits.
    #[test]
    fn check_any_warns_only_when_no_place_fits() {
        let warnings = std::rc::Rc::new(RefCell::new(Vec::<String>::new()));
        let w = warnings.clone();
        let log = Log::new(move |s| w.borrow_mut().push(s.to_string()));
        let mounter = Fit::new("q (mounter)", false, "names neither");
        let arbitrator = Fit::new("q (arbitrator)", true, "raises it");
        assert_eq!(log.check_any(vec![mounter.clone(), arbitrator.clone()], "left out"), vec![false, true]);
        assert!(warnings.borrow().is_empty());
        assert_eq!(*log.fits.borrow(), vec![mounter.clone(), arbitrator]);
        assert_eq!(log.check_any(vec![mounter.clone(), mounter.clone()], "left out"), vec![false, false]);
        assert_eq!(*warnings.borrow(), vec![mounter.warning("left out"), mounter.warning("left out")]);
        assert_eq!(log.check_any(vec![], "left out"), Vec::<bool>::new());
        assert_eq!(warnings.borrow().len(), 2);
    }

    /// GLIDispatch.fields, and the gles-names.h row parse.
    #[test]
    fn gli_fields_and_table() {
        assert_eq!(
            gli_fields(b"xx{__GLIFunctionDispatchRec=\"foo\"^?\"\"\"bar\"^?}").unwrap(),
            vec!["foo".to_string(), "bar".to_string()]
        );
        assert!(gli_fields(b"no encode").is_none());
        let table = "GLES_FN(glFoo,  foo,  0, 1, 0)\nGLES_FN(glBar, bar, 0)\n";
        let known = gles_names_table(table);
        assert!(known.contains("foo") && known.contains("bar") && known.len() == 2);
    }
}

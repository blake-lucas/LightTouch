//! The jailbreak option: afc2 and Cydia with Substrate from the bootstrap (SystemEdits/Cydia.swift and
//! SystemEdits.installAFC2/afc2Copy).
//!
//! The bootstrap is saurik's freeze.tar as Legacy iOS Kit ships it for the jailbroken restores it builds for iOS 3.1
//! to 6.x (github.com/LukeZGD/Legacy-iOS-Kit, resources/jailbreak/freeze.tar.gz, pinned by commit): Cydia 1.1.30,
//! apt, dpkg, bash and the base packages, dpkg's status already listing them, and Cydia's com.saurik.Cydia.Startup
//! job. On the first boot the Startup job writes the firmware package and runs uicache, so SpringBoard lists
//! Cydia.app; the bootstrap's own SpringBoard preference SBShowNonDefaultSystemApps shows it there.
//!
//! Substrate (MobileSubstrate 0.9.7114 and its Safe Mode extension) comes as Legacy iOS Kit adds it: cydiasubstrate.tar
//! puts its .debs in Cydia's AutoInstall folder, which the Startup job installs with dpkg on the first boot. It goes
//! on 3.x to 6.x; 7.x gets none (its root stays read-only, and its install script starts Substrate only from
//! /etc/rc.d, which only a jailbreak's untether runs). On 3.x Cydia HTTPatch (cydiahttpatch.tar) points Cydia at
//! http, as 3.x's TLS can't reach its repositories. Patcyh, which freeze.tar carries for iOS 8.3 and later, comes
//! out, as Legacy iOS Kit takes it out below 8.3: with Substrate it would load into installd beside AppSync.
//!
//! Cydia's default sources are seeded as its own migration reads them (/var/lib/cydia/metadata.plist, Version 1, so
//! its first launch adds none of its own): BigBoss, and ModMyi and ZodTTD at the archives SaurikIT serves them from
//! now, where Cydia 1.1.30 would add hosts that no longer answer and each refresh would fail on them.
//!
//! freeze.tar's /private/var/Keychains (a present-day TrustStore.sqlite3) stays out: iOS's trust store is the
//! firmware's own and the web proxy's trust only. Extracted, it was root's and securityd couldn't open it.
//!
//! Swift runs /usr/bin/tar into a mounted volume. Here the archives are read in-process and extracted into the
//! volume's editor as `tar -xk` would as a normal user: what the firmware already has is kept, new items get the
//! archive's permission bits less the usual umask (022) and without set-user-ID and set-group-ID, which the caller
//! restores from `modes` along with the owners.
use crate::error::{Code, Error, Result};
use crate::hfs::Editor;
use crate::hfs::editor::Kind;
use crate::plistedit::{self, Format};
use plist::{Dictionary, Value};
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// What install_jailbreak did: log lines, the paths to make root's and mobile's, and set-ID modes to restore.
pub struct Jailbreak {
    pub afc2: String,
    pub cydia: String,
    pub root: Vec<String>,
    pub mobile: Vec<String>,
    pub modes: Vec<(String, u16)>,
}

/// What install_cydia did: its log line, the paths to make root's and mobile's, and the set-ID modes to restore.
pub struct Cydia {
    pub line: String,
    pub root: Vec<String>,
    pub mobile: Vec<String>,
    pub modes: Vec<(String, u16)>,
}

pub const SOURCE: &str =
    "https://raw.githubusercontent.com/LukeZGD/Legacy-iOS-Kit/4b0a582f4d53e105be373b495734c8ed639fd634/resources/jailbreak/";
/// The bootstrap, then what Legacy iOS Kit adds beside it: Substrate and Cydia HTTPatch. (name, SHA-1)
pub const FILES: [(&str, &str); 3] = [
    ("freeze.tar.gz", "c943e5ece72b7b71da589c22663a8f9b3b3d1190"),
    ("cydiasubstrate.tar", "264f47938acc09e6cc2890a3cc7c7988a6822da3"),
    ("cydiahttpatch.tar", "3a1302c6294fe290c6d52186dd4fe56cda7fd661"),
];
pub const NAME: &str = FILES[0].0;
pub const NO_STASH: &str = ".cydia_no_stash";
pub const METADATA: &str = "private/var/lib/cydia/metadata.plist";
/// What isn't extracted: the bootstrap's trust store.
pub const EXCLUDED: &str = "private/var/Keychains";
/// Cydia's default third-party sources as they are reachable today (its Sources dictionary, keyed as
/// CydiaAddSource keys them).
pub const SOURCES: [&str; 3] =
    ["http://apt.thebigboss.org/repofiles/cydia/", "http://modmyi.saurik.com/", "http://zodttd.saurik.com/repo/cydia/"];
pub const SPRINGBOARD_PREFS: &str = "private/var/mobile/Library/Preferences/com.apple.springboard.plist";
pub const PATCYH: [&str; 6] = [
    "Library/MobileSubstrate/DynamicLibraries/patcyh.dylib",
    "Library/MobileSubstrate/DynamicLibraries/patcyh.plist",
    "usr/lib/libpatcyh.dylib",
    "private/var/lib/dpkg/info/com.saurik.patcyh.list",
    "private/var/lib/dpkg/info/com.saurik.patcyh.postrm",
    "private/var/lib/dpkg/info/com.saurik.patcyh.extrainst_",
];

pub const DAEMONS: &str = "System/Library/LaunchDaemons";
pub const SPRINGBOARD_JOB: &str = "System/Library/LaunchDaemons/com.apple.SpringBoard.plist";
pub const LOCKDOWN_SERVICES: &str = "System/Library/Lockdown/Services.plist";
pub const AFCD: &str = "usr/libexec/afcd";
pub const AFC2D: &str = "usr/libexec/afc2d";
pub const AFC2_JOB: &str = "System/Library/LaunchDaemons/com.apple.afc2.plist";
const SYSTEM_VERSION: &str = "System/Library/CoreServices/SystemVersion.plist";
/// The umask tar's new files and folders get their modes through.
const UMASK: u32 = 0o022;

/// The bootstrap in `dir`, each file downloaded there first when missing or other bytes.
pub fn bootstrap(dir: &Path, log: &dyn Fn(&str)) -> Result<PathBuf> {
    bootstrap_with(dir, &download, log)
}

/// `bootstrap` with its download (`url`, the file to write) given: the tests' stand-in for the network.
pub fn bootstrap_with(dir: &Path, download: &dyn Fn(&str, &Path) -> Result<()>, log: &dyn Fn(&str)) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| crate::internal!("{}: {e}", dir.display()))?;
    for (name, sha1) in FILES {
        let file = dir.join(name);
        if file.is_file() && sha1_file(&file)? == sha1 {
            continue;
        }
        let url = format!("{SOURCE}{name}");
        let part = dir.join(format!(".{name}.download"));
        log(&format!("Cydia: {url}"));
        let fetched = download(&url, &part).and_then(|()| {
            let got = sha1_file(&part)?;
            if got != sha1 {
                return Err(Error::new(Code::ShaMismatch, format!("Cydia: {name} SHA-1 {got}, not {sha1}")));
            }
            let _ = std::fs::remove_file(&file);
            std::fs::rename(&part, &file).map_err(|e| crate::internal!("{}: {e}", file.display()))
        });
        let _ = std::fs::remove_file(&part);
        fetched?;
    }
    Ok(dir.join(NAME))
}

/// `url` into the file `to`, over HTTPS.
pub fn download(url: &str, to: &Path) -> Result<()> {
    use std::time::Duration;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(concat!("LightTouch-firmwarekit/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .https_only(true)
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| crate::internal!("Cydia: {url}: {e}"))?;
    let mut file = std::fs::File::create(to).map_err(|e| crate::internal!("{}: {e}", to.display()))?;
    std::io::copy(&mut response.body_mut().as_reader(), &mut file).map_err(|e| crate::internal!("Cydia: {url}: {e}"))?;
    Ok(())
}

fn sha1_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// What the jailbreak option leaves on the system volume (its data volume, if any, on private/var): afc2
/// (install_afc2) and Cydia with Substrate (install_cydia from `bootstrap`). `typein_by_substrate` (the iPod touch
/// 2G's guest helpers): it_typein, where the volume has it, loads into SpringBoard as a Substrate extension instead of
/// through SpringBoard's DYLD_INSERT_LIBRARIES: 3.x hands that variable on to the apps SpringBoard launches, and cydo
/// (Cydia's root helper) refuses a parent whose job has any DYLD_ variable, so Cydia could neither install nor link
/// its sources.
pub fn install_jailbreak(system: &mut Editor, bootstrap: &Path, typein_by_substrate: bool) -> Result<Jailbreak> {
    let (afc2_line, afc2_root) = install_afc2(system)?;
    let cydia = install_cydia(system, bootstrap)?;
    let mut root = afc2_root;
    root.extend(cydia.root);
    let typein = "usr/lib/it_typein.dylib";
    if typein_by_substrate && system.exists(typein) {
        plistedit::rewrite(system, SPRINGBOARD_JOB, |d| {
            if d.get("Label").and_then(Value::as_string) != Some("com.apple.SpringBoard") {
                return Err(crate::unsupported!("{SPRINGBOARD_JOB}: not SpringBoard's job"));
            }
            let env = plistedit::dict(d, "EnvironmentVariables");
            let libs: Vec<String> = env
                .get("DYLD_INSERT_LIBRARIES")
                .and_then(Value::as_string)
                .unwrap_or("")
                .split(':')
                .filter(|l| !l.is_empty() && *l != format!("/{typein}"))
                .map(str::to_string)
                .collect();
            if libs.is_empty() {
                env.remove("DYLD_INSERT_LIBRARIES");
            } else {
                env.insert("DYLD_INSERT_LIBRARIES".into(), Value::String(libs.join(":")));
            }
            Ok(())
        })?;
        let dir = "Library/MobileSubstrate/DynamicLibraries";
        system.mkdirs(dir)?;
        let link = format!("{dir}/it_typein.dylib");
        if system.lstat(&link)?.is_some() {
            let _ = system.remove(&link);
        }
        system.symlink(&link, &format!("/{typein}"))?;
        let filter = r#"{ Filter = { Bundles = ( "com.apple.springboard" ); }; }"#;
        system.write(&format!("{dir}/it_typein.plist"), filter.as_bytes().to_vec(), Some(0o644))?;
        root.extend([
            "Library/MobileSubstrate".to_string(),
            dir.to_string(),
            link,
            format!("{dir}/it_typein.plist"),
        ]);
    }
    Ok(Jailbreak { afc2: afc2_line, cydia: cydia.line, root, mobile: cydia.mobile, modes: cydia.modes })
}

/// afc2, as the jailbreaks of the time added it: a com.apple.afc2 lockdown service serving "/" as root, so the host
/// can browse the whole file system over USB. Through 7.0 it is the stock afcd under old-style lockdown at "/".
/// 7.1's afcd (afc-218) takes neither --lockdown nor -d: it is only an XPC service at the media folder. There afc2
/// is a copy of it, afc2d, serving "/" as its own XPC service com.apple.afc2 from a launchd job without the stock
/// job's user (so as root). Returns the log line and the files it added (root's).
pub fn install_afc2(system: &mut Editor) -> Result<(String, Vec<String>)> {
    let stock = system.read(AFCD)?;
    let old_style = find(&stock, b"--lockdown").is_some();
    plistedit::rewrite(system, LOCKDOWN_SERVICES, |services| {
        if !services.contains_key("com.apple.afc") {
            return Err(crate::unsupported!("{LOCKDOWN_SERVICES} has no com.apple.afc service"));
        }
        let entry = if old_style {
            plistedit::dictionary([
                ("AllowUnactivatedService", true.into()),
                ("Label", "com.apple.afc2".into()),
                ("ProgramArguments", plistedit::strings(&["/usr/libexec/afcd", "--lockdown", "-d", "/"])),
            ])
        } else {
            plistedit::dictionary([
                ("AllowUnactivatedService", true.into()),
                ("Label", "com.apple.afc2".into()),
                ("XPCServiceName", "com.apple.afc2".into()),
            ])
        };
        services.insert("com.apple.afc2".into(), Value::Dictionary(entry));
        Ok(())
    })?;
    if old_style {
        return Ok(("afc2: com.apple.afc2 runs afcd at / (Lockdown/Services.plist)".into(), Vec::new()));
    }
    system.write(AFC2D, afc2_copy(&stock)?, Some(0o755))?;
    let job_path = format!("{DAEMONS}/com.apple.afcd.plist");
    let job = plistedit::read(system, &job_path).ok().flatten();
    let Some(mut job) = job.filter(|j| j.get("Label").and_then(Value::as_string) == Some("com.apple.afcd")) else {
        return Err(crate::unsupported!("com.apple.afcd.plist: not com.apple.afcd's job"));
    };
    job.insert("Label".into(), "com.apple.afc2".into());
    job.insert("MachServices".into(), Value::Dictionary(plistedit::dictionary([("com.apple.afc2", true.into())])));
    job.insert("ProgramArguments".into(), plistedit::strings(&[format!("/{AFC2D}")]));
    job.remove("UserName");
    system.write(AFC2_JOB, plistedit::serialize(&Value::Dictionary(job), Format::Xml)?, None)?;
    Ok((
        "afc2: com.apple.afc2 is afc2d, afcd at / as an XPC service (Lockdown/Services.plist)".into(),
        vec![AFC2D.to_string(), AFC2_JOB.to_string()],
    ))
}

/// 7.1's afcd serving "/" as the XPC service com.apple.afc2: its root and service name replaced in place (each string
/// by one no longer, padded with NULs), then signed again ad hoc, keeping its entitlements.
pub fn afc2_copy(afcd: &[u8]) -> Result<Vec<u8>> {
    let mut d = afcd.to_vec();
    for (from, to) in [("/private/var/mobile/Media", "/"), ("com.apple.afcd", "com.apple.afc2")] {
        let f = format!("{from}\0").into_bytes();
        let mut t = format!("{to}\0").into_bytes();
        t.resize(f.len(), 0);
        let mut found = false;
        let mut at = 0;
        while let Some(i) = find(&d[at..], &f) {
            d[at + i..at + i + f.len()].copy_from_slice(&t);
            at += i + f.len();
            found = true;
        }
        if !found {
            return Err(crate::unsupported!("afcd: no {from}"));
        }
    }
    crate::activation::signed(&d)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A tar member as extracted: its volume path, type, mode, link target and data.
struct Member {
    path: String,
    kind: tar::EntryType,
    mode: u32,
    link: Option<String>,
    data: Vec<u8>,
}

/// Every member of the tar (gzipped or not) at `path`.
fn read_tar(path: &Path) -> Result<Vec<Member>> {
    let bytes = std::fs::read(path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let reader: Box<dyn Read> = if bytes.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(std::io::Cursor::new(bytes)))
    } else {
        Box::new(std::io::Cursor::new(bytes))
    };
    let bad = |e: std::io::Error| crate::internal!("tar {name}: {e}");
    let mut archive = tar::Archive::new(reader);
    let mut out = Vec::new();
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let raw = entry.path().map_err(bad)?.to_string_lossy().into_owned();
        let kind = entry.header().entry_type();
        let mode = entry.header().mode().map_err(bad)?;
        let link = entry.link_name().map_err(bad)?.map(|l| l.to_string_lossy().into_owned());
        let mut data = Vec::new();
        if kind.is_file() || kind == tar::EntryType::Continuous {
            entry.read_to_end(&mut data).map_err(bad)?;
        }
        out.push(Member { path: entry_path(&raw), kind, mode, link, data });
    }
    Ok(out)
}

/// A tar member name as a volume path: no leading ./, no leading or trailing /.
pub fn entry_path(name: &str) -> String {
    name.strip_prefix("./").unwrap_or(name).trim_matches('/').to_string()
}

fn excluded(path: &str) -> bool {
    path == EXCLUDED || path.starts_with(&format!("{EXCLUDED}/"))
}

/// One member into the volume as `tar -xk` leaves it: anything already at its path (a dangling symlink too) is kept.
fn extract(ed: &mut Editor, m: &Member) -> Result<()> {
    if m.path.is_empty() || excluded(&m.path) || ed.lstat(&m.path)?.is_some() {
        return Ok(());
    }
    use tar::EntryType as T;
    let perm = (m.mode & 0o1777 & !UMASK) as u16;
    let parent = |ed: &mut Editor| -> Result<()> {
        match m.path.rsplit_once('/') {
            Some((dir, _)) => ed.mkdirs(dir),
            None => Ok(()),
        }
    };
    match m.kind {
        T::Directory => {
            parent(ed)?;
            ed.mkdir(&m.path, Some(perm))
        }
        T::Regular | T::Continuous => {
            parent(ed)?;
            ed.write(&m.path, m.data.clone(), Some(perm))
        }
        T::Symlink => {
            parent(ed)?;
            ed.symlink(&m.path, m.link.as_deref().unwrap_or(""))
        }
        // A hard link (freeze.tar's bin/gunzip to bin/uncompress) becomes a copy: the editor writes no hard links.
        T::Link => {
            let target = entry_path(m.link.as_deref().unwrap_or(""));
            let Some(meta) = ed.stat(&target)? else {
                return Err(crate::internal!("tar: {} links to a missing {target}", m.path));
            };
            let data = ed.read(&target)?;
            parent(ed)?;
            ed.write(&m.path, data, Some(meta.perm & 0o1777))
        }
        _ => Ok(()), // devices and FIFOs: none in the bootstrap, and nothing a volume edit could make
    }
}

/// The bootstrap extracted into the system volume, keeping every file the firmware already has (tar -k). Returns
/// the log line and the volume paths to make root's and mobile's: what it added, mobile's under
/// /private/var/mobile, root's elsewhere (its symbolic links too). `modes`: the set-user-ID and set-group-ID files it
/// added (cydo, su, login, passwd) with their archive modes, which tar, not run as root, doesn't restore.
pub fn install_cydia(system: &mut Editor, bootstrap: &Path) -> Result<Cydia> {
    let exists = |ed: &Editor, rel: &str| matches!(ed.lstat(rel), Ok(Some(_)));
    let is_link = |ed: &Editor, rel: &str| matches!(ed.lstat(rel), Ok(Some(m)) if m.kind == Kind::Symlink);
    // Substrate on 3.x to 6.x, HTTPatch on 3.x
    let major: u32 = plistedit::read(system, SYSTEM_VERSION)
        .ok()
        .flatten()
        .and_then(|d| d.get("ProductVersion").and_then(Value::as_string).map(str::to_string))
        .and_then(|v| v.split('.').next().and_then(|m| m.parse().ok()))
        .unwrap_or(0);
    let dir = bootstrap.parent().unwrap_or(Path::new(""));
    let packages: Vec<PathBuf> = [(FILES[0].0, true), (FILES[1].0, (3..=6).contains(&major)), (FILES[2].0, major == 3)]
        .into_iter()
        .filter(|p| p.1)
        .map(|p| dir.join(p.0))
        .collect();
    let mut archives = Vec::new();
    let mut entries: Vec<String> = Vec::new();
    let mut special: Vec<(String, u16)> = Vec::new();
    for p in &packages {
        let members = read_tar(p)?;
        for m in &members {
            if !m.path.is_empty() && !excluded(&m.path) {
                entries.push(m.path.clone());
            }
            // a regular file with the set-user-ID or set-group-ID bit: its path and permission bits
            if (m.kind == tar::EntryType::Regular || m.kind == tar::EntryType::Continuous) && m.mode & 0o6000 != 0 {
                special.push((m.path.clone(), (m.mode & 0o6777) as u16));
            }
        }
        archives.push(members);
    }
    let added: HashSet<String> = entries.iter().filter(|e| !exists(system, e)).cloned().collect();
    for members in &archives {
        for m in members {
            extract(system, m)?;
        }
    }
    drop(archives);
    if !exists(system, "Applications/Cydia.app/Cydia") {
        return Err(crate::internal!("Cydia bootstrap: no Applications/Cydia.app/Cydia after extracting"));
    }
    for rel in PATCYH {
        if exists(system, rel) {
            let _ = system.remove(rel);
        }
    }
    for rel in ["private/var/lib/dpkg/status", "private/var/lib/dpkg/available"] {
        let Some(text) = system.read(rel).ok().and_then(|b| String::from_utf8(b).ok()) else { continue };
        let kept: Vec<&str> = text.split("\n\n").filter(|p| !p.starts_with("Package: com.saurik.patcyh\n")).collect();
        system.write(rel, kept.join("\n\n").into_bytes(), None)?;
    }
    entries.retain(|e| !PATCYH.contains(&e.as_str()));
    // SpringBoard lists a system app outside its own set only with this key (the bootstrap's copy of the file gives
    // it, unless the firmware has the file already).
    let had_prefs = exists(system, SPRINGBOARD_PREFS);
    plistedit::seed(system, SPRINGBOARD_PREFS, |d| {
        d.insert("SBShowNonDefaultSystemApps".into(), true.into());
        Ok(())
    })?;
    // Cydia's first launch would otherwise stash /Applications, /usr/share and the rest onto the data volume
    // ("Preparing Filesystem", then exit) to free a stock-sized root; this root is grown, so no stash.
    system.write(NO_STASH, Vec::new(), Some(0o644))?;
    // Its sources: read once by Cydia (as mobile), which keeps them in its preferences and removes the file.
    let mut sources = Dictionary::new();
    for uri in SOURCES {
        let source = plistedit::dictionary([
            ("Type", "deb".into()),
            ("URI", uri.into()),
            ("Distribution", "stable".into()),
            ("Sections", plistedit::strings(&["main"])),
        ]);
        sources.insert(format!("deb:{uri}:stable"), Value::Dictionary(source));
    }
    if let Some((parent, _)) = METADATA.rsplit_once('/') {
        system.mkdirs(parent)?;
    }
    let metadata = plistedit::dictionary([("Version", 1.into()), ("Sources", Value::Dictionary(sources))]);
    system.write(METADATA, plistedit::serialize(&Value::Dictionary(metadata), Format::Xml)?, Some(0o644))?;
    let mut root = vec![NO_STASH.to_string(), METADATA.to_string()];
    let mut mobile = Vec::new();
    for rel in &entries {
        if added.contains(rel) || is_link(system, rel) {
            if rel.starts_with("private/var/mobile/") { mobile.push(rel.clone()) } else { root.push(rel.clone()) }
        }
    }
    if !had_prefs && !mobile.iter().any(|m| m == SPRINGBOARD_PREFS) {
        mobile.push(SPRINGBOARD_PREFS.to_string());
    }
    // Under /private/var the data volume's copy keeps its own modes (only /private/var/local, a setgid folder).
    let modes: Vec<(String, u16)> =
        special.into_iter().filter(|(p, _)| added.contains(p) && !p.starts_with("private/var/")).collect();
    let name = bootstrap.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Cydia {
        line: format!("Cydia: {name}, {} of {} entries added", added.len(), entries.len()),
        root,
        mobile,
        modes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::{Scratch, get_plist, put_plist};

    /// The stock com.apple.afc entries: 1.0's, 1.1.4 to 5.x's (afcd at the media folder) and 6.x to 7.x's (afcd as an
    /// XPC service). Through 7.0 afc2 is the same old-style entry on every one: afcd still takes --lockdown.
    fn stock_afc(era: usize) -> Dictionary {
        match era {
            0 => plistedit::dictionary([
                ("Label", "com.apple.afc".into()),
                ("ProgramArguments", plistedit::strings(&["/usr/libexec/afcd", "--lockdown"])),
            ]),
            1 => plistedit::dictionary([
                ("AllowUnactivatedService", true.into()),
                ("Label", "com.apple.afc".into()),
                (
                    "ProgramArguments",
                    plistedit::strings(&["/usr/libexec/afcd", "--lockdown", "-d", "/var/mobile/Media", "-u", "mobile"]),
                ),
            ]),
            _ => plistedit::dictionary([
                ("AllowUnactivatedService", true.into()),
                ("Label", "com.apple.afc".into()),
                ("UserName", "mobile".into()),
                ("XPCServiceName", "com.apple.afcd".into()),
            ]),
        }
    }

    fn put(ed: &mut Editor, path: &str, data: &[u8]) {
        if let Some((dir, _)) = path.rsplit_once('/') {
            ed.mkdirs(dir).unwrap();
        }
        ed.write(path, data.to_vec(), None).unwrap();
    }

    /// afc2ServesTheRootBesideTheStockService
    #[test]
    fn afc2_serves_the_root_beside_the_stock_service() {
        for era in 0..3 {
            let (_s, mut ed) = Scratch::new("afc2");
            let mut stock = Dictionary::new();
            stock.insert("com.apple.afc".into(), Value::Dictionary(stock_afc(era)));
            stock.insert("com.apple.syslog_relay".into(), Value::Dictionary(plistedit::dictionary([("Label", "x".into())])));
            put_plist(&mut ed, LOCKDOWN_SERVICES, stock, Format::Binary);
            put(&mut ed, AFCD, b"usage: -L | --lockdown : run under old-style lockdown");
            assert!(install_afc2(&mut ed).unwrap().1.is_empty());
            assert!(!ed.exists(AFC2D));
            assert_eq!(plistedit::format_of(&ed.read(LOCKDOWN_SERVICES).unwrap()), Format::Binary, "the stock format is kept");
            let edited = get_plist(&ed, LOCKDOWN_SERVICES);
            let afc2 = edited.get("com.apple.afc2").and_then(Value::as_dictionary).unwrap();
            assert_eq!(afc2.get("Label").and_then(Value::as_string), Some("com.apple.afc2"));
            assert_eq!(afc2.get("ProgramArguments"), Some(&plistedit::strings(&["/usr/libexec/afcd", "--lockdown", "-d", "/"])));
            assert_eq!(afc2.get("AllowUnactivatedService").and_then(Value::as_boolean), Some(true));
            assert!(afc2.get("UserName").is_none(), "afcd at / runs as root");
            assert_eq!(edited.get("com.apple.afc"), Some(&Value::Dictionary(stock_afc(era))));
            assert_eq!(edited.len(), 3);
        }
    }

    /// afc2NeedsTheStockService
    #[test]
    fn afc2_needs_the_stock_service() {
        let (_s, mut ed) = Scratch::new("afc2");
        put_plist(&mut ed, LOCKDOWN_SERVICES, Dictionary::new(), Format::Xml);
        put(&mut ed, AFCD, b"--lockdown");
        assert!(install_afc2(&mut ed).is_err());
    }

    /// afc2NeedsAnAfcdItKnows: neither --lockdown nor the 7.1 strings, so refused before any signing.
    #[test]
    fn afc2_needs_an_afcd_it_knows() {
        let (_s, mut ed) = Scratch::new("afc2");
        let mut services = Dictionary::new();
        services.insert("com.apple.afc".into(), Value::Dictionary(stock_afc(2)));
        put_plist(&mut ed, LOCKDOWN_SERVICES, services, Format::Xml);
        put_plist(
            &mut ed,
            &format!("{DAEMONS}/com.apple.afcd.plist"),
            plistedit::dictionary([("Label", "com.apple.afcd".into())]),
            Format::Xml,
        );
        put(&mut ed, AFCD, &[0xce, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]);
        assert!(install_afc2(&mut ed).is_err());
    }

    #[test]
    fn tar_member_names_as_volume_paths() {
        assert_eq!(entry_path("./usr/libexec/cydia/cydo"), "usr/libexec/cydia/cydo");
        assert_eq!(entry_path("./private/var/local/"), "private/var/local");
        assert_eq!(entry_path("./"), "");
        assert_eq!(entry_path("etc"), "etc");
    }

    /// The dpkg status freeze.tar has, Patcyh between two others.
    const STATUS: &str = "Package: apt7\nStatus: install ok installed\n\nPackage: com.saurik.patcyh\nVersion: 1.2.0\n\n\
                          Package: bash\nStatus: install ok installed\n\n";

    fn tar_of(entries: &[(&str, tar::EntryType, u32, &[u8], Option<&str>)]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (path, kind, mode, data, link) in entries {
            let mut h = tar::Header::new_ustar();
            h.set_entry_type(*kind);
            h.set_mode(*mode);
            h.set_size(data.len() as u64);
            h.set_uid(0);
            h.set_gid(0);
            h.set_mtime(1_500_000_000);
            if let Some(l) = link {
                h.set_link_name(l).unwrap();
            }
            b.append_data(&mut h, path, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    /// A bootstrap laid out as freeze.tar is (the /etc link, mobile's SpringBoard preferences, a file the firmware
    /// also has, Patcyh, set-ID cydo, a hard link), gzipped; beside it Substrate's and HTTPatch's packages.
    fn bootstrap_in(dir: &Path) -> PathBuf {
        use tar::EntryType as T;
        std::fs::create_dir_all(dir).unwrap();
        let prefs = plistedit::serialize(
            &Value::Dictionary(plistedit::dictionary([("SBShowNonDefaultSystemApps", true.into())])),
            Format::Xml,
        )
        .unwrap();
        let mut list: Vec<(&str, T, u32, &[u8], Option<&str>)> = vec![
            ("./", T::Directory, 0o755, b"", None),
            ("./Applications/", T::Directory, 0o775, b"", None),
            ("./Applications/Cydia.app/", T::Directory, 0o755, b"", None),
            ("./Applications/Cydia.app/Cydia", T::Regular, 0o755, b"cydia", None),
            ("./bin/", T::Directory, 0o755, b"", None),
            ("./bin/bash", T::Regular, 0o755, b"bash", None),
            ("./bin/uncompress", T::Regular, 0o755, b"gzip", None),
            ("./bin/gunzip", T::Link, 0o755, b"", Some("./bin/uncompress")),
            ("./private/var/lib/dpkg/status", T::Regular, 0o644, STATUS.as_bytes(), None),
            ("./private/var/lib/dpkg/available", T::Regular, 0o644, STATUS.as_bytes(), None),
            ("./usr/libexec/afcd", T::Regular, 0o755, b"not the firmware's", None),
            ("./private/var/Keychains/TrustStore.sqlite3", T::Regular, 0o644, b"roots", None),
            ("./usr/libexec/cydia/cydo", T::Regular, 0o6755, b"cydo", None),
            ("./private/var/mobile/Library/", T::Directory, 0o755, b"", None),
            ("./private/var/mobile/Library/Preferences/", T::Directory, 0o755, b"", None),
            ("./private/var/mobile/Library/Preferences/com.apple.springboard.plist", T::Regular, 0o644, &prefs, None),
            ("./etc", T::Symlink, 0o777, b"", Some("private/etc/")),
        ];
        for p in PATCYH {
            list.push((p, T::Regular, 0o644, b"patcyh", None));
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar_of(&list)).unwrap();
        std::fs::write(dir.join("freeze.tar.gz"), gz.finish().unwrap()).unwrap();
        for (name, rel) in [
            ("cydiasubstrate.tar", "./private/var/root/Media/Cydia/AutoInstall/mobilesubstrate_fix.deb"),
            ("cydiahttpatch.tar", "./private/var/root/Media/Cydia/AutoInstall/cydiahttpatch-1.0.deb"),
        ] {
            std::fs::write(dir.join(name), tar_of(&[("./", T::Directory, 0o755, b"", None), (rel, T::Regular, 0o644, name.as_bytes(), None)]))
                .unwrap();
        }
        dir.join("freeze.tar.gz")
    }

    /// A firmware root: /etc, afcd, /private/var/mobile, (with `prefs`) mobile's SpringBoard preferences and (with
    /// `version`) SystemVersion.plist.
    fn firmware(ed: &mut Editor, prefs: Option<Dictionary>, version: Option<&str>) {
        for rel in ["private/etc", "usr/libexec", "private/var/mobile", "System/Library/CoreServices"] {
            ed.mkdirs(rel).unwrap();
        }
        if let Some(v) = version {
            put_plist(ed, SYSTEM_VERSION, plistedit::dictionary([("ProductVersion", v.into())]), Format::Xml);
        }
        ed.write(AFCD, b"stock afcd".to_vec(), None).unwrap();
        ed.symlink("etc", "private/etc").unwrap();
        if let Some(p) = prefs {
            put_plist(ed, SPRINGBOARD_PREFS, p, Format::Binary);
        }
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> TempDir {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            TempDir(std::env::temp_dir().join(format!("lt-cydia-{name}-{}-{n}", std::process::id())))
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// cydiaLandsBesideTheFirmwaresOwnFiles and cydoKeepsItsSetUserIDMode
    #[test]
    fn cydia_lands_beside_the_firmwares_own_files() {
        let dir = TempDir::new("lands");
        let (_s, mut ed) = Scratch::new("cydia");
        firmware(&mut ed, None, None);
        let r = install_cydia(&mut ed, &bootstrap_in(&dir.0)).unwrap();
        assert_eq!(ed.read("Applications/Cydia.app/Cydia").unwrap(), b"cydia");
        assert_eq!(ed.read(AFCD).unwrap(), b"stock afcd", "a file the firmware has is kept");
        assert_eq!(get_plist(&ed, SPRINGBOARD_PREFS).get("SBShowNonDefaultSystemApps").and_then(Value::as_boolean), Some(true));
        assert!(ed.exists(NO_STASH), "no stash on first launch");
        assert!(!ed.exists("private/var/Keychains"), "no trust store");
        assert!(!r.root.iter().any(|p| p.starts_with("private/var/Keychains")));
        for rel in [".cydia_no_stash", "Applications", "Applications/Cydia.app/Cydia", "bin/bash", "private/var/lib/dpkg/status", "etc"] {
            assert!(r.root.iter().any(|p| p == rel), "{rel} is root's");
        }
        for rel in ["usr/libexec", "usr/libexec/afcd", "private/var/mobile"] {
            assert!(!r.root.iter().any(|p| p == rel) && !r.mobile.iter().any(|p| p == rel), "the firmware's {rel} keeps its owner");
        }
        let mobile: HashSet<&str> = r.mobile.iter().map(String::as_str).collect();
        assert_eq!(
            mobile,
            HashSet::from(["private/var/mobile/Library", "private/var/mobile/Library/Preferences", SPRINGBOARD_PREFS])
        );
        assert_eq!(ed.read_link("etc").unwrap(), "private/etc", "the firmware's link is kept");
        // modes: tar's as a normal user, the set-ID bits for the caller to restore
        assert_eq!(r.modes, [("usr/libexec/cydia/cydo".to_string(), 0o6755)]);
        assert!(r.root.iter().any(|p| p == "usr/libexec/cydia/cydo"));
        assert_eq!(ed.stat("usr/libexec/cydia/cydo").unwrap().unwrap().perm, 0o755);
        assert_eq!(ed.stat("Applications").unwrap().unwrap().perm, 0o755, "the umask applies");
        // the hard link, as a copy
        assert_eq!(ed.read("bin/gunzip").unwrap(), b"gzip");
        assert!(r.line.starts_with("Cydia: freeze.tar.gz, "), "{}", r.line);
    }

    /// cydiasSourcesAreTheOnesThatAnswerToday
    #[test]
    fn cydias_sources_are_the_ones_that_answer_today() {
        let dir = TempDir::new("sources");
        let (_s, mut ed) = Scratch::new("cydia");
        firmware(&mut ed, None, None);
        let r = install_cydia(&mut ed, &bootstrap_in(&dir.0)).unwrap();
        let plist = get_plist(&ed, METADATA);
        assert_eq!(plist.get("Version").and_then(Value::as_signed_integer), Some(1));
        let sources = plist.get("Sources").and_then(Value::as_dictionary).unwrap();
        let keys: HashSet<&str> = sources.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            HashSet::from([
                "deb:http://apt.thebigboss.org/repofiles/cydia/:stable",
                "deb:http://modmyi.saurik.com/:stable",
                "deb:http://zodttd.saurik.com/repo/cydia/:stable",
            ])
        );
        for (key, source) in sources {
            let s = source.as_dictionary().unwrap();
            assert_eq!(*key, format!("deb:{}:stable", s.get("URI").and_then(Value::as_string).unwrap()));
            assert_eq!(s.get("Type").and_then(Value::as_string), Some("deb"));
            assert_eq!(s.get("Sections"), Some(&plistedit::strings(&["main"])));
            assert_eq!(s.get("Distribution").and_then(Value::as_string), Some("stable"));
        }
        assert_eq!(ed.stat(METADATA).unwrap().unwrap().perm, 0o644, "mobile reads it");
        assert!(r.root.iter().any(|p| p == METADATA));
    }

    /// theFirmwaresSpringBoardPreferencesGainTheKey
    #[test]
    fn the_firmwares_springboard_preferences_gain_the_key() {
        let dir = TempDir::new("prefs");
        let (_s, mut ed) = Scratch::new("cydia");
        firmware(&mut ed, Some(plistedit::dictionary([("SBAutoLockTime", 60.into())])), None);
        let r = install_cydia(&mut ed, &bootstrap_in(&dir.0)).unwrap();
        let prefs = get_plist(&ed, SPRINGBOARD_PREFS);
        assert_eq!(prefs.get("SBShowNonDefaultSystemApps").and_then(Value::as_boolean), Some(true));
        assert_eq!(prefs.get("SBAutoLockTime").and_then(Value::as_signed_integer), Some(60));
        assert!(r.mobile.is_empty(), "mobile's own preferences keep their owner");
    }

    /// substrateAndHTTPatchByVersion: Substrate on 3.x to 6.x, HTTPatch on 3.x, none on 7.x; Patcyh out everywhere.
    #[test]
    fn substrate_and_httpatch_by_version() {
        for version in ["3.1.3", "4.2.1", "5.1.1", "6.1.6", "7.1.2"] {
            let dir = TempDir::new("version");
            let (_s, mut ed) = Scratch::new("cydia");
            firmware(&mut ed, None, Some(version));
            let r = install_cydia(&mut ed, &bootstrap_in(&dir.0)).unwrap();
            let major: u32 = version[..1].parse().unwrap();
            let auto = "private/var/root/Media/Cydia/AutoInstall/";
            for (deb, wanted) in [("mobilesubstrate_fix.deb", (3..=6).contains(&major)), ("cydiahttpatch-1.0.deb", major == 3)] {
                assert_eq!(ed.exists(&format!("{auto}{deb}")), wanted, "{version}: {deb}");
                assert_eq!(r.root.iter().any(|p| *p == format!("{auto}{deb}")), wanted, "{version}: {deb} is root's");
            }
            for rel in PATCYH {
                assert!(!ed.exists(rel), "{version}: {rel} is gone");
                assert!(!r.root.iter().any(|p| p == rel));
            }
            for rel in ["private/var/lib/dpkg/status", "private/var/lib/dpkg/available"] {
                assert_eq!(
                    String::from_utf8(ed.read(rel).unwrap()).unwrap(),
                    "Package: apt7\nStatus: install ok installed\n\nPackage: bash\nStatus: install ok installed\n\n",
                    "{version}: {rel} without Patcyh"
                );
            }
        }
    }

    /// aBootstrapOfOtherBytesIsRefused
    #[test]
    fn a_bootstrap_of_other_bytes_is_refused() {
        let dir = TempDir::new("refused");
        let r = bootstrap_with(&dir.0, &|_, file| Ok(std::fs::write(file, b"not freeze.tar")?), &|_| {});
        assert!(matches!(r, Err(e) if e.code == Code::ShaMismatch));
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0, "nothing is left in the cache");
    }

    /// The typein option: it_typein out of SpringBoard's DYLD_INSERT_LIBRARIES, in as a Substrate extension.
    #[test]
    fn typein_loads_by_substrate() {
        let dir = TempDir::new("typein");
        let (_s, mut ed) = Scratch::new("cydia");
        firmware(&mut ed, None, Some("3.1.3"));
        let mut services = Dictionary::new();
        services.insert("com.apple.afc".into(), Value::Dictionary(stock_afc(1)));
        put_plist(&mut ed, LOCKDOWN_SERVICES, services, Format::Binary);
        ed.write(AFCD, b"--lockdown".to_vec(), None).unwrap();
        put(&mut ed, "usr/lib/it_typein.dylib", b"typein");
        let mut job = plistedit::dictionary([("Label", "com.apple.SpringBoard".into())]);
        plistedit::dict(&mut job, "EnvironmentVariables")
            .insert("DYLD_INSERT_LIBRARIES".into(), "/usr/lib/it_typein.dylib:/usr/lib/it_pbd.dylib".into());
        put_plist(&mut ed, SPRINGBOARD_JOB, job, Format::Xml);
        let jb = install_jailbreak(&mut ed, &bootstrap_in(&dir.0), true).unwrap();
        let job = get_plist(&ed, SPRINGBOARD_JOB);
        let env = job.get("EnvironmentVariables").and_then(Value::as_dictionary).unwrap();
        assert_eq!(env.get("DYLD_INSERT_LIBRARIES").and_then(Value::as_string), Some("/usr/lib/it_pbd.dylib"));
        let dylib = "Library/MobileSubstrate/DynamicLibraries/it_typein.dylib";
        assert_eq!(ed.read_link(dylib).unwrap(), "/usr/lib/it_typein.dylib");
        assert!(jb.root.iter().any(|p| p == dylib));
        assert!(jb.afc2.contains("runs afcd at /"));
    }
}

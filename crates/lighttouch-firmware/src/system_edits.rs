//! SystemEdits: the edits a prepared system volume gets, shared by every board (the PAC, AppSync, the SpringBoard job,
//! activation, the guest-package seed; helper names and the dyld cache by arch), and the k48 recipe's system and data
//! volumes (build_k48). Swift's SystemEdits/SystemEdits.swift, over the in-memory editor instead of a mount:
//!
//!   system.img  the IPSW rootfs, grown to partition 1; rw fstab; SpringBoard env (GL CoreAnimation or
//!               CA_ENABLE_OGL=0) + stdio on /dev/console; [appsync] libappsync.dylib injected into installd;
//!               [ca_ogl] the GL front end as OpenGLES.framework/OpenGLES (+ dyld's override switch: OpenGLES is
//!               cached); [web_proxy] the PAC; the guest helpers; storage_mounter loads it_msmquiet; BTServer
//!               Disabled (unless the machine answers its HCI); lockdownd activated; the guest-package loader and
//!               seed package from armv7.itpack, whose jobs it_boot loads.
//!   data.img    fresh journaled HFSX "Data" (sparse) seeded with the system volume's /private/var skeleton
//!               (+ [web_proxy] the en0 AirPort service with the PAC), owners from the source catalog, else root /
//!               mobile by rule.
//!
//! Owners: what an edit adds is 501:20 (as a noowners mount records it); the catalog records are patched afterwards
//! (Volume::set_owner), as the Swift preparer does.
use crate::activation::{self, ActivationResult};
use crate::assets;
use crate::cydia;
use crate::entry::{self, Recipe};
use crate::error::{Code, Result};
use crate::fitcheck::{self, Firmware};
use crate::guest_package::{self, Record};
use crate::hfs::editor::Kind;
use crate::hfs::{Editor, Volume};
use crate::keyboards::KeyboardCatalog;
use crate::plistedit::{self, dict, dict_or, dictionary, move_first, strings};
use crate::setup;
use plist::{Dictionary, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The recipe options SystemEdits reads (Recipe.options), plus the create command's switches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub ca_ogl: bool,
    pub appsync: bool,
    pub web_proxy: bool,
    /// A journaled data volume: data_journal, else entry::journaled_data (unjournaled before 3.2).
    pub data_journal: bool,
    /// The one-shot clean halt the seal step needs (always on for a prepared device).
    pub seal: bool,
    /// The GL fixture job (test devices only: recipe option gl_test, never set in the catalog).
    pub gl_test: bool,
    /// Leave BTServer enabled: the board's machine answers the HCI (never a recipe option). Off, BTServer is
    /// Disabled: with nothing on the UART, BlueTool's HCI_Reset never completes.
    pub bluetooth: bool,
    /// Bake the guest helpers, their jobs and the guest-package seed. Off for firmware whose dyld predates
    /// LC_DYLD_INFO (3.0): such a device gets no it_seal either, so no seal boot.
    pub guest_tools: bool,
    /// A dated build (recipe rtc_epoch): the data ark says unbricked.
    pub dated: bool,
    /// create --skip-setup: the data volume says Setup Assistant is finished (iOS 5 and later).
    pub skip_setup: bool,
    /// create --jailbreak: afc2 and Cydia with Substrate from `cydia`, the bootstrap.
    pub jailbreak: bool,
    pub cydia: Option<PathBuf>,
    /// The device's language and region, a BCP 47 tag ("fr-FR").
    pub language: Option<String>,
    /// The device's keyboards besides its language's (create --keyboards).
    pub keyboards: Option<Vec<String>>,
    /// Disable the jobs of services that cannot work on an emulated device (recipe option quiet_services).
    pub quiet_services: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            ca_ogl: true,
            appsync: false,
            web_proxy: true,
            data_journal: true,
            seal: true,
            gl_test: false,
            bluetooth: false,
            guest_tools: true,
            dated: false,
            skip_setup: false,
            jailbreak: false,
            cydia: None,
            language: None,
            keyboards: None,
            quiet_services: true,
        }
    }
}

impl Options {
    /// `version`: the entry's iOS version, for the options its rules decide (entry::journaled_data).
    pub fn new(recipe: &Recipe, version: &str) -> Options {
        let o = |k: &str, default: bool| recipe.option(k).unwrap_or(default);
        Options {
            ca_ogl: o("ca_ogl", true),
            appsync: o("appsync", false),
            web_proxy: o("web_proxy", true),
            gl_test: o("gl_test", false),
            data_journal: o("data_journal", entry::journaled_data(version)),
            guest_tools: o("guest_tools", true),
            dated: recipe.rtc_epoch.is_some(),
            skip_setup: o("skip_setup", false),
            jailbreak: o("jailbreak", false),
            quiet_services: o("quiet_services", true),
            ..Options::default()
        }
    }
}

/// What the helpers directory holds, by file name (flat).
pub mod helpers {
    /// guest tool -> (install path, mode).
    pub const TOOLS: [(&str, &str, u16); 3] = [
        ("it_pbd", "usr/local/bin/it_pbd", 0o755),
        ("it_prefs", "usr/local/bin/it_prefs", 0o755),
        MSM_QUIET,
    ];
    pub const MSM_QUIET: (&str, &str, u16) = ("it_msmquiet.dylib", "usr/local/lib/it_msmquiet.dylib", 0o755);
    pub const SEAL: (&str, &str, u16) = ("it_seal", "usr/local/bin/it_seal", 0o755);
    pub const GL_TEST: (&str, &str, u16) = ("it_gltest", "usr/local/bin/it_gltest", 0o755);
    /// launchd job file names baked into System/Library/LaunchDaemons (mode 0644). The helpers' own jobs are the
    /// seed package's, loaded by it_boot.
    pub const JOBS: [&str; 0] = [];
    pub const SEAL_JOB: &str = "com.qemu.it-seal.plist";
    pub const GL_TEST_JOB: &str = "com.qemu.it-gltest.plist";
    /// The fat armv6+armv7 AppSync dylib, and 2.x's launcher for mobile_installation_proxy.
    pub const APPSYNC: &str = "libappsync.dylib";
    pub const APPSYNC_LAUNCHER: &str = "appsync-launch";
    /// The GL front end (one fat armv6 + armv7 OpenGLES for every firmware) and the name table it and the host speak.
    pub const OPENGLES: &str = "OpenGLES";
    pub const GLES_NAMES: &str = "gles-names.h";

    /// The guest packages and the loader, one per arch.
    pub fn itpack(arch: &str) -> String {
        format!("{arch}.itpack")
    }
    /// A helper built for `arch`: the armv7 build keeps the bare name, the others carry the arch.
    pub fn name(base: &str, arch: &str) -> String {
        if arch == "armv7" { base.to_string() } else { format!("{base}-{arch}") }
    }
    /// The legacy-linked build of a helper: what a firmware whose dyld predates LC_DYLD_INFO_ONLY loads.
    pub fn legacy(name: &str) -> String {
        match name.strip_suffix(".dylib") {
            Some(stem) => format!("{stem}-legacy.dylib"),
            None => format!("{name}-legacy"),
        }
    }
}

/// What build_k48 made.
pub struct Built {
    pub system: PathBuf,
    pub data: PathBuf,
    pub activation: Option<ActivationResult>,
    /// The GL front end installed (helpers file name), if any.
    pub engine: Option<String>,
    /// The seed package baked (the lock's guest_package).
    pub guest_package: Option<Record>,
    pub notes: Vec<String>,
}

// Volume paths.
pub const FSTAB: &str = "private/etc/fstab";
pub const FSTAB_RW: &str = "/dev/disk0s1 / hfs rw 0 1\n/dev/disk0s2 /private/var hfs rw,nosuid,nodev 0 2\n";
pub const FSTAB_RO: &str = "/dev/disk0s1 / hfs ro 0 1\n/dev/disk0s2 /private/var hfs rw,nosuid,nodev 0 2\n";
pub const DAEMONS: &str = "System/Library/LaunchDaemons";
pub const SPRINGBOARD_JOB: &str = "System/Library/LaunchDaemons/com.apple.SpringBoard.plist";
/// The stock jobs that raise the USB "not supported" notice for a device nothing claims (the emulated keyboard):
/// MobileStorageMounter through 4.x; 5.x moved it to USBDeviceArbitrator; 7.x to DeviceOMatic.
pub const NOTICE_JOBS: [(&str, &str); 3] = [
    ("System/Library/LaunchDaemons/com.apple.mobile.storage_mounter.plist", "com.apple.mobile.storage_mounter"),
    ("System/Library/LaunchDaemons/com.apple.mobile.usb_device_arbitrator.plist", "com.apple.mobile.usb_device_arbitrator"),
    ("System/Library/LaunchDaemons/com.apple.device-o-matic.plist", "com.apple.device-o-matic"),
];
pub const BT_JOB: &str = "System/Library/LaunchDaemons/com.apple.BTServer.plist";
/// Jobs launchd starts at boot for services an emulated device has no use for, each Disabled where present: Apple's
/// servers for them are gone or refuse these releases, or the hardware is absent (together ~12% of n81ap 6.1.6's
/// first-start guest CPU). Not usbptpd: without it usbmux never enumerates. Not backupd: MobileBackup raises without it.
pub const DEAD_SERVICE_JOBS: [&str; 13] = [
    "com.apple.imagent",
    "com.apple.imavagent",
    "com.apple.itunescloudd",
    "com.apple.mediastream.mstreamd",
    "com.apple.ubd",
    "com.apple.syncdefaultsd",
    "com.apple.apsd",
    "com.apple.AOSNotification",
    "com.apple.mobile.fud",
    "com.apple.mobile.softwareupdated",
    "com.apple.softwareupdateservicesd",
    "com.apple.OTATaskingAgent",
    "com.apple.OTACrashCopier",
];
pub const APPSYNC_PATH: &str = "usr/lib/libappsync.dylib";
pub const APPSYNC_LAUNCHER_PATH: &str = "usr/libexec/appsync-launch";
pub const DYLD_OVERRIDE: &str = "System/Library/Caches/com.apple.dyld/enable-dylibs-to-override-cache";
pub const LOCKDOWND: &str = "usr/libexec/lockdownd";
pub const RETIRED: [&str; 2] = ["usr/local/bin/it_notip", "System/Library/LaunchDaemons/com.qemu.it-notip.plist"];
pub const LOCKDOWN_SERVICES: &str = "System/Library/Lockdown/Services.plist";
/// Under /usr/share, not /usr/local: 4.x's MobileSafari sandbox can't read /usr/local, so its CFNetwork found no PAC
/// and went DIRECT while every unsandboxed client used the proxy.
pub const PAC_PATH: &str = "usr/share/ltm/proxy.pac";
pub const PAC: &str = r#"function FindProxyForURL(url, host) {
    if (isPlainHostName(host)) return "DIRECT";
    if (/^\d+\.\d+\.\d+\.\d+$/.test(host) &&
        (isInNet(host, "10.0.0.0", "255.0.0.0") || isInNet(host, "172.16.0.0", "255.240.0.0") ||
         isInNet(host, "192.168.0.0", "255.255.0.0") || isInNet(host, "169.254.0.0", "255.255.0.0") ||
         isInNet(host, "127.0.0.0", "255.0.0.0"))) return "DIRECT";
    return "PROXY 10.0.2.100:3128; DIRECT";
}
"#;
/// SpringBoard's environment: software CoreAnimation, or (ca_ogl) GL CoreAnimation; never page flipping.
pub const SB_ENV: [(&str, &str); 2] = [("CA_ENABLE_OGL", "0"), ("MBX2D_PAGE_FLIP", "0")];
pub const SB_ENV_CA_OGL: [(&str, &str); 1] = [("MBX2D_PAGE_FLIP", "0")];
/// uid 501 on the real unit; the rest of /var is root's.
pub const MOBILE_TOP: [&str; 2] = ["mobile", "ea"];
/// The path the K48 iBoot loads the kernel from (fsboot); the raw IPSW img3 kernelcache is installed there for the
/// real-iBoot chain. kboot omits it (the kernel is in the bundle).
pub const KERNELCACHE_PATH: &str = "System/Library/Caches/com.apple.kernelcaches/kernelcache";
/// The unit's region (UnitIdentity.iPadRegion).
pub const REGION: &str = "LL/A";

pub fn dyld_cache(arch: &str) -> String {
    format!("System/Library/Caches/com.apple.dyld/dyld_shared_cache_{arch}")
}

fn job(name: &str) -> String {
    format!("{DAEMONS}/{name}")
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn read_host(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| crate::internal!("{}: {e}", path.display()))
}

/// The k48 system and data volumes into `work` (system.img, data.img). `rootfs` is the decrypted rootfs DMG (or a
/// bare HFS volume); `system_bytes`/`data_bytes` are partitions 1 and 2 in bytes. `kernelcache`: the raw IPSW img3
/// kernelcache for the real-iBoot chain's fsboot; `kernel`: the decrypted kernelcache the fit checks read.
#[allow(clippy::too_many_arguments)]
pub fn build_k48(
    rootfs: &Path,
    work: &Path,
    system_bytes: u64,
    data_bytes: u64,
    o: &Options,
    helpers_dir: &Path,
    kernelcache: Option<&[u8]>,
    kernel: Option<Vec<u8>>,
    data_volume_uuid: Option<[u8; 8]>,
    fit: &fitcheck::Log,
    now: i64,
    log: &dyn Fn(&str),
) -> Result<Built> {
    let system = work.join("system.img");
    let data = work.join("data.img");
    let mut result =
        Built { system: system.clone(), data: data.clone(), activation: None, engine: None, guest_package: None, notes: Vec::new() };
    let helper = |name: &str| -> Result<PathBuf> {
        let p = helpers_dir.join(name);
        if !p.is_file() {
            return Err(crate::internal!("guest helper {name} missing from {}", helpers_dir.display()));
        }
        Ok(p)
    };

    // Check every helper before touching a volume.
    let mut tools: Vec<(&str, &str, u16)> = if o.guest_tools { helpers::TOOLS.to_vec() } else { Vec::new() };
    let mut jobs: Vec<&str> = if o.guest_tools { helpers::JOBS.to_vec() } else { Vec::new() };
    if o.seal && o.guest_tools {
        tools.push(helpers::SEAL);
        jobs.push(helpers::SEAL_JOB);
    }
    if o.gl_test {
        tools.push(helpers::GL_TEST);
        jobs.push(helpers::GL_TEST_JOB);
    }
    for t in &tools {
        if let Some(why) = activation::guest_tool_problem(&read_host(&helper(t.0)?)?) {
            return Err(crate::internal!("{}/{}: {why}", helpers_dir.display(), t.0));
        }
    }
    for j in &jobs {
        helper(j)?;
    }
    if o.guest_tools {
        helper(&helpers::itpack("armv7"))?;
    }
    if o.appsync {
        if let Some(why) = activation::app_sync_problem(&read_host(&helper(helpers::APPSYNC)?)?) {
            return Err(crate::internal!("{}/{}: {why}", helpers_dir.display(), helpers::APPSYNC));
        }
    }

    log(&format!("system volume from {}", rootfs.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
    crate::udif::extract_rootfs(rootfs, &system)?;
    // The IPSW's newest file: everything the recipe writes gets dated as of it.
    let newest = {
        let v = Volume::open(&system, false)?;
        let bytes = v.total_blocks * v.block_size;
        if bytes > system_bytes {
            return Err(crate::unsupported!("system volume ({bytes} bytes) is larger than partition 1 ({system_bytes})"));
        }
        log(&format!(
            "{} {} x {} B blocks, {} free; partition 1 is {} MiB",
            v.signature,
            v.total_blocks,
            v.block_size,
            v.free_blocks,
            system_bytes >> 20
        ));
        v.newest_date()?
    };
    if std::fs::metadata(&system)?.len() != system_bytes {
        Editor::grow(&system, system_bytes)?;
    }

    log("editing the system volume");
    let mut root_owned: Vec<String> = Vec::new();
    let mut root_modes: Vec<(String, u16)> = Vec::new();
    let mut mobile_owned: Vec<String> = Vec::new();
    let mut ed = Editor::open(&system)?;
    ed.now = now;
    let keyboards = KeyboardCatalog::read(&ed);
    let formats = setup::region_formats(&ed);
    // every baked helper proven to load on this firmware, read before any edit
    let fw = Firmware::new(ed.snapshot()?, "armv7", kernel);
    // a dyld without LC_DYLD_INFO_ONLY (3.0's: none of its own executables carry one) takes the legacy builds
    let legacy = !fw.precedent().contains_key(&fitcheck::LC_DYLD_INFO_ONLY);
    let pick = |n: &str| if legacy { helpers::legacy(n) } else { n.to_string() };
    if legacy {
        log("this dyld predates LC_DYLD_INFO_ONLY: the legacy-linked helpers and AppSync");
    }
    // it_msmquiet only in the jobs that raise the notice it recognizes; else left out, jobs untouched (3.1.x has no
    // storage_mounter job at all). One job raises it per version, so the others' misfits are no warning.
    let msm = helpers::MSM_QUIET;
    let candidates: Vec<(&str, &str)> =
        NOTICE_JOBS.iter().copied().filter(|(j, _)| o.guest_tools && ed.exists(j)).collect();
    let mut places = Vec::new();
    for (j, label) in &candidates {
        let program = stock_program(&ed, j, label)?;
        places.push(fitcheck::msm_quiet(&fw, &program, &read_host(&helper(&pick(msm.0))?)?));
    }
    let quiet_fits = fit.check_any(places, "left out");
    let quiet_jobs: Vec<(&str, &str)> =
        candidates.iter().zip(quiet_fits).filter(|(_, fits)| *fits).map(|(c, _)| *c).collect();
    let quiet = !quiet_jobs.is_empty();
    if !quiet {
        tools.retain(|t| t.0 != msm.0);
    }
    if o.guest_tools {
        for f in fitcheck::prefs(&fw, fitcheck::IT_PREFS) {
            fit.check(f, false, "kept: it_prefs skips the key at boot")?;
        }
    } else {
        fit.not_installed("guest helpers", "guest_tools off");
    }
    for t in tools.iter().filter(|t| t.0 != msm.0) {
        let name = pick(t.0);
        fit.check(fitcheck::loads(&name, &read_host(&helper(&name)?)?, &fw, None), true, "left out")?;
    }
    if o.appsync {
        fitcheck::check_app_sync(fit, &fw, helpers_dir, &pick(helpers::APPSYNC))?;
    } else {
        fit.not_installed("AppSync", "appsync off");
    }
    if let Some(kc) = kernelcache {
        // real-iBoot fsboot: the raw IPSW img3 kernelcache in the system volume
        ed.mkdirs(parent(KERNELCACHE_PATH))?;
        ed.write(KERNELCACHE_PATH, kc.to_vec(), Some(0o644))?;
    }
    let system_version = plistedit::read(&ed, "System/Library/CoreServices/SystemVersion.plist")?.unwrap_or_default();
    let product_version = system_version.get("ProductVersion").and_then(Value::as_string).unwrap_or("").to_string();
    let product_build = system_version.get("ProductBuildVersion").and_then(Value::as_string).unwrap_or("").to_string();
    let product_major: u32 = product_version.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    // 7.x's launchd cannot remount / rw (mount_hfs: Operation not permitted) and reboots; keep its stock ro root
    ed.write(FSTAB, (if product_major >= 7 { FSTAB_RO } else { FSTAB_RW }).as_bytes().to_vec(), None)?;
    if o.web_proxy {
        let now_fw = Firmware::new(ed.snapshot()?, "armv7", None);
        fit.check(fitcheck::web_proxy(&now_fw), false, "kept: the PAC is unused")?;
        root_owned.extend(install_pac(&mut ed, &[])?);
    }
    if o.ca_ogl {
        // GL first
        let (engine, owned) = install_ca_ogl(&mut ed, helpers_dir, &fw, fit, log)?;
        result.engine = Some(engine);
        root_owned.extend(owned);
    }
    let env: Vec<(&str, &str)> = if o.ca_ogl { SB_ENV_CA_OGL.to_vec() } else { SB_ENV.to_vec() };
    let mut switches: Vec<Vec<String>> = env.iter().map(|(k, _)| vec![k.to_string()]).collect();
    switches.sort();
    let also = if result.engine.is_some() {
        vec![(helpers::OPENGLES.to_string(), read_host(&helper(helpers::OPENGLES)?)?)]
    } else {
        Vec::new()
    };
    fit.check(fitcheck::environment(&fw, &switches, &also), false, "kept: a switch nothing reads is inert")?;
    edit_springboard_job(&mut ed, |job| {
        let e = dict(job, "EnvironmentVariables");
        for (k, v) in &env {
            e.insert(k.to_string(), Value::String(v.to_string()));
        }
        job.insert("StandardOutPath".into(), "/dev/console".into());
        job.insert("StandardErrorPath".into(), "/dev/console".into());
        Ok(())
    })?;
    if o.jailbreak {
        let bootstrap = o.cydia.as_deref().ok_or_else(|| crate::internal!("jailbreak: no Cydia bootstrap"))?;
        let jb = cydia::install_jailbreak(&mut ed, bootstrap, false)?;
        log(&jb.afc2);
        log(&jb.cydia);
        root_owned.extend(jb.root);
        mobile_owned.extend(jb.mobile);
        root_modes.extend(jb.modes);
    }
    if o.appsync {
        install_app_sync(&mut ed, &helper(&pick(helpers::APPSYNC))?, &dyld_cache("armv7"), log)?;
        root_owned.push(APPSYNC_PATH.to_string());
    }
    // bake
    for rel in RETIRED {
        if ed.lstat(rel)?.is_some() {
            ed.remove(rel)?;
        }
    }
    for t in &tools {
        ed.mkdirs(parent(t.1))?;
        ed.write(t.1, read_host(&helper(&pick(t.0))?)?, Some(t.2))?;
    }
    for j in &jobs {
        ed.write(&job(j), read_host(&helper(j)?)?, Some(0o644))?;
    }
    for (j, label) in &quiet_jobs {
        plistedit::rewrite(&mut ed, j, |d| {
            if d.get("Label").and_then(Value::as_string) != Some(label) {
                return Err(crate::unsupported!("{j}: not {label}'s job"));
            }
            dict(d, "EnvironmentVariables").insert("DYLD_INSERT_LIBRARIES".into(), format!("/{}", msm.1).into());
            Ok(())
        })?;
    }
    if !o.bluetooth {
        disable(&mut ed, BT_JOB)?;
    }
    if o.quiet_services {
        let quieted: Vec<String> = DEAD_SERVICE_JOBS.iter().map(|n| job(&format!("{n}.plist"))).filter(|j| ed.exists(j)).collect();
        for j in &quieted {
            disable(&mut ed, j)?;
        }
        log(&format!("quiet services: {} jobs disabled", quieted.len()));
    }
    result.activation = Some(match activate(&mut ed, log) {
        Ok(r) => r,
        Err(failure) => {
            let Some(r) = activation::data_ark_route(&ed.read(LOCKDOWND)?) else {
                return Err(crate::error::Error { code: Code::ActivationFailed, message: failure.0, piece: None });
            };
            let keys: Vec<&str> = r.data_ark.iter().flatten().map(|(k, _)| k.as_str()).collect();
            log(&format!("lockdownd: no binary strategy; activation by the data ark ({})", keys.join(", ")));
            r
        }
    });
    root_owned.push(LOCKDOWND.to_string());
    // what this bake left out on purpose: AppSync when off, it_msmquiet where it does not fit
    let mut omitted = HashSet::new();
    if !o.appsync {
        omitted.insert(format!("/{APPSYNC_PATH}"));
    }
    if !quiet {
        omitted.insert(format!("/{}", msm.1));
    }
    if o.guest_tools {
        let (seeded, record) = seed_guest_package(&mut ed, helpers_dir, "armv7", result.engine.is_some(), &omitted, fit, log)?;
        result.guest_package = Some(record);
        root_owned.extend(seeded);
    }
    for d in ["usr/local", "usr/local/bin", "usr/local/lib"] {
        if ed.exists(d) {
            root_owned.push(d.to_string());
        }
    }
    root_owned.extend(jobs.iter().map(|j| job(j)));
    root_owned.extend(tools.iter().map(|t| t.1.to_string()));
    drop(fw);
    remove_junk(&mut ed)?;
    ed.commit()?;

    let sys = Volume::open(&system, true)?;
    let mut n = sys.set_owner(&refs(&root_owned), 0, 0, None, None)? + sys.set_owner(&refs(&mobile_owned), 501, 501, None, None)?;
    for (path, mode) in &root_modes {
        n += sys.set_owner(&[path.as_str()], 0, 0, Some(*mode), None)?;
    }
    let owners = sys.owners("private/var")?;
    let dated = sys.normalize(newest, newest, None)?;
    drop(sys);
    log(&format!("system volume: {n} catalog records set to root, {dated} dated as of the IPSW's newest file"));

    log(&format!("data volume ({} MB, sparse) seeded from /private/var", data_bytes / 1_000_000));
    let _ = std::fs::remove_file(&data);
    Editor::format(&data, data_bytes, "Data", o.data_journal, now)?;
    let mut dv = Editor::open(&data)?;
    dv.now = now;
    {
        let source = Editor::open_read_only(&system)?;
        copy_tree(&source, "private/var", &mut dv, "")?;
    }
    seed_data(&mut dv, "", o, product_major, result.activation.as_ref().and_then(|a| a.data_ark.as_deref()), keyboards.as_ref(), formats.as_ref(), now, log)?;
    let region = REGION.split('/').next().unwrap_or("");
    if let Some(installed) =
        assets::seed_preinstalled_assets(&mut dv, "", product_major, &product_version, &product_build, region)?
    {
        log(&format!("preinstalled assets: {} installed as mobileassetd would at the first boot", installed.len()));
    }
    let paths = dv.walk("")?;
    remove_junk(&mut dv)?;
    dv.commit()?;
    let mut by_owner: BTreeMap<(u32, u32), Vec<String>> = BTreeMap::new();
    for rel in paths {
        let top = rel.split('/').next().unwrap_or("");
        let own = owners.get(&rel).copied().unwrap_or(if MOBILE_TOP.contains(&top) { (501, 501) } else { (0, 0) });
        by_owner.entry(own).or_default().push(rel);
    }
    let dvol = Volume::open(&data, true)?;
    let mut patched = 0;
    for ((uid, gid), paths) in &by_owner {
        patched += dvol.set_owner(&refs(paths), *uid, *gid, None, None)?;
    }
    let summary: Vec<String> = by_owner.iter().map(|((u, g), p)| format!("{u}:{g} x{}", p.len())).collect();
    log(&format!(
        "owners from the skeleton, else root / mobile by rule: {} ({patched} catalog records patched)",
        summary.join(", ")
    ));
    dvol.normalize(newest, newest, data_volume_uuid)?;
    if product_major >= 5 {
        // A restore formats the data volume with content protection. From 5.0 installd gives each app's tmp protection
        // class 4 and, refused, leaves the container without Documents (Library and tmp only).
        dvol.set_content_protection()?;
        log("data volume: content protection on");
    }
    Ok(result)
}

/// Copies `from` in `src` (a folder) into `to` in `dst`, as shutil.copytree(symlinks=True, dirs_exist_ok=True) did
/// into the skeleton: folders, symlinks as symlinks, file contents written plainly (an HFS-compressed source lands
/// uncompressed), permission bits kept; no owners, flags or extended attributes. `to`'s own folder takes `from`'s mode.
pub fn copy_tree(src: &Editor, from: &str, dst: &mut Editor, to: &str) -> Result<()> {
    let join = |base: &str, rel: &str| if base.is_empty() { rel.to_string() } else if rel.is_empty() { base.to_string() } else { format!("{base}/{rel}") };
    let top = src.stat(from)?.ok_or_else(|| crate::internal!("no {from} to copy"))?;
    if !to.is_empty() && !dst.exists(to) {
        dst.mkdirs(to)?;
    }
    dst.chmod(to, top.perm)?;
    for rel in src.walk(from)? {
        let (s, d) = (join(from, &rel), join(to, &rel));
        let m = src.lstat(&s)?.ok_or_else(|| crate::internal!("{s} vanished"))?;
        match m.kind {
            Kind::Symlink => {
                if dst.lstat(&d)?.is_none() {
                    dst.symlink(&d, &src.read_link(&s)?)?;
                }
            }
            Kind::Folder => {
                if !dst.exists(&d) {
                    dst.mkdir(&d, Some(m.perm))?;
                } else {
                    dst.chmod(&d, m.perm)?;
                }
            }
            Kind::File => dst.write(&d, src.read(&s)?, Some(m.perm))?,
        }
    }
    Ok(())
}

/// What macOS drops on a mounted volume, the IPSW's own rootfs included; removed from a volume's root before it is
/// committed, as the Swift preparer's mount removed it before unmounting.
pub const JUNK: [&str; 5] = [".fseventsd", ".Spotlight-V100", ".Trashes", ".TemporaryItems", ".DS_Store"];

pub fn remove_junk(ed: &mut Editor) -> Result<()> {
    for name in JUNK {
        if ed.lstat(name)?.is_some() {
            ed.remove(name)?;
        }
    }
    Ok(())
}

// ---- The shared bake (every board's system volume)

/// The web-proxy PAC at /usr/share/ltm/proxy.pac; `dirs` are created too (the iPod's SystemConfiguration on the
/// system volume). Returns the paths to make root-owned.
pub fn install_pac(ed: &mut Editor, dirs: &[&str]) -> Result<Vec<String>> {
    let mut owned = Vec::new();
    for rel in std::iter::once("usr/share/ltm").chain(dirs.iter().copied()) {
        ed.mkdirs(rel)?;
        owned.push(rel.to_string());
    }
    ed.write(PAC_PATH, PAC.as_bytes().to_vec(), None)?;
    owned.push(PAC_PATH.to_string());
    Ok(owned)
}

/// Injects the AppSync helper into the firmware's installation service (installd, or 2.x's Lockdown-launched
/// mobile_installation_proxy), keeping stock libmis and its cache. Returns (status line, the job edited).
pub fn install_app_sync(ed: &mut Editor, helper: &Path, cache: &str, log: &dyn Fn(&str)) -> Result<(String, String)> {
    let bytes = read_host(helper)?;
    if !ed.exists(cache) {
        if let Some(why) = activation::early_arm_problem(&bytes) {
            return Err(crate::unsupported!("AppSync: {why}"));
        }
    }
    let line = "stock libmis retained; installation-service interposition".to_string();
    log(&line);
    ed.mkdirs(parent(APPSYNC_PATH))?;
    ed.write(APPSYNC_PATH, bytes, Some(0o644))?;
    if let Some(j) = ["com.apple.mobile.installd.plist", "com.apple.installd.plist"].iter().map(|n| job(n)).find(|j| ed.exists(j)) {
        plistedit::rewrite(ed, &j, |d| {
            plistedit::dyld_insert(d, &format!("/{APPSYNC_PATH}"));
            Ok(())
        })?;
        let name = j.rsplit('/').next().unwrap_or(&j).to_string();
        return Ok((line, name));
    }
    // 2.x installs in the Lockdown-launched mobile_installation_proxy, before installd existed.
    let launcher = helper.parent().unwrap_or(Path::new(".")).join(crate::system_edits::helpers::APPSYNC_LAUNCHER);
    let mut launcher_bytes = None;
    plistedit::rewrite(ed, LOCKDOWN_SERVICES, |root| {
        let service = match root.get_mut("com.apple.mobile.installation_proxy") {
            Some(Value::Dictionary(s)) => s,
            _ => return Err(crate::unsupported!("no supported installation service")),
        };
        let arguments: Vec<String> = match service.get("ProgramArguments") {
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_string().map(str::to_string)).collect(),
            _ => Vec::new(),
        };
        if arguments.first().map(String::as_str) != Some("/usr/libexec/mobile_installation_proxy") {
            return Err(crate::unsupported!("no supported installation service"));
        }
        let bytes = read_host(&launcher)?;
        if let Some(why) = activation::early_arm_problem(&bytes) {
            return Err(crate::unsupported!("AppSync launcher: {why}"));
        }
        launcher_bytes = Some(bytes);
        let mut args = vec![format!("/{APPSYNC_LAUNCHER_PATH}")];
        args.extend(arguments);
        service.insert("ProgramArguments".into(), strings(&args));
        Ok(())
    })?;
    if let Some(bytes) = launcher_bytes {
        ed.write(APPSYNC_LAUNCHER_PATH, bytes, Some(0o755))?;
    }
    Ok((line, "Services.plist:com.apple.mobile.installation_proxy".to_string()))
}

/// The program of the stock job `job` (volume-relative), checked by label.
pub fn stock_program(ed: &Editor, job: &str, label: &str) -> Result<String> {
    let not = || crate::unsupported!("{job}: not {label}'s job");
    let d = plistedit::read(ed, job)?.ok_or_else(not)?;
    if d.get("Label").and_then(Value::as_string) != Some(label) {
        return Err(not());
    }
    let first = match d.get("ProgramArguments") {
        Some(Value::Array(a)) => a.first().and_then(Value::as_string).map(str::to_string),
        _ => None,
    };
    first.or_else(|| d.get("Program").and_then(Value::as_string).map(str::to_string)).ok_or_else(not)
}

/// SpringBoard's launchd job, checked by label, edited by `edit`.
pub fn edit_springboard_job(ed: &mut Editor, edit: impl FnOnce(&mut Dictionary) -> Result<()>) -> Result<()> {
    plistedit::rewrite(ed, SPRINGBOARD_JOB, |d| {
        if d.get("Label").and_then(Value::as_string) != Some("com.apple.SpringBoard") {
            return Err(crate::unsupported!("{SPRINGBOARD_JOB}: not SpringBoard's job"));
        }
        edit(d)
    })
}

fn disable(ed: &mut Editor, job: &str) -> Result<()> {
    plistedit::rewrite(ed, job, |d| {
        d.insert("Disabled".into(), true.into());
        Ok(())
    })
}

/// lockdownd activated in place; the caller makes it root-owned.
pub fn activate(ed: &mut Editor, log: &dyn Fn(&str)) -> std::result::Result<ActivationResult, activation::Failure> {
    log("Activating device");
    let before = ed.read(LOCKDOWND).map_err(|e| activation::Failure(e.message))?;
    let (after, result) = activation::run(&before)?;
    ed.write(LOCKDOWND, after, None).map_err(|e| activation::Failure(e.message))?;
    Ok(result)
}

/// The guest-package loader and the arch's seed package.
pub fn seed_guest_package(
    ed: &mut Editor,
    helpers_dir: &Path,
    arch: &str,
    gles: bool,
    omitted: &HashSet<String>,
    fit: &fitcheck::Log,
    log: &dyn Fn(&str),
) -> Result<(Vec<String>, Record)> {
    let (seeded, record) = guest_package::seed(ed, &helpers_dir.join(helpers::itpack(arch)), gles, omitted, fit)?;
    log(&format!("seed package {} serial {}, hooks {:?}", record.family, record.seed, record.hooks));
    Ok((seeded, record))
}

/// [ca_ogl] the GL front end as OpenGLES.framework/OpenGLES, + dyld's override switch where the stock OpenGLES is in
/// the shared cache, once the fit check has proven, from the pristine firmware, everything it looks up at run time:
/// a misfit fails the prepare rather than quietly producing a software-CoreAnimation device. Returns the helper name
/// and the files to own by root.
pub fn install_ca_ogl(
    ed: &mut Editor,
    helpers_dir: &Path,
    fw: &Firmware,
    fit: &fitcheck::Log,
    log: &dyn Fn(&str),
) -> Result<(String, Vec<String>)> {
    let src = helpers_dir.join(helpers::OPENGLES);
    if !src.is_file() {
        return Err(crate::internal!("{} missing", src.display()));
    }
    let bin = read_host(&src)?;
    let names = std::fs::read_to_string(helpers_dir.join(helpers::GLES_NAMES)).unwrap_or_default();
    if let Err(e) = fit.check(fitcheck::gles_front_end(fw, &bin, &names), true, "left out") {
        if e.code == Code::Unsupported {
            let mut e2 = crate::unsupported!(
                "the recipe asks for GL CoreAnimation (ca_ogl) and this firmware cannot composite through the GL bridge: {}",
                e.message
            );
            e2.piece = e.piece;
            return Err(e2);
        }
        return Err(e);
    }
    let cached = fw.cache().and_then(|c| c.image(&format!("/{}", fitcheck::OPENGLES))).is_some();
    let backup = guest_package::preserve_hook(ed, fitcheck::OPENGLES)?;
    if cached {
        set_override_switch(ed, fitcheck::OPENGLES)?;
    }
    log(&format!(
        "GL front end {} as OpenGLES.framework/OpenGLES; {}",
        helpers::OPENGLES,
        if cached { "cached OpenGLES overridden by the file" } else { "the stock file replaced" }
    ));
    ed.write(fitcheck::OPENGLES, bin, Some(0o755))?;
    let mut owned = vec![fitcheck::OPENGLES.to_string(), backup];
    if cached {
        owned.push(DYLD_OVERRIDE.to_string());
    }
    Ok((helpers::OPENGLES.to_string(), owned))
}

/// dyld's enable-dylibs-to-override-cache switch, so a file installed over a cached image loads. Fails closed if this
/// dyld has no such switch.
pub fn set_override_switch(ed: &mut Editor, image: &str) -> Result<()> {
    let dyld = ed.read("usr/lib/dyld")?;
    let needle = format!("/{DYLD_OVERRIDE}\0");
    if !dyld.windows(needle.len()).any(|w| w == needle.as_bytes()) {
        let name = image.rsplit('/').next().unwrap_or(image);
        return Err(crate::unsupported!(
            "{name} is in the shared cache and this dyld has no enable-dylibs-to-override-cache switch: the GL shim cannot load"
        ));
    }
    ed.write(DYLD_OVERRIDE, Vec::new(), None)
}

// ---- The data volume's seeds

pub const NET_SET: &str = "4C54E7A1-0B5E-4D6B-9A1C-534554000001";
pub const WIFI_SERVICE: &str = "4C54E7A1-0B5E-4D6B-9A1C-574946490000";

/// The current set's Network dict, creating CurrentSet / Sets / the set as Python's setdefault chain does.
pub fn current_network(d: &mut Dictionary) -> &mut Dictionary {
    if !d.contains_key("CurrentSet") {
        d.insert("CurrentSet".into(), format!("/Sets/{NET_SET}").into());
    }
    let current = d.get("CurrentSet").and_then(Value::as_string).unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
    let set = dict_or(dict(d, "Sets"), &current, || dictionary([("UserDefinedName", "Automatic".into())]));
    dict(set, "Network")
}

/// A network service's Proxies: the web proxy's PAC.
pub fn pac_proxies() -> Value {
    Value::Dictionary(dictionary([
        ("ExceptionsList", strings(&["*.local", "169.254/16"])),
        ("FTPPassive", 1.into()),
        ("ProxyAutoConfigEnable", 1.into()),
        ("ProxyAutoConfigURLString", format!("file:///{PAC_PATH}").into()),
    ]))
}

/// preferences.plist: the AirPort service on en0 (the unit's own shape) carrying the proxy PAC, first.
pub fn wifi_proxy_prefs(d: &mut Dictionary) -> Result<()> {
    let services = dict(d, "NetworkServices");
    let svc = dict_or(services, WIFI_SERVICE, || {
        dictionary([
            (
                "Interface",
                Value::Dictionary(dictionary([
                    ("DeviceName", "en0".into()),
                    ("Hardware", "AirPort".into()),
                    ("Type", "Ethernet".into()),
                    ("UserDefinedName", "AirPort".into()),
                ])),
            ),
            ("IPv4", Value::Dictionary(dictionary([("ConfigMethod", "DHCP".into())]))),
            ("IPv6", Value::Dictionary(dictionary([("ConfigMethod", "Automatic".into())]))),
            ("DNS", Value::Dictionary(Dictionary::new())),
            ("UserDefinedName", "AirPort".into()),
        ])
    });
    svc.insert("Proxies".into(), pac_proxies());
    let net = current_network(d);
    dict(net, "Service")
        .insert(WIFI_SERVICE.into(), Value::Dictionary(dictionary([("__LINK__", format!("/NetworkServices/{WIFI_SERVICE}").into())])));
    dict_or(dict(net, "Interface"), "en0", || {
        dictionary([("AirPort", Value::Dictionary(dictionary([("JoinMode", "Automatic".into())])))])
    });
    move_first(dict(dict(net, "Global"), "IPv4"), "ServiceOrder", WIFI_SERVICE);
    Ok(())
}

/// What the /private/var skeleton gets before it is the data volume: the web proxy, the language, Setup finished,
/// the data ark. Nothing for timed: a dated machine's network answers NTP in the device's own time, so automatic time
/// and zone stay on, as a real unit has them. `var_root`: the folder that is /private/var ("" in a data volume).
#[allow(clippy::too_many_arguments)]
pub fn seed_data(
    ed: &mut Editor,
    var_root: &str,
    o: &Options,
    product_major: u32,
    data_ark: Option<&[(String, String)]>,
    keyboards: Option<&KeyboardCatalog>,
    formats: Option<&HashSet<String>>,
    now: i64,
    log: &dyn Fn(&str),
) -> Result<()> {
    let at = |rel: &str| if var_root.is_empty() { rel.to_string() } else { format!("{var_root}/{rel}") };
    if o.web_proxy {
        plistedit::seed(ed, &at("preferences/SystemConfiguration/preferences.plist"), wifi_proxy_prefs)?;
    }
    let language = setup::seed_language(ed, var_root, o.language.as_deref(), o.keyboards.as_deref(), keyboards, formats)?;
    if let Some(l) = &language {
        log(&format!("language {}, region {}, keyboards {:?}", l.language, l.locale, l.keyboards));
    }
    if o.skip_setup && product_major >= 5 {
        let (l, locale) = language.as_ref().map(|l| (l.language.as_str(), l.locale.as_str())).unwrap_or(("en", "en_US"));
        setup::seed_finished_setup(ed, var_root, product_major, l, locale, now)?;
        log(&format!("Setup Assistant: seeded as finished (iOS {product_major})"));
    }
    if let Some(ark) = data_ark {
        // 7.0 beta 1's lockdownd turns the brick state on for an ark without one and lifts it only for a valid
        // activation record, which this route has none of. A dated build says unbricked, as lockdownd's own
        // "factoryactivation unbricks by default".
        plistedit::seed(ed, &at("root/Library/Lockdown/data_ark.plist"), |d| {
            for (k, v) in ark {
                d.insert(k.clone(), v.clone().into());
            }
            if o.dated {
                d.insert("-BrickState".into(), false.into());
            }
            Ok(())
        })?;
    }
    Ok(())
}

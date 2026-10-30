//! N72Board: the iPod touch 2G (n72ap, recipe "n72") side of create. Swift's N72/N72Recipe.swift (ipod2g_device
//! build/bake + build_nand.py --epoch).
//!
//! Boot files: nor.bin, iBoot.bin (3.x+: the machine's direct-iboot), gid-blobs.bin. Volume: the IPSW rootfs grown to
//! the recipe, fstab, the kernelcache at the path iBoot names, the bake (the shared pieces of system_edits plus the
//! iPod's own: sound defaults, guest-tools markers), owners patched in the catalog. Store: n72nand's page directory.
//! There is no seal boot: the legacy FTL store needs no clean halt. Data protection (4.x: entry::data_protection, or
//! options.data_protection) adds the restore-ramdisk keybag one-shot (n72keybag) through the helper.
//!
//! The recipe: storage "8g" (model MB528; 16g MB531, 32g MB533; region LL/A), system_mib = the volume, options
//! gles_shim / appsync / web_proxy / data_protection / jailbreak. The guest tools hold OpenGLES (the GL front end,
//! every build, 2.x to 4.x) and gles-names.h, sblaunch, sbdlicon (optional), it_agent, it_typein.dylib,
//! com.qemu.it-agent.plist, libappsync.dylib, armv6.itpack and it_keybag-armv6 (data protection). Without the helpers
//! (a set that does not load on this firmware), the SpringBoard preferences it_prefs would set are baked instead.
use crate::create::{filled, Board, Context, Options};
use crate::error::{Code, Error, Result};
use crate::fitcheck::{self, Firmware, Fit};
use crate::hfs::{Editor, Volume};
use crate::identity::{UnitIdentity, IPAD_REGION};
use crate::plistedit;
use crate::system_edits::{self, helpers, APPSYNC_LAUNCHER_PATH, APPSYNC_PATH, LOCKDOWND, PAC_PATH};
use crate::{files, img3, n72nand, n72nor};
use plist::Value as Plist;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

const MODELS: [(&str, &str); 3] = [("8g", "MB528"), ("16g", "MB531"), ("32g", "MB533")];
const KC_PREFIX: &str = "/System/Library/Caches/com.apple.kernelcaches/";
const PREFS: &str = "private/var/mobile/Library/Preferences";
const AGENT_JOB: &str = "System/Library/LaunchDaemons/com.qemu.it-agent.plist";
pub(crate) const FSTAB_RW: &str = "/dev/disk0s1 / hfs rw 0 1\n";
pub(crate) const SPRINGBOARD: &str = "System/Library/CoreServices/SpringBoard.app/SpringBoard";
const MBX: &str = "System/Library/Frameworks/OpenGLES.framework/MBXGLEngine.bundle/MBXGLEngine";
/// The pair the machine boots every n72 device with (qemu-ios ipod_touch_2g.c; FitCheck.amfiArgs).
const AMFI_ARGS: &str = "amfi_allow_any_signature=1 cs_enforcement_disable=1";
/// The switches the bake sets in SpringBoard's job, each under its 3.x+ (CoreAnimation) and 2.x (LayerKit) name.
const SB_SWITCHES: [[&str; 2]; 2] = [["CA_ENABLE_OGL", "LK_ENABLE_OGL"], ["CA_AUTO_ENABLE_OGL", "LK_AUTO_ENABLE_OGL"]];
/// The baked guest tools (and it_typein in SpringBoard) that must all load for any to be installed.
const GUEST_TOOLS: [&str; 4] = ["it_agent", "it_typein.dylib", "sblaunch", "sbdlicon"];
/// SpringBoard's first-run "Edit Home Screen" tip stays down once com.apple.springboard SBDidShowReorderText is true.
const REORDER_TIP: &str = "SBDidShowReorderText";
const KEYBAG_HELPER_BASE: &str = "it_keybag";

/// The -machine options every device this recipe builds boots with (the lock's "machine"). hardware: the AES mode
/// register reads back, so iBoot's UID derivation encrypts, as n72nor wraps for.
pub const MACHINE: [(&str, &str); 1] = [("aes-uid", "hardware")];

/// set-sound-defaults.py: the five Sounds switches of a new device.
fn sound_defaults() -> Vec<(&'static str, Vec<(&'static str, Plist)>)> {
    vec![
        ("com.apple.mobilemail.plist", vec![("PlayNewMailSound", true.into()), ("PlaySentMailSound", true.into())]),
        (
            "com.apple.springboard.plist",
            vec![("calendar-alarm", "/Applications/MobileCal.app/alarm.aiff".into()), ("lock-unlock", true.into())],
        ),
        ("com.apple.preferences.sounds.plist", vec![("keyboard", true.into())]),
    ]
}

/// Paths the guest-tools bake creates (ipod2g_device.GUEST_TOOL_OWNERS), owner set in the catalog if present.
fn guest_tool_owners() -> Vec<(u32, String)> {
    let mut v: Vec<(u32, String)> = [
        "usr/local",
        "usr/local/bin",
        "usr/local/bin/it_agent",
        "usr/local/bin/sblaunch",
        "usr/local/bin/sbdlicon",
        "usr/lib/it_typein.dylib",
        AGENT_JOB,
    ]
    .iter()
    .map(|p| (0, p.to_string()))
    .collect();
    v.push((0, format!("{MBX}.stock")));
    for p in ["com.apple.mobilemail.plist", "com.apple.springboard.plist", "com.apple.preferences.sounds.plist"] {
        v.push((501, format!("{PREFS}/{p}")));
    }
    for n in 1..=3 {
        v.push((501, format!("private/var/mobile/Media/.lt-guest-tools-v{n}")));
    }
    v
}

/// bake_prefs' keys and values (it_prefs' SETTINGS and defaults() for SpringBoard): the reorder tip, Brightness at
/// maximum (SBBacklightLevel2, 1.x SBBacklightLevel), Auto-Lock Never (SBAutoLockTime -1 with SBAutoDimTime -1), the
/// AC-power UI hidden, Auto-Brightness off and Battery % off.
fn prefs_baked() -> Vec<(&'static str, Plist)> {
    vec![
        (REORDER_TIP, true.into()),
        ("SBBacklightLevel2", Plist::Real(1.0)),
        ("SBBacklightLevel", Plist::Real(1.0)),
        ("SBAutoLockTime", Plist::Integer((-1).into())),
        ("SBAutoDimTime", Plist::Integer((-1).into())),
        ("SBHideACPower", true.into()),
        ("SBEnableALS", false.into()),
        ("SBShowBatteryPercentage", false.into()),
    ]
}

pub struct N72Board {
    recipe: crate::entry::Recipe,
    model: &'static str,
    blocks: usize,
    data_protection: bool,
    bootrom: Option<PathBuf>,
    ident: Option<UnitIdentity>,
    epoch: u32,
    major: u32,
    kc_path: String,
    kc_member: String,
    prefix: String,
    volume: Option<PathBuf>,
    derived: Map<String, Value>,
}

pub(crate) fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| crate::internal!("{}: {e}", path.display()))
}

pub(crate) fn file_record(c: &Context, name: &str) -> Result<Value> {
    Ok(json!({"path": name, "sha256": files::sha256_file(&c.file(name))?}))
}

pub(crate) fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// "Darwin Kernel Version ..." up to its NUL.
pub(crate) fn kernel_version(d: &[u8]) -> Option<String> {
    let start = find(d, b"Darwin Kernel Version ", 0)?;
    let end = d[start..].iter().position(|&b| b == 0).map(|n| start + n).unwrap_or(d.len());
    Some(String::from_utf8_lossy(&d[start..end]).into_owned())
}

/// The first "iBoot-<digits and dots>".
pub(crate) fn iboot_version(d: &[u8]) -> Option<String> {
    let mut from = 0;
    while let Some(i) = find(d, b"iBoot-", from) {
        let n = d[i + 6..].iter().take_while(|b| b.is_ascii_digit() || **b == b'.').count();
        if n > 0 {
            return Some(String::from_utf8_lossy(&d[i..i + 6 + n]).into_owned());
        }
        from = i + 6;
    }
    None
}

/// The volume path the decrypted iBoot loads the kernelcache from (its one KC_PREFIX string), without the "/".
pub fn kernelcache_path(iboot: &[u8]) -> Result<String> {
    let p = KC_PREFIX.as_bytes();
    let mut hits = BTreeSet::new();
    let mut i = 0;
    while let Some(r) = find(iboot, p, i) {
        let mut j = r + p.len();
        while j < iboot.len() && (0x21..=0x7E).contains(&iboot[j]) {
            j += 1;
        }
        if j > r + p.len() {
            hits.insert(String::from_utf8_lossy(&iboot[r + 1..j]).into_owned());
        }
        i = r + p.len();
    }
    if hits.len() != 1 {
        return Err(crate::unsupported!("iBoot names {} kernelcache paths ({hits:?}); expected exactly one", hits.len()));
    }
    Ok(hits.into_iter().next().unwrap_or_default())
}

/// KBAG || IV-key for NOR, normal boot and stock restore (the emulated AES engine's GID table): the all_flash img3s
/// in archive order, then the other build components sorted.
pub fn gid_blobs(ipsw: &crate::ipsw::Ipsw, entry: &crate::entry::Entry) -> Result<(Vec<u8>, Vec<String>)> {
    let prefix = format!("Firmware/all_flash/all_flash.{}.production/", entry.board);
    let images: Vec<String> = ipsw.names()?.into_iter().filter(|n| n.starts_with(&prefix) && n.ends_with(".img3")).collect();
    let components = crate::ipsw::build_components(ipsw, &entry.board)?;
    let rest: BTreeSet<&String> = components.values().filter(|v| !images.contains(v)).collect();
    let members: Vec<String> = images.iter().cloned().chain(rest.into_iter().cloned()).collect();
    let (mut out, mut names) = (Vec::new(), Vec::new());
    for n in members {
        let name = n.rsplit('/').next().unwrap_or(&n).to_string();
        let Some(k) = entry.keys.values().find(|k| k.file == name) else { continue };
        let (Some(iv), Some(key)) = (k.iv.as_deref().and_then(crate::entry::from_hex), crate::entry::from_hex(&k.key)) else {
            continue;
        };
        if iv.len() + key.len() != 32 {
            continue;
        }
        let d = ipsw.read(&n)?;
        let Some(t) = img3::tags(&d)?.get("KBAG").copied() else { continue };
        if t.data_length < 40 || t.offset + 52 > d.len() {
            continue;
        }
        let le = |o: usize| u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
        if le(t.offset + 12) != 1 || le(t.offset + 16) != 128 {
            continue; // production, AES-128
        }
        out.extend_from_slice(&d[t.offset + 20..t.offset + 52]);
        out.extend_from_slice(&iv);
        out.extend_from_slice(&key);
        names.push(name);
    }
    Ok((out, names))
}

/// The iPod bootrom the keybag boot's machine loads: --bootrom, LTM_FILES, then Resources/Device beside the helper's
/// folder, then the development assets.
pub fn bootrom_path(given: Option<&Path>, helper: &Path) -> Option<PathBuf> {
    if let Some(p) = given.filter(|p| p.is_file()) {
        return Some(p.to_path_buf());
    }
    let env = std::env::var_os("LTM_FILES").map(PathBuf::from);
    let bundled = files::resolved(helper).parent().map(|d| d.join("../Resources/Device"));
    let dev = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Developer/qemu-ios-files"));
    [env, bundled, dev].into_iter().flatten().map(|d| d.join("bootrom_240_4")).find(|p| p.is_file())
}

/// One Fit for the iPod's baked guest tools: each proven to load (sbdlicon only if the helpers have it).
fn guest_tools_fit(fw: &Firmware, helpers_dir: &Path) -> Result<Fit> {
    let mut fits = Vec::new();
    for n in GUEST_TOOLS {
        let p = helpers_dir.join(n);
        if !p.is_file() {
            if n == "sbdlicon" {
                continue;
            }
            return Err(crate::internal!("guest helper {n} missing from {}", helpers_dir.display()));
        }
        let host = (n == "it_typein.dylib").then(|| format!("/{SPRINGBOARD}"));
        fits.push(fitcheck::loads(n, &read(&p)?, fw, host.as_deref()));
    }
    let piece = format!("guest tools ({})", fits.iter().map(|f| f.piece.as_str()).collect::<Vec<_>>().join(", "));
    let lost: Vec<String> = fits.iter().filter(|f| !f.fits).map(|f| format!("{}: {}", f.piece, f.proof)).collect();
    if !lost.is_empty() {
        return Ok(Fit::new(piece, false, lost.join("; ")));
    }
    let first = &fits[0];
    let proof = format!("each loads ({}: {})", first.piece, first.proof);
    Ok(Fit::new(piece, true, proof))
}

/// contrib/it-prefs offline, where it cannot run: what it writes into the user's com.apple.springboard.plist in
/// `dir`, each key only if SpringBoard names it (the key and its NUL). Returns the report line.
pub(crate) fn bake_prefs(ed: &mut Editor, dir: &str) -> Result<String> {
    let Ok(sb) = ed.read(SPRINGBOARD) else { return Ok("no SpringBoard: left alone".into()) };
    let all = prefs_baked();
    let named: Vec<(&str, Plist)> =
        all.iter().filter(|(k, _)| find(&sb, format!("{k}\0").as_bytes(), 0).is_some()).cloned().collect();
    if named.is_empty() {
        let keys: Vec<&str> = all.iter().map(|(k, _)| *k).collect();
        return Ok(format!("SpringBoard names none of {}: left alone", keys.join(", ")));
    }
    let keys: Vec<&str> = named.iter().map(|(k, _)| *k).collect();
    plistedit::seed(ed, &format!("{dir}/com.apple.springboard.plist"), |d| {
        for (k, v) in &named {
            d.insert(k.to_string(), v.clone());
        }
        Ok(())
    })?;
    Ok(format!("{} baked (no helpers)", keys.join(", ")))
}

impl N72Board {
    pub fn new(o: &Options) -> Result<N72Board> {
        let e = &o.entry;
        let recipe = e.recipe.clone().ok_or_else(|| crate::unsupported!("{}: no n72 recipe for storage none", e.id))?;
        let model = MODELS
            .iter()
            .find(|(s, _)| *s == recipe.storage)
            .map(|(_, m)| *m)
            .ok_or_else(|| crate::unsupported!("{}: no n72 recipe for storage {}", e.id, recipe.storage))?;
        Ok(N72Board {
            model,
            blocks: (recipe.system_mib * 256) as usize,
            data_protection: recipe.option("data_protection").unwrap_or_else(|| crate::entry::data_protection(&e.version)),
            recipe,
            bootrom: None,
            ident: None,
            epoch: 0,
            major: 0,
            kc_path: String::new(),
            kc_member: String::new(),
            prefix: String::new(),
            volume: None,
            derived: Map::new(),
        })
    }

    fn it_keybag(&self) -> String {
        helpers::name(KEYBAG_HELPER_BASE, "armv6")
    }

    /// ipod2g_device.bake over the volume (bake-guest-tools.sh, patch-appsync-dylib.sh, install_web_proxy,
    /// activation) with the shared pieces of system_edits. Appends the owners to patch; returns the report (the
    /// lock's `derived`) and the Cydia modes.
    fn bake(&self, ed: &mut Editor, c: &mut Context, owners: &mut Vec<(u32, String)>) -> Result<(Map<String, Value>, Vec<(String, u16)>)> {
        let helpers_dir = c.o.guest_tools.clone();
        let opt = |k: &str| self.recipe.option(k);
        let cache = system_edits::dyld_cache("armv6");
        let helper = |n: &str| -> Result<Vec<u8>> {
            let p = helpers_dir.join(n);
            if !p.is_file() {
                return Err(crate::internal!("guest helper {n} missing from {}", helpers_dir.display()));
            }
            read(&p)
        };
        let keyboards = crate::keyboards::KeyboardCatalog::read(ed);
        let formats = crate::setup::region_formats(ed);
        // The guest helpers are checked against the volume's dyld and imports: old helper sets with
        // LC_DYLD_INFO_ONLY fail the fit check on 2.x/3.0 and are left out with a warning.
        let fw = Firmware::new(ed.snapshot()?, "armv6", None);
        let tools_fit = guest_tools_fit(&fw, &helpers_dir)?;
        let tools_proof = tools_fit.proof.clone();
        let tools = c.fit.check(tools_fit, false, "left out")?;
        let appsync = opt("appsync") == Some(true);
        if appsync {
            fitcheck::check_app_sync(&c.fit, &fw, &helpers_dir, helpers::APPSYNC)?;
        } else {
            c.fit.not_installed("AppSync", "appsync off");
        }
        // the reorder tip's key: set by it_prefs at boot (tools) or baked below; either way only if SpringBoard reads it
        if let Some(p) = fitcheck::IT_PREFS.first() {
            let outcome = if tools { "kept: it_prefs skips the key at boot" } else { "not baked" };
            if let Some(f) = fitcheck::prefs(&fw, &[*p]).into_iter().next() {
                c.fit.check(f, false, outcome)?;
            }
        }
        let mut report = Map::new();
        let mut gles = false;
        if opt("gles_shim").unwrap_or(true) {
            let log = |s: &str| c.log(s);
            let (engine, owned) = system_edits::install_ca_ogl(ed, &helpers_dir, &fw, &c.fit, &log)?;
            gles = true;
            owners.extend(owned.into_iter().map(|p| (0, p)));
            report.insert("gles".into(), json!(format!("GL front end {engine} as OpenGLES; CoreAnimation composites through it (CA_ENABLE_OGL=1)")));
        } else {
            report.insert("gles".into(), json!("stock OpenGLES, software CA: options.gles_shim off"));
        }
        report.insert("gles_shim".into(), json!(gles));
        report.insert("gles_engine".into(), if gles { json!(helpers::OPENGLES) } else { Value::Null });
        report.insert("guest_tools".into(), json!(if tools { "installed".to_string() } else { format!("omitted: {tools_proof}") }));

        // bake-guest-tools.sh
        if tools {
            ed.mkdirs("usr/local/bin")?;
            ed.write("usr/local/bin/sblaunch", helper("sblaunch")?, Some(0o755))?;
            if helpers_dir.join("sbdlicon").is_file() {
                ed.write("usr/local/bin/sbdlicon", helper("sbdlicon")?, Some(0o755))?;
            }
            ed.write("usr/local/bin/it_agent", helper("it_agent")?, Some(0o755))?;
            ed.write("usr/lib/it_typein.dylib", helper("it_typein.dylib")?, Some(0o755))?;
        }
        let switches: Vec<Vec<String>> = SB_SWITCHES.iter().map(|s| s.iter().map(|n| n.to_string()).collect()).collect();
        // the live volume's view: with the front end in place, its OpenGLES is the one read
        let also = if gles { vec![(helpers::OPENGLES.to_string(), helper(helpers::OPENGLES)?)] } else { Vec::new() };
        c.fit.check(fitcheck::environment(&fw, &switches, &also), false, "kept: a switch nothing reads is inert")?;
        system_edits::edit_springboard_job(ed, |job| {
            let env = plistedit::dict(job, "EnvironmentVariables");
            for k in ["CA_ENABLE_OGL", "LK_ENABLE_OGL"] {
                env.insert(k.into(), (if gles { "1" } else { "0" }).into());
            }
            for k in ["CA_AUTO_ENABLE_OGL", "LK_AUTO_ENABLE_OGL"] {
                env.insert(k.into(), "0".into());
            }
            let old: Vec<String> = env
                .get("DYLD_INSERT_LIBRARIES")
                .and_then(Plist::as_string)
                .map(|s| s.split(':').filter(|p| !p.is_empty()).map(str::to_string).collect())
                .unwrap_or_default();
            let mut libs: Vec<String> =
                old.into_iter().filter(|l| l != "/usr/lib/it_kbd_agent.dylib" && l != "/usr/lib/it_typein.dylib").collect();
            if tools {
                libs.push("/usr/lib/it_typein.dylib".into());
            }
            if libs.is_empty() {
                env.remove("DYLD_INSERT_LIBRARIES");
            } else {
                env.insert("DYLD_INSERT_LIBRARIES".into(), libs.join(":").into());
            }
            Ok(())
        })?;
        if tools {
            ed.write(AGENT_JOB, helper("com.qemu.it-agent.plist")?, Some(0o644))?;
        }
        let pbd = "System/Library/LaunchDaemons/com.qemu.it-pbd.plist";
        if ed.lstat(pbd)?.is_some() {
            ed.remove(pbd)?;
        }
        let sbp = format!("{PREFS}/com.apple.springboard.plist");
        if let Some(d) = plistedit::read(ed, &sbp).ok().flatten() {
            if d.contains_key("SBDontLockEver") || d.contains_key("SBDisableCABlanking") {
                plistedit::rewrite(ed, &sbp, |d| {
                    d.remove("SBDontLockEver");
                    d.remove("SBDisableCABlanking");
                    Ok(())
                })?;
            }
        }
        for (name, changes) in sound_defaults() {
            plistedit::seed(ed, &format!("{PREFS}/{name}"), |d| {
                for (k, v) in changes {
                    d.insert(k.into(), v);
                }
                Ok(())
            })?;
        }
        if let Some(l) = crate::setup::seed_language(
            ed,
            "private/var",
            c.o.language.as_deref(),
            c.o.keyboards.as_deref(),
            keyboards.as_ref(),
            formats.as_ref(),
        )? {
            report.insert("language".into(), json!(format!("{}, region {}, keyboards {:?}", l.language, l.locale, l.keyboards)));
        }
        let media = "private/var/mobile/Media";
        let markers: Vec<String> = (1..=3).map(|v| format!("{media}/.lt-guest-tools-v{v}")).collect();
        if tools {
            ed.mkdirs(media)?;
            for (v, m) in markers.iter().enumerate() {
                ed.write(m, format!("v{}\n", v + 1).into_bytes(), None)?;
            }
        } else {
            for rel in std::iter::once(AGENT_JOB.to_string()).chain(markers) {
                if ed.lstat(&rel)?.is_some() {
                    ed.remove(&rel)?;
                }
            }
        }

        let mut modes = Vec::new();
        if opt("jailbreak") == Some(true) {
            let bootstrap = c.cydia.clone().ok_or_else(|| crate::internal!("jailbreak: no Cydia bootstrap"))?;
            let jb = crate::cydia::install_jailbreak(ed, &bootstrap, tools)?;
            report.insert("afc2".into(), json!(jb.afc2));
            report.insert("cydia".into(), json!(jb.cydia));
            modes = jb.modes;
            owners.extend(jb.root.into_iter().map(|p| (0, p)));
            owners.extend(jb.mobile.into_iter().map(|p| (501, p)));
        }
        if appsync {
            // patch-appsync-dylib.sh
            let log = |s: &str| c.log(s);
            let (line, job) = system_edits::install_app_sync(ed, &helpers_dir.join(helpers::APPSYNC), &cache, &log)?;
            report.insert(
                "appsync".into(),
                json!([line, format!("installation service ({job}) DYLD_INSERT_LIBRARIES += /{APPSYNC_PATH}")]),
            );
            owners.push((0, APPSYNC_PATH.into()));
            if ed.exists(APPSYNC_LAUNCHER_PATH) {
                owners.push((0, APPSYNC_LAUNCHER_PATH.into()));
            }
        }
        let prefs_line = if tools {
            // the seed package's it_prefs (its com.qemu.guest-prefs job), as on every later package
            "it_prefs (guest package): SBDidShowReorderText, then Brightness and Auto-Lock once, at first boot".to_string()
        } else {
            bake_prefs(ed, PREFS)?
        };
        report.insert("prefs".into(), json!(prefs_line));
        // install_web_proxy: the PAC, and the Wi-Fi service on the system volume's /private/var
        if opt("web_proxy").unwrap_or(true) {
            c.fit.check(fitcheck::web_proxy(&fw), false, "kept: the PAC is unused")?;
            let sc = "private/var/preferences/SystemConfiguration";
            owners.extend(system_edits::install_pac(ed, &[sc])?.into_iter().map(|p| (0, p)));
            let prefs = format!("{sc}/preferences.plist");
            plistedit::seed(ed, &prefs, system_edits::wifi_proxy_prefs)?;
            owners.push((0, prefs));
            report.insert("web_proxy".into(), json!(format!("PAC /{PAC_PATH} on the en0 Wi-Fi service")));
        }
        let activation = {
            let log = |s: &str| c.log(s);
            system_edits::activate(ed, &log)
                .map_err(|f| Error { code: Code::ActivationFailed, message: f.0, piece: None })?
        };
        c.activation = Some(activation);
        owners.push((0, LOCKDOWND.into()));
        // mkpkg.seed: the loader and the seed package; it_boot loads the package's jobs (com.qemu.it-agent). On 2.x
        // and 3.0 the package carries only the GL front end's hook.
        if tools || gles {
            let mut omitted = HashSet::new();
            if !appsync {
                omitted.insert(format!("/{APPSYNC_PATH}"));
            }
            let log = |s: &str| c.log(s);
            let (seeded, record) = system_edits::seed_guest_package(ed, &helpers_dir, "armv6", gles, &omitted, &c.fit, &log)?;
            c.guest_package = Some(record);
            owners.extend(seeded.into_iter().map(|p| (0, p)));
        }
        for (uid, p) in guest_tool_owners() {
            if ed.lstat(&p)?.is_some() {
                owners.push((uid, p));
            }
        }
        let keys: Vec<&String> = report.keys().collect();
        c.log(&format!("bake: {keys:?}"));
        drop(fw);
        Ok((report, modes))
    }
}

impl Board for N72Board {
    fn arch(&self) -> &str {
        "armv6"
    }
    fn seed_prefix(&self) -> String {
        "ipod2g".into()
    }
    fn boot_step(&self) -> String {
        "Writing the identity, NOR and boot files".into()
    }
    fn volumes_step(&self) -> String {
        "Building the system volume".into()
    }
    fn keybag_step(&self) -> String {
        "Booting the restore ramdisk".into()
    }
    fn data_protection(&self) -> bool {
        self.data_protection
    }
    fn needs_seal(&self) -> bool {
        false
    }
    fn shipped(&self) -> Vec<String> {
        let mut v = vec!["nor.bin".to_string(), "gid-blobs.bin".to_string()];
        if self.major >= 3 {
            v.push("iBoot.bin".into());
        }
        v
    }

    fn check(&mut self, c: &mut Context) -> Result<()> {
        // Cydia's first boot installs Substrate, whose launchd hook (cynject from /etc/launchd.conf) then hangs
        // launchctl on every later boot of this machine (docs/TODO.md): no jailbreak on the 2G until that is solved.
        if self.recipe.option("jailbreak") == Some(true) {
            return Err(crate::unsupported!("{}: no jailbreak on the iPod touch 2G yet", c.e().id));
        }
        self.bootrom = bootrom_path(c.o.bootrom.as_deref(), &c.o.helper_path);
        if self.data_protection {
            if self.bootrom.is_none() {
                return Err(crate::internal!(
                    "the keybag boot needs the iPod touch 2G's boot ROM (iBoot-240.4): --bootrom, or bootrom_240_4 next to the helper"
                ));
            }
            let kb = self.it_keybag();
            if !c.o.guest_tools.join(&kb).is_file() {
                return Err(crate::internal!("guest helper {kb} missing from {}", c.o.guest_tools.display()));
            }
        }
        Ok(())
    }

    /// Restore.plist: the NAND epoch (DeviceMap SCEP) and the iOS major.
    fn inspect(&mut self, c: &mut Context) -> Result<()> {
        let rp: Plist = plist::from_bytes(&c.ipsw()?.read("Restore.plist")?)
            .map_err(|e| crate::unsupported!("Restore.plist: {e}"))?;
        let epoch = rp
            .as_dictionary()
            .and_then(|d| d.get("DeviceMap"))
            .and_then(Plist::as_array)
            .and_then(|a| a.first())
            .and_then(Plist::as_dictionary)
            .and_then(|d| d.get("SCEP"))
            .and_then(|v| v.as_unsigned_integer().or_else(|| v.as_signed_integer().map(|i| i as u64)));
        let major = c.restore.as_ref().and_then(|r| r.product_version.split('.').next().and_then(|m| m.parse().ok()));
        let (Some(epoch), Some(major)) = (epoch, major) else {
            return Err(crate::unsupported!("Restore.plist has no DeviceMap SCEP (NAND epoch)"));
        };
        self.epoch = epoch as u32;
        self.major = major;
        Ok(())
    }

    fn identity(&mut self, _c: &mut Context, seed: &str, path: &Path) -> Result<Option<String>> {
        let ident = UnitIdentity::synthesize_ipod(seed, self.model, IPAD_REGION, true)?;
        ident.write(path)?;
        let udid = ident.udid().map(str::to_string);
        self.ident = Some(ident);
        Ok(udid)
    }

    /// nor.bin, gid-blobs.bin, iBoot.bin (3.x+), and the derived facts the lock records.
    fn boot_files(&mut self, c: &mut Context) -> Result<()> {
        let ident = self.ident.as_ref().ok_or_else(|| filled("the identity"))?;
        let e = c.e().clone();
        let ipsw = c.ipsw()?;
        let iboot = read(&c.dec_file("iBoot.bin")?)?;
        self.kc_path = kernelcache_path(&iboot)?;
        let components = crate::ipsw::build_components(&ipsw, &e.board)?;
        self.kc_member =
            components.get("KernelCache").cloned().ok_or_else(|| crate::unsupported!("{}: the IPSW names no KernelCache", e.id))?;
        // the machine boots every n72 device with the AMFI pair (qemu-ios ipod_touch_2g.c)
        let kernel = read(&c.dec_file("kernelcache.mach")?)?;
        fitcheck::check_boot_args(&c.fit, Some(&kernel), AMFI_ARGS)?;
        let major = self.major;
        let d = &mut self.derived;
        d.insert("storage_layout".into(), json!("n72-generated-v1"));
        d.insert("nand_epoch".into(), json!(self.epoch));
        d.insert("wrap_shsh".into(), json!(major >= 3));
        d.insert("kernelcache_path".into(), json!(self.kc_path));
        d.insert("kernelcache_member".into(), json!(self.kc_member));
        d.insert("kernel".into(), kernel_version(&kernel).map(Value::String).unwrap_or(Value::Null));
        d.insert("iboot".into(), json!(iboot_version(&iboot).unwrap_or_else(|| "?".into())));
        d.insert("direct_iboot".into(), json!(major >= 3));
        self.prefix = format!("Firmware/all_flash/all_flash.{}.production/", e.board);
        let mut images: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for n in ipsw.names()?.into_iter().filter(|n| n.starts_with(&self.prefix) && n.ends_with(".img3")) {
            let data = ipsw.read(&n)?;
            let t = n72nor::img3_type(&data)?;
            if images.contains_key(&t) {
                return Err(crate::unsupported!("duplicate img3 type {t} in all_flash"));
            }
            images.insert(t, data);
        }
        let manifest = String::from_utf8_lossy(&ipsw.read(&format!("{}manifest", self.prefix))?).into_owned();
        let mut shipped = HashSet::new();
        for m in manifest.split_whitespace() {
            shipped.insert(n72nor::img3_type(&ipsw.read(&format!("{}{m}", self.prefix))?)?);
        }
        let nor_types: Vec<String> = n72nor::ORDER.iter().filter(|t| shipped.contains(**t)).map(|t| t.to_string()).collect();
        // Whose SHSH is wrapped under the emulated UID, as a restore leaves the NOR: 3.x+ every image (the machine
        // enters iBoot directly). 2.x boots SecureROM -> LLB -> iBoot: the epoch-1 chain (iBoot-385.22, 2.1.1)
        // unwraps only iBoot, the epoch-2 chain (385.49, 2.2/2.2.1) every image it loads but the LLB.
        let wrap: Vec<String> = if major >= 3 {
            nor_types.clone()
        } else if self.epoch >= 2 {
            nor_types.iter().filter(|t| *t != "illb").cloned().collect()
        } else {
            vec!["ibot".into()]
        };
        self.derived.insert("nor_images".into(), json!(nor_types));
        self.derived.insert("wrap_shsh_types".into(), json!(wrap));
        let nor = n72nor::build(ident, &images, &nor_types, if major >= 3 { None } else { Some(&wrap) })?;
        std::fs::write(c.file("nor.bin"), nor)?;
        // Stock DFU/recovery also decrypts the restore ramdisks and boot images: their GID inputs, same table.
        let (blobs, names) = gid_blobs(&ipsw, &e)?;
        self.derived.insert("gid_blobs".into(), json!(names));
        std::fs::write(c.file("gid-blobs.bin"), blobs)?;
        if major >= 3 {
            std::fs::write(c.file("iBoot.bin"), &iboot)?;
        }
        Ok(())
    }

    /// The system volume: IPSW rootfs grown to the recipe, fstab, kernelcache, the bake; owners and dates patched.
    fn volumes(&mut self, c: &mut Context) -> Result<()> {
        let volume = c.work.join("volume.img");
        self.volume = Some(volume.clone());
        crate::udif::extract_rootfs(&c.dec_file("rootfs.dmg")?, &volume)?;
        Editor::grow(&volume, (self.blocks * 4096) as u64)?;
        // the IPSW's newest file: everything the recipe writes gets dated as of it
        let newest = {
            let v = Volume::open(&volume, false)?;
            c.log(&format!(
                "{} blocksize={} total={} free={}",
                v.signature, v.block_size, v.total_blocks, v.free_blocks
            ));
            if v.total_blocks as usize != self.blocks || v.block_size != 4096 {
                return Err(crate::internal!(
                    "resize produced {} x {} B blocks, wanted {} x 4096",
                    v.total_blocks,
                    v.block_size,
                    self.blocks
                ));
            }
            v.newest_date()?
        };
        let mut owners: Vec<(u32, String)> = vec![(0, self.kc_path.clone())];
        let mut ed = Editor::open(&volume)?;
        ed.now = c.now;
        ed.write(system_edits::FSTAB, FSTAB_RW.as_bytes().to_vec(), None)?;
        if let Some((dir, _)) = self.kc_path.rsplit_once('/') {
            ed.mkdirs(dir)?;
        }
        ed.write(&self.kc_path, c.ipsw()?.read(&self.kc_member)?, None)?;
        let (report, modes) = self.bake(&mut ed, c, &mut owners)?;
        system_edits::remove_junk(&mut ed)?;
        ed.commit()?;
        for (k, v) in report {
            self.derived.insert(k, v);
        }
        let hfs = Volume::open(&volume, true)?;
        let uids: BTreeSet<u32> = owners.iter().map(|(u, _)| *u).collect();
        for uid in uids {
            let paths: Vec<&str> = owners.iter().filter(|(u, _)| *u == uid).map(|(_, p)| p.as_str()).collect();
            let n = hfs.set_owner(&paths, uid, uid, None, None)?;
            c.log(&format!("{uid}:{uid} patched {n} catalog record(s)"));
        }
        for (path, mode) in &modes {
            hfs.set_owner(&[path.as_str()], 0, 0, Some(*mode), None)?;
        }
        let n = hfs.normalize(newest, newest, None)?;
        c.log(&format!("{n} catalog records dated as of the IPSW's newest file"));
        Ok(())
    }

    fn store(&mut self, c: &mut Context) -> Result<()> {
        let volume = self.volume.clone().ok_or_else(|| filled("the system volume"))?;
        let (written, meta) = n72nand::write(&volume, self.blocks, self.epoch, &c.nand)?;
        c.log(&format!("{written} filesystem pages, {meta} metadata pages generated (epoch {})", self.epoch));
        std::fs::remove_file(&volume)?;
        Ok(())
    }

    /// 4.x data protection: effaceable + system keybag from the IPSW's own Update ramdisk.
    fn keybag(&mut self, c: &mut Context) -> Result<()> {
        let (source, name) = crate::k48_recipe::keybag_ramdisk(c)?;
        let bootrom = self.bootrom.clone().ok_or_else(|| filled("the iPod bootrom"))?;
        let machine: BTreeMap<String, String> = MACHINE.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let log = |l: &str| c.log(l);
        crate::n72keybag::run(
            &c.o.out,
            c.decrypted()?,
            &source,
            &c.o.guest_tools.join(self.it_keybag()),
            &bootrom,
            &c.o.helper,
            &machine,
            &c.work,
            &log,
        )?;
        self.derived.insert("keybag_ramdisk".into(), json!(name));
        Ok(())
    }

    fn lock(&mut self, c: &Context) -> Result<Value> {
        let ident = self.ident.as_ref().ok_or_else(|| filled("the identity"))?;
        let (Some(mac), Some(bt), Some(ecid)) = (ident.get("wifi-mac"), ident.get("bt-mac"), ident.get("unique-chip-id")) else {
            return Err(crate::internal!("{}: the identity has no Wi-Fi MAC, Bluetooth MAC or ECID", c.e().id));
        };
        let mut machine: Map<String, Value> = MACHINE.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        machine.insert("wifi-mac".into(), json!(mac));
        machine.insert("bt-mac".into(), json!(bt));
        machine.insert("ecid".into(), json!(ecid));
        Ok(json!({
            "inputs": {"kernelcache": self.kc_member, "iboot": "iBoot.bin", "all_flash": self.prefix},
            "outputs": {
                "nand": {"pages": c.nand_hashes.len()},
                "nor": file_record(c, "nor.bin")?,
                "iboot": if self.major >= 3 { file_record(c, "iBoot.bin")? } else { Value::Null },
                "gid_blobs": file_record(c, "gid-blobs.bin")?,
            },
            "derived": Value::Object(self.derived.clone()),
            // 3.x+ enters its decrypted iBoot directly (the bootrom rejects a personalized LLB); 2.x runs the real
            // bootrom -> NOR LLB -> iBoot chain and ships no iBoot.bin
            "boot_strategy": if self.major >= 3 { "iboot" } else { "bootrom" },
            // The BCM4325 CIS and NOR wifiaddr belong to the same unit.
            "machine": Value::Object(machine),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_paths() {
        let d = b"xx\0Darwin Kernel Version 10.0.0d3: Fri; root:xnu\0iBoot-\0iBoot-636.66.33\0";
        assert_eq!(kernel_version(d).as_deref(), Some("Darwin Kernel Version 10.0.0d3: Fri; root:xnu"));
        assert_eq!(iboot_version(d).as_deref(), Some("iBoot-636.66.33"));
        let ib = b"..\0/System/Library/Caches/com.apple.kernelcaches/kernelcache.s5l8720x\0../System/Library/Caches/com.apple.kernelcaches/kernelcache.s5l8720x\0";
        assert_eq!(kernelcache_path(ib).unwrap(), "System/Library/Caches/com.apple.kernelcaches/kernelcache.s5l8720x");
    }
}

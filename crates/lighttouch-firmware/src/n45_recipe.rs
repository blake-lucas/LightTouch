//! N45Board: the iPod touch 1G (n45ap, recipe "n45") and the original iPhone (m68ap, recipe "m68") side of create,
//! for iPhone OS 1.x. Swift's N45/N45Recipe.swift.
//!
//! Boot files: iBoot.bin (the IPSW's iBoot-204 IMG2 payload, which the machine enters at 0x18000000) and nor.bin
//! (n45nor over the IPSW's all_flash IMG2s). The bootrom (bootrom_s5l8900) is a boot input, like the 2G's.
//! Volume: the IPSW rootfs grown to the recipe, then the 1.x bake of devos50's qemu-ios-generate-nand changes:
//! fstab rw with no /private/var line (one partition), the kernelcache (the IPSW's 8900 container, which the
//! machine's 8900 engine decrypts) where iBoot loads it, the LaunchDaemons changes.md keeps (and 1.1.3+'s lockbot),
//! the /var/root/Library skeleton, Wi-Fi as a device that has joined the emulator's network, SpringBoard's
//! preferences; then ipod1g_device.bake: when the stock OpenGLES exports exactly opengles-1x.exports (a guest helper),
//! the seed package's n45-ios1 hook (OpenGLES-1x, the GL front end) replaces it and SpringBoard gets LK_ENABLE_OGL=1
//! LK_AUTO_ENABLE_OGL=0 (else software LayerKit); the loader and the seed package go in either way. The volume is
//! journaled (it is also /private/var), the journal left for the device's kernel; activation as every board has it;
//! owners patched in the catalog. Store: n45nand. No other guest tools on 1.x, no keybag, no seal.
//!
//! The recipe: storage "8g" (MA623), system_mib = the volume. The original iPhone: storage "4g" (MA501: four chip
//! enables), an iPhone identity (IMEI, Bluetooth MAC), BTServer kept, the NAND signature its iBoot carries.
use crate::create::{filled, Board, Context, Options};
use crate::error::{Code, Error, Result};
use crate::fitcheck::{self, Firmware, Fit};
use crate::hfs::{Editor, Volume};
use crate::identity::{UnitIdentity, IPAD_REGION};
use crate::img3::{apple8900, img2};
use crate::macho::{is_export, MachO32};
use crate::n72_recipe::{bake_prefs, file_record, find, iboot_version, kernel_version, kernelcache_path, read, FSTAB_RW, SPRINGBOARD};
use crate::plistedit::{self, Format};
use crate::system_edits::{self, helpers, DAEMONS, LOCKDOWND, PAC_PATH, SPRINGBOARD_JOB};
use crate::{n45nand, n45nor};
use plist::{Dictionary, Value as Plist};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// changes.md: every other job is removed (the rest wait on hardware this machine does not have). Plus usbptpd (no
/// usbmux without it: IOIpodUSBDevice starts no USB stack until every function, PTP among them, has registered),
/// coreaudiod.plist (mediaserverd: every system sound, played on N45 as Beep + Buzz on the piezo) and mDNSResponder
/// (1.x's libSystem resolves every host name through it, unicast DNS included).
pub const KEPT_DAEMONS: [&str; 9] = [
    "com.apple.AddressBook.plist",
    "com.apple.CommCenter.plist",
    "com.apple.configd.plist",
    "com.apple.mDNSResponder.plist",
    "com.apple.mobile.lockdown.plist",
    "com.apple.notifyd.plist",
    "com.apple.SpringBoard.plist",
    "com.apple.usbptpd.plist",
    "coreaudiod.plist",
];
/// Kept where the firmware ships them: 1.1.3+ lockdownd starts each service (AFC among them) through lockbot.
pub const KEPT_WHEN_SHIPPED: [&str; 1] = ["com.apple.mobile.lockbot.plist"];
/// Kept on the iPhone (m68ap) only: the machine answers BTServer's HCI there; the iPod touch has no Bluetooth.
pub const IPHONE_DAEMONS: [&str; 1] = ["com.apple.BTServer.plist"];
pub const ROOT_LIBRARY: &str = "private/var/root/Library";
pub const OPENGLES_EXPORTS: &str = "opengles-1x.exports";
/// The piece a 2.0-or-later entry is refused for (the app words it for the user).
pub const ONE_PIECE: &str = "iPhone OS 1.x boot chain";
/// The journal the volume gets, as diskutil enableJournal sizes it for a volume this small.
const JOURNAL_BYTES: u64 = 8 << 20;

/// The jobs the bake removes from this firmware's LaunchDaemons.
pub fn removed_daemons(jobs: &[String], iphone: bool) -> Vec<String> {
    let mut v: Vec<String> = jobs
        .iter()
        .filter(|j| {
            !KEPT_DAEMONS.contains(&j.as_str())
                && !KEPT_WHEN_SHIPPED.contains(&j.as_str())
                && !(iphone && IPHONE_DAEMONS.contains(&j.as_str()))
        })
        .cloned()
        .collect();
    v.sort();
    v
}

/// The jobs this machine keeps must all be among the firmware's (usbptpd: no usbmux without it).
pub fn kept_daemons_fit(jobs: &[String]) -> Fit {
    let absent: Vec<&str> = KEPT_DAEMONS.iter().copied().filter(|k| !jobs.iter().any(|j| j == k)).collect();
    Fit::new(
        format!("LaunchDaemons kept on 1.x ({})", KEPT_DAEMONS.len()),
        absent.is_empty(),
        if absent.is_empty() { "all shipped by this firmware".to_string() } else { format!("this firmware ships no {}", absent.join(", ")) },
    )
}

/// The directories of the kept jobs' StandardOutPath/StandardErrorPath that the image lacks, made (volume-relative,
/// to own as root). 1.x's launchd does not start a job whose log file it cannot open.
pub fn make_log_dirs(ed: &mut Editor, jobs: &[String]) -> Result<Vec<String>> {
    let mut made: Vec<String> = Vec::new();
    let mut sorted = jobs.to_vec();
    sorted.sort();
    for job in sorted {
        let Some(d) = plistedit::read(ed, &format!("{DAEMONS}/{job}")).ok().flatten() else { continue };
        for key in ["StandardOutPath", "StandardErrorPath"] {
            let Some(path) = d.get(key).and_then(Plist::as_string).filter(|p| p.starts_with('/')) else { continue };
            let mut dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("").trim_start_matches('/').to_string();
            if dir.starts_with("var/") {
                dir = format!("private/{dir}");
            }
            if dir.is_empty() || made.contains(&dir) || ed.exists(&dir) {
                continue;
            }
            ed.mkdirs(&dir)?;
            made.push(dir);
        }
    }
    Ok(made)
}

/// Whose Library SpringBoard reads its preferences from: mobile (501) when its launchd job says so (1.1.3 on), else
/// root.
pub fn springboard_user(ed: &Editor) -> (&'static str, u32) {
    let user = plistedit::read(ed, SPRINGBOARD_JOB).ok().flatten();
    if user.as_ref().and_then(|d| d.get("UserName")).and_then(Plist::as_string) == Some("mobile") {
        ("private/var/mobile/Library", 501)
    } else {
        (ROOT_LIBRARY, 0)
    }
}

/// Where configd keeps its SCPreferences: root's home while everything runs as root (1.0 to 1.1.2); from 1.1.3,
/// /Library/Preferences/SystemConfiguration, which the root links to /private/var/preferences.
pub fn sc_prefs(ed: &Editor) -> String {
    if springboard_user(ed).1 == 0 {
        format!("{ROOT_LIBRARY}/Preferences/SystemConfiguration")
    } else {
        "private/var/preferences/SystemConfiguration".into()
    }
}

/// configd's Aeropuerto plug-in keeps the Wi-Fi power preference and the networks it has joined here.
pub fn wifi_prefs(ed: &Editor) -> String {
    format!("{}/com.apple.wifi.plist", sc_prefs(ed))
}

/// The emulator's access point (the 88W8686 model's open "qemu-ios", channel 6) as a known-networks entry.
pub fn qemu_network() -> Plist {
    Plist::Dictionary(plistedit::dictionary([
        ("AP_MODE", 2.into()),
        ("BEACON_INT", 10.into()),
        ("CAPABILITIES", 1.into()),
        ("CHANNEL", 6.into()),
        ("CHANNEL_FLAGS", 8.into()),
        ("HIDDEN_NETWORK", false.into()),
        ("SSID", Plist::Data(b"qemu-ios".to_vec())),
        ("SSID_STR", "qemu-ios".into()),
    ]))
}

/// The PAC, the en0 AirPort service carrying it (preferences.plist) and the known network (wifi_prefs) in sc_prefs,
/// merged into what configd may have written there already. Returns the paths to make root-owned.
pub fn seed_system_configuration(ed: &mut Editor) -> Result<Vec<String>> {
    let sc = sc_prefs(ed);
    let wifi = wifi_prefs(ed);
    let mut owned = system_edits::install_pac(ed, &[sc.as_str()])?;
    plistedit::seed(ed, &format!("{sc}/preferences.plist"), |d| {
        let mut en0 = false;
        if let Some(Plist::Dictionary(services)) = d.get_mut("NetworkServices") {
            for s in services.values_mut() {
                let Plist::Dictionary(s) = s else { continue };
                let is_en0 = s
                    .get("Interface")
                    .and_then(Plist::as_dictionary)
                    .and_then(|i| i.get("DeviceName"))
                    .and_then(Plist::as_string)
                    == Some("en0");
                if is_en0 {
                    s.insert("Proxies".into(), system_edits::pac_proxies());
                    en0 = true;
                }
            }
        }
        if !en0 {
            system_edits::wifi_proxy_prefs(d)?;
        }
        // 1.0's cellular data service is CommCenter's, made at run time: configd applies the set's global Proxies
        plistedit::dict(system_edits::current_network(d), "Global").insert("Proxies".into(), system_edits::pac_proxies());
        Ok(())
    })?;
    let mut known: Dictionary = plistedit::read(ed, &wifi).ok().flatten().unwrap_or_default();
    if !known.contains_key("AllowEnable") {
        known.insert("AllowEnable".into(), true.into());
    }
    let mut list: Vec<Plist> = match known.get("List of known networks") {
        Some(Plist::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let has = list.iter().any(|n| n.as_dictionary().and_then(|n| n.get("SSID_STR")).and_then(Plist::as_string) == Some("qemu-ios"));
    if !has {
        list.push(qemu_network());
        known.insert("List of known networks".into(), Plist::Array(list));
    }
    ed.write(&wifi, plistedit::serialize(&Plist::Dictionary(known), Format::Xml)?, Some(0o644))?;
    owned.push(format!("{sc}/preferences.plist"));
    owned.push(wifi);
    Ok(owned)
}

/// 1.x's SpringBoard puts the system to deep sleep when the lock screen idles, and the machine has no resume path:
/// SBDisableIdleSleep keeps it to a display sleep. Set in `prefs` when SpringBoard names it; true if set.
pub fn bake_no_idle_sleep(ed: &mut Editor, prefs: &str) -> Result<bool> {
    let key = "SBDisableIdleSleep";
    let Ok(sb) = ed.read(SPRINGBOARD) else { return Ok(false) };
    if find(&sb, format!("{key}\0").as_bytes(), 0).is_none() {
        return Ok(false);
    }
    plistedit::seed(ed, prefs, |d| {
        d.insert(key.into(), true.into());
        Ok(())
    })?;
    Ok(true)
}

/// An IPSW kernelcache that is the restore environment's, not the system's.
pub fn restore_only(member: &str) -> bool {
    member.rsplit('/').next().unwrap_or(member).contains(".restore.")
}

/// gles2x_exports.scan: the defined external symbols of the armv6 (else armv7) slice, without the leading _.
pub fn exported_symbols(data: &[u8]) -> Result<Vec<String>> {
    let (m, _) = MachO32::slice(data, "armv6")
        .or_else(|| MachO32::slice(data, "armv7"))
        .ok_or_else(|| crate::unsupported!("OpenGLES: not a 32-bit Mach-O"))?;
    let mut out: Vec<String> = m
        .symbols()
        .into_iter()
        .filter(|s| is_export(s.kind))
        .map(|s| s.name.strip_prefix('_').map(str::to_string).unwrap_or(s.name))
        .collect();
    out.sort();
    Ok(out)
}

/// ipod1g_device's front-end check: (true, line) if the stock OpenGLES exports exactly the names in `exports`, so
/// the package's hook may replace it; else (false, why), stock kept.
pub fn front_end(ed: &Editor, exports: &Path) -> Result<(bool, String)> {
    let Ok(stock) = ed.read(fitcheck::OPENGLES) else {
        return Ok((false, format!("no {}", fitcheck::OPENGLES)));
    };
    let list = std::fs::read_to_string(exports).map_err(|_| {
        let name = exports.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = exports.parent().map(|d| d.display().to_string()).unwrap_or_default();
        crate::internal!("guest helper {name} missing from {dir}")
    })?;
    let want: BTreeSet<String> =
        list.lines().filter(|l| !l.starts_with('#')).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    let got: BTreeSet<String> = exported_symbols(&stock)?.into_iter().collect();
    if want != got {
        let missing: Vec<&String> = want.difference(&got).take(4).collect();
        let extra: Vec<&String> = got.difference(&want).take(4).collect();
        let name = exports.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok((false, format!("stock OpenGLES exports differ from {name} (missing {missing:?}, extra {extra:?}): stock kept")));
    }
    Ok((true, format!("GL front end replaces OpenGLES ({} exports, the firmware's own)", got.len())))
}

pub struct N45Board {
    recipe: crate::entry::Recipe,
    model: &'static str,
    bytes: u64,
    iphone: bool,
    ident: Option<UnitIdentity>,
    volume: Option<PathBuf>,
    kc_path: String,
    kc_member: String,
    prefix: String,
    derived: Map<String, Value>,
}

impl N45Board {
    pub fn new(o: &Options) -> Result<N45Board> {
        let e = &o.entry;
        let iphone = e.board == "m68ap";
        let name = e.recipe.as_ref().map(|r| r.name.as_str()).unwrap_or("n45");
        let storage = e.recipe.as_ref().map(|r| r.storage.as_str()).unwrap_or("none");
        let model = match (iphone, storage) {
            (false, "8g") => "MA623",
            (true, "4g") => "MA501",
            _ => return Err(crate::unsupported!("{}: no {name} recipe for storage {storage}", e.id)),
        };
        let recipe = e.recipe.clone().ok_or_else(|| filled("the recipe"))?;
        Ok(N45Board {
            bytes: (recipe.system_mib as u64) << 20,
            recipe,
            model,
            iphone,
            ident: None,
            volume: None,
            kc_path: String::new(),
            kc_member: String::new(),
            prefix: String::new(),
            derived: Map::new(),
        })
    }

    fn banks(&self) -> usize {
        if self.iphone { 4 } else { n45nand::BANKS }
    }

    /// ipod1g_device.bake: the GL front end if the stock OpenGLES exports exactly opengles-1x.exports (and `gles`),
    /// the seed package (the loader and n45-ios1), SpringBoard's LK_* environment. Returns the root-owned paths.
    fn bake(&mut self, ed: &mut Editor, c: &mut Context, gles: bool) -> Result<Vec<String>> {
        let helpers_dir = c.o.guest_tools.clone();
        let mut front = false;
        if gles {
            let (ok, line) = front_end(ed, &helpers_dir.join(OPENGLES_EXPORTS))?;
            front = ok;
            let tail = if ok { "; LayerKit composites through it (LK_ENABLE_OGL=1)" } else { "; software LayerKit" };
            self.derived.insert("gles".into(), json!(format!("{line}{tail}")));
        } else {
            self.derived.insert("gles".into(), json!("gles off; software LayerKit"));
        }
        let (seeded, record) = {
            let log = |s: &str| c.log(s);
            system_edits::seed_guest_package(ed, &helpers_dir, "armv6", front, &Default::default(), &c.fit, &log)?
        };
        let hook = format!("/{}", fitcheck::OPENGLES);
        if front && !record.hooks.contains(&hook) {
            // LK_ENABLE_OGL=1 over the stock IMG driver drives the MBX's tile accelerator, which qemu-ios doesn't
            // model: fail rather than wedge
            return Err(crate::internal!("{} has no OpenGLES hook for this build; rebuild the guest package", helpers::itpack("armv6")));
        }
        c.guest_package = Some(record);
        if front {
            let fw = Firmware::new(ed.snapshot()?, "armv6", None);
            let switches = vec![vec!["LK_ENABLE_OGL".to_string()], vec!["LK_AUTO_ENABLE_OGL".to_string()]];
            c.fit.check(fitcheck::environment(&fw, &switches, &[]), false, "kept: a switch nothing reads is inert")?;
        }
        system_edits::edit_springboard_job(ed, |job| {
            let env = plistedit::dict(job, "EnvironmentVariables");
            if front {
                env.insert("LK_ENABLE_OGL".into(), "1".into());
                env.insert("LK_AUTO_ENABLE_OGL".into(), "0".into());
            } else {
                env.remove("LK_ENABLE_OGL");
                env.remove("LK_AUTO_ENABLE_OGL");
            }
            Ok(())
        })?;
        self.derived.insert("gles_engine".into(), if front { json!("OpenGLES") } else { Value::Null });
        Ok(std::iter::once(SPRINGBOARD_JOB.to_string()).chain(seeded).collect())
    }
}

impl Board for N45Board {
    fn arch(&self) -> &str {
        "armv6"
    }
    fn seed_prefix(&self) -> String {
        if self.iphone { "iphone2g" } else { "ipod1g" }.into()
    }
    fn boot_step(&self) -> String {
        "Writing the identity, NOR and boot files".into()
    }
    fn volumes_step(&self) -> String {
        "Building the system volume".into()
    }
    fn keybag_step(&self) -> String {
        String::new()
    }
    fn data_protection(&self) -> bool {
        false
    }
    fn needs_seal(&self) -> bool {
        false
    }
    fn shipped(&self) -> Vec<String> {
        vec!["nor.bin".into(), "iBoot.bin".into()]
    }

    fn check(&mut self, c: &mut Context) -> Result<()> {
        // 2.0 and later boot an IMG3 chain that qemu-ios's ipod_touch_1g doesn't model: refused before the IPSW is
        // hashed, instead of at n45nor's IMG2 read.
        if c.e().major().max(1) >= 2 {
            return Err(Error {
                code: Code::Unsupported,
                message: format!("{}: this machine boots iPhone OS 1.x only ({} is an IMG3 firmware)", c.e().id, c.e().version),
                piece: Some(ONE_PIECE.into()),
            });
        }
        // 1.0's lockdownd starts an afc2 entry but its afcd answers no path: no jailbreak on 1.x.
        if self.recipe.option("jailbreak") == Some(true) {
            return Err(crate::unsupported!("{}: the jailbreak option needs iPhone OS 2.0 or later", c.e().id));
        }
        Ok(())
    }

    fn identity(&mut self, _c: &mut Context, seed: &str, path: &Path) -> Result<Option<String>> {
        let ident = if self.iphone {
            UnitIdentity::synthesize_iphone(seed, self.model, IPAD_REGION)?
        } else {
            UnitIdentity::synthesize_ipod(seed, self.model, IPAD_REGION, false)?
        };
        ident.write(path)?;
        let udid = ident.udid().map(str::to_string);
        self.ident = Some(ident);
        Ok(udid)
    }

    fn boot_files(&mut self, c: &mut Context) -> Result<()> {
        let ident = self.ident.as_ref().ok_or_else(|| filled("the identity"))?;
        let ipsw = c.ipsw()?;
        let iboot = read(&c.dec_file("iBoot.bin")?)?;
        self.kc_path = kernelcache_path(&iboot)?;
        self.kc_member = crate::ipsw::build_components(&ipsw, &c.e().board)?
            .get("KernelCache")
            .cloned()
            .ok_or_else(|| crate::unsupported!("{}: the IPSW names no KernelCache", c.e().id))?;
        self.prefix = format!("Firmware/all_flash/all_flash.{}.production/", c.e().board);
        let mut images = BTreeMap::new();
        for n in ipsw.names()?.into_iter().filter(|n| n.starts_with(&self.prefix) && n.ends_with(".img2")) {
            let body = apple8900::body(&ipsw.read(&n)?)?;
            images.insert(img2::header(&body)?.kind, body);
        }
        std::fs::write(c.file("nor.bin"), n45nor::build(ident, &images, n45nor::BOOT_ARGS)?)?;
        std::fs::write(c.file("iBoot.bin"), &iboot)?;
        let kernel = read(&c.dec_file("kernelcache.mach")?)?;
        let d = &mut self.derived;
        d.insert("kernelcache_path".into(), json!(self.kc_path));
        d.insert("kernelcache_member".into(), json!(self.kc_member));
        d.insert("nor_images".into(), json!(n45nor::ORDER));
        d.insert("boot_args".into(), json!(n45nor::BOOT_ARGS));
        d.insert("kernel".into(), kernel_version(&kernel).map(Value::String).unwrap_or(Value::Null));
        d.insert("iboot".into(), json!(iboot_version(&iboot).unwrap_or_else(|| "?".into())));
        Ok(())
    }

    fn volumes(&mut self, c: &mut Context) -> Result<()> {
        let volume = c.work.join("volume.img");
        self.volume = Some(volume.clone());
        crate::udif::extract_rootfs(&c.dec_file("rootfs.dmg")?, &volume)?;
        Editor::grow(&volume, self.bytes)?;
        let newest = {
            let v = Volume::open(&volume, false)?;
            c.log(&format!("{} blocksize={} total={} free={}", v.signature, v.block_size, v.total_blocks, v.free_blocks));
            if v.total_blocks * v.block_size != self.bytes {
                return Err(crate::internal!("resize produced {} x {} B, wanted {} B", v.total_blocks, v.block_size, self.bytes));
            }
            v.newest_date()?
        };
        let mut owners: Vec<(u32, String)> = vec![(0, self.kc_path.clone()), (0, LOCKDOWND.into())];
        let mut ed = Editor::open(&volume)?;
        ed.now = c.now;
        ed.write(system_edits::FSTAB, FSTAB_RW.as_bytes().to_vec(), None)?;
        if let Some((dir, _)) = self.kc_path.rsplit_once('/') {
            ed.mkdirs(dir)?;
        }
        // 1.0 ships only the restore kernelcache in the IPSW; its system volume carries the one iBoot boots: kept.
        if !(restore_only(&self.kc_member) && ed.exists(&self.kc_path)) {
            ed.write(&self.kc_path, c.ipsw()?.read(&self.kc_member)?, None)?;
        }
        let jobs: Vec<String> = ed.list(DAEMONS)?.into_iter().filter(|j| j.ends_with(".plist")).collect();
        c.fit.check(kept_daemons_fit(&jobs), false, "the rest removed as planned")?;
        let appsync_why = if self.recipe.option("appsync") == Some(true) { "the 1.x recipe has no AppSync" } else { "appsync off" };
        c.fit.not_installed("AppSync", appsync_why);
        let removed = removed_daemons(&jobs, self.iphone);
        for n in &removed {
            ed.remove(&format!("{DAEMONS}/{n}"))?;
        }
        let kept: Vec<String> = jobs.iter().filter(|j| !removed.contains(j)).cloned().collect();
        owners.extend(make_log_dirs(&mut ed, &kept)?.into_iter().map(|d| (0, d)));
        for d in ["", "/AddressBook", "/Lockdown", "/Preferences"] {
            let dir = format!("{ROOT_LIBRARY}{d}");
            ed.mkdirs(&dir)?;
            owners.push((0, dir));
        }
        // An iPod's first iTunes handshake clears BrickState independently of activation; the 1G has no host USB
        // transport yet, so its fresh data volume says so. lockdownd owns the rest of this dictionary.
        let ark = format!("{ROOT_LIBRARY}/Lockdown/data_ark.plist");
        let ark_bytes = plistedit::serialize(&Plist::Dictionary(plistedit::dictionary([("-BrickState", false.into())])), Format::Xml)?;
        ed.write(&ark, ark_bytes, Some(0o600))?;
        owners.push((0, ark));
        // Wi-Fi as a device that has joined the emulator's network before, carrying the web proxy's PAC
        {
            let fw = Firmware::new(ed.snapshot()?, "armv6", None);
            c.fit.check(fitcheck::web_proxy(&fw), false, "kept: the PAC is unused")?;
        }
        owners.extend(seed_system_configuration(&mut ed)?.into_iter().map(|p| (0, p)));
        let wifi = format!("en0 AirPort service (PAC /{PAC_PATH}); known network qemu-ios, Wi-Fi on (/{})", wifi_prefs(&ed));
        self.derived.insert("wifi".into(), json!(wifi));
        // 1.x runs no guest helpers (it_prefs): its SpringBoard preferences, in the home of the user SpringBoard
        // runs as
        let (library, uid) = springboard_user(&ed);
        let sb_prefs = format!("{library}/Preferences/com.apple.springboard.plist");
        let prefs = bake_prefs(&mut ed, &format!("{library}/Preferences"))?;
        let prefs = if bake_no_idle_sleep(&mut ed, &sb_prefs)? { format!("{prefs}; SBDisableIdleSleep (no deep sleep)") } else { prefs };
        self.derived.insert("prefs".into(), json!(prefs));
        if ed.exists(&sb_prefs) {
            owners.push((uid, sb_prefs));
        }
        let keyboards = crate::keyboards::KeyboardCatalog::read(&ed);
        let formats = crate::setup::region_formats(&ed);
        if let Some(l) = crate::setup::seed_language(
            &mut ed,
            "private/var",
            c.o.language.as_deref(),
            c.o.keyboards.as_deref(),
            keyboards.as_ref(),
            formats.as_ref(),
        )? {
            self.derived.insert("language".into(), json!(format!("{}, region {}, keyboards {:?}", l.language, l.locale, l.keyboards)));
        }
        let gles = self.recipe.option("gles_shim").unwrap_or(true);
        owners.extend(self.bake(&mut ed, c, gles)?.into_iter().map(|p| (0, p)));
        // One partition, so the root holds what a device keeps in its journaled /private/var: journaled, so a hard
        // power-off is replayed at mount, the journal left for the device's first mount to initialize.
        ed.enable_journal(JOURNAL_BYTES)?;
        let activation = {
            let log = |s: &str| c.log(s);
            system_edits::activate(&mut ed, &log).map_err(|f| Error { code: Code::ActivationFailed, message: f.0, piece: None })?
        };
        c.activation = Some(activation);
        system_edits::remove_junk(&mut ed)?;
        ed.commit()?;
        self.derived.insert("launch_daemons_removed".into(), json!(removed));
        let hfs = Volume::open(&volume, true)?;
        let uids: BTreeSet<u32> = owners.iter().map(|(u, _)| *u).collect();
        for uid in uids {
            let paths: Vec<&str> = owners.iter().filter(|(u, _)| *u == uid).map(|(_, p)| p.as_str()).collect();
            let n = hfs.set_owner(&paths, uid, uid, None, None)?;
            c.log(&format!("{uid}:{uid} patched {n} catalog record(s)"));
        }
        let n = hfs.normalize(newest, newest, None)?;
        c.log(&format!("{n} catalog records dated as of the IPSW's newest file"));
        Ok(())
    }

    fn store(&mut self, c: &mut Context) -> Result<()> {
        let volume = self.volume.clone().ok_or_else(|| filled("the system volume"))?;
        let fil = n45nand::fil_id(&read(&c.file("iBoot.bin"))?)?;
        let (written, meta) = n45nand::write(&volume, &c.nand, fil, self.banks(), self.iphone)?;
        c.log(&format!("{written} filesystem pages, {meta} metadata pages generated (NAND signature 0x{fil:x})"));
        std::fs::remove_file(&volume)?;
        Ok(())
    }

    fn lock(&mut self, c: &Context) -> Result<Value> {
        let ident = self.ident.as_ref().ok_or_else(|| filled("the identity"))?;
        let mut machine = Map::new();
        machine.insert("wifi-mac".into(), json!(ident.get("wifi-mac").unwrap_or("")));
        if self.iphone {
            machine.insert("imei".into(), json!(ident.get("imei").unwrap_or("")));
        }
        Ok(json!({
            "inputs": {
                "kernelcache": self.kc_member,
                "iboot": format!("{}iBoot.{}.RELEASE.img2", self.prefix, c.e().board),
                "all_flash": self.prefix,
            },
            "outputs": {
                "nand": {"pages": c.nand_hashes.len()},
                "nor": file_record(c, "nor.bin")?,
                "iboot": file_record(c, "iBoot.bin")?,
            },
            "derived": Value::Object(self.derived.clone()),
            // The 88W8686's EEPROM MAC: the same unit whose nvram wifiaddr iBoot copies into the DT (n45nor).
            "machine": Value::Object(machine),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemons() {
        let jobs: Vec<String> = ["com.apple.SpringBoard.plist", "com.apple.iapd.plist", "com.apple.BTServer.plist", "com.apple.mobile.lockbot.plist"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(removed_daemons(&jobs, false), ["com.apple.BTServer.plist", "com.apple.iapd.plist"]);
        assert_eq!(removed_daemons(&jobs, true), ["com.apple.iapd.plist"]);
        assert!(!kept_daemons_fit(&jobs).fits);
        assert!(restore_only("kernelcache.restore.s5l8900x"));
        assert!(!restore_only("kernelcache.release.s5l8900x"));
    }
}

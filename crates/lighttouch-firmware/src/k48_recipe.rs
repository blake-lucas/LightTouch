//! K48Board: the A4 boards' side of create: the iPad 1 (k48ap, recipe "k48"), the iPod touch 4G (n81ap: kboot only,
//! the NOR grafted, -M iPod-Touch-4G), the iPhone 4 (n90ap: as n81, its baseband kept), the S5L8920 iPhone 3GS
//! (n88ap: kboot with its own NOR, its store unwhitened at the IPSW's SCEP epoch) and the S5L8922 iPod touch 3G
//! (n18ap: as n88, the NOR grafted). Swift's K48/K48Recipe.swift.
//!
//! Boot files by strategy: `iboot` (k48ap's default) iBoot.bin (pattern-patched), nor.bin (packed, writable) and
//! gid-blobs.bin; `kboot` kboot.bin and, for 4.x data protection, a blank nor.bin. Volumes: system_edits::build_k48.
//! Store: k48nand. The keybag one-shot (4.x) and the seal (one clean halt) each boot once; a failure fails the
//! preparation.
use crate::boot::{self, Board as Hardware, DeviceInfo, IPadBoot};
use crate::create::{filled, Board, Context, Options};
use crate::error::{Code, Error, Result};
use crate::fitcheck::{self, Fit};
use crate::hfs::{Editor, Volume};
use crate::oneshot::{self, Helper, OneShot};
use crate::system_edits::{self, helpers};
use crate::identity::UnitIdentity;
use crate::{files, k48iboot, k48nand, kboot, sandbox_patch};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A jailbroken device's kernel leaves code signatures to AMFI alone (amfi_get_out_of_my_way), as the jailbreaks'
/// kernel patches did: with AMFI asking amfid about every ad hoc signed library, Substrate's launcher, once in launchd,
/// puts its library into amfid too, amfid waits on itself and the boot stops before SpringBoard.
pub const JAILBREAK_BOOT_ARG: &str = "amfi_get_out_of_my_way=1";

pub fn boot_args(jailbreak: bool) -> String {
    let base = kboot::DEFAULT_BOOT_ARGS;
    if jailbreak { format!("{base} {JAILBREAK_BOOT_ARG}") } else { base.to_string() }
}

/// The keybag ramdisk's helper: root's restored_external, which the ramdisk's rc.boot runs first.
pub const KEYBAG_HELPER: &str = "usr/local/bin/restored_external";
pub const KEYBAG_DONE: &str = "it_keybag: effaceable formatted, system keybag created";
pub const HALTING: &str = "it_seal: halting";

pub struct K48Board {
    board: Hardware,
    recipe: crate::entry::Recipe,
    version: String,
    strategy: String,
    iboot: bool,
    data_protection: bool,
    helper: Option<Helper>,
    hardware: Option<DeviceInfo>,
    mbr: Option<PathBuf>,
    built: Option<system_edits::Built>,
    gid_components: Vec<String>,
    ramdisk: Option<String>,
    ident: Option<UnitIdentity>,
    /// The keybag and seal one-shots' limit: it_seal waits out the first boot's data migration (6.1.6's took about 2
    /// minutes under TCG), and 7.x's launchd starts our daemons about 170 s in, so every release gets 900 s.
    oneshot_timeout: f64,
}

impl K48Board {
    pub fn new(o: &Options) -> Result<K48Board> {
        let e = &o.entry;
        let recipe = e.recipe.clone().ok_or_else(|| crate::unsupported!("{}: no recipe", e.id))?;
        let board = Hardware::parse(&e.board).ok_or_else(|| crate::unsupported!("{}: unknown board {}", e.id, e.board))?;
        let kboot_only = board != Hardware::K48;
        let strategy = recipe.boot.clone().unwrap_or_else(|| if kboot_only { "kboot" } else { "iboot" }.to_string());
        if strategy != "iboot" && strategy != "kboot" {
            return Err(crate::unsupported!("{}: unknown boot strategy {strategy}", e.id));
        }
        if kboot_only && strategy == "iboot" {
            return Err(crate::unsupported!(
                "{}: the {} boots by kboot only (no NAND boot chain yet)",
                e.id,
                board.marketing_name()
            ));
        }
        let data_protection = recipe.option("writable_nor").unwrap_or_else(|| crate::entry::data_protection(&e.version));
        Ok(K48Board {
            board,
            iboot: strategy == "iboot",
            strategy,
            data_protection,
            version: e.version.clone(),
            recipe,
            helper: None,
            hardware: None,
            mbr: None,
            built: None,
            gid_components: Vec::new(),
            ramdisk: None,
            ident: None,
            oneshot_timeout: 900.0,
        })
    }

    fn kboot_board(&self) -> bool {
        self.board != Hardware::K48
    }
    /// The S5L8920 family (-M n18, n88): the IPSW's NAND epoch.
    fn s5l8920(&self) -> bool {
        self.board.soc() == boot::Soc::S5l8920
    }
    /// A kboot device ships a writable NOR for 4.x data protection, and always on the N88, whose DT keeps its own NOR.
    fn kboot_nor(&self) -> bool {
        self.data_protection || self.s5l8920()
    }
    fn system_options(&self) -> system_edits::Options {
        let mut o = system_edits::Options::new(&self.recipe, &self.version);
        o.bluetooth = kboot::Board::for_board(self.board.name()).bluetooth_hci;
        o
    }
    fn die_id(&self) -> String {
        self.ident.as_ref().and_then(|i| i.die_id()).map(|d| d.join(":")).unwrap_or_default()
    }
    /// What every boot of the device carries (the lock's "machine"): the modem's IMEI, the recipe's pinned clock and
    /// K48's BCM4329 CIS address.
    fn machine_options(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        if let Some(i) = &self.ident {
            if let Some(imei) = i.get("imei") {
                m.insert("imei".into(), imei.to_string());
            }
            if self.board == Hardware::K48 {
                if let Some(mac) = i.get("wifi-mac") {
                    m.insert("wifi-mac".into(), mac.to_string());
                }
            }
        }
        if let Some(epoch) = self.recipe.rtc_epoch {
            m.insert("rtc-epoch".into(), epoch.to_string());
        }
        m
    }
    fn nor_path(&self, c: &Context) -> Option<PathBuf> {
        (self.iboot || self.kboot_nor()).then(|| c.file("nor.bin"))
    }
    fn geometry(&self) -> k48nand::Geometry {
        k48nand::Geometry::for_vendor_type(self.recipe.nand_vendor_type)
    }

    /// The iBoot names exactly one kernelcache path and it is the one the bake installs.
    pub fn kernelcache_fit(iboot: &[u8]) -> Fit {
        let piece = "kernelcache at the path iBoot loads";
        match crate::n72_recipe::kernelcache_path(iboot) {
            Ok(path) if path == system_edits::KERNELCACHE_PATH => Fit::new(piece, true, format!("iBoot names /{path}")),
            Ok(path) => Fit::new(piece, false, format!("iBoot names /{path}, the bake installs /{}", system_edits::KERNELCACHE_PATH)),
            Err(e) => Fit::new(piece, false, e.message),
        }
    }

    /// One boot of the store as the device boots, with no keyboard and no reboot: it ends when the guest shuts down,
    /// and a restart is a shutdown too. `overlay` None writes the store.
    fn oneshot(&self, c: &Context, boot: IPadBoot, serial: &Path, stop: Option<&str>) -> Result<(OneShot, String)> {
        let d = boot::IPad {
            boot,
            nand: c.nand.to_string_lossy().into_owned(),
            overlay: None,
            die_id: Some(self.die_id()),
            wifi: true,
            machine_options: self.machine_options(),
        };
        let hardware = self.hardware.as_ref().ok_or_else(|| filled("the board's hardware"))?;
        let config = boot::ipad(&d, hardware, &format!("file:{}", serial.display()));
        oneshot::run(self.helper.as_ref().ok_or_else(|| filled("the helper"))?, &config.argv, serial, stop, None, self.oneshot_timeout, &|l| c.log(l), None)
    }
}

/// The ramdisk the keybag one-shot boots: the build's own Update (else Restore) ramdisk out of the decrypt cache.
/// Returns (its path, the lock's name for it).
pub fn keybag_ramdisk(c: &Context) -> Result<(PathBuf, String)> {
    // A restore-only build ships just the Restore ramdisk; a build whose Update ramdisk has no public key boots its
    // keyed Restore ramdisk: it runs the same restored_external.
    let comp = crate::ipsw::build_components(&c.ipsw()?, &c.e().board)?;
    let candidates: Vec<&String> = ["UpdateRamDisk", "RestoreRamDisk"].iter().filter_map(|k| comp.get(*k)).collect();
    let update = candidates
        .iter()
        .find(|p| c.e().key_for_path(p).is_ok())
        .or(candidates.first())
        .ok_or_else(|| crate::unsupported!("{}: no ramdisk", c.e().id))?;
    let stem = update.strip_suffix(".dmg").unwrap_or(update);
    let name = format!("{}-ramdisk.dmg", stem.rsplit('/').next().unwrap_or(stem));
    let Some(from) = &c.recipe.keybag_ramdisk_from else { return Ok((c.dec_file(&name)?, name)) };
    let Some((sibling, ipsw)) = c.o.sibling.as_ref().filter(|(s, _)| &s.id == from) else {
        return Err(crate::unsupported!(
            "{}: the keybag ramdisk comes from {from}; pass --sibling-entry/--sibling-ipsw for it",
            c.e().id
        ));
    };
    let rd = sibling_ramdisk(sibling, ipsw, &c.work)?;
    let file = rd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    c.log(&format!("keybag ramdisk: {from}:{file} (this build has no ramdisk key)"));
    Ok((rd, format!("{from}:{file}")))
}

/// The sibling entry's Update (else Restore) ramdisk, decrypted out of its IPSW into `work`.
pub fn sibling_ramdisk(sibling: &crate::entry::Entry, ipsw: &Path, work: &Path) -> Result<PathBuf> {
    let archive = crate::ipsw::Ipsw::open(ipsw)?;
    let comp = crate::ipsw::build_components(&archive, &sibling.board)?;
    let path = comp
        .get("UpdateRamDisk")
        .or_else(|| comp.get("RestoreRamDisk"))
        .ok_or_else(|| crate::unsupported!("{}: no ramdisk", sibling.id))?;
    let k = sibling.key_for_path(path)?;
    let (Some(iv), Some(key)) = (k.iv.as_deref().and_then(crate::entry::from_hex), crate::entry::from_hex(&k.key)) else {
        return Err(Error::new(Code::KeyMissing, format!("{}: no IV/key for {}", sibling.id, k.file)));
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    let out = work.join(format!("sibling-{}-ramdisk.dmg", name.strip_suffix(".dmg").unwrap_or(name)));
    std::fs::create_dir_all(work)?;
    std::fs::write(&out, crate::img3::decrypt(&archive.read(path)?, &iv, &key, false)?)?;
    Ok(out)
}

/// A private copy of the restore ramdisk, 1 MiB larger, with `helper` as root's restored_external (mode 755).
pub fn ramdisk_with_helper(src: &Path, helper: &Path, work: &Path) -> Result<PathBuf> {
    let rd = work.join("keybag-ramdisk.dmg");
    std::fs::copy(src, &rd)?;
    let size = std::fs::metadata(&rd)?.len();
    Editor::grow(&rd, (size + (1 << 20) + 4095) / 4096 * 4096)?;
    let mut ed = Editor::open(&rd)?;
    if let Some((dir, _)) = KEYBAG_HELPER.rsplit_once('/') {
        ed.mkdirs(dir)?;
    }
    ed.write(KEYBAG_HELPER, std::fs::read(helper).map_err(|e| crate::internal!("{}: {e}", helper.display()))?, Some(0o755))?;
    ed.commit()?;
    Volume::open(&rd, true)?.set_owner(&[KEYBAG_HELPER], 0, 0, None, None)?;
    Ok(rd)
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| crate::internal!("{}: {e}", path.display()))
}

fn file_record(c: &Context, name: &str) -> Result<Value> {
    Ok(json!({"path": name, "sha256": files::sha256_file(&c.file(name))?}))
}

impl Board for K48Board {
    fn arch(&self) -> &str {
        "armv7"
    }
    fn seed_prefix(&self) -> String {
        match self.board {
            Hardware::N81 => "ipod4",
            Hardware::N90 => "iphone4",
            Hardware::N88 => "iphone3gs",
            Hardware::N18 => "ipod3",
            _ => "ipad1",
        }
        .to_string()
    }
    fn boot_step(&self) -> String {
        if self.iboot { "Writing the identity and boot chain" } else { "Writing the identity and boot image" }.to_string()
    }
    fn volumes_step(&self) -> String {
        "Building the system and data volumes".into()
    }
    fn keybag_step(&self) -> String {
        "Creating the data-protection keybag".into()
    }
    fn data_protection(&self) -> bool {
        self.data_protection
    }
    /// The seal boot halts through it_seal, a guest helper: none without them (guest_tools off).
    fn needs_seal(&self) -> bool {
        self.system_options().guest_tools
    }
    fn shipped(&self) -> Vec<String> {
        if self.iboot {
            vec!["iBoot.bin".into(), "nor.bin".into(), "gid-blobs.bin".into()]
        } else if self.kboot_nor() {
            vec!["kboot.bin".into(), "nor.bin".into()]
        } else {
            vec!["kboot.bin".into()]
        }
    }

    fn check(&mut self, c: &mut Context) -> Result<()> {
        if self.recipe.storage != "16g" || self.recipe.data_size != "partition" {
            return Err(crate::unsupported!("{}: storage {}, data_size {}", c.e().id, self.recipe.storage, self.recipe.data_size));
        }
        let helper = c.o.helper.clone();
        self.hardware = Some(helper.hardware(self.board)?);
        self.helper = Some(helper);
        Ok(())
    }

    fn identity(&mut self, _c: &mut Context, seed: &str, path: &Path) -> Result<Option<String>> {
        let model = self.kboot_board().then(|| kboot::Board::for_board(self.board.name()).model_number);
        // The radio boards: the modem reports the IMEI, and lockdownd/MobileGestalt hash it into the UDID.
        let mut ident = UnitIdentity::synthesize(seed, &self.recipe.storage, model)?;
        if self.board.kboot_phone() {
            ident = ident.adding_imei(seed);
        }
        ident.write(path)?;
        let udid = ident.udid().map(str::to_string);
        self.ident = Some(ident);
        Ok(udid)
    }

    fn boot_files(&mut self, c: &mut Context) -> Result<()> {
        let ident = self.ident.as_ref().ok_or_else(|| filled("the identity"))?;
        let args = boot_args(c.cydia.is_some());
        let kernel = read(&c.dec_file("kernelcache.mach")?)?;
        fitcheck::check_boot_args(&c.fit, Some(&kernel), &args)?;
        // A jailbroken device's kernel is sandbox-patched so Substrate's inserted library loads inside the built-in app
        // profiles; recorded here, applied to the boot kernel (kboot.bin below, or the iBoot kernelcache in volumes).
        let mut jailbreak_kernel = None;
        if c.cydia.is_some() {
            c.fit.check(fitcheck::sandbox_patch(Some(&kernel)), false, "left out")?;
            jailbreak_kernel = sandbox_patch::apply(&kernel).ok().map(|(k, _)| k);
        }
        // both chains add it
        c.fit.check(fitcheck::device_tree_property(Some(&kernel), "arm-io/usb-complex", "hsic-enabled"), false, "left out")?;
        if self.iboot {
            // fsboot: the kernelcache goes where this iBoot loads it from, the path the volumes step installs to
            c.fit.check(Self::kernelcache_fit(&read(&c.dec_file("iBoot.bin")?)?), true, "left out")?;
            let log = |s: &str| c.log(s);
            self.gid_components = k48iboot::write_boot_files(&c.ipsw()?, c.e(), c.decrypted()?, ident, &args, &c.o.out, &log)?;
        } else {
            kboot::write(c.decrypted()?, &c.file("kboot.bin"), ident, &args, None, jailbreak_kernel.as_deref())?;
            if self.kboot_nor() && !self.data_protection {
                std::fs::write(c.file("nor.bin"), vec![0xFFu8; 1 << 20])?;
            }
        }
        Ok(())
    }

    fn volumes(&mut self, c: &mut Context) -> Result<()> {
        let mbr = c.work.join("mbr.bin");
        // At least what a restore gives this unit (MinimumSystemPartition + padding): the catalog's 1280 MiB left an
        // iPhone 4 6.1.3 5.0 % free on its writable root, at HFS's very-low-disk limit ("Storage Almost Full").
        let stock = c.restore.as_ref().and_then(|r| r.system_partition_mib(&self.recipe.storage)).unwrap_or(0);
        let system_mib = self.recipe.system_mib.max(stock);
        c.log(&format!("system partition {system_mib} MiB (catalog {}, restore {stock})", self.recipe.system_mib));
        std::fs::write(&mbr, k48nand::make_mbr(&self.geometry(), system_mib as usize))?;
        let parts = k48nand::partitions(&read(&mbr)?);
        self.mbr = Some(mbr);
        let mut kernelcache_img3 = None;
        if self.iboot {
            let comp = crate::ipsw::build_components(&c.ipsw()?, &c.e().board)?;
            let kc = comp.get("KernelCache").ok_or_else(|| crate::unsupported!("{}: the IPSW names no KernelCache", c.e().id))?;
            let raw = c.ipsw()?.read(kc)?;
            // A jailbroken iBoot-chain device boots a sandbox-patched kernelcache, re-encrypted with the component's
            // key so iBoot decrypts it as it does the stock one.
            if c.cydia.is_some() {
                let k = c.e().key_for_path(kc)?;
                if let (Some(iv), Some(key)) = (k.iv.as_deref().and_then(crate::entry::from_hex), crate::entry::from_hex(&k.key)) {
                    if let Ok((img3, found)) = sandbox_patch::rewrap_kernelcache(&raw, &read(&c.dec_file("kernelcache.mach")?)?, &iv, &key) {
                        c.log(&format!("[sandbox] kernelcache re-wrapped with sb_evaluate patched at 0x{:x}", found.address));
                        kernelcache_img3 = Some(img3);
                    }
                }
            }
            kernelcache_img3 = kernelcache_img3.or(Some(raw));
        }
        let mut options = self.system_options();
        options.cydia = c.cydia.clone();
        options.language = c.o.language.clone();
        options.keyboards = c.o.keyboards.clone();
        let uuid_hash = files::sha256(format!("k48 data volume {}", c.seed).as_bytes());
        let mut uuid = [0u8; 8];
        uuid.copy_from_slice(&hex::decode(&uuid_hash[..16]).unwrap_or_default());
        let built = system_edits::build_k48(
            &c.dec_file("rootfs.dmg")?,
            &c.work,
            parts[0].count as u64 * 4096,
            parts[1].count as u64 * 4096,
            &options,
            &c.o.guest_tools,
            kernelcache_img3.as_deref(),
            Some(read(&c.dec_file("kernelcache.mach")?)?),
            Some(uuid),
            &c.fit,
            c.now,
            &|l| c.log(l),
        )?;
        for n in &built.notes {
            (c.emit)(crate::progress::Event::Warning(n.clone()));
        }
        c.activation = built.activation.clone();
        c.guest_package = built.guest_package.clone();
        c.engine = built.engine.clone();
        self.built = Some(built);
        Ok(())
    }

    fn store(&mut self, c: &mut Context) -> Result<()> {
        let mbr = self.mbr.clone().ok_or_else(|| filled("the MBR"))?;
        let built = self.built.as_ref().ok_or_else(|| filled("the volumes"))?;
        let kc = c.dec_file("kernelcache.mach")?;
        let mut epoch = k48nand::signature_epoch(&kc)?;
        // WMR refuses a whitened store on a board whose DT does not ask for one ("Metadata whitening not supported").
        let dt = read(&c.dec_file("DeviceTree.bin")?)?;
        let needle = b"metadata-whitening\0";
        let whitening = dt.windows(needle.len()).any(|w| w == needle);
        c.log(&format!("NAND metadata whitening {} (the DT's)", if whitening { "on" } else { "off" }));
        match (self.s5l8920(), if self.s5l8920() { k48nand::restore_epoch_of(&c.ipsw()?, &c.e().board)? } else { None }) {
            (true, Some(scep)) => {
                epoch = scep; // a store behind it waits for an epoch roll only restored performs
                c.log(&format!("NAND signature epoch {epoch} (Restore.plist SCEP)"));
            }
            _ => c.log(&format!("NAND signature epoch {epoch} (this kernel's FIL)")),
        }
        let mut b = k48nand::Build::new(&mbr, k48nand::kernel_version(&kc)?, &built.system, k48nand::DataVolume::Image(built.data.clone()), &c.nand);
        b.geometry = self.geometry();
        b.epoch = epoch;
        b.whitening = whitening;
        b.sig_flags = self.recipe.nand_sig_flags.map(|f| f as u32);
        k48nand::build(&b, &|l| c.log(l))?;
        let _ = std::fs::remove_file(&built.system);
        let _ = std::fs::remove_file(&built.data);
        Ok(())
    }

    /// 4.x data protection: effaceable + system keybag from the IPSW's own Update ramdisk, booted once as md0 on the
    /// store + NOR with it_keybag as restored_external. One attempt: a panic fails the preparation.
    fn keybag(&mut self, c: &mut Context) -> Result<()> {
        let nor = self.nor_path(c).ok_or_else(|| filled("the NOR"))?;
        if !self.iboot {
            std::fs::write(&nor, vec![0xFFu8; 1 << 20])?; // iboot already built the packed NOR
        }
        let (source, name) = keybag_ramdisk(c)?;
        self.ramdisk = Some(name);
        let rd = ramdisk_with_helper(&source, &c.o.guest_tools.join(helpers::name("it_keybag", "armv7")), &c.work)?;
        let kboot = c.work.join("kboot-restore.bin");
        kboot::write(c.decrypted()?, &kboot, self.ident.as_ref().ok_or_else(|| filled("the identity"))?, kboot::DEFAULT_BOOT_ARGS, Some(&rd), None)?;
        let before = read(&nor)?;
        let serial = c.work.join("keybag-1.log");
        let boot = IPadBoot::Kernel { image: kboot.to_string_lossy().into_owned(), writable_nor: Some(nor.to_string_lossy().into_owned()) };
        let (r, text) = self.oneshot(c, boot, &serial, Some("panic("))?;
        for line in text.lines().filter(|l| l.contains("it_keybag:")) {
            c.log(line);
        }
        if !(r.exited && text.contains(KEYBAG_DONE)) {
            let why = text
                .lines()
                .find_map(|l| l.find("panic(").map(|i| l[i..].chars().take(160).collect::<String>()))
                .unwrap_or_else(|| if r.exited { "halted without the keybag" } else { "no halt" }.to_string());
            return Err(Error { code: Code::OneshotFailed, message: format!("keybag boot: {why}"), piece: None });
        }
        if read(&nor)? == before {
            return Err(Error { code: Code::OneshotFailed, message: "keybag boot: effaceable was not written to the NOR".into(), piece: None });
        }
        let _ = std::fs::remove_file(&rd);
        let _ = std::fs::remove_file(&kboot);
        Ok(())
    }

    /// Boots the store until the guest halts (it_seal); the clean halt writes the FTL context. No check boot after it:
    /// the lock's hashes are the sanity check, and a bad seal fails the device's first start.
    fn seal(&mut self, c: &mut Context) -> Result<()> {
        let boot = if self.iboot {
            IPadBoot::IBoot {
                image: c.file("iBoot.bin").to_string_lossy().into_owned(),
                writable_nor: c.file("nor.bin").to_string_lossy().into_owned(),
                gid_blobs: c.file("gid-blobs.bin").to_string_lossy().into_owned(),
            }
        } else {
            IPadBoot::Kernel {
                image: c.file("kboot.bin").to_string_lossy().into_owned(),
                writable_nor: self.nor_path(c).map(|p| p.to_string_lossy().into_owned()),
            }
        };
        let serial = c.work.join("seal.log");
        let (r, text) = self.oneshot(c, boot, &serial, None)?;
        if !(r.exited && text.contains(HALTING)) {
            return Err(Error {
                code: Code::OneshotFailed,
                message: format!(
                    "seal boot: {} after {} s",
                    if r.exited { "QEMU exited without it_seal" } else { "no clean halt" },
                    r.seconds as i64
                ),
                piece: None,
            });
        }
        Ok(())
    }

    fn lock(&mut self, c: &Context) -> Result<Value> {
        let built = self.built.as_ref().ok_or_else(|| filled("the volumes"))?;
        let mbr = self.mbr.as_ref().ok_or_else(|| filled("the MBR"))?;
        let mut outputs = serde_json::Map::new();
        let files_map: serde_json::Map<String, Value> = c.nand_hashes.iter().map(|(f, h)| (f.clone(), Value::String(h.clone()))).collect();
        outputs.insert("nand".into(), json!({"files": files_map}));
        if self.iboot {
            for (k, n) in [("iboot", "iBoot.bin"), ("gid_blobs", "gid-blobs.bin"), ("nor", "nor.bin")] {
                outputs.insert(k.into(), file_record(c, n)?);
            }
        } else {
            outputs.insert("kboot".into(), file_record(c, "kboot.bin")?);
            outputs.insert("nor".into(), if self.kboot_nor() { file_record(c, "nor.bin")? } else { Value::Null });
        }
        let helper = &c.o.helper_path;
        Ok(json!({
            "boot_strategy": self.strategy,
            "gid_components": if self.iboot { json!(self.gid_components) } else { Value::Null },
            "iboot_signature_checks": if self.iboot { json!("pattern-patched") } else { Value::Null },
            "tool": {
                "helper": helper.to_string_lossy(), "helper_sha256": files::sha256_file(helper)?,
                // compiled in (vendor/iBoot32Patcher): the pinned source in place of a separate binary
                "iboot32patcher": if self.iboot { json!({"path": "built-in", "commit": k48iboot::IBOOT32PATCHER_COMMIT, "sha256": k48iboot::IBOOT32PATCHER_SHA256}) } else { Value::Null },
                "built": {"OpenGLES": built.engine.clone().map(Value::String).unwrap_or(Value::Null)},
            },
            "inputs": {
                "kernelcache": "kernelcache.mach", "devicetree": "DeviceTree.bin",
                "restore_ramdisk": self.ramdisk.clone().map(Value::String).unwrap_or(Value::Null),
                "mbr": {"sha256": files::sha256_file(mbr)?}, "stash": Value::Null,
            },
            "identity": {"die_id": self.die_id()},
            "outputs": Value::Object(outputs),
            "gl_test": self.system_options().gl_test,
            "skip_setup": self.system_options().skip_setup,
            "machine": self.machine_options(),
        }))
    }
}

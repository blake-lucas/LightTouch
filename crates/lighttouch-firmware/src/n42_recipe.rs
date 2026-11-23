//! N42Board: the iPhone 5's (n42ap, recipe "n42") side of create.
//!
//! A port of Light Touch for Mac's N42Recipe.swift. Its store isn't written on the host: the volumes step is the
//! A6 bake (`a6_preparation`), and the store step formats new PPN media (`a6nand`) and restores it in the guest
//! as a restore does: the volumes as an allocated-block A6DI stream, and one one-shot of the IPSW's own restore
//! ramdisk with qemu-ios's it_initialize as its restored_external (`a6_restore`), which writes the stream through
//! the guest's disk driver, formats Data, makes the system keybag, seeds Data and the MobileGestalt cache, and
//! halts. Then kboot.bin, its kernel trusting the staged guest helpers (`a6_assets`), no NOR, and the first normal
//! boot (the seal), to the Home screen and the guest's own shutdown.
use crate::a6_assets;
use crate::a6_restore;
use crate::a6_storage::A6StorageLayout;
use crate::a6nand;
use crate::boot::{self as machine, DeviceInfo};
use crate::create::{self, Board, Context};
use crate::entry::Recipe;
use crate::error::{Code, Error, Result};
use crate::fitcheck;
use crate::identity::UnitIdentity;
use crate::system_edits;
use crate::{files, kboot, oneshot};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const RECIPE: &str = "n42";
pub const SEED_PREFIX: &str = "iphone5";
pub const STORAGE: &str = "16g";
pub const MODEL_NUMBER: &str = "MD299";
pub const BOOT_STEP: &str = "Writing the identity";
pub const VOLUMES_STEP: &str = "Building the system and data volumes";
pub const STORE_STEP: &str = "Restoring the flash";
pub const KEYBAG_STEP: &str = "Creating the data-protection keybag";
/// A6Preparation.defaultBootArgs: kboot's (`kboot::DEFAULT_BOOT_ARGS`) and the AMFI setting the N42's boots use.
pub const A6_BOOT_ARGS: &str =
    "serial=3 debug=0x8 amfi_allow_any_signature=1 cs_enforcement_disable=1 enable-hsic=1 amfi_get_out_of_my_way=1";
/// The normal boot's: root on the LwVM System partition.
pub const BOOT_ARGS: &str = "serial=3 debug=0x8 amfi_allow_any_signature=1 cs_enforcement_disable=1 enable-hsic=1 amfi_get_out_of_my_way=1 rd=disk0s1";
pub const GUEST_TOOLS: &[&str] =
    &["it_initialize", "it_mobilegestalt", "launchd-cache-template", "it_agent", "httpget-armv7", "libappsync.dylib"];
pub const BASE_MACHINE_OPTIONS: [(&str, &str); 3] = [("ppn", "on"), ("camera", "on"), ("quiet-boot", "on")];
pub const FIRST_BOOT_DONE: &str = "Process SpringBoard (";
/// The restore boot's limit: 14G61's takes ~200 s on a Ryzen 7 3700X under Linux (120 s of it the restore job's
/// start delay); a phone is several times slower again, so the same two hours as the first boot.
pub const RESTORE_TIMEOUT: f64 = 7200.0;
/// The first boot's limit: 14G61's migration lit SpringBoard after 380-430 s at a host load of 5 on a Mac, 800 s
/// at 30; on a Ryzen 7 3700X under Linux its last stretch alone takes ~25 minutes, and a phone is slower again.
pub const FIRST_BOOT_TIMEOUT: f64 = 7200.0;
/// How long after SpringBoard starts the first boot asks the guest to shut down (Swift's powerdown delay).
pub const FIRST_BOOT_POWERDOWN_DELAY: Duration = Duration::from_secs(30);

pub struct N42Board {
    pub recipe: Recipe,
    pub helper_hardware: Option<DeviceInfo>,
    pub ident: Option<UnitIdentity>,
    pub vols: Option<system_edits::Built>,
    pub records: Option<PathBuf>,
    pub restore_ramdisk: Option<String>,
}

impl N42Board {
    pub fn new(o: &create::Options) -> Result<N42Board> {
        let recipe = o.entry.recipe.clone().ok_or_else(|| crate::unsupported!("{}: no recipe", o.entry.id))?;
        Ok(N42Board { recipe, helper_hardware: None, ident: None, vols: None, records: None, restore_ramdisk: None })
    }

    pub fn machine_options(&self) -> BTreeMap<String, String> {
        let mut m: BTreeMap<String, String> =
            BASE_MACHINE_OPTIONS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        if let Some(imei) = self.ident.as_ref().and_then(|i| i.get("imei")) {
            m.insert("imei".into(), imei.to_string());
        }
        m
    }

    pub fn records(&self) -> Result<&Path> {
        self.records.as_deref().ok_or_else(|| create::filled("the volume records"))
    }

    pub fn vols(&self) -> Result<&system_edits::Built> {
        self.vols.as_ref().ok_or_else(|| create::filled("the volumes"))
    }

    pub fn ident(&self) -> Result<&UnitIdentity> {
        self.ident.as_ref().ok_or_else(|| create::filled("the identity"))
    }

    /// A preparation boot of `kboot` on the store at `nand` (BootRecipe.iPhone5 with Wi-Fi off, one-shot).
    fn one_shot_config(
        &self,
        kboot: &Path,
        nand: &Path,
        options: BTreeMap<String, String>,
        imei: Option<&str>,
        serial: &Path,
    ) -> Result<machine::BootConfig> {
        let hardware = self.helper_hardware.clone().ok_or_else(|| create::filled("the board's hardware"))?;
        let d = machine::IPad {
            boot: machine::IPadBoot::Kernel { image: kboot.to_string_lossy().into_owned(), writable_nor: None },
            nand: nand.to_string_lossy().into_owned(),
            overlay: None,
            die_id: None,
            wifi: false,
            machine_options: options,
        };
        Ok(machine::iphone5(
            &d,
            &hardware,
            &format!("file:{}", serial.display()),
            &["-audio".into(), "driver=none".into()],
            None,
            &[],
            imei,
        ))
    }

    /// Write every input the restore boot reads (`a6_restore::Staged` names them) and kboot.bin.
    pub fn stage_store(&mut self, c: &mut Context) -> Result<a6_restore::Staged> {
        self.restore_ramdisk = Some(a6_restore::RESTORE_RAMDISK.to_string());
        let vols = self.vols()?;
        let ident = self.ident()?;
        let layout = A6StorageLayout::standard();
        let work = &c.work;
        let stream = a6_restore::stage(&vols.system, &vols.data, work, &layout)?;
        a6nand::create(&c.nand)?;
        let plan = self.records()?.join("activation-plan.plist");
        let inputs = a6_restore::BundleInputs {
            decrypted: c.decrypted()?,
            initializer: &c.o.guest_tools.join("it_initialize"),
            mobile_gestalt: Some(&c.o.guest_tools.join("it_mobilegestalt")),
            factory_format: true,
            data_seed: &vols.data,
            seed_base_system: &vols.system,
            activation_plan: if plan.is_file() { Some(&plan) } else { None },
            guest_assets_system: &vols.system,
            work,
            identity: ident,
        };
        let mut staged = a6_restore::write_bundle(&inputs, c.now, &|l| c.log(l))?;
        staged.stream = stream;
        // The normal boot's kernel: patch the A6 trust table while the System image is present.
        let kernel = std::fs::read(c.dec_file("kernelcache.mach")?)?;
        let assets = a6_assets::prepare(&kernel, &vols.system, None)?;
        let normal_kernel = a6_assets::knowing_amfid(&assets.kernel, &assets.assets)?;
        kboot::write_rooted(
            c.decrypted()?,
            &c.file("kboot.bin"),
            ident,
            BOOT_ARGS,
            None,
            Some(&normal_kernel),
            Some("disk0s1"),
        )?;
        let _ = std::fs::remove_file(&vols.system);
        let _ = std::fs::remove_file(&vols.data);
        Ok(staged)
    }
}

impl Board for N42Board {
    fn arch(&self) -> &str {
        "armv7"
    }
    fn seed_prefix(&self) -> String {
        SEED_PREFIX.to_string()
    }
    fn boot_step(&self) -> String {
        BOOT_STEP.to_string()
    }
    fn volumes_step(&self) -> String {
        VOLUMES_STEP.to_string()
    }
    fn store_step(&self) -> String {
        STORE_STEP.to_string()
    }
    fn keybag_step(&self) -> String {
        KEYBAG_STEP.to_string()
    }
    fn data_protection(&self) -> bool {
        false
    }
    fn needs_seal(&self) -> bool {
        // LT_N42_FIRST_BOOT=off (diagnostic): no first boot here; the device's first Start runs the migration.
        std::env::var("LT_N42_FIRST_BOOT").as_deref() != Ok("off")
    }
    fn shipped(&self) -> Vec<String> {
        vec!["kboot.bin".to_string()]
    }
    fn check(&mut self, c: &mut Context) -> Result<()> {
        let boot = self.recipe.boot.as_deref().unwrap_or("kboot");
        if self.recipe.storage != STORAGE || boot != "kboot" {
            return Err(crate::unsupported!("{}: storage {}, boot {boot}", c.e().id, self.recipe.storage));
        }
        if !c.o.helper_path.is_file() {
            return Err(crate::internal!(
                "the restore boot needs --helper (LightTouchDevice); got {}",
                c.o.helper_path.display()
            ));
        }
        self.helper_hardware = Some(c.o.helper.hardware(machine::Board::N42)?);
        let missing: Vec<&str> = GUEST_TOOLS.iter().copied().filter(|n| !c.o.guest_tools.join(n).is_file()).collect();
        if !missing.is_empty() {
            return Err(crate::internal!("--guest-tools {} lacks {}", c.o.guest_tools.display(), missing.join(", ")));
        }
        Ok(())
    }
    fn identity(&mut self, _c: &mut Context, seed: &str, path: &Path) -> Result<Option<String>> {
        // The UDID is the guest's own (the A6 hashes its ECID, not the IMEI): the host's pair record is named by it.
        let ident = UnitIdentity::synthesize(seed, STORAGE, Some(MODEL_NUMBER))?.adding_imei(seed).with_a6_udid();
        ident.write(path)?;
        let udid = ident.udid().map(str::to_string);
        self.ident = Some(ident);
        Ok(udid)
    }
    fn boot_files(&mut self, c: &mut Context) -> Result<()> {
        let kernel = std::fs::read(c.dec_file("kernelcache.mach")?)?;
        fitcheck::check_boot_args(&c.fit, Some(&kernel), BOOT_ARGS)?;
        Ok(())
    }
    fn volumes(&mut self, c: &mut Context) -> Result<()> {
        let records = c.work.join("a6");
        let setup_finished = c.e().released.as_deref().and_then(release_noon);
        let inputs = crate::a6_preparation::Inputs {
            ipsw: &c.o.ipsw,
            decrypted: c.decrypted()?,
            helpers: &c.o.guest_tools,
            out: &records,
            language: c.o.language.clone(),
            keyboards: c.o.keyboards.clone(),
            seed: Some(c.seed.clone()),
            setup_finished,
            now: c.now,
        };
        let mut vols = crate::a6_preparation::volumes(&inputs, &c.fit, &|s| c.log(s))?;
        let system = c.work.join("system.img");
        let data = c.work.join("data.img");
        std::fs::rename(&vols.system, &system)?;
        std::fs::rename(&vols.data, &data)?;
        vols.system = system;
        vols.data = data;
        for n in &vols.notes {
            c.warn(n);
        }
        // The guest trusts LightTouch from its first start (a6_pairing): its key and pair record in Data, the host's
        // side in the prepared folder.
        let ident = self.ident.as_ref().ok_or_else(|| create::filled("the identity"))?;
        let udid = ident.udid().ok_or_else(|| create::filled("the UDID"))?;
        let pairing = crate::a6_pairing::make(udid, ident.get("wifi-mac").unwrap_or_default(), c.now)?;
        pairing.install(&vols.data, &c.e().build)?;
        pairing.write_host(&c.o.out)?;
        c.log(&format!("Paired with LightTouch at preparation (host {})", pairing.host_id));
        c.activation = vols.activation.clone();
        c.guest_package = vols.guest_package.clone();
        c.engine = vols.engine.clone();
        self.records = Some(records);
        self.vols = Some(vols);
        Ok(())
    }
    /// The restore boot: the IPSW's own restore ramdisk with it_initialize as its restored_external writes the
    /// A6DI stream through the guest's disk driver, formats Data, makes the system keybag, seeds Data and the
    /// MobileGestalt cache, and halts (A6RESTORE DONE). Then kboot.bin stays for the normal boot.
    fn store(&mut self, c: &mut Context) -> Result<()> {
        let staged = self.stage_store(c)?;
        let mut options = self.machine_options();
        let imei = options.remove("imei");
        options.insert("restore-image".to_string(), staged.stream.to_string_lossy().into_owned());
        let serial = c.work.join("restore.log");
        let config = self.one_shot_config(&staged.kboot_restore, &c.nand, options, imei.as_deref(), &serial)?;
        let (r, text) =
            oneshot::run(&c.o.helper, &config.argv, &serial, None, None, RESTORE_TIMEOUT, &|l| c.log(l), None)?;
        for line in
            text.lines().filter(|l| l.contains("A6RESTORE") || l.contains("A6NATIVE") || l.contains("it_keybag:"))
        {
            c.log(line);
        }
        if !(r.exited && text.contains(a6_restore::RESTORE_DONE) && text.contains(a6_restore::KEYBAG_DONE)) {
            let why = text
                .lines()
                .find_map(|line| {
                    ["A6RESTORE FAIL", "it_keybag: FAILED", "panic("]
                        .iter()
                        .find_map(|m| line.find(m).map(|i| line[i..].chars().take(160).collect::<String>()))
                })
                .unwrap_or_else(|| {
                    if r.exited {
                        format!("halted without the keybag (exit {})", r.exit_code)
                    } else {
                        format!("no halt after {} s", r.seconds as i64)
                    }
                });
            return Err(Error::new(Code::OneshotFailed, format!("restore boot: {why}")));
        }
        for n in [a6_restore::STREAM, a6_restore::RESTORE_RAMDISK, a6_restore::KBOOT_RESTORE] {
            let _ = std::fs::remove_file(c.work.join(n));
        }
        Ok(())
    }

    /// The first normal boot (the other boards' seal boot): kboot.bin on the restored store until SpringBoard is
    /// up, then the guest's own shutdown, so the store keeps iOS's minutes-long data migration done and a clean
    /// FTL, and a new device's first Start fits the app's boot budget. Fails unless the guest halted by itself.
    fn seal(&mut self, c: &mut Context) -> Result<()> {
        let mut options = self.machine_options();
        let imei = options.remove("imei");
        let serial = c.work.join("seal.log");
        let config = self.one_shot_config(&c.file("kboot.bin"), &c.nand, options, imei.as_deref(), &serial)?;
        let (r, text) = oneshot::run_powerdown(
            &c.o.helper,
            &config.argv,
            &serial,
            FIRST_BOOT_DONE,
            FIRST_BOOT_POWERDOWN_DELAY,
            FIRST_BOOT_TIMEOUT,
            &|l| c.log(l),
        )?;
        if !(r.exited && text.contains(FIRST_BOOT_DONE)) {
            return Err(Error::new(
                Code::OneshotFailed,
                format!(
                    "first boot: {} after {} s",
                    if text.contains(FIRST_BOOT_DONE) { "no clean halt" } else { "no Home screen" },
                    r.seconds as i64
                ),
            ));
        }
        Ok(())
    }
    fn lock(&mut self, c: &Context) -> Result<Value> {
        let nand_files: Map<String, Value> =
            c.nand_hashes.iter().map(|(f, h)| (f.clone(), Value::String(h.clone()))).collect();
        let machine: Map<String, Value> =
            self.machine_options().into_iter().map(|(k, v)| (k, Value::String(v))).collect();
        Ok(json!({
            "boot_strategy": "kboot",
            "tool": {
                "helper": c.o.helper_path.to_string_lossy(),
                "helper_sha256": files::sha256_file(&c.o.helper_path)?,
            },
            "inputs": {
                "kernelcache": "kernelcache.mach", "devicetree": "DeviceTree.bin",
                "restore_ramdisk": self.restore_ramdisk.clone().map(Value::String).unwrap_or(Value::Null),
            },
            "outputs": {"nand": {"files": Value::Object(nand_files)}, "kboot": file_record(c, "kboot.bin")?},
            "skip_setup": true,
            "derived": {"guest_tools": "installed"},
            "machine": Value::Object(machine),
        }))
    }
}

fn file_record(c: &Context, name: &str) -> Result<Value> {
    Ok(json!({"path": name, "sha256": files::sha256_file(&c.file(name))?}))
}

/// `ISO8601DateFormatter().date(from: released + "T12:00:00Z")`, as Unix seconds.
fn release_noon(date: &str) -> Option<i64> {
    let mut p = date.split('-');
    let y: i64 = p.next()?.parse().ok()?;
    let m: i64 = p.next()?.parse().ok()?;
    let d: i64 = p.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + 43_200)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_noon_14g61() {
        assert_eq!(release_noon("2019-07-22"), Some(1_563_796_800));
    }
    #[test]
    fn a6_boot_args_extend_kboots() {
        assert_eq!(A6_BOOT_ARGS, format!("{} amfi_get_out_of_my_way=1", crate::kboot::DEFAULT_BOOT_ARGS));
        assert_eq!(BOOT_ARGS, format!("{A6_BOOT_ARGS} rd=disk0s1"));
    }
    #[test]
    fn boot_args_are_the_swift_ones() {
        assert!(BOOT_ARGS.starts_with("serial=3 debug=0x8 amfi_allow_any_signature=1"));
        assert!(BOOT_ARGS.ends_with("amfi_get_out_of_my_way=1 rd=disk0s1"));
    }
}

//! N42Board: the iPhone 5's (n42ap, recipe "n42") side of create.
//!
//! The store step is deliberately a boundary: `stage_store` writes every data input the restore boot
//! consumes (A6DI stream, restore-ramdisk.hfs, kboot-restore.bin) and then stops with an explicit
//! "restore boot required" error. The parent runs the one-shot and the first-boot seal; see
//! `a6_restore` and the module docs in `IMPLEMENTATION.md`.
use crate::a6_assets;
use crate::a6nand;
use crate::a6_restore;
use crate::a6_storage::A6StorageLayout;
use crate::boot::{self as machine, DeviceInfo};
use crate::create::{self, Board, Context};
use crate::entry::Recipe;
use crate::error::{Code, Error, Result};
use crate::fitcheck;
use crate::identity::UnitIdentity;
use crate::system_edits;
use crate::{files, kboot, oneshot};
use serde_json::{json, Map, Value};
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
/// Restore boot args. `cpus=1`: the two A6 application cores spin on each
/// other's locks against the timebase; under MTTCG host scheduling that trips
/// xnu's 0.52 s hw_lock_bit timeout before launchd (the upstream vCPU-stall
/// clock fix mitigates but does not eliminate it on a loaded phone). The IOP
/// and AE2 cores still run; only the restore boot is uniprocessor.
pub const PREPARATION_BOOT_ARGS: &str =
    "serial=3 debug=0x8 amfi_allow_any_signature=1 cs_enforcement_disable=1 enable-hsic=1 amfi_get_out_of_my_way=1 cpus=1";
pub const BOOT_ARGS: &str =
    "serial=3 debug=0x8 amfi_allow_any_signature=1 cs_enforcement_disable=1 enable-hsic=1 amfi_get_out_of_my_way=1 rd=disk0s1";
pub const GUEST_TOOLS: &[&str] = &[
    "it_initialize",
    "it_mobilegestalt",
    "launchd-cache-template",
    "it_agent",
    "httpget-armv7",
    "libappsync.dylib",
];
pub const BASE_MACHINE_OPTIONS: [(&str, &str); 3] =
    [("ppn", "on"), ("camera", "on"), ("quiet-boot", "on")];
pub const FIRST_BOOT_DONE: &str = "Process SpringBoard (";
// Swift's 1200 s budget is for an M-series Mac; the same restore on a phone-class host is
// several times slower, so the one-shot gets 40 minutes rather than failing a working stream.
pub const RESTORE_TIMEOUT: f64 = 2400.0;
pub const FIRST_BOOT_TIMEOUT: f64 = 2400.0;

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
        let recipe = o
            .entry
            .recipe
            .clone()
            .ok_or_else(|| crate::unsupported!("{}: no recipe", o.entry.id))?;
        Ok(N42Board {
            recipe,
            helper_hardware: None,
            ident: None,
            vols: None,
            records: None,
            restore_ramdisk: None,
        })
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

    /// Write all data inputs the parent's restore boot needs, then stop. `a6_restore::Staged`
    /// names every file; no boot is faked.
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
        kboot::write_rooted(
            c.decrypted()?,
            &c.file("kboot.bin"),
            ident,
            BOOT_ARGS,
            None,
            Some(&assets.kernel),
            Some("disk0s1"),
        )?;
        let _ = std::fs::remove_file(&vols.system);
        let _ = std::fs::remove_file(&vols.data);
        Ok(staged)
    }
}


/// The preparation boots' accelerator. MTTCG runs the machine's four CPUs on four host threads, which is
/// where the phone's engine has crashed at random early-boot points while the same code on a host never
/// did; the restore and seal do not need the parallelism, so they run one TCG thread unless
/// `LT_PREP_MTTCG=1` asks for the old behaviour.
fn prep_config(mut c: machine::BootConfig) -> machine::BootConfig {
    // Single-threaded TCG was tried against the phone's SIGILL and made no difference, so the
    // preparation boots keep MTTCG (four vCPUs) and `LT_PREP_TCG_SINGLE=1` remains for an A/B.
    if std::env::var("LT_PREP_TCG_SINGLE").as_deref() == Ok("1") {
        if let Some(i) = c.argv.iter().position(|a| a == "-accel") {
            c.argv[i + 1] = "tcg,thread=single".to_string();
        }
    }
    c
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
        true
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
        let missing: Vec<&str> =
            GUEST_TOOLS.iter().copied().filter(|n| !c.o.guest_tools.join(n).is_file()).collect();
        if !missing.is_empty() {
            return Err(crate::internal!(
                "--guest-tools {} lacks {}",
                c.o.guest_tools.display(),
                missing.join(", ")
            ));
        }
        Ok(())
    }
    fn identity(&mut self, _c: &mut Context, seed: &str, path: &Path) -> Result<Option<String>> {
        let ident = UnitIdentity::synthesize(seed, STORAGE, Some(MODEL_NUMBER))?.adding_imei(seed);
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
        c.activation = vols.activation.clone();
        c.guest_package = vols.guest_package.clone();
        c.engine = vols.engine.clone();
        self.records = Some(records);
        self.vols = Some(vols);
        Ok(())
    }
    fn store(&mut self, c: &mut Context) -> Result<()> {
        let staged = self.stage_store(c)?;
        let ident = self.ident()?;
        let imei = ident.get("imei").map(str::to_string);
        let mut options = self.machine_options();
        options.remove("imei");
        // The initializer reads the A6DI stream through the machine's restore-image service (QC 0x500).
        options.insert("restore-image".to_string(), staged.stream.to_string_lossy().into_owned());
        let serial = c.work.join("restore.log");
        let hardware = self.helper_hardware.clone().ok_or_else(|| create::filled("the board's hardware"))?;
        let d = machine::IPad {
            boot: machine::IPadBoot::Kernel {
                image: staged.kboot_restore.to_string_lossy().into_owned(),
                writable_nor: None,
            },
            nand: c.nand.to_string_lossy().into_owned(),
            overlay: None,
            die_id: None,
            wifi: false,
            machine_options: options,
        };
        let config = prep_config(machine::iphone5_net(
            &d,
            &hardware,
            &format!("file:{}", serial.display()),
            &["-audio".into(), "driver=none".into()],
            None,
            &[],
            imei.as_deref(),
            false,
        ));
        if let Some(dir) = serial.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // The restore boot is the one stage that runs two guest CPUs against a software TCG host; xnu's
        // hw_lock_bit timeout can fire before launchd even with cpus=1, and the failure is a panic in the
        // serial, not a wrong image. A panic before the initializer has started has written nothing, so
        // the same factory NAND can be booted again; a run that reached A6RESTORE is never repeated.
        let mut attempt = 0;
        let (r, text) = loop {
            attempt += 1;
            let (r, text) = oneshot::run(
                &c.o.helper,
                &config.argv,
                &serial,
                None,
                None,
                RESTORE_TIMEOUT,
                &|l| c.log(l),
                None,
            )?;
            let panicked_early = text.contains("panic(") && !text.contains(a6_restore::RESTORE_START);
            if panicked_early && attempt == 1 {
                c.log("restore boot: the guest panicked before the initializer ran; booting it once more");
                continue;
            }
            break (r, text);
        };
        for line in text.lines().filter(|l| {
            l.contains("A6RESTORE") || l.contains("A6NATIVE") || l.contains("it_keybag:")
        }) {
            c.log(line);
        }
        if !(r.exited && text.contains(a6_restore::RESTORE_DONE) && text.contains(a6_restore::KEYBAG_DONE)) {
            // The serial is the only place a guest-side refusal is explained; keep its tail in the log the
            // app shows, so a failed preparation on a phone says why instead of just "halted".
            let tail: Vec<&str> = text
                .lines()
                .map(str::trim_end)
                .filter(|l| !l.is_empty() && !l.contains("ApplePMGRTempSensor"))
                .rev()
                .take(12)
                .collect();
            for line in tail.into_iter().rev() {
                c.log(&format!("restore serial: {line}"));
            }
            let why = text
                .lines()
                .find_map(|line| {
                    ["A6RESTORE FAIL", "it_keybag: FAILED", "panic("]
                        .iter()
                        .find_map(|m| line.find(m).map(|i| line[i..].chars().take(160).collect::<String>()))
                })
                .unwrap_or_else(|| {
                    if r.exited {
                        let how = if r.exit_code < 0 {
                            format!("killed by signal {} after {:.0} s", -r.exit_code, r.seconds)
                        } else {
                            format!("exit {} after {:.0} s", r.exit_code, r.seconds)
                        };
                        format!("halted without the keybag ({how})")
                    } else {
                        format!("no halt after {} s", r.seconds as i64)
                    }
                });
            // The app shows this message and nothing else of the guest's own output, so the reason a
            // guest halted (a panic, a refusal, a missing device) has to travel inside it.
            let tail: Vec<&str> = text
                .lines()
                .map(str::trim_end)
                .filter(|l| !l.is_empty() && !l.contains("ApplePMGRTempSensor"))
                .rev()
                .take(4)
                .collect();
            // The engine's own last words are worth more than the guest's when it is the engine that died.
            let engine = std::fs::read_to_string(serial.with_file_name("restore.log.engine.log")).unwrap_or_default();
            // A crash report ("qemu: fatal signal ...") is worth every line it prints: signal, pc, lr
            // and the backtrace. Otherwise the last couple of lines say what the engine was doing.
            let all: Vec<&str> = engine.lines().collect();
            let crash_at = all.iter().rposition(|l| l.contains("qemu: fatal signal"));
            let engine_tail: Vec<String> = match crash_at {
                Some(at) => all[at..].iter().filter(|l| !l.trim().is_empty()).take(26).map(|l| (*l).to_string()).collect(),
                None => engine
                    .lines()
                    .map(str::trim_end)
                    .filter(|l| !l.is_empty())
                    .rev()
                    .take(2)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect(),
            };
            let guest = tail.into_iter().rev().collect::<Vec<_>>().join(" | ");
            let detail = if engine_tail.is_empty() {
                guest
            } else {
                let mut lines: Vec<String> = engine_tail;
                if crash_at.is_none() {
                    lines.reverse();
                }
                format!("{guest} ‖ engine: {}", lines.join(" | "))
            };
            let detail: String = detail.chars().take(600).collect();
            let message = if detail.is_empty() {
                // Nothing in the serial at all: the engine never got the guest to its first print, and its
                // stderr says why (an unknown option, a name it could not open, a machine it refused).
                let engine = std::fs::read_to_string(serial.with_file_name("restore.log.engine.log"))
                    .unwrap_or_default();
                let engine_tail: Vec<String> = engine
                    .lines()
                    .map(str::trim_end)
                    .filter(|l| !l.is_empty())
                    .rev()
                    .take(3)
                    .map(str::to_string)
                    .collect();
                if engine_tail.is_empty() {
                    // Not even the engine said anything: name the machine line it was given, which is the
                    // one thing that differs between a host that works and a phone that does not.
                    let machine = config
                        .argv
                        .iter()
                        .position(|a| a == "-M")
                        .and_then(|i| config.argv.get(i + 1))
                        .cloned()
                        .unwrap_or_default();
                    let machine: String = machine.chars().take(220).collect();
                    format!("restore boot: {why} (no guest or engine output; -M {machine})")
                } else {
                    format!(
                        "restore boot: {why} (no guest output; engine: {})",
                        engine_tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
                    )
                }
            } else {
                format!("restore boot: {why} — {detail}")
            };
            return Err(Error::new(Code::OneshotFailed, message));
        }
        for n in [a6_restore::STREAM, a6_restore::RESTORE_RAMDISK, a6_restore::KBOOT_RESTORE] {
            let _ = std::fs::remove_file(c.work.join(n));
        }
        Ok(())
    }

    fn seal(&mut self, c: &mut Context) -> Result<()> {
        let ident = self.ident()?;
        let imei = ident.get("imei").map(str::to_string);
        let mut options = self.machine_options();
        options.remove("imei");
        let serial = c.work.join("seal.log");
        let hardware = self.helper_hardware.clone().ok_or_else(|| create::filled("the board's hardware"))?;
        let d = machine::IPad {
            boot: machine::IPadBoot::Kernel {
                image: c.file("kboot.bin").to_string_lossy().into_owned(),
                writable_nor: None,
            },
            nand: c.nand.to_string_lossy().into_owned(),
            overlay: None,
            die_id: None,
            wifi: false,
            machine_options: options,
        };
        let config = prep_config(machine::iphone5_net(
            &d,
            &hardware,
            &format!("file:{}", serial.display()),
            &["-audio".into(), "driver=none".into()],
            None,
            &[],
            imei.as_deref(),
            false,
        ));
        let (r, text) = oneshot::run_wait(
            &c.o.helper,
            &config.argv,
            &serial,
            Some(FIRST_BOOT_DONE),
            None,
            FIRST_BOOT_TIMEOUT,
            Duration::from_secs(30),
            &|l| c.log(l),
            None,
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
    fn boot_args_are_the_swift_ones() {
        assert!(BOOT_ARGS.starts_with("serial=3 debug=0x8 amfi_allow_any_signature=1"));
        assert!(BOOT_ARGS.ends_with("amfi_get_out_of_my_way=1 rd=disk0s1"));
    }
}

//! `A6Preparation.volumes`: the N42 System/Data bake and its records.
//!
//! The volumes are built by the shared `system_edits::build_k48` bake with the iPhone 5's options;
//! this module then performs the A6-only edits in the reference order: 14G61 graphics cache, Mav5
//! baseband, SideloadTools, MBR/fit, activation records and the 14G61 first-boot records.
use crate::a6_firstboot;
use crate::a6_graphics;
use crate::a6_storage::A6StorageLayout;
use crate::baseband;
use crate::error::Result;
use crate::fitcheck;
use crate::hfs::{Editor, Volume};
use crate::ipsw::Ipsw;
use crate::sideload;
use crate::{lock, plistedit, system_edits};
use plist::Value as PlistValue;
use std::path::Path;

pub struct Inputs<'a> {
    pub ipsw: &'a Path,
    pub decrypted: &'a Path,
    pub helpers: &'a Path,
    pub out: &'a Path,
    pub language: Option<String>,
    pub keyboards: Option<Vec<String>>,
    pub seed: Option<String>,
    /// Unix seconds: `entry.released` at 12:00:00Z. 14G61 = 1563796800.
    pub setup_finished: Option<i64>,
    pub now: i64,
}

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    use std::io::Write;
    f.write_all(bytes)?;
    Ok(())
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let bytes =
        serde_json::to_vec_pretty(&lock::sorted(value)).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    std::fs::write(path, bytes).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    Ok(())
}

fn set_root(system: &Path, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let v = Volume::open(system, true)?;
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    v.set_owner(&refs, 0, 0, None, None)?;
    Ok(())
}

pub fn volumes(i: &Inputs<'_>, fit: &fitcheck::Log, log: &dyn Fn(&str)) -> Result<system_edits::Built> {
    if i.out.exists() {
        return Err(crate::internal!("A6 volume output already exists"));
    }
    std::fs::create_dir_all(i.out)?;
    let layout = A6StorageLayout::standard();
    let kernel = std::fs::read(i.decrypted.join("kernelcache.mach"))?;
    let options = system_edits::Options {
        ca_ogl: true,
        web_proxy: false,
        appsync: false,
        guest_tools: false,
        seal: false,
        skip_setup: true,
        native_gestalt_activation: true,
        location_services_enabled: true,
        setup_finished: i.setup_finished,
        firmware_arch: "armv7s".into(),
        quiet_services: false,
        language: i.language.clone(),
        keyboards: i.keyboards.clone(),
        ..Default::default()
    };
    let result = system_edits::build_k48(
        &i.decrypted.join("rootfs.dmg"),
        i.out,
        layout.system_bytes,
        layout.data_bytes,
        &options,
        i.helpers,
        None,
        Some(kernel),
        None,
        fit,
        i.now,
        log,
    )?;

    // Swift reopens `result.system` with HFSPlusWriter(owner=(0,0)) for these edits.
    let mut ed = Editor::open(&result.system)?;
    let build = plistedit::read(&ed, "System/Library/CoreServices/SystemVersion.plist")?
        .and_then(|d| d.get("ProductBuildVersion").and_then(PlistValue::as_string).map(str::to_string))
        .unwrap_or_default();
    let mut root_owned: Vec<String> = Vec::new();
    if build == "14G61" {
        let path = a6_graphics::CACHE_PATH;
        let bytes = ed.read(path)?;
        let prepared = a6_graphics::prepare(&bytes, "14G61")?;
        if prepared != bytes {
            ed.write(path, prepared, Some(0o644))?;
            root_owned.push(path.to_string());
        }
        log("14G61 development cache enables the installed OpenGLES framework; native code retained");
    }
    let mav5 = baseband::install_mav5(&Ipsw::open(i.ipsw)?, "n42ap", &mut ed)?;
    log(&format!("Original modem firmware: {} files", mav5.len()));
    root_owned.extend(mav5);
    ed.commit()?;
    set_root(&result.system, &root_owned)?;

    sideload::install(&result.system, i.helpers)?;
    // The folders the modem firmware and the helpers made are root's, as the Swift writer (owner 0:0) makes them.
    set_root(
        &result.system,
        &[
            "usr/local",
            "usr/local/bin",
            "usr/local/standalone",
            "usr/local/standalone/firmware",
            "usr/local/standalone/firmware/Baseband",
        ]
        .map(String::from),
    )?;

    write_exclusive(&i.out.join("mbr.bin"), &layout.partition_table())?;
    write_json(&i.out.join("fit.json"), &fit.object())?;

    if let Some(activation) = result.activation.as_ref() {
        write_json(&i.out.join("activation.json"), &activation.record())?;
        if activation.native_cache_required == Some(true)
            && let Some(overrides) = activation.gestalt_overrides.clone()
        {
            let mut plan = plist::Dictionary::new();
            plan.insert("Build".into(), PlistValue::String(build.clone()));
            let mut o = plist::Dictionary::new();
            for (k, v) in overrides {
                o.insert(k, PlistValue::Boolean(v));
            }
            plan.insert("Overrides".into(), PlistValue::Dictionary(o));
            let mut bytes = Vec::new();
            PlistValue::Dictionary(plan)
                .to_writer_xml(&mut bytes)
                .map_err(|e| crate::internal!("activation-plan.plist: {e}"))?;
            write_exclusive(&i.out.join("activation-plan.plist"), &bytes)?;
            if build == "14G61" {
                let prep = a6_firstboot::install(&result.system, &result.data, &bytes, i.seed.as_deref())?;
                write_exclusive(&i.out.join("first-boot-preparation.json"), &prep)?;
                log("14G61 preparation records retained through native migration; no userland sealing boot required");
            }
            log("Native activation plan staged; require verified cache initialization before normal boot");
        }
    }
    Ok(result)
}

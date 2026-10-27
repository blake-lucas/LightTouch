//! fit-survey (development): runs the Mach-O-side fit checks and the guest-package seed against a pristine, extracted
//! system volume, mirroring the recipe's order, so the Fits, the activation and the seeded files can be compared
//! byte-for-byte with a Swift `create`'s device.lock.json and system volume.
//!
//!   firmwarekit fit-survey --system IMG --guest-tools DIR --recipe k48|n72 [--kernel FILE] [--commit]
//!
//! It stages the volume exactly as SystemEdits does before the seed (the it_msmquiet/libappsync/OpenGLES hook
//! targets, the storage-mounter DYLD insert), prints every Fit the log recorded as JSON Lines on stdout, then the activation record
//! and the guest_package record; with --commit it writes the seed into the image so `hfs-list` can hash it.
use lighttouch_firmware::fitcheck::{self, Firmware, Fit, Log};
use lighttouch_firmware::hfs::Editor;
use lighttouch_firmware::{activation, guest_package, plistedit};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const KBOOT_DEFAULT_ARGS: &str = "serial=3 debug=0x8 amfi_allow_any_signature=1 cs_enforcement_disable=1 enable-hsic=1";
const SPRINGBOARD: &str = "System/Library/CoreServices/SpringBoard.app/SpringBoard";
const DAEMONS: &str = "System/Library/LaunchDaemons";
const OPENGLES: &str = "System/Library/Frameworks/OpenGLES.framework/OpenGLES";
const APPSYNC_PATH: &str = "usr/lib/libappsync.dylib";
const MSM_PATH: &str = "usr/local/lib/it_msmquiet.dylib";

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn read(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    std::fs::read(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()))
}

/// The ProgramArguments[0] (else Program) of a daemon job, with the Label checked.
fn stock_program(ed: &Editor, job: &str, label: &str) -> Result<String, String> {
    let d = plistedit::read(ed, job).map_err(|e| e.to_string())?.ok_or_else(|| format!("{job}: no plist"))?;
    if d.get("Label").and_then(|v| v.as_string()) != Some(label) {
        return Err(format!("{job}: not {label}'s job"));
    }
    d.get("ProgramArguments")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_string())
        .map(str::to_string)
        .or_else(|| d.get("Program").and_then(|v| v.as_string()).map(str::to_string))
        .ok_or_else(|| format!("{job}: names no program"))
}

/// Every Fit the log recorded, in order, as JSON Lines.
fn emit_all(log: &Log) {
    for f in log.fits.borrow().iter() {
        println!("{}", serde_json::to_string(f).unwrap());
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let system = PathBuf::from(opt(args, "--system").ok_or("--system IMG required")?);
    let tools = PathBuf::from(opt(args, "--guest-tools").ok_or("--guest-tools DIR required")?);
    let recipe = opt(args, "--recipe").unwrap_or_else(|| "k48".into());
    let commit = args.iter().any(|a| a == "--commit");
    let kernel = opt(args, "--kernel").map(|k| std::fs::read(k)).transpose().map_err(|e| e.to_string())?;

    let mut ed = Editor::open(&system).map_err(|e| e.to_string())?;
    let lockdownd = ed.read("usr/libexec/lockdownd").map_err(|e| e.to_string())?;

    if recipe == "k48" {
        survey_k48(&mut ed, &tools, kernel.as_deref())?;
    } else {
        survey_n72(&mut ed, &tools, kernel.as_deref())?;
    }

    // Activation: on the pristine lockdownd bytes, byte-for-byte what the recipe records and bakes.
    match activation::run(&lockdownd) {
        Ok((bytes, result)) => {
            if commit {
                ed.write("usr/libexec/lockdownd", bytes, None).map_err(|e| e.to_string())?;
            }
            println!("ACTIVATION {}", serde_json::to_string(&result.record()).unwrap());
        }
        Err(e) => match activation::data_ark_route(&lockdownd) {
            Some(r) => println!("ACTIVATION {}", serde_json::to_string(&r.record()).unwrap()),
            None => println!("ACTIVATION-FAILED {e}"),
        },
    }

    if commit {
        ed.commit().map_err(|e| e.to_string())?;
        eprintln!("committed the seeded volume");
    }
    Ok(())
}

/// The k48 (armv7, iPod touch 4G 6.1.6) fit sequence: K48Recipe.bootFiles then SystemEdits.apply then the seed.
fn survey_k48(ed: &mut Editor, tools: &Path, kernel: Option<&[u8]>) -> Result<(), String> {
    let log = Log::default();
    let arch = "armv7";

    // --- K48Recipe.bootFiles: the boot-args and the DeviceTree property.
    fitcheck::check_boot_args(&log, kernel, KBOOT_DEFAULT_ARGS).map_err(|e| e.to_string())?;
    log.check(fitcheck::device_tree_property(kernel, "arm-io/usb-complex", "hsic-enabled"), false, "left out")
        .map_err(|e| e.to_string())?;

    // --- Stage the hook targets SystemEdits writes before the seed, so the later checks and seed see them.
    // it_msmquiet into usr/local/lib, inserted into the arbitrator's job; libappsync into usr/lib.
    let msm = read(tools, "it_msmquiet.dylib")?;
    let appsync = read(tools, "libappsync.dylib")?;
    let front_end = read(tools, "OpenGLES")?;
    let gles_names = String::from_utf8_lossy(&read(tools, "gles-names.h")?).into_owned();

    // The noticeJobs that exist, and whether it_msmquiet fits each (checkAny): storage-mounter, usb-arbitrator.
    let notice: [(&str, &str); 2] = [
        ("com.apple.mobile.storage_mounter.plist", "com.apple.mobile.storage_mounter"),
        ("com.apple.mobile.usb_device_arbitrator.plist", "com.apple.mobile.usb_device_arbitrator"),
    ];
    let mut quiet_places = Vec::new();
    let mut quiet_programs = Vec::new();
    {
        let fw = Firmware::new(ed.snapshot().map_err(|e| e.to_string())?, arch, kernel.map(|k| k.to_vec()));
        for (file, label) in notice {
            let job = format!("{DAEMONS}/{file}");
            if !ed.exists(&job) {
                continue;
            }
            let program = stock_program(ed, &job, label)?;
            quiet_places.push(fitcheck::msm_quiet(&fw, &program, &msm));
            quiet_programs.push((job, label, program));
        }
    }
    let quiet_fits = log.check_any(quiet_places.clone(), "left out");

    // it_prefs' three settings.
    let pref_fits = {
        let fw = Firmware::new(ed.snapshot().map_err(|e| e.to_string())?, arch, None);
        fitcheck::prefs(&fw, fitcheck::IT_PREFS)
    };
    for f in pref_fits {
        log.check(f, false, "kept: it_prefs skips the key at boot").map_err(|e| e.to_string())?;
    }

    // The baked helper tools: it_pbd, it_prefs, it_seal (it_msmquiet handled above; no it_gltest here).
    {
        let fw = Firmware::new(ed.snapshot().map_err(|e| e.to_string())?, arch, None);
        for (name, _path) in [("it_pbd", "usr/local/bin/it_pbd"), ("it_prefs", "usr/local/bin/it_prefs"), ("it_seal", "usr/local/bin/it_seal")] {
            let bytes = read(tools, name)?;
            let f = fitcheck::loads(name, &bytes, &fw, None);
            log.check(f, true, "left out").map_err(|e| e.to_string())?;
        }
        // AppSync (libappsync in installd).
        fitcheck::check_app_sync(&log, &fw, tools, "libappsync.dylib").map_err(|e| e.to_string())?;
        // the web proxy PAC.
        let wp = fitcheck::web_proxy(&fw);
        log.check(wp, false, "kept: the PAC is unused").map_err(|e| e.to_string())?;
        // the GL front end.
        let gl = fitcheck::gles_front_end(&fw, &front_end, &gles_names);
        log.check(gl, true, "left out").map_err(|e| e.to_string())?;
        // SpringBoard environment (caOGL: MBX2D_PAGE_FLIP only), with the front end as an extra reader.
        let switches = vec![vec!["MBX2D_PAGE_FLIP".to_string()]];
        let also = vec![("OpenGLES".to_string(), front_end.clone())];
        let env = fitcheck::environment(&fw, &switches, &also);
        log.check(env, false, "kept: a switch nothing reads is inert").map_err(|e| e.to_string())?;
    }

    // --- Stage the volume for the seed: the hooks' targets as SystemEdits writes them.
    ed.mkdirs("usr/local/lib").map_err(|e| e.to_string())?;
    ed.write(MSM_PATH, msm.clone(), Some(0o755)).map_err(|e| e.to_string())?;
    // Insert it_msmquiet into whichever notice jobs fit (so host(of:) finds the host at seed time).
    for ((job, label, _), &fits) in quiet_programs.iter().zip(quiet_fits.iter()) {
        if !fits {
            continue;
        }
        plistedit::rewrite(ed, job, |d| {
            if d.get("Label").and_then(|v| v.as_string()) != Some(label.as_ref()) {
                return Err(lighttouch_firmware::error::Error::unsupported(format!("{job}: not {label}'s job")));
            }
            plistedit::dyld_insert(d, &format!("/{MSM_PATH}"));
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    }
    // libappsync (installAppSync puts it 0644 and inserts it into installd).
    ed.write(APPSYNC_PATH, appsync, Some(0o644)).map_err(|e| e.to_string())?;
    let installd = format!("{DAEMONS}/com.apple.mobile.installd.plist");
    if ed.exists(&installd) {
        plistedit::rewrite(ed, &installd, |d| {
            plistedit::dyld_insert(d, &format!("/{APPSYNC_PATH}"));
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    }
    // installCAOGL: preserve the OpenGLES hook target's absence, then write the front end there.
    guest_package::preserve_hook(ed, OPENGLES).map_err(|e| e.to_string())?;
    ed.write(OPENGLES, front_end, Some(0o755)).map_err(|e| e.to_string())?;

    // --- The seed (loader + package), which appends the loader and package-binary Fits.
    let omitted: HashSet<String> = HashSet::new();
    let (_written, record) = guest_package::seed(ed, &tools.join("armv7.itpack"), true, &omitted, &log).map_err(|e| e.to_string())?;
    emit_all(&log);
    println!("GUEST-PACKAGE {}", serde_json::to_string(&record.object()).unwrap());
    Ok(())
}

/// The n72 (armv6, iPod touch 2G 3.1.3) fit sequence: N72Recipe.bootFiles then systemEdits then the seed.
fn survey_n72(ed: &mut Editor, tools: &Path, kernel: Option<&[u8]>) -> Result<(), String> {
    let log = Log::default();
    let arch = "armv6";

    // The AMFI pair, in sorted "name=1" form.
    let pair = "amfi_allow_any_signature=1 cs_enforcement_disable=1";
    fitcheck::check_boot_args(&log, kernel, pair).map_err(|e| e.to_string())?;

    let appsync = read(tools, "libappsync.dylib")?;
    let front_end = read(tools, "OpenGLES")?;
    let gles_names = String::from_utf8_lossy(&read(tools, "gles-names.h")?).into_owned();
    let typein = read(tools, "it_typein.dylib")?;

    {
        let fw = Firmware::new(ed.snapshot().map_err(|e| e.to_string())?, arch, None);
        // guestToolsFit: it_agent, it_typein.dylib (host SpringBoard), sblaunch, sbdlicon — one combined Fit.
        let names: [(&str, Option<String>); 4] = [
            ("it_agent", None),
            ("it_typein.dylib", Some(format!("/{SPRINGBOARD}"))),
            ("sblaunch", None),
            ("sbdlicon", None),
        ];
        let mut fits = Vec::new();
        for (n, host) in &names {
            if *n == "sbdlicon" && !tools.join(n).exists() {
                continue;
            }
            let bytes = read(tools, n)?;
            fits.push(fitcheck::loads(n, &bytes, &fw, host.as_deref()));
        }
        let piece = format!("guest tools ({})", fits.iter().map(|f| f.piece.clone()).collect::<Vec<_>>().join(", "));
        let lost: Vec<&Fit> = fits.iter().filter(|f| !f.fits).collect();
        let tools_fit = if lost.is_empty() {
            Fit::new(piece, true, format!("each loads ({}: {})", fits[0].piece, fits[0].proof))
        } else {
            Fit::new(piece, false, lost.iter().map(|f| format!("{}: {}", f.piece, f.proof)).collect::<Vec<_>>().join("; "))
        };
        log.check(tools_fit, false, "left out").map_err(|e| e.to_string())?;

        // AppSync.
        fitcheck::check_app_sync(&log, &fw, tools, "libappsync.dylib").map_err(|e| e.to_string())?;
        // it_prefs SBDidShowReorderText only.
        let pf = fitcheck::prefs(&fw, &fitcheck::IT_PREFS[..1]);
        for f in pf {
            log.check(f, false, "kept: it_prefs skips the key at boot").map_err(|e| e.to_string())?;
        }
        // the GL front end.
        let gl = fitcheck::gles_front_end(&fw, &front_end, &gles_names);
        log.check(gl, true, "left out").map_err(|e| e.to_string())?;
        // SpringBoard environment: the CA/LK switch pairs.
        let switches = vec![
            vec!["CA_ENABLE_OGL".to_string(), "LK_ENABLE_OGL".to_string()],
            vec!["CA_AUTO_ENABLE_OGL".to_string(), "LK_AUTO_ENABLE_OGL".to_string()],
        ];
        let env = fitcheck::environment(&fw, &switches, &[]);
        log.check(env, false, "kept: a switch nothing reads is inert").map_err(|e| e.to_string())?;
        // web proxy PAC.
        let wp = fitcheck::web_proxy(&fw);
        log.check(wp, false, "kept: the PAC is unused").map_err(|e| e.to_string())?;
    }

    // Stage: libappsync, it_typein, the front end for the seed's hooks.
    ed.write(APPSYNC_PATH, appsync, Some(0o644)).map_err(|e| e.to_string())?;
    ed.mkdirs("usr/lib").map_err(|e| e.to_string())?;
    ed.write("usr/lib/it_typein.dylib", typein, Some(0o755)).map_err(|e| e.to_string())?;
    // N72Recipe's SpringBoard job edit: it_typein inserted into SpringBoard (host(of:) finds it at seed time).
    plistedit::rewrite(ed, &format!("{DAEMONS}/com.apple.SpringBoard.plist"), |d| {
        plistedit::dyld_insert(d, "/usr/lib/it_typein.dylib");
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    guest_package::preserve_hook(ed, OPENGLES).map_err(|e| e.to_string())?;
    ed.write(OPENGLES, front_end, Some(0o755)).map_err(|e| e.to_string())?;

    let omitted: HashSet<String> = HashSet::new();
    let (_written, record) = guest_package::seed(ed, &tools.join("armv6.itpack"), true, &omitted, &log).map_err(|e| e.to_string())?;
    emit_all(&log);
    println!("GUEST-PACKAGE {}", serde_json::to_string(&record.object()).unwrap());
    Ok(())
}

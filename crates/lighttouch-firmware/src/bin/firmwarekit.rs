//! firmwarekit: the preparer. `create` is the command the apps run (JSON Lines on stdout, diagnostics on stderr);
//! the others are for development.
//!
//!   firmwarekit create (--entry FILE | --catalog FILE --id ID) --ipsw FILE --out DIR [...]   the command the apps run
//!   firmwarekit oneshot ARGV... | '{"mode":{"machines":{}}}'   one preparation boot (the engine in this process)
//!   firmwarekit decrypt --catalog FILE --id ID --ipsw FILE --out DIR [--no-rootfs]
//!   firmwarekit extract-rootfs DMG OUT
//!   firmwarekit hfs-list IMG [PREFIX]          JSON Lines: owner, mode, flags, sizes, sha256, link per path
//!   firmwarekit hfs-edit IMG [EDIT...]         edits, then commit: write:PATH=HOSTFILE mkdirs:PATH
//!                                              symlink:PATH=TARGET rm:PATH mv:FROM=TO chmod:PATH=OCTAL journal:MIB
//!   firmwarekit hfs-grow IMG BYTES
//!   firmwarekit hfs-format IMG BYTES NAME [--journaled]
//!   firmwarekit k48-build --mbr FILE --system IMG [--data IMG] [--s3 IMG] --out DIR --kernelcache FILE
//!                         [--epoch N | --ipsw FILE --board BOARD] [--plain] [--sig-flags N] [--vendor-type N]
//!   firmwarekit k48-rebuild --store DIR [--overlay DIR] --system IMG [--data IMG] --out DIR [--work DIR]
//!   firmwarekit k48-check DIR [--mbr FILE] [--system IMG]
//!   firmwarekit write-n72-nand VOLUME OUT --blocks N --epoch E
//!   firmwarekit read-n72-nand NAND OUT --blocks N   the volume back from an n72 store
//!   firmwarekit read-n45-nand NAND OUT --bytes N    the volume back from an n45/m68 store
//!   firmwarekit write-n45-nand VOLUME OUT (--fil-id 0xN | --iboot FILE) [--banks N] [--bbt-map]
//!   firmwarekit build-n72-nor --ipsw FILE --identity JSON --out FILE [--board B] [--types A,B] [--wrap all|A,B]
//!   firmwarekit build-n45-nor --ipsw FILE --identity JSON --out FILE [--board B] [--boot-args S]
//!   firmwarekit seed-data --system IMG [--var-root P] [--language TAG] [--keyboards A,B] [--skip-setup]
//!                         [--region LL] [--now SECS] [--jailbreak DIR]   the data-volume seeds on an image
//!   firmwarekit fit-survey --system IMG --guest-tools DIR --recipe k48|n72 [--kernel FILE] [--commit]
//!   firmwarekit kboot --decrypted DIR --identity JSON --out FILE [--ramdisk FILE] [--boot-args S] [--sandbox-patch]
//!   firmwarekit k48-iboot --catalog FILE --id ID --ipsw FILE --decrypted DIR --out DIR [--seed S] [--boot-args S]
use lighttouch_firmware::entry::Entry;
use lighttouch_firmware::hfs::{Editor, Volume};
use lighttouch_firmware::ipsw::Ipsw;
use std::path::PathBuf;
use std::process::ExitCode;

#[path = "support/fit_survey.rs"]
mod fit_survey;

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn required(args: &[String], name: &str) -> Result<String, String> {
    option(args, name).ok_or_else(|| format!("{name} is required"))
}

fn decrypt(args: &[String]) -> Result<(), String> {
    let entry = Entry::load_from_catalog(&PathBuf::from(required(args, "--catalog")?), &required(args, "--id")?)
        .map_err(|e| e.to_string())?;
    let ipsw = Ipsw::open(&PathBuf::from(required(args, "--ipsw")?)).map_err(|e| e.to_string())?;
    let out = PathBuf::from(required(args, "--out")?);
    let rootfs = !args.iter().any(|a| a == "--no-rootfs");
    let started = std::time::Instant::now();
    let result = lighttouch_firmware::decrypt::decrypt(&ipsw, &entry, &out, rootfs, &|line| eprintln!("{line}"))
        .map_err(|e| e.to_string())?;
    for name in &result.files {
        let sha = lighttouch_firmware::cache::sha256_file(&out.join(name)).map_err(|e| e.to_string())?;
        println!("{sha}  {name}");
    }
    eprintln!("decrypted in {:.1} s (plain tail: {})", started.elapsed().as_secs_f64(), result.plain_tail);
    Ok(())
}

fn extract_rootfs(args: &[String]) -> Result<(), String> {
    let (Some(dmg), Some(out)) = (args.first(), args.get(1)) else { return Err("extract-rootfs DMG OUT".into()) };
    let started = std::time::Instant::now();
    lighttouch_firmware::udif::extract_rootfs(&PathBuf::from(dmg), &PathBuf::from(out)).map_err(|e| e.to_string())?;
    eprintln!("extracted in {:.1} s", started.elapsed().as_secs_f64());
    Ok(())
}

fn hfs_list(args: &[String]) -> Result<(), String> {
    use std::io::Write;
    let Some(image) = args.first() else { return Err("hfs-list IMG [PREFIX]".into()) };
    let volume = Volume::open(&PathBuf::from(image), false).map_err(|e| e.to_string())?;
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for e in volume.listing(args.get(1).map(String::as_str).unwrap_or(""), true).map_err(|e| e.to_string())? {
        writeln!(out, "{}", serde_json::to_string(&e).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// One file out of an HFS+ volume, to stdout: `hfs-cat IMG PATH`.
fn hfs_cat(args: &[String]) -> Result<(), String> {
    let (Some(image), Some(path)) = (args.first(), args.get(1)) else { return Err("hfs-cat IMG PATH".into()) };
    let ed = Editor::open_read_only(&PathBuf::from(image)).map_err(|e| e.to_string())?;
    let data = ed.read(path).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut std::io::stdout().lock(), &data).map_err(|e| e.to_string())
}

fn hfs_edit(args: &[String]) -> Result<(), String> {
    let Some(image) = args.first() else { return Err("hfs-edit IMG [EDIT...]".into()) };
    let started = std::time::Instant::now();
    let mut ed = Editor::open(&PathBuf::from(image)).map_err(|e| e.to_string())?;
    for edit in &args[1..] {
        let (verb, rest) = edit.split_once(':').ok_or_else(|| format!("not an edit: {edit}"))?;
        let pair = || rest.split_once('=').ok_or_else(|| format!("{verb} wants A=B: {edit}"));
        let result = match verb {
            "write" => pair().and_then(|(p, f)| ed.write_from(p, &PathBuf::from(f), None).map_err(|e| e.to_string())),
            "mkdirs" => ed.mkdirs(rest).map_err(|e| e.to_string()),
            "symlink" => pair().and_then(|(p, t)| ed.symlink(p, t).map_err(|e| e.to_string())),
            "rm" => ed.remove(rest).map_err(|e| e.to_string()),
            "mv" => pair().and_then(|(a, b)| ed.rename(a, b).map_err(|e| e.to_string())),
            "chmod" => pair().and_then(|(p, m)| {
                let mode = u16::from_str_radix(m, 8).map_err(|e| e.to_string())?;
                ed.chmod(p, mode).map_err(|e| e.to_string())
            }),
            "journal" => {
                let mib: u64 = rest.parse().map_err(|_| format!("journal:{rest}: not a size in MiB"))?;
                ed.enable_journal(mib << 20).map_err(|e| e.to_string())
            }
            _ => Err(format!("unknown edit: {verb}")),
        };
        result?;
    }
    ed.commit().map_err(|e| e.to_string())?;
    eprintln!("committed in {:.2} s", started.elapsed().as_secs_f64());
    Ok(())
}

fn hfs_grow(args: &[String]) -> Result<(), String> {
    let (Some(image), Some(bytes)) = (args.first(), args.get(1)) else { return Err("hfs-grow IMG BYTES".into()) };
    Editor::grow(&PathBuf::from(image), bytes.parse().map_err(|_| "BYTES: a number")?).map_err(|e| e.to_string())
}

fn hfs_format(args: &[String]) -> Result<(), String> {
    let (Some(image), Some(bytes), Some(name)) = (args.first(), args.get(1), args.get(2)) else {
        return Err("hfs-format IMG BYTES NAME [--journaled]".into());
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let bytes = bytes.parse().map_err(|_| "BYTES: a number")?;
    Editor::format(&PathBuf::from(image), bytes, name, args.iter().any(|a| a == "--journaled"), now).map_err(|e| e.to_string())
}

fn number(s: &str) -> Result<u32, String> {
    let r = match s.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|_| format!("not a number: {s}"))
}

fn k48_build(args: &[String]) -> Result<(), String> {
    use lighttouch_firmware::k48nand::{self, Build, DataVolume, Geometry};
    let kc = PathBuf::from(required(args, "--kernelcache")?);
    let data = option(args, "--data").map_or(DataVolume::None, |d| DataVolume::Image(PathBuf::from(d)));
    let mut b = Build::new(
        &PathBuf::from(required(args, "--mbr")?),
        k48nand::kernel_version(&kc).map_err(|e| e.to_string())?,
        &PathBuf::from(required(args, "--system")?),
        data,
        &PathBuf::from(required(args, "--out")?),
    );
    b.geometry = Geometry::for_vendor_type(option(args, "--vendor-type").map(|v| number(&v)).transpose()?.map(i64::from));
    // as the K48 recipe picks it: an explicit one, else the IPSW's SCEP for --board (the S5L8920 boards), else the
    // kernel's FIL
    let scep = match (option(args, "--ipsw"), option(args, "--board")) {
        (Some(ipsw), Some(board)) => {
            let ipsw = Ipsw::open(&PathBuf::from(ipsw)).map_err(|e| e.to_string())?;
            k48nand::restore_epoch_of(&ipsw, &board).map_err(|e| e.to_string())?
        }
        _ => None,
    };
    b.epoch = match (option(args, "--epoch"), scep) {
        (Some(e), _) => number(&e)? as u8,
        (None, Some(scep)) => scep,
        (None, None) => k48nand::signature_epoch(&kc).map_err(|e| e.to_string())?,
    };
    eprintln!("NAND signature epoch {}", b.epoch);
    b.s3 = option(args, "--s3").map(PathBuf::from);
    b.whitening = !args.iter().any(|a| a == "--plain");
    b.sig_flags = option(args, "--sig-flags").map(|f| number(&f)).transpose()?;
    let started = std::time::Instant::now();
    let r = k48nand::build(&b, &|line| eprintln!("{line}")).map_err(|e| e.to_string())?;
    eprintln!("{} records in {:.1} s", r.records, started.elapsed().as_secs_f64());
    Ok(())
}

fn k48_rebuild(args: &[String]) -> Result<(), String> {
    let out = PathBuf::from(required(args, "--out")?);
    let work = option(args, "--work").map(PathBuf::from).unwrap_or_else(|| out.with_extension("work"));
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let started = std::time::Instant::now();
    let r = lighttouch_firmware::k48nand::rebuild(
        &PathBuf::from(required(args, "--store")?),
        option(args, "--overlay").map(PathBuf::from).as_deref(),
        &PathBuf::from(required(args, "--system")?),
        option(args, "--data").map(PathBuf::from).as_deref(),
        &out,
        &work,
        &|line| eprintln!("{line}"),
    )
    .map_err(|e| e.to_string())?;
    eprintln!("{} records in {:.1} s", r.records, started.elapsed().as_secs_f64());
    Ok(())
}

fn k48_check(args: &[String]) -> Result<(), String> {
    let Some(dir) = args.first() else { return Err("k48-check DIR [--mbr FILE] [--system IMG]".into()) };
    let mbr = option(args, "--mbr").map(PathBuf::from);
    let system = option(args, "--system").map(PathBuf::from);
    let passed = lighttouch_firmware::k48nand_check::check(
        &PathBuf::from(dir),
        mbr.as_deref(),
        system.as_deref(),
        &|line| println!("{line}"),
    )
    .map_err(|e| e.to_string())?;
    if passed { Ok(()) } else { Err("the store failed its check".into()) }
}

/// A store of chip files (geometry.json + bus<b>-ce<c>.pages or cs<N>.pages) packed into its NAND container,
/// nand.qcow2, in place: each chip file is removed once read.
fn pack_nand(args: &[String]) -> Result<(), String> {
    let Some(nand) = args.first() else {
        return Err("pack-nand NAND_DIR".into());
    };
    let started = std::time::Instant::now();
    let read = std::sync::atomic::AtomicU64::new(0);
    let layout = lighttouch_firmware::nand_disk::pack(&PathBuf::from(nand), &|n| {
        read.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    })
    .map_err(|e| e.to_string())?;
    eprintln!(
        "{} chips, {} data bytes read, block stride {} in {:.1} s",
        layout.chips,
        read.into_inner(),
        layout.block_stride,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn write_n72_nand(args: &[String]) -> Result<(), String> {
    let (Some(volume), Some(out)) = (args.first(), args.get(1)) else {
        return Err("write-n72-nand VOLUME OUT --blocks N --epoch E".into());
    };
    let blocks = number(&required(args, "--blocks")?)? as usize;
    let epoch = number(&required(args, "--epoch")?)?;
    let started = std::time::Instant::now();
    let (written, meta) = lighttouch_firmware::n72nand::write(&PathBuf::from(volume), blocks, epoch, &PathBuf::from(out))
        .map_err(|e| e.to_string())?;
    eprintln!("{written} filesystem pages, {meta} metadata pages (epoch {epoch}) in {:.1} s", started.elapsed().as_secs_f64());
    Ok(())
}

/// The volume an n72 store holds, read back through the page predictor (holes and blank pages as zeros).
fn read_n72_nand(args: &[String]) -> Result<(), String> {
    use lighttouch_firmware::n72nand::{predict, PAGE};
    use std::io::{Seek, SeekFrom, Write};
    let (Some(nand), Some(out)) = (args.first(), args.get(1)) else {
        return Err("read-n72-nand NAND OUT --blocks N".into());
    };
    let blocks = number(&required(args, "--blocks")?)? as usize;
    let store = lighttouch_firmware::page_nand::Store::open(&PathBuf::from(nand), false).map_err(|e| e.to_string())?;
    let mut f = std::fs::File::create(out).map_err(|e| format!("{out}: {e}"))?;
    f.set_len((blocks * PAGE) as u64).map_err(|e| format!("{out}: {e}"))?;
    let mut read = 0;
    for n in 0..blocks {
        let p = predict(n);
        let rec = store.read(p.cs, p.page).map_err(|e| e.to_string())?;
        let data = &rec[..PAGE];
        if data.iter().all(|&b| b == 0) {
            continue;
        }
        f.seek(SeekFrom::Start((n * PAGE) as u64)).map_err(|e| e.to_string())?;
        f.write_all(data).map_err(|e| e.to_string())?;
        read += 1;
    }
    eprintln!("{read} pages read back");
    Ok(())
}

/// The volume an n45/m68 store holds, read back through its FTL (volume page i is logical page FIRST_LBA + i).
fn read_n45_nand(args: &[String]) -> Result<(), String> {
    use lighttouch_firmware::n45nand::{FIRST_LBA, PAGE};
    use std::io::{Seek, SeekFrom, Write};
    let (Some(nand), Some(out)) = (args.first(), args.get(1)) else {
        return Err("read-n45-nand NAND OUT --bytes N".into());
    };
    let bytes = number(&required(args, "--bytes")?)? as usize;
    let ftl = lighttouch_firmware::n45ftl::N45Ftl::open(&PathBuf::from(nand), None).map_err(|e| e.to_string())?;
    let mut f = std::fs::File::create(out).map_err(|e| format!("{out}: {e}"))?;
    f.set_len(bytes as u64).map_err(|e| format!("{out}: {e}"))?;
    let mut read = 0;
    for i in 0..bytes.div_ceil(PAGE) {
        let Some((data, _)) = ftl.read(FIRST_LBA + i).map_err(|e| e.to_string())? else { continue };
        if data.iter().all(|&b| b == 0) {
            continue;
        }
        f.seek(SeekFrom::Start((i * PAGE) as u64)).map_err(|e| e.to_string())?;
        f.write_all(&data).map_err(|e| e.to_string())?;
        read += 1;
    }
    eprintln!("{read} pages read back");
    Ok(())
}

fn write_n45_nand(args: &[String]) -> Result<(), String> {
    use lighttouch_firmware::n45nand;
    let (Some(volume), Some(out)) = (args.first(), args.get(1)) else {
        return Err("write-n45-nand VOLUME OUT (--fil-id 0xN | --iboot FILE) [--banks N] [--bbt-map]".into());
    };
    let fil = match (option(args, "--fil-id"), option(args, "--iboot")) {
        (Some(f), _) => number(&f)?,
        (None, Some(ib)) => {
            n45nand::fil_id(&std::fs::read(&ib).map_err(|e| format!("{ib}: {e}"))?).map_err(|e| e.to_string())?
        }
        _ => return Err("--fil-id or --iboot is required".into()),
    };
    let banks = option(args, "--banks").map(|b| number(&b)).transpose()?.unwrap_or(n45nand::BANKS as u32) as usize;
    let started = std::time::Instant::now();
    let (written, meta) =
        n45nand::write(&PathBuf::from(volume), &PathBuf::from(out), fil, banks, args.iter().any(|a| a == "--bbt-map"))
            .map_err(|e| e.to_string())?;
    eprintln!("{written} filesystem pages, {meta} metadata pages (NAND signature {fil:#x}) in {:.1} s", started.elapsed().as_secs_f64());
    Ok(())
}

fn identity_file(args: &[String]) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let path = required(args, "--identity")?;
    let text = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    serde_json::from_slice(&text).map_err(|e| format!("{path}: {e}"))
}

fn build_n72_nor(args: &[String]) -> Result<(), String> {
    use lighttouch_firmware::n72nor;
    let ipsw = Ipsw::open(&PathBuf::from(required(args, "--ipsw")?)).map_err(|e| e.to_string())?;
    let board = option(args, "--board").unwrap_or_else(|| "n72ap".into());
    let prefix = format!("Firmware/all_flash/all_flash.{board}.production/");
    let mut images = std::collections::BTreeMap::new();
    for n in ipsw.names().map_err(|e| e.to_string())? {
        if n.starts_with(&prefix) && n.ends_with(".img3") {
            let d = ipsw.read(&n).map_err(|e| e.to_string())?;
            images.insert(n72nor::img3_type(&d).map_err(|e| e.to_string())?, d);
        }
    }
    // as N72Recipe: the stock order, of the types the manifest ships
    let manifest = String::from_utf8_lossy(&ipsw.read(&format!("{prefix}manifest")).map_err(|e| e.to_string())?).into_owned();
    let mut shipped = std::collections::BTreeSet::new();
    for m in manifest.split_whitespace() {
        let d = ipsw.read(&format!("{prefix}{m}")).map_err(|e| e.to_string())?;
        shipped.insert(n72nor::img3_type(&d).map_err(|e| e.to_string())?);
    }
    let types: Vec<String> = match option(args, "--types") {
        Some(t) => t.split(',').map(String::from).collect(),
        None => n72nor::ORDER.iter().filter(|t| shipped.contains(**t)).map(|t| t.to_string()).collect(),
    };
    let wrap: Option<Vec<String>> = option(args, "--wrap").filter(|w| w != "all").map(|w| w.split(',').map(String::from).collect());
    let nor = n72nor::build(&identity_file(args)?, &images, &types, wrap.as_deref()).map_err(|e| e.to_string())?;
    let out = required(args, "--out")?;
    std::fs::write(&out, nor).map_err(|e| format!("{out}: {e}"))?;
    eprintln!("types {types:?}");
    Ok(())
}

fn build_n45_nor(args: &[String]) -> Result<(), String> {
    use lighttouch_firmware::img3::{apple8900, img2};
    let ipsw = Ipsw::open(&PathBuf::from(required(args, "--ipsw")?)).map_err(|e| e.to_string())?;
    let board = option(args, "--board").unwrap_or_else(|| "n45ap".into());
    let prefix = format!("Firmware/all_flash/all_flash.{board}.production/");
    let mut images = std::collections::BTreeMap::new();
    for n in ipsw.names().map_err(|e| e.to_string())? {
        if n.starts_with(&prefix) && n.ends_with(".img2") {
            let body = apple8900::body(&ipsw.read(&n).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            images.insert(img2::header(&body).map_err(|e| e.to_string())?.kind, body);
        }
    }
    let boot_args = option(args, "--boot-args").unwrap_or_else(|| lighttouch_firmware::n45nor::BOOT_ARGS.into());
    let nor = lighttouch_firmware::n45nor::build(&identity_file(args)?, &images, &boot_args).map_err(|e| e.to_string())?;
    let out = required(args, "--out")?;
    std::fs::write(&out, nor).map_err(|e| format!("{out}: {e}"))
}

/// `create`: the command the apps run. stdout is JSON Lines only (begin, step, progress, warning, done, error);
/// diagnostics go to stderr. Exit 0 after done, 1 after an error event, 64 for a command line it doesn't take.
fn create(args: &[String]) -> ExitCode {
    use lighttouch_firmware::create::{self, Options};
    use lighttouch_firmware::oneshot::Helper;
    use lighttouch_firmware::progress::Event;
    use std::io::Write;
    use std::sync::Arc;
    let emit: lighttouch_firmware::progress::Emit = Arc::new(|e: Event| {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", e.json());
        let _ = out.flush();
    });
    let flag = |name: &str| args.iter().any(|a| a == name);
    let path = |name: &str| option(args, name).map(PathBuf::from);
    let fail = |e: lighttouch_firmware::error::Error| {
        eprintln!("firmwarekit: {}", e.message);
        emit(Event::error(&e));
        ExitCode::from(1)
    };
    let (Some(ipsw), Some(out)) = (path("--ipsw"), path("--out")) else {
        eprintln!("usage: firmwarekit create (--entry FILE | --catalog FILE --id ID) --ipsw FILE --out DIR [--seed S] [--helper FILE] [--cache DIR] [--guest-tools DIR] [[--sibling-entry FILE] --sibling-ipsw FILE] [--stop-after volumes] [--language TAG] [--keyboards A,B] [--gl-test] [--skip-setup] [--jailbreak] [--built-listing] [--bootrom FILE]");
        return ExitCode::from(64);
    };
    let setup = || -> lighttouch_firmware::error::Result<Options> {
        let mut entry = match (path("--entry"), path("--catalog")) {
            (Some(e), _) => Entry::load(&e)?,
            (None, Some(c)) => Entry::load_from_catalog(&c, &option(args, "--id").unwrap_or_default())?,
            _ => return Err(lighttouch_firmware::unsupported!("--entry or --catalog with --id is required")),
        };
        if let Some(r) = entry.recipe.as_mut() {
            for (name, key) in [("--gl-test", "gl_test"), ("--skip-setup", "skip_setup"), ("--jailbreak", "jailbreak")] {
                if flag(name) {
                    r.options.insert(key.into(), true);
                }
            }
        }
        let exe = std::env::current_exe().map_err(|e| lighttouch_firmware::internal!("this program's path: {e}"))?;
        let dir = exe.parent().map(PathBuf::from).unwrap_or_default();
        // the app's guest tools beside this program (the desktop's resources/), else a Mac bundle's ../Resources
        let guest_tools = path("--guest-tools").unwrap_or_else(|| {
            let beside = dir.join("guest-tools");
            if beside.is_dir() { beside } else { dir.join("../Resources/guest-tools") }
        });
        let helper_arg = path("--helper");
        // the sibling entry from its file, else (with --catalog) the catalog's entry the recipe's keybag_ramdisk_from names
        let from = entry.recipe.as_ref().and_then(|r| r.keybag_ramdisk_from.clone());
        let sibling = match (path("--sibling-entry"), path("--sibling-ipsw"), path("--catalog"), from) {
            (Some(e), Some(i), _, _) => Some((Entry::load(&e)?, i)),
            (None, Some(i), Some(c), Some(from)) => Some((Entry::load_from_catalog(&c, &from)?, i)),
            _ => None,
        };
        std::fs::create_dir_all(&out)?;
        Ok(Options {
            entry,
            ipsw: ipsw.clone(),
            out: out.clone(),
            seed: option(args, "--seed"),
            helper: Helper::new(helper_arg.as_deref())?,
            helper_path: helper_arg.unwrap_or(exe),
            cache: path("--cache"),
            guest_tools,
            sibling,
            stop_after_volumes: option(args, "--stop-after").as_deref() == Some("volumes"),
            built_listing: flag("--built-listing"),
            language: option(args, "--language"),
            keyboards: option(args, "--keyboards").map(|k| k.split(',').map(str::to_string).collect()),
            bootrom: path("--bootrom"),
        })
    };
    let options = match setup() {
        Ok(o) => o,
        Err(e) => return fail(e),
    };
    // a panic is an error event too: the app always hears how create ended
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| create::create(options, emit.clone())));
    match run {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(e)) => fail(e),
        Err(panic) => {
            let why = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "a preparation step failed".into());
            fail(lighttouch_firmware::internal!("{why}"))
        }
    }
}

/// The data-volume seeds (language and keyboards, Setup finished, 6.x assets) applied to an image, as create seeds
/// /private/var; with --jailbreak, afc2 and Cydia from the bootstrap in DIR (downloaded there when missing) first.
fn seed_data(args: &[String]) -> Result<(), String> {
    use lighttouch_firmware::{assets, cydia, keyboards::KeyboardCatalog, plistedit, setup};
    let e = |e: lighttouch_firmware::Error| e.to_string();
    let image = PathBuf::from(required(args, "--system")?);
    let var_root = option(args, "--var-root").unwrap_or_else(|| "private/var".into());
    let now = match option(args, "--now") {
        Some(n) => n.parse().map_err(|_| "--now: a number")?,
        None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
    };
    let mut ed = Editor::open(&image).map_err(e)?;
    if let Some(dir) = option(args, "--jailbreak") {
        let bootstrap = cydia::bootstrap(&PathBuf::from(dir), &|l| eprintln!("{l}")).map_err(e)?;
        let jb = cydia::install_jailbreak(&mut ed, &bootstrap, false).map_err(e)?;
        eprintln!("{}\n{}", jb.afc2, jb.cydia);
        eprintln!("root {} paths, mobile {}, modes {:?}", jb.root.len(), jb.mobile.len(), jb.modes);
    }
    let version = plistedit::read(&ed, "System/Library/CoreServices/SystemVersion.plist").map_err(e)?.unwrap_or_default();
    let field = |k: &str| version.get(k).and_then(plist::Value::as_string).unwrap_or("").to_string();
    let (product, build) = (field("ProductVersion"), field("ProductBuildVersion"));
    let major: u32 = product.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    let catalog = KeyboardCatalog::read(&ed);
    let formats = setup::region_formats(&ed);
    let keyboards: Option<Vec<String>> = option(args, "--keyboards").map(|k| k.split(',').map(String::from).collect());
    let language = setup::seed_language(
        &mut ed,
        &var_root,
        option(args, "--language").as_deref(),
        keyboards.as_deref(),
        catalog.as_ref(),
        formats.as_ref(),
    )
    .map_err(e)?;
    if let Some(l) = &language {
        eprintln!("language {}, region {}, keyboards {:?}", l.language, l.locale, l.keyboards);
    }
    if args.iter().any(|a| a == "--skip-setup") && major >= 5 {
        let (l, locale) = language.as_ref().map_or(("en", "en_US"), |l| (l.language.as_str(), l.locale.as_str()));
        setup::seed_finished_setup(&mut ed, &var_root, major, l, locale, now).map_err(e)?;
        eprintln!("Setup Assistant: seeded as finished (iOS {major})");
    }
    let region = option(args, "--region").unwrap_or_else(|| "LL".into());
    if let Some(installed) = assets::seed_preinstalled_assets(&mut ed, &var_root, major, &product, &build, &region).map_err(e)? {
        eprintln!("preinstalled assets: {} installed as mobileassetd would at the first boot", installed.len());
    }
    ed.commit().map_err(e)
}

fn kboot(args: &[String]) -> Result<(), String> {
    use lighttouch_firmware::{identity::UnitIdentity, kboot};
    let dec = PathBuf::from(required(args, "--decrypted")?);
    let identity = UnitIdentity::load(&PathBuf::from(required(args, "--identity")?)).map_err(|e| e.to_string())?;
    let boot_args = option(args, "--boot-args").unwrap_or_else(|| kboot::DEFAULT_BOOT_ARGS.into());
    let ramdisk = option(args, "--ramdisk").map(PathBuf::from);
    let out = required(args, "--out")?;
    kboot::write(&dec, &PathBuf::from(&out), &identity, &boot_args, ramdisk.as_deref(), None).map_err(|e| e.to_string())
}

fn k48_iboot(args: &[String]) -> Result<(), String> {
    use lighttouch_firmware::{identity::UnitIdentity, k48iboot, kboot};
    let entry = Entry::load_from_catalog(&PathBuf::from(required(args, "--catalog")?), &required(args, "--id")?)
        .map_err(|e| e.to_string())?;
    let ipsw = Ipsw::open(&PathBuf::from(required(args, "--ipsw")?)).map_err(|e| e.to_string())?;
    let storage = entry.recipe.as_ref().map(|r| r.storage.clone()).unwrap_or_else(|| "16g".into());
    // as the K48 recipe: the default seed unless one is given
    let seed = option(args, "--seed").unwrap_or_else(|| format!("ipad1-{}-default", entry.build));
    let identity = UnitIdentity::synthesize(&seed, &storage, None).map_err(|e| e.to_string())?;
    let boot_args = option(args, "--boot-args").unwrap_or_else(|| kboot::DEFAULT_BOOT_ARGS.into());
    let out = PathBuf::from(required(args, "--out")?);
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let names = k48iboot::write_boot_files(
        &ipsw,
        &entry,
        &PathBuf::from(required(args, "--decrypted")?),
        &identity,
        &boot_args,
        &out,
        &|line| eprintln!("{line}"),
    )
    .map_err(|e| e.to_string())?;
    eprintln!("gid components: {names:?}");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("oneshot") {
        // the engine in this process for one preparation boot; no event lines, its own status
        return ExitCode::from(lighttouch_firmware::oneshot::engine_main(&args[1..]).clamp(0, 255) as u8);
    }
    if args.first().map(String::as_str) == Some("create") {
        return create(&args[1..]);
    }
    let result = match args.first().map(String::as_str) {
        Some("decrypt") => decrypt(&args[1..]),
        Some("extract-rootfs") => extract_rootfs(&args[1..]),
        Some("k48-build") => k48_build(&args[1..]),
        Some("k48-rebuild") => k48_rebuild(&args[1..]),
        Some("k48-check") => k48_check(&args[1..]),
        Some("hfs-list") => hfs_list(&args[1..]),
        Some("hfs-cat") => hfs_cat(&args[1..]),
        Some("hfs-edit") => hfs_edit(&args[1..]),
        Some("hfs-grow") => hfs_grow(&args[1..]),
        Some("hfs-format") => hfs_format(&args[1..]),
        Some("write-n72-nand") => write_n72_nand(&args[1..]),
        Some("pack-nand") => pack_nand(&args[1..]),
        Some("read-n72-nand") => read_n72_nand(&args[1..]),
        Some("read-n45-nand") => read_n45_nand(&args[1..]),
        Some("write-n45-nand") => write_n45_nand(&args[1..]),
        Some("build-n72-nor") => build_n72_nor(&args[1..]),
        Some("build-n45-nor") => build_n45_nor(&args[1..]),
        Some("seed-data") => seed_data(&args[1..]),
        Some("fit-survey") => fit_survey::run(&args[1..]),
        Some("kboot") => kboot(&args[1..]),
        Some("k48-iboot") => k48_iboot(&args[1..]),
        _ => {
            eprintln!("usage: firmwarekit decrypt --catalog FILE --id ID --ipsw FILE --out DIR [--no-rootfs]");
            return ExitCode::from(64);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("firmwarekit: {e}");
            ExitCode::from(1)
        }
    }
}

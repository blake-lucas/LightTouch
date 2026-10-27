//! `firmwarekit create`: every board runs the same steps in the same order; a board contributes only what differs
//! (Board): its boot files, how its volumes are laid out, its NAND writer, its keybag one-shot, whether it needs a
//! seal, and its keys in the lock (Swift's Recipe.create and Preparer.create).
//!
//!   create = verify -> decrypt -> identity + board.boot_files -> board.volumes (the shared bake in system_edits)
//!            -> board.store -> [board.keybag if data protection] -> [board.seal if needs_seal] -> lock
//!
//! Shared here: the IPSW sha1 and Restore.plist checks, the decrypt cache, the seed name
//! ("<board.seed_prefix>-<build>-default"), the step and progress events, packing the store into the NAND
//! container (nand_disk: the work store's chip files become nand/nand.qcow2), the store listing hashes
//! (listing_sha256; built_listing_sha256 before any boot, opt-in) and the lock, whose board-specific keys are merged
//! in from `board.lock`. Outputs keep their ordinary permissions: the lock's hashes are what vouch for them.
use crate::activation::ActivationResult;
use crate::entry::{Entry, Recipe};
use crate::error::{Code, Error, Result};
use crate::fitcheck;
use crate::guest_package::Record;
use crate::ipsw::{Ipsw, RestoreInfo};
use crate::oneshot::Helper;
use crate::progress::{ByteCount, Emit, Event, StepProgress, SEAL_STEP};
use crate::{files, lock};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What create was asked for.
pub struct Options {
    pub entry: Entry,
    pub ipsw: PathBuf,
    pub out: PathBuf,
    pub seed: Option<String>,
    pub helper: Helper,
    /// The helper as the lock names it (`--helper`'s path, else this program).
    pub helper_path: PathBuf,
    pub cache: Option<PathBuf>,
    /// The flat guest-helpers directory system_edits reads (+ it_keybag).
    pub guest_tools: PathBuf,
    /// The entry named by recipe.keybag_ramdisk_from and its IPSW: its restore ramdisk boots the keybag one-shot.
    pub sibling: Option<(Entry, PathBuf)>,
    /// Stop after the volumes step and write fit.json instead of a device (`--stop-after volumes`).
    pub stop_after_volumes: bool,
    /// Hash the store as built, before any boot writes into it (`--built-listing`).
    pub built_listing: bool,
    pub language: Option<String>,
    pub keyboards: Option<Vec<String>>,
    /// The iPod touch 2G's SecureROM, for its keybag boot.
    pub bootrom: Option<PathBuf>,
}

/// What the steps share; boards read and fill it.
pub struct Context {
    pub o: Options,
    pub recipe: Recipe,
    pub emit: Emit,
    pub work: PathBuf,
    pub nand: PathBuf,
    /// Filled by the verify, decrypt and identity steps.
    pub sha1: String,
    pub restore: Option<RestoreInfo>,
    pub dec: Option<PathBuf>,
    pub seed: String,
    pub udid: Option<String>,
    /// The Cydia bootstrap (recipe option jailbreak), fetched by the decrypt step.
    pub cydia: Option<PathBuf>,
    /// Filled by `volumes`.
    pub activation: Option<ActivationResult>,
    pub guest_package: Option<Record>,
    pub engine: Option<String>,
    /// Filled by the store and lock steps.
    pub built: String,
    pub nand_hashes: Vec<(String, String)>,
    pub listing: String,
    /// Every fit check the steps ran (the lock's "fit"); an optional piece that does not fit is a warning event.
    pub fit: fitcheck::Log,
    pub warnings: Arc<Mutex<Vec<String>>>,
    /// The time the preparation dates what it writes (Unix seconds).
    pub now: i64,
}

impl Context {
    pub fn e(&self) -> &Entry {
        &self.o.entry
    }
    pub fn file(&self, name: &str) -> PathBuf {
        self.o.out.join(name)
    }
    pub fn ipsw(&self) -> Result<Ipsw> {
        Ipsw::open(&self.o.ipsw)
    }
    /// The decrypted firmware, once the decrypt step has filled it.
    pub fn decrypted(&self) -> Result<&Path> {
        self.dec.as_deref().ok_or_else(|| filled("the decrypted firmware"))
    }
    pub fn dec_file(&self, name: &str) -> Result<PathBuf> {
        Ok(self.decrypted()?.join(name))
    }
    /// stderr; a "warning: " line is also a warning event.
    pub fn log(&self, s: &str) {
        log_line(&self.emit, &self.warnings, s);
    }
    pub fn warn(&self, s: &str) {
        self.log(&format!("warning: {s}"));
    }
}

fn log_line(emit: &Emit, warnings: &Mutex<Vec<String>>, s: &str) {
    if let Some(w) = s.strip_prefix("warning: ") {
        warnings.lock().unwrap_or_else(|e| e.into_inner()).push(w.to_string());
        emit(Event::Warning(w.to_string()));
    }
    eprintln!("{s}");
}

/// A value an earlier step fills in; missing means a board ran its steps out of order.
pub fn filled(what: &str) -> Error {
    crate::internal!("{what} is not set: a recipe step ran out of order")
}

/// What one board adds to create. Methods run in the order above, each inside its step.
pub trait Board {
    /// "armv7" or "armv6": guest helper names and the dyld shared cache path.
    fn arch(&self) -> &str;
    /// The default seed's prefix ("ipad1", "ipod2g"); the lock's identity.seed keeps it.
    fn seed_prefix(&self) -> String;
    fn boot_step(&self) -> String;
    fn volumes_step(&self) -> String;
    fn keybag_step(&self) -> String;
    fn data_protection(&self) -> bool;
    fn needs_seal(&self) -> bool;
    /// Outputs hashed for the lock, besides nand/.
    fn shipped(&self) -> Vec<String>;
    /// Before `begin`: what this recipe needs (helper, storage, guest helpers).
    fn check(&mut self, c: &mut Context) -> Result<()>;
    /// After the IPSW is verified: board facts read from it (the iPod's NAND epoch).
    fn inspect(&mut self, _c: &mut Context) -> Result<()> {
        Ok(())
    }
    /// Writes identity.json for `seed` at `path`; returns the UDID.
    fn identity(&mut self, c: &mut Context, seed: &str, path: &Path) -> Result<Option<String>>;
    fn boot_files(&mut self, c: &mut Context) -> Result<()>;
    fn volumes(&mut self, c: &mut Context) -> Result<()>;
    /// Writes nand/ from the volumes (the volumes are deleted after).
    fn store(&mut self, c: &mut Context) -> Result<()>;
    fn keybag(&mut self, _c: &mut Context) -> Result<()> {
        Ok(())
    }
    fn seal(&mut self, _c: &mut Context) -> Result<()> {
        Ok(())
    }
    /// The board's lock keys, merged (objects recursively) onto the shared ones.
    fn lock(&mut self, c: &Context) -> Result<Value>;
}

/// The board's recipe, by the entry's board and recipe name.
pub fn board_for(o: &Options) -> Result<Box<dyn Board>> {
    let e = &o.entry;
    let recipe = e.recipe.as_ref().map(|r| r.name.as_str()).unwrap_or("none");
    match (e.board.as_str(), recipe) {
        ("k48ap", "k48") | ("n81ap", "n81") | ("n90ap", "n90") | ("n88ap", "n88") | ("n18ap", "n18") => {
            Ok(Box::new(crate::k48_recipe::K48Board::new(o)?))
        }
        ("n45ap", "n45") | ("m68ap", "m68") => Ok(Box::new(crate::n45_recipe::N45Board::new(o)?)),
        ("n72ap", "n72") => Ok(Box::new(crate::n72_recipe::N72Board::new(o)?)),
        _ => Err(crate::unsupported!("{}: no preparer for board {} recipe {recipe}", e.id, e.board)),
    }
}

/// The entry's bytes for the lock's entry.sha256: sorted keys, compact, as Swift's JSONEncoder wrote them.
pub fn entry_json(e: &Entry) -> Result<(String, Value)> {
    let value = serde_json::to_value(e).map_err(|err| crate::internal!("the entry as JSON: {err}"))?;
    Ok((lock::swift_compact(&value), lock::sorted(&value)))
}

/// Runs create with `o`, emitting events through `emit`.
pub fn create(o: Options, emit: Emit) -> Result<()> {
    let mut board = board_for(&o)?;
    run(o, board.as_mut(), emit)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The work directory goes once the device is made; LT_KEEP_WORK=1 keeps it (the volumes as built, for comparisons).
fn remove_work(work: &Path) {
    if std::env::var_os("LT_KEEP_WORK").is_none_or(|v| v != "1") {
        files::remove_tree(work);
    }
}

fn iso8601(unix: i64) -> String {
    // civil from days (Howard Hinnant), UTC
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

pub fn run(o: Options, board: &mut dyn Board, emit: Emit) -> Result<()> {
    let e = o.entry.clone();
    let recipe = e.recipe.clone().ok_or_else(|| crate::unsupported!("{}: no recipe", e.id))?;
    let sha1 = e.source.sha1.clone().ok_or_else(|| crate::unsupported!("{} pins no IPSW sha1", e.id))?;
    let empty = std::fs::read_dir(&o.out).map(|mut d| d.next().is_none()).unwrap_or(false);
    if !empty {
        return Err(crate::internal!("{} is not an empty directory", o.out.display()));
    }
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let fit = {
        let (emit, warnings) = (emit.clone(), warnings.clone());
        fitcheck::Log::new(move |s| log_line(&emit, &warnings, &format!("warning: {s}")))
    };
    let mut c = Context {
        work: o.out.join("work"),
        nand: o.out.join("nand"),
        o,
        recipe: recipe.clone(),
        emit: emit.clone(),
        sha1: String::new(),
        restore: None,
        dec: None,
        seed: String::new(),
        udid: None,
        cydia: None,
        activation: None,
        guest_package: None,
        engine: None,
        built: String::new(),
        nand_hashes: Vec::new(),
        listing: String::new(),
        fit,
        warnings,
        now: unix_now(),
    };
    board.check(&mut c)?;
    if e.estimates.peak_bytes > 0 {
        files::require_space(e.estimates.peak_bytes - e.source.bytes.unwrap_or(0), &c.o.out)?;
    }
    let mut steps = vec!["Verifying the IPSW".to_string(), "Decrypting the firmware".to_string(), board.boot_step(), board.volumes_step(), "Writing the NAND".to_string()];
    if board.data_protection() {
        steps.push(board.keybag_step());
    }
    if board.needs_seal() {
        steps.push(SEAL_STEP.to_string());
    }
    steps.push("Writing the lock".to_string());
    let major = e.major().max(0) as u32;
    emit(Event::Begin { steps: steps.len(), seconds: steps.iter().map(|s| crate::progress::StepPlan::plan(s, major).seconds).collect() });
    let progress = StepProgress::new(Some(c.work.clone()), major, emit.clone());
    let result = (|| -> Result<()> {
        let mut index = 0;
        let mut step = |c: &Context| {
            index += 1;
            progress.next(index, &steps[index - 1]);
            c.log(&format!("[{index}/{}] {}", steps.len(), steps[index - 1]));
        };

        step(&c); // verify
        let ipsw_path = files::resolved(&c.o.ipsw);
        let bytes = ByteCount::new(std::fs::metadata(&ipsw_path).map(|m| m.len()).unwrap_or(0));
        {
            let b = bytes.clone();
            progress.measure(Some(Box::new(move || b.fraction())));
        }
        c.sha1 = files::sha1_file(&c.o.ipsw, &|n| bytes.add(n))?;
        if c.sha1 != sha1.to_lowercase() {
            let name = c.o.ipsw.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            return Err(Error { code: Code::ShaMismatch, message: format!("{name}: sha1 {}, {} pins {sha1}", c.sha1, e.id), piece: None });
        }
        let restore = RestoreInfo::read(&c.ipsw()?)?;
        restore.verify(&e)?;
        c.restore = Some(restore);
        board.inspect(&mut c)?;

        step(&c); // decrypt, once per IPSW
        std::fs::create_dir_all(&c.work)?;
        let cache_root = c.o.cache.clone().unwrap_or_else(|| c.work.join("cache"));
        let _lease = files::consume_cache(&cache_root)?;
        let dec = {
            let ipsw = c.ipsw()?;
            crate::cache::resolve(
                &cache_root,
                &crate::cache::Identity::new(&sha1.to_lowercase(), &e),
                |tmp| Ok(crate::decrypt::decrypt(&ipsw, &e, tmp, true, &|l| c.log(l))?.files),
                || c.log(&format!("reusing verified decrypt-cache manifest for {}", e.id)),
            )?
        };
        c.dec = Some(dec);
        if recipe.option("jailbreak") == Some(true) {
            // Cydia's bootstrap beside the decrypt cache, as the app's caches keep IPSW and Decrypted
            let dir = cache_root.parent().unwrap_or(&cache_root).join("Jailbreak");
            c.cydia = Some(crate::cydia::bootstrap(&dir, &|l| c.log(l))?);
        }

        step(&c); // identity.json + the board's boot files
        c.seed = c.o.seed.clone().unwrap_or_else(|| format!("{}-{}-default", board.seed_prefix(), e.build));
        let seed = c.seed.clone();
        let identity = c.file("identity.json");
        c.udid = board.identity(&mut c, &seed, &identity)?;
        board.boot_files(&mut c)?;

        step(&c); // volumes (+ the shared bake, activation, guest package)
        board.volumes(&mut c)?;
        if c.o.stop_after_volumes {
            let survey = json!({
                "entry": e.id, "build": e.build, "board": e.board, "fit": c.fit.object(),
                "guest_package": c.guest_package.as_ref().map(Record::object).unwrap_or(Value::Null),
                "warnings": *c.warnings.lock().unwrap_or_else(|e| e.into_inner()),
            });
            remove_work(&c.work);
            std::fs::write(c.file("fit.json"), lock::pretty(&survey))?;
            progress.finish();
            emit(Event::Done { lock: "fit.json".into() });
            return Ok(());
        }

        step(&c); // the store; with --built-listing, its listing before any boot writes into it
        board.store(&mut c)?;
        let boots = board.data_protection() || board.needs_seal();
        if c.o.built_listing && boots {
            c.built = files::nand_listing(&c.nand, &files::tree_files(&c.nand)?, &|_| {})?.1;
            c.log(&format!("store as built: listing sha256 {}", c.built));
        }
        if board.data_protection() {
            step(&c); // 4.x data protection: effaceable + system keybag from the IPSW's own ramdisk
            board.keybag(&mut c)?;
        }
        if board.needs_seal() {
            step(&c); // one clean halt (the FTL context)
            board.seal(&mut c)?;
        }

        step(&c); // the store packed into its container, lock
        // Its data is read once to pack and the container (about as large) once to hash.
        let data = crate::nand_disk::data_bytes(&c.nand)?;
        let done = ByteCount::new(2 * data);
        {
            let d = done.clone();
            progress.measure(Some(Box::new(move || d.fraction())));
        }
        crate::nand_disk::pack(&c.nand, &|n| done.add(n))?;
        let nand_files = files::tree_files(&c.nand)?;
        let (hashes, listing) = files::nand_listing(&c.nand, &nand_files, &|n| done.add(n))?;
        c.nand_hashes = hashes;
        c.listing = listing;
        let mut tools: Vec<String> = std::fs::read_dir(&c.o.guest_tools)?
            .filter_map(|d| d.ok())
            .filter(|d| d.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .collect();
        tools.sort();
        let mut built_tools = serde_json::Map::new();
        for t in &tools {
            built_tools.insert(t.clone(), Value::String(files::sha256_file(&c.o.guest_tools.join(t))?));
        }
        let (entry_bytes, entry_content) = entry_json(&e)?;
        let restore = c.restore.as_ref().ok_or_else(|| filled("Restore.plist"))?;
        let opt = |v: Option<Value>| v.unwrap_or(Value::Null);
        let shared = json!({
            "format": 1, "created": iso8601(c.now), "preparation": lock::PREPARATION_SERIAL,
            "entry": {"id": e.id, "sha256": files::sha256(entry_bytes.as_bytes()), "content": entry_content},
            "build": e.build, "product_version": restore.product_version, "product_type": e.product_type,
            "board": e.board,
            "storage": recipe.storage,
            "tool": {
                "name": "firmwarekit", "version": crate::VERSION,
                "helper": c.o.helper_path.to_string_lossy(),
                "built": {"guest tools": Value::Object(built_tools)},
            },
            "inputs": {
                "ipsw": {"path": c.o.ipsw.to_string_lossy(), "sha1": c.sha1},
                "decrypted": c.decrypted()?.to_string_lossy(), "identity": "identity.json",
                "activation": opt(c.activation.as_ref().map(ActivationResult::record)),
                "rootfs": "rootfs.dmg", "guest_tools": c.o.guest_tools.to_string_lossy(), "lockdown": Value::Null,
                "language": opt(c.o.language.clone().map(Value::String)),
                "keyboards": opt(c.o.keyboards.clone().map(|k| json!(k))),
                "sibling": opt(c.o.sibling.as_ref().map(|(s, p)| json!({"entry": s.id, "ipsw": p.to_string_lossy()}))),
            },
            "identity": {
                "seed": c.seed, "udid": c.udid.clone().unwrap_or_default(),
                "sha256": files::sha256_file(&c.file("identity.json"))?,
            },
            "outputs": {"nand": {
                "path": "nand", "listing_sha256": c.listing,
                "built_listing_sha256": if boots { if c.built.is_empty() { Value::Null } else { json!(c.built) } } else { json!(c.listing) },
            }},
            "guest_package": opt(c.guest_package.as_ref().map(Record::object)),
            "fit": c.fit.object(),
        });
        let lock_value = lock::merged(shared, board.lock(&c)?);
        remove_work(&c.work);
        std::fs::write(c.file(lock::FILE_NAME), lock::pretty(&lock_value))?;
        c.log(&format!("{}: UDID {}", c.o.out.display(), c.udid.as_deref().unwrap_or("-")));
        progress.finish();
        emit(Event::Done { lock: lock::FILE_NAME.into() });
        Ok(())
    })();
    progress.stop();
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn iso_dates() {
        assert_eq!(super::iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::iso8601(1_792_899_927), "2026-10-25T03:45:27Z");
    }
}

#[cfg(test)]
mod oracle {
    /// The lock's entry.sha256 as the Swift tool computed it for n81ap-10B500
    /// (~/Documents/LightTouch-Development/desktop/fwkit-oracle/n81-a/device.lock.json).
    #[test]
    #[ignore]
    fn entry_sha256_matches_swift() {
        let home = std::env::var("HOME").unwrap();
        let catalog = std::path::PathBuf::from(&home).join("Documents/LightTouch-Development/desktop/data/cache/firmware-catalog.json");
        let mut e = crate::entry::Entry::load_from_catalog(&catalog, "n81ap-10B500").unwrap();
        e.recipe.as_mut().unwrap().options.insert("skip_setup".into(), true); // create --skip-setup
        let (bytes, content) = super::entry_json(&e).unwrap();
        let lock: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::path::PathBuf::from(&home).join("Documents/LightTouch-Development/desktop/fwkit-oracle/n81-a/device.lock.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(content, lock["entry"]["content"]);
        assert_eq!(crate::files::sha256(bytes.as_bytes()), lock["entry"]["sha256"].as_str().unwrap());
    }
}

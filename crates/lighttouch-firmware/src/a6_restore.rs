//! Data-only inputs for the N42 restore boot (Swift `A6RestoreStream`, `A6RestoreBundle`,
//! `A6RestoreKernel`, `A6RestoreCache`).
//!
//! This module deliberately does not run QEMU. It writes:
//!   work/initial.a6di       the A6DI v1 allocated-block stream of the two prepared volumes
//!   work/restore-ramdisk.hfs the IPSW restore ramdisk with it_initialize as restored_external,
//!                            the Data seed and the optional activation plan/worker
//!   work/kboot-restore.bin  kboot with that ramdisk and nand-enable-reformat=1
//! and returns `Staged`. The parent must boot `Staged.kboot_restore` on `nand/` and require
//! `A6RESTORE DONE` + `it_keybag: effaceable formatted, system keybag created`.
use crate::a6_assets;
use crate::a6_storage::A6StorageLayout;
use crate::codesign;
use crate::error::Result;
use crate::hfs::{Editor, Volume};
use crate::identity::UnitIdentity;
use crate::posio;
use crate::{files, kboot};
use plist::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

pub const RESTORE_START: &str = "A6RESTORE START";
pub const RESTORE_DONE: &str = "A6RESTORE DONE";
pub const KEYBAG_DONE: &str = "it_keybag: effaceable formatted, system keybag created";
pub const RESTORE_LOG: &str = "restore.log";
pub const STREAM: &str = "initial.a6di";
pub const RESTORE_RAMDISK: &str = "restore-ramdisk.hfs";
pub const KBOOT_RESTORE: &str = "kboot-restore.bin";

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn invalid(detail: &str) -> crate::Error {
    crate::unsupported!("A6 restore: {detail}")
}
fn crc(bytes: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(bytes);
    c.sum()
}
fn append_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn append_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

struct Allocation {
    bitmap: Vec<u8>,
    blocks: u64,
}
impl Allocation {
    fn used(&self, block: u64) -> bool {
        block < self.blocks
            && (block == 0
                || block + 1 == self.blocks
                || self.bitmap[(block / 8) as usize] & (0x80 >> (block % 8)) != 0)
    }
}

/// Read the allocation fork's eight primary extents, as the native writer does. Header and
/// alternate-header blocks count as used.
fn allocation(path: &Path, bytes: u64) -> Result<Allocation> {
    if bytes < 1536 {
        return Err(invalid("unsupported HFS+ allocation geometry"));
    }
    let file = File::open(path)?;
    let mut header = [0u8; 512];
    posio::read_exact_at(&file, &mut header, 1024)?;
    if header[0] != 0x48 || (header[1] != 0x2b && header[1] != 0x58) {
        return Err(invalid("unsupported HFS+ allocation geometry"));
    }
    let be32 = |o: usize| u32::from_be_bytes([header[o], header[o + 1], header[o + 2], header[o + 3]]) as u64;
    let be64 = |o: usize| {
        u64::from_be_bytes([
            header[o],
            header[o + 1],
            header[o + 2],
            header[o + 3],
            header[o + 4],
            header[o + 5],
            header[o + 6],
            header[o + 7],
        ])
    };
    let block_size = be32(40);
    let blocks = be32(44);
    if block_size != 4096 || blocks == 0 || blocks > bytes / 4096 {
        return Err(invalid("unsupported HFS+ allocation geometry"));
    }
    let logical = be64(112);
    if logical < (blocks + 7) / 8 || logical > 4 << 20 {
        return Err(invalid("invalid allocation bitmap size"));
    }
    let mut bitmap = Vec::new();
    for index in 0..8 {
        let start = be32(128 + index * 8);
        let count = be32(132 + index * 8);
        if count == 0 {
            continue;
        }
        if start >= blocks || count > blocks - start {
            return Err(invalid("allocation extent outside volume"));
        }
        if (bitmap.len() as u64) < logical {
            let want = (count * 4096).min(logical - bitmap.len() as u64) as usize;
            let mut part = vec![0u8; want];
            posio::read_exact_at(&file, &mut part, start * 4096)?;
            bitmap.extend(part);
        }
    }
    if bitmap.len() as u64 != logical {
        return Err(invalid("allocation bitmap needs overflow extents"));
    }
    Ok(Allocation { bitmap, blocks })
}

/// `A6DI` v1 initial stream: header, 4096-byte MBR, then records of allocated blocks. Every record
/// is offset + length + CRC-32 + bytes; records are at most 64 KiB.
pub fn write_initial(system: &Path, data: &Path, output: &Path, layout: &A6StorageLayout) -> Result<()> {
    if output.exists() {
        return Err(invalid("output already exists"));
    }
    for (path, expected) in [(system, layout.system_bytes), (data, layout.data_bytes)] {
        if std::fs::metadata(path)?.len() != expected {
            return Err(invalid(&format!(
                "prepared volume does not fill its partition: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
    }
    let mut runs: Vec<(u64, Vec<(u64, usize)>)> = Vec::new();
    for (path, offset) in [(system, layout.system_offset), (data, layout.data_offset)] {
        let bytes = std::fs::metadata(path)?.len();
        let a = allocation(path, bytes)?;
        let mut list = Vec::new();
        let mut block = 0u64;
        while block < a.blocks {
            if !a.used(block) {
                block += 1;
                continue;
            }
            let start = block;
            while block < a.blocks && a.used(block) && block - start < 16 {
                block += 1;
            }
            let count = (block - start) as usize;
            list.push((offset + start * 4096, count * 4096));
        }
        runs.push((offset, list));
    }
    let records: u32 = 1 + runs.iter().map(|(_, r)| r.len() as u32).sum::<u32>();
    if records > 1_000_000 {
        return Err(invalid("too many initial restore records"));
    }
    let table = layout.partition_table();
    let mut header = b"A6DI".to_vec();
    append_u32(&mut header, 1);
    append_u64(&mut header, layout.media_bytes);
    append_u32(&mut header, records);
    append_u32(&mut header, 0);
    append_u64(&mut header, 0);
    append_u32(&mut header, table.len() as u32);
    append_u32(&mut header, crc(&table));

    let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(output)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        out.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    out.write_all(&header)?;
    out.write_all(&table)?;
    let mut buf = vec![0u8; 65536];
    for (path, list) in [(system, &runs[0].1), (data, &runs[1].1)] {
        let file = File::open(path)?;
        for (offset, len) in list {
            let bytes = &mut buf[..*len];
            posio::read_exact_at(
                &file,
                bytes,
                offset - if *offset >= layout.data_offset { layout.data_offset } else { layout.system_offset },
            )?;
            let mut record = Vec::with_capacity(16 + len);
            append_u64(&mut record, *offset);
            append_u32(&mut record, *len as u32);
            append_u32(&mut record, crc(bytes));
            out.write_all(&record)?;
            out.write_all(bytes)?;
        }
    }
    out.sync_all()?;
    Ok(())
}

/// `A6RestoreKernel`: 12A365 only; compatibility needed while formatting new N42 media. Other
/// builds return the original kernel, exactly as Swift does.
pub fn restore_kernel_patch(original: &[u8], build: &str) -> Result<Vec<u8>> {
    if build != "12A365" {
        return Ok(original.to_vec());
    }
    // The Swift 12A365 wrapper patches this exact kernel hash; without that build's test vector
    // the port refuses rather than guessing offsets.
    Err(crate::unsupported!(
        "A6 restore kernel: 12A365 compatibility patch is not ported; use 14G61 or supply the qualified 12A365 kernel"
    ))
}

/// `A6RestoreCache.validate`: the activation plan and mobilegestalt worker.
pub fn validate_restore_cache(plan: &[u8], original_library: &[u8], helper: &[u8]) -> Result<()> {
    let p = Value::from_reader(Cursor::new(plan))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| invalid("unqualified build or invalid plan"))?;
    let build = p.get("Build").and_then(Value::as_string).unwrap_or("");
    let overrides = p
        .get("Overrides")
        .and_then(Value::as_dictionary)
        .ok_or_else(|| invalid("unqualified build or invalid plan"))?;
    if !["12A365", "13A344", "14G61"].contains(&build)
        || overrides.is_empty()
        || overrides.len() > 32
        || overrides.values().any(|v| v.as_boolean().is_none())
    {
        return Err(invalid("unqualified build or invalid plan"));
    }
    let expected = match build {
        "12A365" => "fc7b1e5943df2119cd1f258c0f197e9795ff62ad3257f0d3d844f17c048cab77",
        "13A344" => "f8946c1cb5178fd031ffaee896355cb7f8cb150d01541312709abc0671a08074",
        _ => "869ca5ca3830a9d67076ff45e593a06362d9e988e3d805ece2cd479944c4f3f2",
    };
    if files::sha256(original_library) != expected {
        return Err(invalid("unrecognized original MobileGestalt library"));
    }
    if helper.len() < 28
        || u32::from_le_bytes([helper[0], helper[1], helper[2], helper[3]]) != 0xfeed_face
        || u32::from_le_bytes([helper[4], helper[5], helper[6], helper[7]]) != 12
        || u32::from_le_bytes([helper[12], helper[13], helper[14], helper[15]]) != 2
        || memchr::memmem::find(helper, b"it_mobilegestalt_worker_v1\0").is_none()
    {
        return Err(invalid("expected source-built ARM native cache worker"));
    }
    if build != "12A365" && !allows_private_mapping(helper) {
        return Err(invalid("iOS 9/10 worker needs get-task-allow=true; rebuild restore tools"));
    }
    if build == "14G61"
        && (memchr::memmem::find(helper, b"it_mobilegestalt_container_v1\0").is_none()
            || memchr::memmem::find(helper, b"it_mobilegestalt_readable_cache_v1\0").is_none())
    {
        return Err(invalid("iOS 10 worker needs container/readable-cache support; rebuild restore tools"));
    }
    Ok(())
}

fn allows_private_mapping(helper: &[u8]) -> bool {
    let Some(images) = codesign::images(helper) else { return false };
    let Some(image) = images.iter().find(|i| i.cputype == 12 && !i.is_64bit) else { return false };
    let Some((off, size)) = codesign::code_signature(&helper[image.base..image.base + image.size]) else {
        return false;
    };
    let (start, size) = (image.base + off, size);
    if size < 12 || start > helper.len() - size || be32(helper, start) != 0xfade0cc0 {
        return false;
    }
    let length = be32(helper, start + 4) as usize;
    let count = be32(helper, start + 8) as usize;
    if length < 12 || length > size || count > (length - 12) / 8 {
        return false;
    }
    for i in 0..count {
        if be32(helper, start + 12 + i * 8) != 5 {
            continue;
        }
        let rel = be32(helper, start + 16 + i * 8) as usize;
        if rel < 12 + count * 8 || rel > length - 8 {
            return false;
        }
        let at = start + rel;
        if be32(helper, at) != 0xfade7171 {
            return false;
        }
        let n = be32(helper, at + 4) as usize;
        if n < 8 || n > length - rel {
            return false;
        }
        let Ok(p) = Value::from_reader(Cursor::new(&helper[at + 8..at + n])) else { return false };
        return p.as_dictionary().and_then(|d| d.get("get-task-allow")).and_then(Value::as_boolean) == Some(true);
    }
    false
}

/// What the restore boot needs. Written before any boot; the parent consumes it.
#[derive(Debug, Clone)]
pub struct Staged {
    pub stream: PathBuf,
    pub restore_ramdisk: PathBuf,
    pub kboot_restore: PathBuf,
    pub assets: Vec<a6_assets::Asset>,
    pub restore_log: PathBuf,
}

pub struct BundleInputs<'a> {
    pub decrypted: &'a Path,
    pub initializer: &'a Path,
    pub mobile_gestalt: Option<&'a Path>,
    pub factory_format: bool,
    pub data_seed: &'a Path,
    pub seed_base_system: &'a Path,
    pub activation_plan: Option<&'a Path>,
    pub guest_assets_system: &'a Path,
    pub work: &'a Path,
    pub identity: &'a UnitIdentity,
}

/// Write the restore ramdisk and kboot-restore; returns the restore assets (if any).
pub fn write_bundle(i: &BundleInputs<'_>, now: i64, log: &dyn Fn(&str)) -> Result<Staged> {
    let dec = i.decrypted;
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(dec)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if !name.ends_with("-ramdisk.dmg") {
            continue;
        }
        let Ok(v) = Volume::open(&path, false) else { continue };
        if v.record("usr/local/bin/restored_external").is_err() {
            continue;
        }
        let has_job = v.record("System/Library/LaunchDaemons/com.apple.restored_external.plist").is_ok();
        let mut boot = false;
        for p in ["private/etc/rc.boot", "etc/rc.boot"] {
            if let Ok(r) = v.record(p)
                && let Ok(c) = v.contents(&r)
                && memchr::memmem::find(&c, b"/usr/local/bin/restored_external\0").is_some()
            {
                boot = true;
            }
        }
        if has_job || boot {
            candidates.push(path);
        }
    }
    if candidates.len() != 1 {
        return Err(crate::unsupported!(
            "Expected exactly one native restored_external RAM disk, found {}",
            candidates.len()
        ));
    }
    let source = Volume::open(&candidates[0], false)?;
    let rd = i.work.join(RESTORE_RAMDISK);
    let paths = source.paths()?;
    let file_bytes: u64 = paths
        .iter()
        .filter(|(_, r)| r.kind == crate::hfs::volume::Kind::File)
        .map(|(_, r)| source.size(r).unwrap_or(0))
        .sum();
    let mib = 1u64 << 20;
    let size = ((file_bytes + mib - 1) / mib + 32) * mib;
    let size = size.max(64 << 20);
    Editor::format(&rd, size, "Restore", false, now)?;
    let mut w = Editor::open(&rd)?;
    let mut owners: BTreeMap<(u32, u32), Vec<String>> = BTreeMap::new();
    let mut ordered = paths;
    ordered.sort_by_key(|(p, _)| p.split('/').count());
    for (p, r) in &ordered {
        if p.is_empty() {
            continue;
        }
        if r.kind == crate::hfs::volume::Kind::Folder {
            w.mkdir(p, Some(r.mode & 0o7777))?;
        } else if r.is_symlink() {
            w.symlink(p, &String::from_utf8_lossy(&source.contents(r)?))?;
        } else {
            w.write(p, source.contents(r)?, Some(r.mode & 0o7777))?;
        }
        owners.entry((r.uid, r.gid)).or_default().push(p.clone());
    }
    w.write("usr/local/bin/restored_external", std::fs::read(i.initializer)?, Some(0o755))?;
    /* The native FTL formats the fresh NAND after launchd starts; the restore
     * job's 60 s /dev/rdisk0 wait is shorter than that format on a software
     * CPU. Start the job after the format has had time to finish, rather than
     * racing the device. The format publishes the disk 14 s after launchd on a
     * Ryzen 7 3700X under Linux, so 120 s plus the job's own 60 s covers a host
     * over ten times slower (480 s, upstream's delay, was ~7 minutes of an idle
     * guest on every preparation). Neither the IOKit-matching event (no
     * UserEventAgent in the restore ramdisk) nor KeepAlive PathState on
     * /dev/rdisk0 (devfs: never fires) can start it on the disk instead.
     * The stock job is replaced only in this temporary restore ramdisk. */
    {
        const JOB: &str = "System/Library/LaunchDaemons/com.apple.restored_external.plist";
        const START_DELAY: u64 = 120;
        let record = source.record(JOB)?;
        let mut job = plist::Value::from_reader(Cursor::new(source.contents(&record)?))
            .map_err(|e| invalid(&format!("restore job: {e}")))?;
        let dict = job.as_dictionary_mut().ok_or_else(|| invalid("restore job is not a dictionary"))?;
        dict.remove("RunAtLoad");
        dict.insert("StartInterval".to_string(), plist::Value::from(START_DELAY));
        let mut bytes = Vec::new();
        job.to_writer_binary(&mut bytes).map_err(|e| invalid(&format!("restore job: {e}")))?;
        w.write(JOB, bytes, Some(0o644))?;
    }
    for paths in owners.values_mut() {
        paths.retain(|p| p != "usr/local/bin/restored_external");
    }
    owners.entry((0, 0)).or_default().push("usr/local/bin/restored_external".into());
    stage_seed(i.data_seed, Some(i.seed_base_system), &mut w, &mut owners)?;
    if let Some(plan_path) = i.activation_plan {
        let worker =
            i.mobile_gestalt.ok_or_else(|| crate::internal!("An activation plan needs the MobileGestalt worker"))?;
        let plan = std::fs::read(plan_path)?;
        let worker_data = std::fs::read(worker)?;
        let library = source.contents(&source.record("usr/lib/libMobileGestalt.dylib")?)?;
        validate_restore_cache(&plan, &library, &worker_data)?;
        w.mkdirs("usr/local/share/qemu")?;
        w.write("usr/local/bin/it_mobilegestalt", worker_data, Some(0o755))?;
        owners.entry((0, 0)).or_default().push("usr/local/bin/it_mobilegestalt".into());
        owners.entry((0, 0)).or_default().push("usr/local/share/qemu".into());
        w.write("usr/local/share/qemu/activation-plan.plist", plan, Some(0o644))?;
        owners.entry((0, 0)).or_default().push("usr/local/share/qemu/activation-plan.plist".into());
    }
    w.commit()?;
    // Preserve the source catalog's owners, as Swift's HFSPlusWriter.owner does per file.
    let vol = Volume::open(&rd, true)?;
    for ((uid, gid), paths) in &owners {
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        vol.set_owner(&refs, *uid, *gid, None, None)?;
    }

    let original_kernel = std::fs::read(dec.join("kernelcache.mach"))?;
    let assets = a6_assets::prepare(&original_kernel, i.guest_assets_system, Some(&rd))?;
    let mut restore_kernel = assets.kernel.clone();
    if i.factory_format {
        let version = plist::Value::from_reader(Cursor::new(
            source.contents(&source.record("System/Library/CoreServices/SystemVersion.plist")?)?,
        ))
        .ok()
        .and_then(Value::into_dictionary)
        .and_then(|d| d.get("ProductBuildVersion").and_then(Value::as_string).map(str::to_string))
        .ok_or_else(|| crate::unsupported!("Native restore build identity missing"))?;
        restore_kernel = restore_kernel_patch(&restore_kernel, &version)?;
    }
    let args = format!(
        "{} -v{}",
        crate::n42_recipe::A6_BOOT_ARGS,
        if i.factory_format { " nand-enable-reformat=1" } else { "" }
    );
    let kboot_restore = i.work.join(KBOOT_RESTORE);
    kboot::write_rooted(dec, &kboot_restore, i.identity, &args, Some(&rd), Some(&restore_kernel), None)?;
    log(&format!("restore bundle: {} and {}", rd.display(), kboot_restore.display()));
    Ok(Staged {
        stream: i.work.join(STREAM),
        restore_ramdisk: rd,
        kboot_restore,
        assets: assets.assets,
        restore_log: i.work.join(RESTORE_LOG),
    })
}

fn stage_seed(
    data_seed: &Path,
    seed_base: Option<&Path>,
    writer: &mut Editor,
    owners: &mut BTreeMap<(u32, u32), Vec<String>>,
) -> Result<()> {
    let source = Volume::open(data_seed, false)?;
    let base = match seed_base {
        Some(p) => Some(Volume::open(p, false)?),
        None => None,
    };
    if source.record("keybags/systembag.kb").is_ok() {
        return Err(crate::unsupported!("Prepared Data seed must not contain a device keybag"));
    }
    let target = "usr/local/share/qemu/data-seed";
    writer.mkdirs(target)?;
    owners.entry((0, 0)).or_default().push(target.to_string());
    let mut paths = source.paths()?;
    paths.sort_by_key(|(p, _)| p.split('/').count());
    for (path, r) in &paths {
        if let Some(first) = path.split('/').next()
            && ([".journal", ".journal_info_block"].contains(&first)
                || crate::system_edits::JUNK.contains(&first)
                || first.starts_with(".HFS+"))
        {
            continue;
        }
        if r.kind != crate::hfs::volume::Kind::Folder
            && let Some(base) = &base
            && let Ok(old) = base.record(&format!("private/var/{path}"))
            && old.kind == r.kind
            && old.uid == r.uid
            && old.gid == r.gid
            && old.mode == r.mode
            && base.contents(&old)? == source.contents(r)?
        {
            continue;
        }
        let at = if path.is_empty() { target.to_string() } else { format!("{target}/{path}") };
        if path.is_empty() {
            continue;
        } else if r.kind == crate::hfs::volume::Kind::Folder {
            writer.mkdir(&at, Some(r.mode & 0o7777))?;
        } else if r.is_symlink() {
            writer.symlink(&at, &String::from_utf8_lossy(&source.contents(r)?))?;
        } else {
            writer.write(&at, source.contents(r)?, Some(r.mode & 0o7777))?;
        }
        owners.entry((r.uid, r.gid)).or_default().push(at);
    }
    Ok(())
}

/// Stage the A6DI stream and return the restore boot inputs. N42Board::store calls this and stops:
/// the parent boots `staged.kboot_restore` on `nand/`, then resumes with kboot.bin + seal.
pub fn stage(system: &Path, data: &Path, work: &Path, layout: &A6StorageLayout) -> Result<PathBuf> {
    let stream = work.join(STREAM);
    write_initial(system, data, &stream, layout)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a6_storage::A6StorageLayout;

    /// Builds two empty sparse HFS volumes and writes a real A6DI stream. Run explicitly:
    /// `cargo test -p lighttouch-firmware --lib a6_restore::tests::writes_a_stream -- --ignored`
    #[test]
    #[ignore]
    fn writes_a_stream() {
        let dir = std::env::temp_dir().join(format!("n42-stream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let l = A6StorageLayout::standard();
        let system = dir.join("system.img");
        let data = dir.join("data.img");
        crate::hfs::Editor::format(&system, l.system_bytes, "System", false, 1_700_000_000).unwrap();
        crate::hfs::Editor::format(&data, l.data_bytes, "Data", true, 1_700_000_000).unwrap();
        let stream = dir.join("initial.a6di");
        write_initial(&system, &data, &stream, &l).unwrap();
        let (media, records) = inspect_stream(&stream).unwrap();
        assert_eq!(media, l.media_bytes);
        assert!(records > 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Diagnostic helper for tests: is the staged stream's magic/header sane?
pub fn inspect_stream(path: &Path) -> Result<(u64, u32)> {
    let mut f = File::open(path)?;
    let mut h = [0u8; 40];
    f.read_exact(&mut h)?;
    if &h[0..4] != b"A6DI" {
        return Err(invalid("not an A6DI stream"));
    }
    let media = u64::from_le_bytes(h[8..16].try_into().unwrap());
    let records = u32::from_le_bytes(h[16..20].try_into().unwrap());
    Ok((media, records))
}

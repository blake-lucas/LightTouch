//! IPSW member access (zip64 included), Restore.plist and the component paths.
//!
//! ```ignore
//! let ipsw = Ipsw::open(path)?;
//! let restore = RestoreInfo::read(&ipsw)?;
//! restore.verify(&entry)?;
//! let comp = build_components(&ipsw, &entry.board)?;   // {component: IPSW path}
//! ipsw.stream(&comp["OS"], |reader| vfdecrypt::decrypt(reader, out, &key))?;
//! ```
use crate::entry::Entry;
use crate::error::{Error, Result};
use plist::{Dictionary, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

pub struct Ipsw {
    path: PathBuf,
}

impl Ipsw {
    pub fn open(path: &Path) -> Result<Ipsw> {
        let ipsw = Ipsw { path: path.to_path_buf() };
        ipsw.archive()?;
        Ok(ipsw)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    fn archive(&self) -> Result<zip::ZipArchive<BufReader<File>>> {
        let file = File::open(&self.path).map_err(|e| crate::unsupported!("{}: {e}", self.path.display()))?;
        zip::ZipArchive::new(BufReader::new(file))
            .map_err(|e| crate::unsupported!("{}: not a zip archive ({e})", self.file_name()))
    }

    /// Every member path.
    pub fn names(&self) -> Result<Vec<String>> {
        Ok(self.archive()?.file_names().map(str::to_string).collect())
    }

    pub fn contains(&self, member: &str) -> Result<bool> {
        Ok(self.archive()?.index_for_name(member).is_some())
    }

    /// A small member, into memory.
    pub fn read(&self, member: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.stream(member, |r| r.read_to_end(&mut out).map(|_| ()).map_err(Error::from))?;
        Ok(out)
    }

    /// A large member, streamed to `file` (created or truncated).
    pub fn extract(&self, member: &str, file: &Path) -> Result<()> {
        let mut out = File::create(file).map_err(|e| crate::internal!("cannot create {}: {e}", file.display()))?;
        self.stream(member, |r| {
            std::io::copy(r, &mut out)?;
            out.flush()?;
            Ok(())
        })
    }

    /// Runs `body` with a reader over `member`'s inflated bytes; `body` should read to the end.
    pub fn stream<T>(&self, member: &str, body: impl FnOnce(&mut dyn Read) -> Result<T>) -> Result<T> {
        let mut archive = self.archive()?;
        let name = self.file_name();
        let mut entry =
            archive.by_name(member).map_err(|_| crate::unsupported!("{name}: no member {member}"))?;
        body(&mut entry).map_err(|e| {
            if e.code == crate::error::Code::Internal && e.message.contains("Invalid checksum") {
                crate::unsupported!("{name}: could not read {member}: {}", e.message)
            } else {
                e
            }
        })
    }
}

/// The Restore.plist fields the preparer reads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RestoreInfo {
    pub product_type: String,
    pub product_build_version: String,
    pub product_version: String,
    pub board_config: String,
    pub platform: String,
    /// The 2.x component paths (3.x+ read them from the BuildManifest).
    pub kernel_cache: Option<String>,
    pub system_image: Option<String>,
    pub restore_ram_disk: Option<String>,
    pub update_ram_disk: Option<String>,
    /// MinimumSystemPartition and this board's SystemPartitionPadding ({capacity GB: MiB}).
    pub minimum_system_mib: Option<i64>,
    pub system_padding_mib: BTreeMap<String, i64>,
}

fn dict<'a>(d: &'a Dictionary, key: &str) -> Option<&'a Dictionary> {
    d.get(key).and_then(Value::as_dictionary)
}

fn string(d: &Dictionary, key: &str) -> Option<String> {
    d.get(key).and_then(Value::as_string).map(str::to_string)
}

fn integer(v: &Value) -> Option<i64> {
    v.as_signed_integer().or_else(|| v.as_unsigned_integer().map(|u| u as i64)).or_else(|| v.as_real().map(|r| r as i64))
}

impl RestoreInfo {
    pub fn read(ipsw: &Ipsw) -> Result<RestoreInfo> {
        Self::parse(&ipsw.read("Restore.plist")?)
    }

    pub fn parse(data: &[u8]) -> Result<RestoreInfo> {
        let missing = || crate::unsupported!("Restore.plist lacks ProductType/ProductBuildVersion/DeviceMap");
        let p = Value::from_reader(std::io::Cursor::new(data)).map_err(|_| missing())?;
        let p = p.as_dictionary().ok_or_else(missing)?;
        let product_type = string(p, "ProductType").ok_or_else(missing)?;
        let product_build_version = string(p, "ProductBuildVersion").ok_or_else(missing)?;
        let product_version = string(p, "ProductVersion").ok_or_else(missing)?;
        let maps: Vec<&Dictionary> =
            p.get("DeviceMap").and_then(Value::as_array).ok_or_else(missing)?.iter().filter_map(Value::as_dictionary).collect();
        // 1.x lists the platform's DFU-only "s5l8900xall" first; the device's own board follows
        let map = maps
            .iter()
            .find(|m| !string(m, "BoardConfig").unwrap_or_default().ends_with("xall"))
            .or(maps.first())
            .ok_or_else(missing)?;
        let board_config = string(map, "BoardConfig").ok_or_else(missing)?;
        let platform = string(map, "Platform").unwrap_or_default();
        let kernel_cache = dict(p, "KernelCachesByPlatform")
            .and_then(|k| dict(k, &platform))
            .and_then(|k| string(k, "Release"))
            .or_else(|| dict(p, "RestoreKernelCaches").and_then(|k| string(k, "Release")));
        let minimum_system_mib = match p.get("MinimumSystemPartition") {
            Some(Value::Dictionary(d)) => d.values().next().and_then(integer),
            Some(v) => integer(v),
            None => None,
        };
        let board_prefix = &board_config[..board_config.len().saturating_sub(2)];
        let system_padding_mib = dict(p, "SystemPartitionPadding")
            .and_then(|d| dict(d, board_prefix))
            .map(|d| d.iter().filter_map(|(k, v)| integer(v).map(|n| (k.clone(), n))).collect())
            .unwrap_or_default();
        Ok(RestoreInfo {
            kernel_cache,
            system_image: dict(p, "SystemRestoreImages").and_then(|d| string(d, "User")),
            restore_ram_disk: dict(p, "RestoreRamDisks").and_then(|d| string(d, "User")),
            update_ram_disk: dict(p, "RestoreRamDisks").and_then(|d| string(d, "Update")),
            product_type,
            product_build_version,
            product_version,
            board_config,
            platform,
            minimum_system_mib,
            system_padding_mib,
        })
    }

    /// The system partition a restore gives a `storage` ("16g") unit, in MiB: minimum plus padding.
    pub fn system_partition_mib(&self, storage: &str) -> Option<i64> {
        let gb = &storage[..storage.len().saturating_sub(1)];
        self.minimum_system_mib.map(|m| m + self.system_padding_mib.get(gb).copied().unwrap_or(0))
    }

    /// (ProductType, ProductBuildVersion, BoardConfig) must be the entry's. The 1.1.2 beta gives ProductType as the
    /// board name ("N45AP"), which stands for the entry's product type.
    pub fn verify(&self, entry: &Entry) -> Result<()> {
        let product = if self.product_type.eq_ignore_ascii_case(&entry.board) {
            entry.product_type.clone()
        } else {
            self.product_type.clone()
        };
        let found = [&self.product_type, &self.product_build_version, &self.board_config];
        let want = [&entry.product_type, &entry.build, &entry.board];
        if [&product, &self.product_build_version, &self.board_config] != want {
            return Err(crate::unsupported!(
                "IPSW is {}, {} wants {}",
                found.map(String::as_str).join(" "),
                entry.id,
                want.map(String::as_str).join(" ")
            ));
        }
        Ok(())
    }
}

/// {component: IPSW path}. With a BuildManifest: `board`'s first (Erase) identity, plus its Update identity's
/// RestoreRamDisk as "UpdateRamDisk". Without (1.x/2.x): derived from Restore.plist.
pub fn build_components(ipsw: &Ipsw, board: &str) -> Result<BTreeMap<String, String>> {
    let names: BTreeSet<String> = ipsw.names()?.into_iter().collect();
    if names.contains("BuildManifest.plist") {
        return components_from_build_manifest(&ipsw.read("BuildManifest.plist")?, board);
    }
    let r = RestoreInfo::read(ipsw)?;
    let all_flash = format!("Firmware/all_flash/all_flash.{}.production/", r.board_config);
    let img2 = names.contains(&format!("{all_flash}iBoot.{}.RELEASE.img2", r.board_config));
    let comp = components_from_restore(&r, img2)?;
    let mut missing: Vec<&String> = comp.values().filter(|p| !names.contains(*p)).collect();
    missing.sort();
    if !missing.is_empty() {
        return Err(crate::unsupported!("no BuildManifest.plist, and Restore.plist-derived paths are missing: {missing:?}"));
    }
    Ok(comp)
}

/// Only `board`'s identities (Info.DeviceClass): the 4.3 betas list the k48dev development board's first.
pub fn components_from_build_manifest(data: &[u8], board: &str) -> Result<BTreeMap<String, String>> {
    let p = Value::from_reader(std::io::Cursor::new(data))
        .ok()
        .and_then(|v| v.into_dictionary())
        .ok_or_else(|| crate::unsupported!("BuildManifest.plist has no BuildIdentities"))?;
    let all = p
        .get("BuildIdentities")
        .and_then(Value::as_array)
        .ok_or_else(|| crate::unsupported!("BuildManifest.plist has no BuildIdentities"))?;
    let info = |bi: &Dictionary, key: &str| dict(bi, "Info").and_then(|i| string(i, key));
    let ids: Vec<&Dictionary> = all
        .iter()
        .filter_map(Value::as_dictionary)
        .filter(|bi| info(bi, "DeviceClass").map(|c| c.eq_ignore_ascii_case(board)).unwrap_or(false))
        .collect();
    let first = ids
        .iter()
        .find(|bi| info(bi, "RestoreBehavior").as_deref() != Some("Update"))
        .or(ids.first())
        .ok_or_else(|| crate::unsupported!("BuildManifest.plist has no {board} identity"))?;
    let manifest = dict(first, "Manifest").ok_or_else(|| crate::unsupported!("BuildManifest.plist has no {board} identity"))?;
    let path = |v: &Value| v.as_dictionary().and_then(|d| dict(d, "Info")).and_then(|i| string(i, "Path"));
    let mut comp: BTreeMap<String, String> = manifest.iter().filter_map(|(k, v)| path(v).map(|p| (k.clone(), p))).collect();
    for bi in ids.iter().filter(|bi| info(bi, "RestoreBehavior").as_deref() == Some("Update")) {
        if let Some(rd) = dict(bi, "Manifest").and_then(|m| m.get("RestoreRamDisk")).and_then(path) {
            comp.insert("UpdateRamDisk".into(), rd);
        }
    }
    Ok(comp)
}

/// 1.x/2.x: Restore.plist names the kernelcache, rootfs and ramdisks; the rest follow the board's names (1.x:
/// `.img2` all_flash members, and the logo carries no platform).
pub fn components_from_restore(r: &RestoreInfo, img2: bool) -> Result<BTreeMap<String, String>> {
    let board = &r.board_config;
    let platform = &r.platform;
    let all_flash = format!("Firmware/all_flash/all_flash.{board}.production/");
    // 1.0 (1A543a) has no update ramdisk and no iBEC: a restore was the only install path
    let (Some(kc), Some(os), Some(user)) = (&r.kernel_cache, &r.system_image, &r.restore_ram_disk) else {
        return Err(crate::unsupported!("no BuildManifest.plist, and Restore.plist lacks the 2.x component keys"));
    };
    if !img2 && r.update_ram_disk.is_none() {
        return Err(crate::unsupported!("no BuildManifest.plist, and Restore.plist lacks the 2.x component keys"));
    }
    let x = if img2 { "img2" } else { "img3" };
    let logo = if img2 { "applelogo.img2".to_string() } else { format!("applelogo.{platform}.img3") };
    let mut comp = BTreeMap::from([
        ("iBSS".to_string(), format!("Firmware/dfu/iBSS.{board}.RELEASE.dfu")),
        ("iBoot".to_string(), format!("{all_flash}iBoot.{board}.RELEASE.{x}")),
        ("LLB".to_string(), format!("{all_flash}LLB.{board}.RELEASE.{x}")),
        ("DeviceTree".to_string(), format!("{all_flash}DeviceTree.{board}.{x}")),
        ("AppleLogo".to_string(), format!("{all_flash}{logo}")),
        ("KernelCache".to_string(), kc.clone()),
        ("OS".to_string(), os.clone()),
        ("RestoreRamDisk".to_string(), user.clone()),
    ]);
    if let Some(update) = &r.update_ram_disk {
        comp.insert("UpdateRamDisk".into(), update.clone());
        comp.insert("iBEC".into(), format!("Firmware/dfu/iBEC.{board}.RELEASE.dfu"));
    }
    Ok(comp)
}

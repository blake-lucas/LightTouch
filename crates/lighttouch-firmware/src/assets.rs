//! mobileassetd's preinstall for 6.x, done at prepare time (SystemEdits/PreinstalledAssets.swift, its ODC cpio).
//!
//! A 6.x restore leaves /var/mobile/Library/PreinstalledAssets/<type>.cpio (odc cpio: one <hash>.cpio per asset and
//! the type's catalog, <type>.plist). On the first boot mobileassetd copies each asset whose __PreinstallRegionCodes
//! (if any) hold the unit's region to Assets/<type>/<hash>.asset/<hash>, writes the type's catalog of those (each
//! "__DownloadState" "Verifying", without FormatVersion), removes PreinstalledAssets and writes
//! Assets/preinstall-is-complete.plist; DataMigrator's later migrators wait for it. On n81ap 6.1.6 that is 142 MB of
//! dictionaries through the emulated flash, ~45 s of the seal boot, so it is done here instead. The layout is what
//! mobileassetd left on a sealed n81ap 10B500 data volume; the fonts are China, Taiwan and Hong Kong units' only.
use crate::error::Result;
use crate::hfs::Editor;
use crate::plistedit::{self, Format};
use crate::setup::{join, string_array};
use plist::{Dictionary, Value};
use std::collections::BTreeMap;

pub const PREINSTALLED_ASSETS: &str = "mobile/Library/PreinstalledAssets";
pub const ASSETS: &str = "mobile/Library/Assets";

/// The assets installed ("<type>/<hash>"), or None when the volume has nothing to preinstall (not 6.x, or no
/// archives). `version` and `build` as SystemVersion.plist has them; `region` the unit's region code ("LL" of
/// "LL/A"). `var_root`: see setup.rs.
pub fn seed_preinstalled_assets(
    data: &mut Editor,
    var_root: &str,
    major: u32,
    version: &str,
    build: &str,
    region: &str,
) -> Result<Option<Vec<String>>> {
    let source = join(var_root, PREINSTALLED_ASSETS);
    if major != 6 {
        return Ok(None);
    }
    let Ok(names) = data.list(&source) else { return Ok(None) };
    let mut archives: Vec<String> = names.into_iter().filter(|n| n.ends_with(".cpio")).collect();
    archives.sort();
    if archives.is_empty() {
        return Ok(None);
    }
    let root = join(var_root, ASSETS);
    let mut installed = Vec::new();
    for archive in &archives {
        let kind = archive.strip_suffix(".cpio").unwrap_or(archive);
        let bytes = data.read(&format!("{source}/{archive}"))?;
        let cpio = Odc { name: archive, bytes: &bytes };
        let catalog = match plistedit::parse(&cpio.read(&format!("{kind}.plist"))?) {
            Ok(Value::Dictionary(d)) => d,
            _ => return Err(crate::unsupported!("{archive}: no {kind}.plist catalog")),
        };
        let all: Vec<Dictionary> = match catalog.get("Assets").and_then(Value::as_array) {
            Some(a) => a
                .iter()
                .map(|v| v.as_dictionary().cloned())
                .collect::<Option<_>>()
                .ok_or_else(|| crate::unsupported!("{archive}: no {kind}.plist catalog"))?,
            None => return Err(crate::unsupported!("{archive}: no {kind}.plist catalog")),
        };
        let wanted: Vec<Dictionary> = all
            .into_iter()
            .filter(|a| string_array(a.get("__PreinstallRegionCodes")).is_none_or(|codes| codes.iter().any(|c| c == region)))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let dir = format!("{root}/{kind}");
        let mut members: BTreeMap<String, String> = BTreeMap::new();
        let mut entries: Vec<Value> = Vec::new();
        for mut asset in wanted {
            let rel = asset.get("__RelativePath").and_then(Value::as_string).filter(|r| r.ends_with(".cpio"));
            let Some(rel) = rel else {
                return Err(crate::unsupported!("{archive}: an asset without a .cpio __RelativePath"));
            };
            let leaf = rel.split('/').rfind(|s| !s.is_empty()).unwrap_or("");
            let hash = leaf.strip_suffix(".cpio").unwrap_or(leaf).to_string();
            let home = format!("{dir}/{hash}.asset");
            data.mkdirs(&home)?;
            members.insert(format!("{hash}.cpio"), format!("{home}/{hash}"));
            asset.insert("__DownloadState".into(), "Verifying".into());
            entries.push(Value::Dictionary(asset));
            installed.push(format!("{kind}/{hash}"));
        }
        cpio.extract(data, members)?;
        let mut out = catalog;
        out.insert("Assets".into(), Value::Array(entries));
        out.remove("FormatVersion");
        data.write(&format!("{dir}/{kind}.plist"), plistedit::serialize(&Value::Dictionary(out), Format::Xml)?, None)?;
    }
    data.remove(&source)?;
    data.mkdirs(&root)?;
    let done = plistedit::dictionary([("BuildVersion", build.into()), ("ProductVersion", version.into())]);
    data.write(
        &format!("{root}/preinstall-is-complete.plist"),
        plistedit::serialize(&Value::Dictionary(done), Format::Xml)?,
        None,
    )?;
    Ok(Some(installed))
}

/// An odc ("070707") cpio archive, as 6.x ships its preinstalled assets: a 76-byte octal header, the name (NUL
/// included), the data, no padding, up to TRAILER!!!. `name` is the archive's, for errors.
pub struct Odc<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

impl<'a> Odc<'a> {
    /// Each member: its name ("./" dropped) and data, in order.
    pub fn members(&self) -> Result<Vec<(String, &'a [u8])>> {
        let b = self.bytes;
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let header = b.get(offset..offset + 76).filter(|h| h.starts_with(b"070707"));
            let Some(header) = header else {
                return Err(crate::unsupported!("{}: not an odc cpio at {offset}", self.name));
            };
            let octal = |at: usize, width: usize| -> Result<usize> {
                std::str::from_utf8(&header[at..at + width])
                    .ok()
                    .and_then(|s| usize::from_str_radix(s, 8).ok())
                    .ok_or_else(|| crate::unsupported!("{}: bad cpio header at {offset}", self.name))
            };
            let (name_size, size) = (octal(59, 6)?, octal(65, 11)?);
            let at = offset + 76;
            let raw = &b[at.min(b.len())..(at + name_size).min(b.len())];
            let name = String::from_utf8_lossy(raw.split(|&c| c == 0).next().unwrap_or(&[])).into_owned();
            if name == "TRAILER!!!" {
                return Ok(out);
            }
            let name = name.strip_prefix("./").map(str::to_string).unwrap_or(name);
            let start = at + name_size;
            let Some(body) = b.get(start..start + size) else {
                return Err(crate::unsupported!("{}: {name} cut short", self.name));
            };
            out.push((name, body));
            offset = start + size;
        }
    }

    /// One member's data.
    pub fn read(&self, member: &str) -> Result<Vec<u8>> {
        self.members()?
            .into_iter()
            .find(|(n, _)| n == member)
            .map(|(_, d)| d.to_vec())
            .ok_or_else(|| crate::unsupported!("{}: no {member}", self.name))
    }

    /// The named members (name -> volume path), each written to its new file in `ed`.
    pub fn extract(&self, ed: &mut Editor, mut targets: BTreeMap<String, String>) -> Result<()> {
        for (name, body) in self.members()? {
            if let Some(path) = targets.remove(&name) {
                ed.write(&path, body.to_vec(), None)?;
            }
        }
        if !targets.is_empty() {
            let left: Vec<&str> = targets.keys().map(String::as_str).collect();
            return Err(crate::unsupported!("{}: no {}", self.name, left.join(", ")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::{Scratch, get_plist};

    fn odc(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut d = Vec::new();
        let trailer = ("TRAILER!!!", Vec::new());
        for (name, body) in members.iter().chain(std::iter::once(&trailer)) {
            let n = name.len() + 1;
            d.extend(
                format!("070707{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:011o}{:06o}{:011o}", 0, 0, 0o100644, 0, 0, 1, 0, 0, n, body.len())
                    .into_bytes(),
            );
            d.extend_from_slice(name.as_bytes());
            d.push(0);
            d.extend_from_slice(body);
        }
        d
    }

    fn catalog(kind: &str, assets: Vec<Dictionary>) -> Vec<u8> {
        let d = plistedit::dictionary([
            ("AssetType", kind.into()),
            ("Assets", Value::Array(assets.into_iter().map(Value::Dictionary).collect())),
            ("FormatVersion", 1.into()),
        ]);
        plistedit::serialize(&Value::Dictionary(d), Format::Xml).unwrap()
    }

    /// preinstalledAssetsAsMobileassetdLeavesThem: each asset for this unit's region (here none of a font's) at
    /// Assets/<type>/<hash>.asset/<hash>, the catalog without FormatVersion and each asset Verifying, the completion
    /// marker, PreinstalledAssets gone. Not before 6.
    #[test]
    fn preinstalled_assets_as_mobileassetd_leaves_them() {
        let (_s, mut ed) = Scratch::new("assets");
        let var = "private/var";
        let source = join(var, PREINSTALLED_ASSETS);
        ed.mkdirs(&source).unwrap();
        let dict = "com_apple_MobileAsset_DictionaryServices_dictionary";
        let font = "com_apple_MobileAsset_Font";
        let rel = |p: String| plistedit::dictionary([("__RelativePath", p.into())]);
        ed.write(
            &format!("{source}/{dict}.cpio"),
            odc(&[
                ("./aa.cpio", b"dictionary aa".to_vec()),
                ("./bb.cpio", b"dictionary bb".to_vec()),
                (&format!("./{dict}.plist"), catalog("d", vec![rel(format!("{dict}/aa.cpio")), rel(format!("{dict}/bb.cpio"))])),
            ]),
            None,
        )
        .unwrap();
        let mut f = rel(format!("{font}/cc.cpio"));
        f.insert("__PreinstallRegionCodes".into(), plistedit::strings(&["CH"]));
        ed.write(
            &format!("{source}/{font}.cpio"),
            odc(&[("./cc.cpio", b"font".to_vec()), (&format!("./{font}.plist"), catalog("f", vec![f]))]),
            None,
        )
        .unwrap();

        assert_eq!(seed_preinstalled_assets(&mut ed, var, 5, "5.1.1", "9B206", "LL").unwrap(), None);
        let installed = seed_preinstalled_assets(&mut ed, var, 6, "6.1.6", "10B500", "LL").unwrap();
        assert_eq!(installed, Some(vec![format!("{dict}/aa"), format!("{dict}/bb")]));
        let assets = join(var, ASSETS);
        assert_eq!(ed.read(&format!("{assets}/{dict}/bb.asset/bb")).unwrap(), b"dictionary bb");
        let meta = ed.stat(&format!("{assets}/{dict}/bb.asset/bb")).unwrap().unwrap();
        assert_eq!(meta.perm, 0o644);
        let out = get_plist(&ed, &format!("{assets}/{dict}/{dict}.plist"));
        assert!(out.get("FormatVersion").is_none());
        assert_eq!(out.get("AssetType").and_then(Value::as_string), Some("d"));
        let states: Vec<Option<&str>> = out
            .get("Assets")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|a| a.as_dictionary().and_then(|d| d.get("__DownloadState")).and_then(Value::as_string))
            .collect();
        assert_eq!(states, [Some("Verifying"), Some("Verifying")]);
        assert!(!ed.exists(&format!("{assets}/{font}")), "fonts are China, Taiwan and Hong Kong units' only");
        let done = get_plist(&ed, &format!("{assets}/preinstall-is-complete.plist"));
        assert_eq!(done, plistedit::dictionary([("BuildVersion", "10B500".into()), ("ProductVersion", "6.1.6".into())]));
        assert!(!ed.exists(&source));
    }
}

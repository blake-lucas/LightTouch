//! The 14G61 preparation records retained through the native first migration
//! (Swift `A6FirstBoot`). This module only patches data and container metadata; the first-boot
//! seal one-shot itself is the parent's.
//!
//! Swift uses `HFSPlusWriter` with `owner=(0,0)`; Rust's `Editor` creates 501:20, so every written
//! record is re-owned after commit.
use crate::activation;
use crate::codesign;
use crate::error::Result;
use crate::files;
use crate::hfs::{Editor, Volume};
use crate::plistedit;
use plist::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

pub const GROUPS: [&str; 2] =
    ["systemgroup.com.apple.mobilegestaltcache", "systemgroup.com.apple.configurationprofiles"];
pub const GROUP_ROOT: &str = "containers/Shared/SystemGroup/";
pub const METADATA_NAME: &str = ".com.apple.mobile_container_manager.metadata.plist";
pub const CLOUD_PATH: &str = "containers/Shared/SystemGroup/systemgroup.com.apple.configurationprofiles/Library/ConfigurationProfiles/CloudConfigurationDetails.plist";
pub const SEAL_EXECUTABLE: &str = "usr/local/bin/it_seal";
pub const SEAL_JOB: &str = "System/Library/LaunchDaemons/com.qemu.it-seal.plist";

pub struct Component {
    pub path: &'static str,
    pub original_sha256: &'static str,
    pub prepared_sha256: &'static str,
    /// (offset, original, replacement)
    pub edits: &'static [(usize, &'static [u8], &'static [u8])],
}

pub const COMPONENTS: [Component; 2] = [
    Component {
        path: "System/Library/DataClassMigrators/MobileGestaltMigrator.migrator/MobileGestaltMigrator",
        original_sha256: "c849fe475fdf8209e0bafe1e0ae7be96cb1686bdeea2721ca35af54b343a45a1",
        prepared_sha256: "52b2a6f670e515bd70d0a9222fed7f1afd7e76c20561dc1de1d3999c4a70e695",
        edits: &[(0x3f3c, &[0x00, 0xf0, 0x5e, 0xe8], &[0x00, 0xbf, 0x00, 0xbf])],
    },
    Component {
        path: "System/Library/DataClassMigrators/MCProfile.migrator/MCProfile",
        original_sha256: "ed767fe8c1ec39657e9d5332b9f65592ecf87294f1efd1bc7d7d69a00be8e951",
        prepared_sha256: "fd32b295887e8bd8fc93d965a7975a2fe73255903ab005e0678b6539a560be9d",
        edits: &[
            (0x3672, &[0x00, 0xf0, 0xbc, 0xec], &[0x01, 0x20, 0x00, 0xbf]),
            (0x36aa, &[0x00, 0xf0, 0xa0, 0xec], &[0x01, 0x20, 0x00, 0xbf]),
        ],
    },
];

fn invalid(reason: &str) -> crate::Error {
    crate::unsupported!("N42 first boot: {reason}")
}

pub fn prepare(original: &[u8], component: &Component) -> Result<Vec<u8>> {
    let hash = files::sha256(original);
    if hash == component.prepared_sha256 {
        return Ok(original.to_vec());
    }
    if hash != component.original_sha256 {
        return Err(invalid(&format!("unrecognized {}", component.path)));
    }
    let mut prepared = original.to_vec();
    for (offset, before, after) in component.edits {
        let end = offset + before.len();
        if end > original.len() || &original[*offset..end] != *before {
            return Err(invalid("migration call layout differs"));
        }
        prepared[*offset..end].copy_from_slice(after);
    }
    prepared = activation::signed(&prepared)?;
    if files::sha256(&prepared) != component.prepared_sha256 {
        return Err(invalid("signed output differs"));
    }
    codesign::verified_code_directories(&prepared)?;
    Ok(prepared)
}

/// `seed`: the device identity seed the container UUID is derived from; nil: a random one.
pub fn container_metadata(identifier: &str, seed: Option<&str>) -> Result<plist::Dictionary> {
    if !GROUPS.contains(&identifier) {
        return Err(invalid("unqualified SystemGroup"));
    }
    let uuid = match seed {
        Some(seed) => {
            let mut b: [u8; 16] = Sha256::digest(format!("{seed} {identifier}").as_bytes())[..16].try_into().unwrap();
            b[6] = (b[6] & 0x0f) | 0x50;
            b[8] = (b[8] & 0x3f) | 0x80;
            format!(
                "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
            )
        }
        None => {
            // A random v4 UUID; no uuid crate in the manifest.
            let mut b = [0u8; 16];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b))
                .map_err(|e| crate::internal!("random UUID: {e}"))?;
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            format!(
                "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
            )
        }
    };
    Ok(plistedit::dictionary([
        ("MCMMetadataIdentifier", identifier.into()),
        ("MCMMetadataUUID", uuid.into()),
        ("MCMMetadataPersona", 499.into()),
        ("MCMMetadataContentClass", 13.into()),
    ]))
}

pub fn valid_metadata(metadata: &plist::Dictionary, identifier: &str) -> bool {
    GROUPS.contains(&identifier)
        && metadata.get("MCMMetadataIdentifier").and_then(Value::as_string) == Some(identifier)
        && metadata.get("MCMMetadataPersona").and_then(Value::as_signed_integer) == Some(499)
        && metadata.get("MCMMetadataContentClass").and_then(Value::as_signed_integer) == Some(13)
        && metadata.get("MCMMetadataUUID").and_then(Value::as_string).is_some_and(|u| !u.is_empty())
}

/// Inventory only the exact prepared components; a partially prepared pair is never accepted.
pub fn prepared_paths(volume: &Volume) -> Result<Vec<&'static str>> {
    let mut paths = Vec::new();
    for component in &COMPONENTS {
        let bytes = volume.contents(&volume.record(component.path)?)?;
        let hash = files::sha256(&bytes);
        if hash == component.prepared_sha256 {
            paths.push(component.path);
        } else if hash != component.original_sha256 {
            return Err(invalid("migration component differs"));
        }
    }
    if !paths.is_empty() && paths.len() != COMPONENTS.len() {
        return Err(invalid("incomplete migration preparation"));
    }
    Ok(paths)
}

fn binary_plist(d: &plist::Dictionary) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    Value::Dictionary(d.clone()).to_writer_binary(&mut out).map_err(|e| crate::internal!("binary plist: {e}"))?;
    Ok(out)
}

/// `A6FirstBoot.install`: patched migrators + the two SystemGroup metadata records, and the
/// `first-boot-preparation.json` provenance object.
pub fn install(system: &Path, data: &Path, activation_plan: &[u8], seed: Option<&str>) -> Result<Vec<u8>> {
    let s_version = plistedit::read(&Editor::open_read_only(system)?, "System/Library/CoreServices/SystemVersion.plist")?
        .and_then(|d| d.get("ProductBuildVersion").and_then(Value::as_string).map(str::to_string));
    let plan = plist::Value::from_reader(std::io::Cursor::new(activation_plan))
        .ok()
        .and_then(|v| v.into_dictionary())
        .ok_or_else(|| invalid("activation plan is not a dictionary"))?;
    let d = Editor::open(data)?;
    let cloud = plistedit::read(&d, CLOUD_PATH)?
        .and_then(|p| p.get("CloudConfigurationUIComplete").and_then(Value::as_boolean));
    let s = Editor::open(system)?;
    let fresh = s_version.as_deref() == Some("14G61")
        && plan.get("Build").and_then(Value::as_string) == Some("14G61")
        && plan
            .get("Overrides")
            .and_then(Value::as_dictionary)
            .and_then(|o| o.get("a6vjPkzcRjrsXmniFsm0dg"))
            .and_then(Value::as_boolean)
            == Some(true)
        && cloud == Some(true)
        && !d.exists("keybags/systembag.kb")
        && !s.exists(SEAL_EXECUTABLE)
        && !s.exists(SEAL_JOB);
    if !fresh {
        return Err(invalid("requires fresh 14G61 volumes and the existing activation/setup plan"));
    }

    let mut originals = Vec::new();
    let mut prepared = Vec::new();
    for component in &COMPONENTS {
        let original = s.read(component.path)?;
        originals.push(original.clone());
        prepared.push(prepare(&original, component)?);
    }
    let mut metadata: Vec<Vec<u8>> = Vec::new();
    for identifier in GROUPS {
        let path = format!("{GROUP_ROOT}{identifier}/{METADATA_NAME}");
        if d.exists(&path) {
            let raw = d.read(&path)?;
            let existing = plist::Value::from_reader(std::io::Cursor::new(&raw))
                .ok()
                .and_then(plist::Value::into_dictionary)
                .ok_or_else(|| invalid("existing SystemGroup metadata differs"))?;
            if !valid_metadata(&existing, identifier) {
                return Err(invalid("existing SystemGroup metadata differs"));
            }
            metadata.push(raw);
        } else {
            metadata.push(binary_plist(&container_metadata(identifier, seed)?)?);
        }
    }
    drop(d);
    drop(s);

    let mut system_ed = Editor::open(system)?;
    for (component, bytes) in COMPONENTS.iter().zip(prepared.iter()) {
        system_ed.write(component.path, bytes.clone(), Some(0o755))?;
    }
    system_ed.commit()?;
    let mut data_ed = Editor::open(data)?;
    for (identifier, bytes) in GROUPS.iter().zip(metadata.iter()) {
        let dir = format!("{GROUP_ROOT}{identifier}");
        data_ed.mkdirs(&dir)?;
        data_ed.write(&format!("{dir}/{METADATA_NAME}"), bytes.clone(), Some(0o644))?;
    }
    data_ed.commit()?;

    let sv = Volume::open(system, true)?;
    let paths: Vec<&str> = COMPONENTS.iter().map(|c| c.path).collect();
    sv.set_owner(&paths, 0, 0, None, None)?;
    drop(sv);
    let dv = Volume::open(data, true)?;
    let mut dirs: Vec<String> = GROUPS
        .iter()
        .flat_map(|g| {
            let dir = format!("{GROUP_ROOT}{g}");
            [dir.clone(), format!("{dir}/{METADATA_NAME}")]
        })
        .collect();
    dirs.shrink_to_fit();
    let dpaths: Vec<&str> = dirs.iter().map(String::as_str).collect();
    dv.set_owner(&dpaths, 0, 0, None, None)?;
    drop(dv);

    let components: Vec<serde_json::Value> = COMPONENTS
        .iter()
        .zip(originals.iter())
        .map(|(c, original)| {
            serde_json::json!({
                "path": c.path,
                "inputSHA256": files::sha256(original),
                "outputSHA256": c.prepared_sha256,
            })
        })
        .collect();
    let report = serde_json::json!({
        "schema": 1, "build": "14G61", "mode": "first-normal-boot",
        "components": components, "containers": GROUPS,
    });
    serde_json::to_vec_pretty(&crate::lock::sorted(&report)).map_err(|e| crate::internal!("first-boot report: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_qualified_groups() {
        assert!(container_metadata("systemgroup.com.apple.mobilegestaltcache", Some("x")).is_ok());
        assert!(container_metadata("com.example.bad", Some("x")).is_err());
    }
}

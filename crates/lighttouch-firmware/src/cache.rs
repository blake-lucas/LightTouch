//! The decrypt cache: immutable, versioned results under `<root>/decrypted-v2/<ipsw sha1>/<identity digest>/`, each
//! with a manifest of its files' sizes and SHA-256s. Publication and inspection share a cross-process lock; a failed
//! producer never replaces a completed result. Same layout as the Swift tool's (its entries carry its own version in
//! the identity, so the two never reuse each other's).
use crate::entry::{Entry, Key};
use crate::error::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Bump when decrypt output conventions change.
pub const FORMAT: i64 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub board: String,
    pub format: i64,
    pub ipsw: String,
    pub keys: BTreeMap<String, Key>,
    #[serde(rename = "productType")]
    pub product_type: String,
    pub tool: String,
}

impl Identity {
    pub fn new(ipsw_sha1: &str, entry: &Entry) -> Identity {
        Identity {
            board: entry.board.clone(),
            format: FORMAT,
            ipsw: ipsw_sha1.to_string(),
            keys: entry.keys.clone(),
            product_type: entry.product_type.clone(),
            tool: crate::VERSION.to_string(),
        }
    }

    fn digest(&self) -> String {
        hex::encode(Sha256::digest(serde_json::to_vec(self).expect("identity serializes")))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileRecord {
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    files: BTreeMap<String, FileRecord>,
    identity: Identity,
}

/// The SHA-256 of a file, hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| crate::internal!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && name != "manifest.json"
}

/// The cache entry for `identity`, made by `produce` (which writes into the directory it is given and returns the
/// file names) unless a complete, verified one exists.
pub fn resolve(
    root: &Path,
    identity: &Identity,
    produce: impl FnOnce(&Path) -> Result<Vec<String>>,
    reused: impl FnOnce(),
) -> Result<PathBuf> {
    let parent = root.join(format!("decrypted-v{FORMAT}")).join(&identity.ipsw);
    fs::create_dir_all(&parent)?;
    let key = identity.digest();
    let result = parent.join(&key);
    let lock = File::options().create(true).truncate(false).read(true).write(true).open(parent.join(format!("{key}.lock")))?;
    lock.lock()?;
    if let Ok(data) = fs::read(result.join("manifest.json")) {
        if let Ok(manifest) = serde_json::from_slice::<Manifest>(&data) {
            let intact = manifest.identity == *identity
                && !manifest.files.is_empty()
                && manifest.files.iter().all(|(name, record)| {
                    let path = result.join(name);
                    valid_name(name)
                        && fs::metadata(&path).map(|m| m.len() == record.bytes).unwrap_or(false)
                        && sha256_file(&path).map(|s| s == record.sha256).unwrap_or(false)
                });
            if intact {
                reused();
                return Ok(result);
            }
        }
    }
    let unique = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    let staged = parent.join(format!(".{key}-{unique}.tmp"));
    fs::create_dir(&staged)?;
    let outcome = (|| -> Result<PathBuf> {
        let mut files = BTreeMap::new();
        for name in produce(&staged)? {
            let path = staged.join(&name);
            if !valid_name(&name) || !path.is_file() {
                return Err(crate::internal!("decrypt cache: invalid output {name}"));
            }
            files.insert(name, FileRecord { bytes: fs::metadata(&path)?.len(), sha256: sha256_file(&path)? });
        }
        if files.is_empty() {
            return Err(crate::internal!("decrypt cache: no outputs"));
        }
        fs::write(staged.join("manifest.json"), serde_json::to_vec(&Manifest { files, identity: identity.clone() }).expect("manifest serializes"))?;
        // An invalid result is quarantined only after its replacement is ready.
        let old = parent.join(format!(".{key}-{unique}.old"));
        let had_old = result.exists();
        if had_old {
            fs::rename(&result, &old)?;
        }
        if let Err(e) = fs::rename(&staged, &result) {
            if had_old {
                let _ = fs::rename(&old, &result);
            }
            return Err(e.into());
        }
        if had_old {
            let _ = fs::remove_dir_all(&old);
        }
        Ok(result.clone())
    })();
    let _ = fs::remove_dir_all(&staged);
    outcome
}

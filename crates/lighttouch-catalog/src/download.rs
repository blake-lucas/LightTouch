//! Downloads: stock IPSWs (ipsw.me's link for the build, then the catalog's own links, then the Internet Archive's
//! copy; resumable, checked against the catalog's size and SHA-1) and boot ROMs (checked by SHA-256).
use crate::catalog::Firmware;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const USER_AGENT: &str =
    concat!("LightTouch/", env!("CARGO_PKG_VERSION"), " (+https://github.com/blake-lucas/LightTouch)");

/// Progress: what is happening, bytes done and total. Return `false` to cancel.
pub type Progress<'a> = dyn FnMut(&str, u64, u64) -> bool + 'a;

#[derive(Debug)]
pub enum DownloadError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DownloadError {}

pub(crate) fn agent(read_timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(read_timeout))
        .timeout_recv_body(None)
        .http_status_as_error(false)
        .https_only(true)
        .build()
        .into()
}

fn host(url: &str) -> &str {
    url.trim_start_matches("https://").split('/').next().unwrap_or(url)
}

/// Where an IPSW can come from, in order.
pub fn ipsw_candidates(firmware: &Firmware) -> Vec<String> {
    let mut urls = Vec::new();
    if let Some(url) = ipsw_me_url(firmware) {
        urls.push(url);
    }
    urls.extend(firmware.source_urls());
    urls.push(format!(
        "https://archive.org/download/iOS_Firmware_Exhibit_Collection/{}",
        firmware.file_name().replace(',', "%2C")
    ));
    let mut seen = Vec::<String>::new();
    urls.retain(|u| {
        let key = u.replace("%2C", ",").replace("%2c", ",");
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    urls
}

/// ipsw.me's link for the build, if it lists one with the catalog's SHA-1.
fn ipsw_me_url(firmware: &Firmware) -> Option<String> {
    let expected = firmware.source.as_ref()?.sha1.to_lowercase();
    let url = format!("https://api.ipsw.me/v4/ipsw/{}/{}", firmware.product_type, firmware.build);
    let mut response = agent(Duration::from_secs(10)).get(&url).header("Accept", "application/json").call().ok()?;
    if response.status() != 200 {
        return None;
    }
    let value: serde_json::Value = response.body_mut().read_json().ok()?;
    let link = value.get("url")?.as_str()?;
    let sha1 = value.get("sha1sum")?.as_str()?.to_lowercase();
    (link.starts_with("https://") && sha1 == expected).then(|| link.to_string())
}

/// Download `firmware`'s IPSW into `dir` (or reuse a matching copy there). Returns the file's path.
pub fn download_ipsw(firmware: &Firmware, dir: &Path, progress: &mut Progress<'_>) -> Result<PathBuf, DownloadError> {
    let source =
        firmware.source.as_ref().ok_or_else(|| DownloadError::Failed("no download for this version".into()))?;
    fs::create_dir_all(dir).map_err(|e| DownloadError::Failed(format!("{}: {e}", dir.display())))?;
    let target = dir.join(firmware.file_name());
    if target.metadata().is_ok_and(|m| m.len() == source.bytes) {
        if sha1_file(&target, progress)? == source.sha1.to_lowercase() {
            return Ok(target);
        }
        let _ = fs::remove_file(&target);
    }
    let partial = dir.join(format!("{}.partial", firmware.file_name()));
    let mut failure = String::from("no download link worked");
    for url in ipsw_candidates(firmware) {
        // Which server, as Android shows it: "Downloading from updates.cdn-apple.com".
        let from = host(&url).to_string();
        if !progress(&format!("Connecting to {from}"), 0, source.bytes) {
            return Err(DownloadError::Cancelled);
        }
        let mut tagged = |phase: &str, done: u64, total: u64| match phase {
            "Downloading" => progress(&format!("Downloading from {from}"), done, total),
            other => progress(other, done, total),
        };
        match fetch_resumable(&url, &partial, source.bytes, &mut tagged) {
            Ok(()) => {}
            Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
            Err(DownloadError::Failed(message)) => {
                failure = format!("{}: {message}", host(&url));
                continue;
            }
        }
        if sha1_file(&partial, progress)? != source.sha1.to_lowercase() {
            let _ = fs::remove_file(&partial);
            failure = format!("{}: the download didn't match the expected file", host(&url));
            continue;
        }
        fs::rename(&partial, &target).map_err(|e| DownloadError::Failed(format!("save the IPSW: {e}")))?;
        return Ok(target);
    }
    Err(DownloadError::Failed(failure))
}

fn fetch_resumable(url: &str, partial: &Path, expected: u64, progress: &mut Progress<'_>) -> Result<(), DownloadError> {
    let mut have = partial.metadata().map(|m| m.len()).unwrap_or(0);
    if have > expected {
        let _ = fs::remove_file(partial);
        have = 0;
    }
    if have == expected {
        return Ok(());
    }
    let mut request = agent(Duration::from_secs(30)).get(url);
    if have > 0 {
        request = request.header("Range", &format!("bytes={have}-"));
    }
    let mut response = request.call().map_err(|e| DownloadError::Failed(e.to_string()))?;
    let status = response.status().as_u16();
    let append = match status {
        206 => true,
        200 => false,
        416 if have == expected => return Ok(()),
        _ => return Err(DownloadError::Failed(format!("HTTP {status}"))),
    };
    if !append {
        have = 0;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(partial)
        .map_err(|e| DownloadError::Failed(format!("{}: {e}", partial.display())))?;
    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0u8; 256 << 10];
    let mut last = 0u64;
    loop {
        let n = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(DownloadError::Failed(e.to_string())),
        };
        file.write_all(&buffer[..n]).map_err(|e| DownloadError::Failed(format!("write the download: {e}")))?;
        have += n as u64;
        if have > expected {
            drop(file);
            let _ = fs::remove_file(partial);
            return Err(DownloadError::Failed("the download is larger than expected".into()));
        }
        if have - last >= (1 << 20) || have == expected {
            last = have;
            if !progress("Downloading", have, expected) {
                return Err(DownloadError::Cancelled);
            }
        }
    }
    file.sync_all().map_err(|e| DownloadError::Failed(e.to_string()))?;
    if have != expected {
        return Err(DownloadError::Failed(format!("the download stopped at {have} of {expected} bytes")));
    }
    Ok(())
}

pub fn sha1_file(path: &Path, progress: &mut Progress<'_>) -> Result<String, DownloadError> {
    let mut file = File::open(path).map_err(|e| DownloadError::Failed(format!("{}: {e}", path.display())))?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut hasher = Sha1::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = file.read(&mut buffer).map_err(|e| DownloadError::Failed(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        done += n as u64;
        if done % (16 << 20) < n as u64 && !progress("Checking", done, total) {
            return Err(DownloadError::Cancelled);
        }
    }
    Ok(hex(&hasher.finalize()))
}

/// Check that a user-chosen IPSW is `firmware`'s exact file. On the desktop it is used where it is, not copied.
pub fn verify_ipsw(firmware: &Firmware, path: &Path, progress: &mut Progress<'_>) -> Result<(), DownloadError> {
    let expected = firmware.source.as_ref().ok_or_else(|| DownloadError::Failed("unknown firmware file".into()))?;
    let size = path.metadata().map_err(|e| DownloadError::Failed(e.to_string()))?.len();
    if size != expected.bytes {
        return Err(DownloadError::Failed(format!(
            "That isn't the {} IPSW for iOS {}.",
            firmware.device_name(),
            firmware.label()
        )));
    }
    if sha1_file(path, progress)? != expected.sha1.to_lowercase() {
        return Err(DownloadError::Failed("That file doesn't match the expected IPSW.".into()));
    }
    Ok(())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A SecureROM a device starts from: mask ROM in the chip, the same for every iOS version and never part of an
/// IPSW. The iPod touch 1G and the original iPhone share the S5L8900's; the iPod touch 2G has the S5L8720's
/// (iBoot-240.4). Later devices start from their prepared files and need none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bootrom {
    S5L8720,
    S5L8900,
}

impl Bootrom {
    pub fn for_board(board: &str) -> Option<Self> {
        match board {
            "n72ap" => Some(Self::S5L8720),
            "n45ap" | "m68ap" => Some(Self::S5L8900),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::S5L8720 => "iBoot-240.4",
            Self::S5L8900 => "S5L8900",
        }
    }

    pub fn sha256(self) -> &'static str {
        match self {
            Self::S5L8720 => "55f4d8ea2791ba51dd89934168f38f0fb21ce8762ff614c1e742407c0d3ca054",
            Self::S5L8900 => "220027d6f8ea24603f8c5ad5e920ab553d85a9cd809d79c7e6f6f0eefdc2bc95",
        }
    }

    fn links(self) -> &'static [&'static str] {
        match self {
            Self::S5L8720 => &[
                "https://securerom.fun/resources/APROM/SecureROM%20for%20s5l8720xsi%2C%20iBoot-240.4",
                "https://github.com/hekapooios/hekapooios.github.io/raw/master/resources/APROM/SecureROM%20for%20s5l8720xsi%2C%20iBoot-240.4",
                "https://github.com/devos50/qemu-ios/releases/download/n72ap_v1/bootrom_240_4",
            ],
            Self::S5L8900 => &["https://github.com/devos50/qemu-ios/releases/download/n45ap_v1/bootrom_s5l8900"],
        }
    }

    /// "securerom.fun or github.com", for the prepare screen.
    pub fn sources(self) -> String {
        let mut hosts: Vec<&str> = self.links().iter().map(|l| host(l)).collect();
        hosts.dedup();
        hosts.join(" or ")
    }

    pub fn matches(self, bytes: &[u8]) -> bool {
        hex(&Sha256::digest(bytes)) == self.sha256()
    }

    /// Whether `path` holds this ROM (at most 1 MiB, matching hash).
    pub fn matches_file(self, path: &Path) -> bool {
        path.metadata().is_ok_and(|m| m.is_file() && m.len() <= 1 << 20)
            && fs::read(path).is_ok_and(|bytes| self.matches(&bytes))
    }

    /// Download the ROM to `target`.
    pub fn fetch(self, target: &Path) -> Result<(), String> {
        let mut failure = String::new();
        for link in self.links() {
            let attempt = (|| -> Result<Vec<u8>, String> {
                let mut response = agent(Duration::from_secs(30)).get(*link).call().map_err(|e| e.to_string())?;
                if response.status() != 200 {
                    return Err(format!("HTTP {}", response.status()));
                }
                let bytes =
                    response.body_mut().with_config().limit(1 << 20).read_to_vec().map_err(|e| e.to_string())?;
                if !self.matches(&bytes) {
                    return Err("the file did not match".into());
                }
                Ok(bytes)
            })();
            match attempt {
                Ok(bytes) => {
                    let partial = target.with_extension("partial");
                    fs::write(&partial, bytes).and_then(|_| fs::rename(&partial, target)).map_err(|e| e.to_string())?;
                    return Ok(());
                }
                Err(e) => failure = format!("{}: {e}", host(link)),
            }
        }
        Err(failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    #[test]
    fn candidates_end_with_the_archive_and_dedupe() {
        let firmware = Catalog::get().find("k48ap-7B367").unwrap();
        let urls: Vec<String> = firmware.source_urls();
        assert!(urls[0].contains("apple.com"));
        let last = format!(
            "https://archive.org/download/iOS_Firmware_Exhibit_Collection/{}",
            firmware.file_name().replace(',', "%2C")
        );
        // The catalog's own archive.org mirror and the fallback are the same file.
        let mut all = urls.clone();
        all.push(last.clone());
        let mut seen = Vec::new();
        all.retain(|u| {
            let k = u.replace("%2C", ",");
            !seen.contains(&k) && {
                seen.push(k);
                true
            }
        });
        assert_eq!(all.len(), urls.len());
        assert_eq!(Bootrom::for_board("n72ap"), Some(Bootrom::S5L8720));
        assert_eq!(Bootrom::S5L8720.sources(), "securerom.fun or github.com");
    }
}

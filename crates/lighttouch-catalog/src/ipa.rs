//! Checking an IPA before it goes to the guest, ported from the Android app's IpaPreflight.kt: bounded ZIP,
//! Info.plist and Mach-O checks against the target, then a copy whose main executable has Unix mode 0755. Archives
//! from old collections often store no mode bits, and installd then installs an app SpringBoard can't launch. The
//! archive is never extracted; the repair rewrites only central-directory fields of the copy.
use crate::target::{Target, requires_newer, version};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAX_IPA: u64 = 2 << 30;
const MAX_UNPACKED: u64 = 8 << 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpaError(pub String);

impl std::fmt::Display for IpaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for IpaError {}

fn fail<T>(message: impl Into<String>) -> Result<T, IpaError> {
    Err(IpaError(message.into()))
}

/// What the installer needs from a checked IPA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpaInfo {
    pub bundle_id: String,
    pub name: String,
    pub version: Option<String>,
    pub short_version: Option<String>,
    pub minimum_os: Option<String>,
    pub bytes: u64,
    /// Bytes once unpacked (the sum of the archive's file sizes).
    pub unpacked: u64,
}

struct Entry {
    name: String,
    method: u16,
    flags: u16,
    compressed: u64,
    size: u64,
    local_offset: u64,
    external_attr: u32,
    central_offset: u64,
}

impl Entry {
    fn mode(&self) -> u32 {
        self.external_attr >> 16
    }
    fn symlink(&self) -> bool {
        self.mode() & 0xF000 == 0xA000
    }
}

fn le16(b: &[u8], i: usize) -> u64 {
    u16::from_le_bytes([b[i], b[i + 1]]) as u64
}
fn le32(b: &[u8], i: usize) -> u64 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as u64
}
fn le64(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

struct Zip {
    file: File,
    entries: Vec<Entry>,
}

impl Zip {
    fn open(path: &Path) -> Result<Self, IpaError> {
        let damaged = |e: std::io::Error| IpaError(format!("The IPA is incomplete or damaged: {e}"));
        let mut file = File::open(path).map_err(damaged)?;
        let length = file.metadata().map_err(damaged)?.len();
        let tail_len = length.min(65_557);
        let mut tail = vec![0u8; tail_len as usize];
        file.seek(SeekFrom::Start(length - tail_len)).map_err(damaged)?;
        file.read_exact(&mut tail).map_err(damaged)?;
        let Some(eocd) = (0..=tail.len().saturating_sub(22)).rev().find(|&i| le32(&tail, i) == 0x0605_4b50) else {
            return fail("This file isn't a ZIP archive.");
        };
        let (mut count, mut cd_size, mut cd_offset) =
            (le16(&tail, eocd + 10), le32(&tail, eocd + 12), le32(&tail, eocd + 16));
        if count == 0xFFFF || cd_offset == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF {
            // ZIP64: the locator sits immediately before the classic record.
            if eocd < 20 || le32(&tail, eocd - 20) != 0x0706_4b50 {
                return fail("The IPA has a damaged ZIP64 directory.");
            }
            let mut record = [0u8; 56];
            file.seek(SeekFrom::Start(le64(&tail, eocd - 12))).map_err(damaged)?;
            file.read_exact(&mut record).map_err(damaged)?;
            if le32(&record, 0) != 0x0606_4b50 {
                return fail("The IPA has a damaged ZIP64 directory.");
            }
            (count, cd_size, cd_offset) = (le64(&record, 32), le64(&record, 40), le64(&record, 48));
        }
        if count > 100_000 || cd_size > 64 << 20 || cd_offset + cd_size > length {
            return fail("The IPA's directory is too large or damaged.");
        }
        let mut cd = vec![0u8; cd_size as usize];
        file.seek(SeekFrom::Start(cd_offset)).map_err(damaged)?;
        file.read_exact(&mut cd).map_err(damaged)?;
        let mut entries = Vec::with_capacity(count as usize);
        let mut at = 0usize;
        for _ in 0..count {
            if at + 46 > cd.len() || le32(&cd, at) != 0x0201_4b50 {
                return fail("The IPA's directory is damaged.");
            }
            let name_len = le16(&cd, at + 28) as usize;
            let extra_len = le16(&cd, at + 30) as usize;
            let comment_len = le16(&cd, at + 32) as usize;
            if at + 46 + name_len + extra_len + comment_len > cd.len() {
                return fail("The IPA's directory is damaged.");
            }
            let (mut compressed, mut size, mut local) = (le32(&cd, at + 20), le32(&cd, at + 24), le32(&cd, at + 42));
            let mut extra = at + 46 + name_len;
            let extra_end = extra + extra_len;
            while extra + 4 <= extra_end {
                // ZIP64 extended sizes replace saturated fields, in order.
                let (id, len) = (le16(&cd, extra), le16(&cd, extra + 2) as usize);
                if id == 1 {
                    let mut p = extra + 4;
                    let mut take = |field: &mut u64| {
                        if *field == 0xFFFF_FFFF && p + 8 <= extra_end {
                            *field = le64(&cd, p);
                            p += 8;
                        }
                    };
                    take(&mut size);
                    take(&mut compressed);
                    take(&mut local);
                }
                extra += 4 + len;
            }
            entries.push(Entry {
                name: String::from_utf8_lossy(&cd[at + 46..at + 46 + name_len]).into_owned(),
                method: le16(&cd, at + 10) as u16,
                flags: le16(&cd, at + 8) as u16,
                compressed,
                size,
                local_offset: local,
                external_attr: le32(&cd, at + 38) as u32,
                central_offset: cd_offset + at as u64,
            });
            at = extra_end + comment_len;
        }
        Ok(Self { file, entries })
    }

    fn read(&mut self, index: usize, limit: u64) -> Result<Vec<u8>, IpaError> {
        let entry = &self.entries[index];
        if entry.size > limit || entry.flags & 1 != 0 {
            return fail(format!("The IPA contains an unsupported or oversized member: {}", entry.name));
        }
        let damaged = |e: std::io::Error| IpaError(format!("The IPA is incomplete or damaged: {e}"));
        let mut header = [0u8; 30];
        self.file.seek(SeekFrom::Start(entry.local_offset)).map_err(damaged)?;
        self.file.read_exact(&mut header).map_err(damaged)?;
        if le32(&header, 0) != 0x0403_4b50 {
            return fail("The IPA has a damaged member header.");
        }
        let skip = 30 + le16(&header, 26) + le16(&header, 28);
        self.file.seek(SeekFrom::Start(entry.local_offset + skip)).map_err(damaged)?;
        let mut raw = vec![0u8; entry.compressed as usize];
        self.file.read_exact(&mut raw).map_err(damaged)?;
        match entry.method {
            0 => Ok(raw),
            8 => {
                let mut out = Vec::with_capacity(entry.size as usize);
                flate2::read::DeflateDecoder::new(&raw[..]).take(entry.size).read_to_end(&mut out).map_err(damaged)?;
                if out.len() as u64 != entry.size {
                    return fail(format!("The IPA member {} is truncated.", entry.name));
                }
                Ok(out)
            }
            _ => fail("The IPA uses an unsupported compression method."),
        }
    }
}

/// (CPU subtype, minimum iOS from load commands) for each 32-bit ARM slice of an executable.
fn arm_slices(data: &[u8]) -> Result<Vec<(u32, Option<[u32; 3]>)>, IpaError> {
    if data.len() < 28 {
        return fail("The app's executable is truncated.");
    }
    let be32 = |i: usize| u32::from_be_bytes(data[i..i + 4].try_into().unwrap());
    if be32(0) == 0xCAFE_BABE {
        let count = be32(4) as usize;
        if !(1..=32).contains(&count) || data.len() < 8 + count * 20 {
            return fail("Invalid universal executable.");
        }
        let mut slices = Vec::new();
        for i in 0..count {
            let (cpu, offset, size) = (be32(8 + i * 20), be32(16 + i * 20) as usize, be32(20 + i * 20) as usize);
            if offset < 8 + count * 20 || size < 28 || offset + size > data.len() {
                return fail("Invalid universal executable slices.");
            }
            if cpu == 12 {
                slices.push(arm_slice(&data[offset..offset + size])?);
            }
        }
        if slices.is_empty() {
            return fail("This IPA doesn't contain a supported ARM executable.");
        }
        return Ok(slices);
    }
    Ok(vec![arm_slice(data)?])
}

fn arm_slice(data: &[u8]) -> Result<(u32, Option<[u32; 3]>), IpaError> {
    let word = |i: usize| -> Result<u32, IpaError> {
        data.get(i..i + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .ok_or_else(|| IpaError("Invalid executable load commands.".into()))
    };
    if data.len() < 28 || word(0)? != 0xFEED_FACE {
        return fail("This IPA needs an unsupported executable format.");
    }
    let (cpu, subtype, file_type, commands, size) =
        (word(4)?, word(8)? & 0xFF_FFFF, word(12)?, word(16)? as usize, word(20)? as usize);
    if cpu != 12 || file_type != 2 || commands > 65_536 || 28 + size > data.len() {
        return fail("This IPA doesn't contain a supported ARM executable.");
    }
    let (mut cursor, mut minimum) = (28usize, None::<[u32; 3]>);
    for _ in 0..commands {
        if cursor + 8 > 28 + size {
            return fail("Invalid executable load commands.");
        }
        let (command, length) = (word(cursor)?, word(cursor + 4)? as usize);
        if length < 8 || cursor + length > 28 + size {
            return fail("Invalid executable load commands.");
        }
        if command == 0x21 && (length < 20 || word(cursor + 16)? != 0) {
            return fail("This app is encrypted; choose a decrypted IPA.");
        }
        let packed = match command {
            0x25 => Some(word(cursor + 8)?),
            0x32 if word(cursor + 8)? != 2 => return fail("The app's executable targets an unsupported platform."),
            0x32 => Some(word(cursor + 12)?),
            _ => None,
        };
        if let Some(packed) = packed {
            let v = [packed >> 16, (packed >> 8) & 255, packed & 255];
            if minimum.is_none_or(|m| v > m) {
                minimum = Some(v);
            }
        }
        cursor += length;
    }
    Ok((subtype, minimum))
}

fn plist_string(dict: &plist::Dictionary, key: &str) -> Option<String> {
    dict.get(key).and_then(|v| v.as_string()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Check `source` against `target`, then write the copy to install (executable mode repaired) to `destination`.
pub fn prepare(source: &Path, destination: &Path, target: &Target) -> Result<IpaInfo, IpaError> {
    let bytes = std::fs::metadata(source).map(|m| if m.is_file() { m.len() } else { 0 }).unwrap_or(0);
    if bytes == 0 || bytes > MAX_IPA {
        return fail("Choose an IPA file smaller than 2 GB.");
    }
    let mut zip = Zip::open(source)?;
    let unpacked: u64 = zip.entries.iter().map(|e| e.size).sum();
    if unpacked > MAX_UNPACKED {
        return fail("The unpacked IPA is larger than LightTouch supports.");
    }
    let mut names = std::collections::HashSet::new();
    for index in 0..zip.entries.len() {
        let entry = &zip.entries[index];
        let parts: Vec<&str> = entry.name.split('/').collect();
        if !names.insert(entry.name.clone())
            || entry.name.starts_with('/')
            || parts.contains(&"..")
            || entry.name.contains('\\')
            || entry.flags & 1 != 0
        {
            return fail("The IPA contains unsafe, duplicate or encrypted ZIP members.");
        }
        if entry.symlink() {
            let parts: Vec<String> = parts.iter().map(|p| p.to_string()).collect();
            let link = String::from_utf8_lossy(&zip.read(index, 4096)?).into_owned();
            if link.is_empty()
                || link.starts_with('/')
                || link.contains('\\')
                || link.contains('\0')
                || parts.len() < 3
                || parts[0] != "Payload"
                || !parts[1].ends_with(".app")
            {
                return fail("The IPA contains an unsafe symbolic link.");
            }
            let mut resolved: Vec<String> = parts[..parts.len() - 1].to_vec();
            for component in link.split('/') {
                match component {
                    ".." => {
                        if resolved.len() <= 2 {
                            return fail("The IPA contains a symbolic link outside its app.");
                        }
                        resolved.pop();
                    }
                    "." | "" => {}
                    other => resolved.push(other.to_string()),
                }
            }
        }
    }
    let candidates: Vec<usize> = (0..zip.entries.len())
        .filter(|&i| {
            let p: Vec<&str> = zip.entries[i].name.split('/').collect();
            p.len() == 3 && p[0] == "Payload" && p[1].ends_with(".app") && p[2] == "Info.plist"
        })
        .collect();
    let [info_index] = candidates[..] else { return fail("Choose an IPA with one app and its Info.plist in Payload.") };
    let info_bytes = zip.read(info_index, 1 << 20)?;
    let info = plist::Value::from_reader(std::io::Cursor::new(&info_bytes))
        .ok()
        .and_then(plist::Value::into_dictionary)
        .ok_or_else(|| IpaError("The app's Info.plist is damaged.".into()))?;
    let bundle_id = plist_string(&info, "CFBundleIdentifier");
    let executable_name = plist_string(&info, "CFBundleExecutable");
    let (Some(bundle_id), Some(executable_name)) = (bundle_id, executable_name) else {
        return fail("The app has invalid bundle or executable information.");
    };
    if executable_name.contains('/') || executable_name == "." || executable_name == ".." {
        return fail("The app has invalid bundle or executable information.");
    }
    let app_dir = zip.entries[info_index].name.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let executable_path = format!("{app_dir}/{executable_name}");
    let Some(executable) = zip.entries.iter().position(|e| e.name == executable_path) else {
        return fail("The app's executable is missing.");
    };
    if zip.entries[executable].symlink() {
        return fail("The app's executable is unsupported.");
    }
    // CPU subtypes: 0 all, 6 armv6, 9 armv7.
    let supported: &[u32] = if target.arch == "armv6" { &[0, 6] } else { &[0, 6, 9] };
    let slices: Vec<_> =
        arm_slices(&zip.read(executable, 256 << 20)?)?.into_iter().filter(|(s, _)| supported.contains(s)).collect();
    let Some(best) = slices.iter().map(|(s, _)| *s).max() else {
        return fail("This app needs a newer processor than the guest has.");
    };
    let guest = version(&target.ios).ok_or_else(|| IpaError("Invalid guest iOS version.".into()))?;
    // iOS runs the most specific slice it can; an older slice's lower minimum doesn't count.
    for minimum in slices.iter().filter(|(s, _)| *s == best).filter_map(|(_, m)| *m) {
        if minimum > guest {
            return fail(format!("The app's executable requires iOS {}.{}.{}.", minimum[0], minimum[1], minimum[2]));
        }
    }
    let minimum_os = plist_string(&info, "MinimumOSVersion");
    if let Some(minimum) = &minimum_os {
        if requires_newer(minimum, target) {
            return fail(format!("This app requires iOS {minimum}; the guest runs {}.", target.ios));
        }
    }
    if let Some(families) = info.get("UIDeviceFamily").and_then(|v| v.as_array()) {
        let families: Vec<String> = families
            .iter()
            .map(|f| {
                f.as_unsigned_integer().map(|n| n.to_string()).or(f.as_string().map(str::to_string)).unwrap_or_default()
            })
            .collect();
        if !families.is_empty() && !families.contains(&target.family) {
            return fail("This app isn't made for the guest's kind of device.");
        }
    }
    let result = IpaInfo {
        name: plist_string(&info, "CFBundleDisplayName")
            .or_else(|| plist_string(&info, "CFBundleName"))
            .unwrap_or_else(|| bundle_id.clone()),
        bundle_id,
        version: plist_string(&info, "CFBundleVersion"),
        short_version: plist_string(&info, "CFBundleShortVersionString"),
        minimum_os,
        bytes,
        unpacked,
    };
    let exe = &zip.entries[executable];
    let (mode, central, attr) = (exe.mode(), exe.central_offset, exe.external_attr);
    drop(zip);
    std::fs::copy(source, destination).map_err(|e| IpaError(format!("Couldn't copy the IPA: {e}")))?;
    if mode & 0o111 == 0 {
        // Central directory: "version made by" host byte 3 (Unix), external attributes S_IFREG|0755.
        let repair = || -> std::io::Result<()> {
            let mut out = std::fs::OpenOptions::new().write(true).open(destination)?;
            out.seek(SeekFrom::Start(central + 5))?;
            out.write_all(&[3])?;
            out.seek(SeekFrom::Start(central + 38))?;
            out.write_all(&((0o100755u32 << 16) | (attr & 0xFFFF)).to_le_bytes())?;
            out.sync_all()
        };
        repair().map_err(|e| IpaError(format!("Couldn't prepare the IPA: {e}")))?;
    }
    Ok(result)
}

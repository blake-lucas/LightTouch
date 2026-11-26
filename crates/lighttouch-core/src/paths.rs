//! Where the app keeps its data and finds its bundled resources.
use std::path::{Path, PathBuf};

/// The app's data folder. Separate from the old Tk launcher's (`~/.local/share/lighttouch`, `LightTouch\SharedWindows`,
/// `LightTouch Shared Desktop`) so both can coexist. `LIGHTTOUCH_DATA` overrides it.
pub fn data_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("LIGHTTOUCH_DATA") {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home().join(".local/share"));
        base.join("LightTouch")
    }
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/LightTouch")
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home).join("LightTouch").join("App")
    }
}

#[cfg(not(windows))]
fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(windows)]
fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Folders inside the data root, and the device library, which can be elsewhere (Settings → Storage).
#[derive(Debug, Clone)]
pub struct DataDirs {
    pub root: PathBuf,
    /// Where the user keeps their devices instead of `root/library` (settings.json's `library`).
    custom_library: Option<PathBuf>,
}

impl DataDirs {
    pub fn new(root: PathBuf) -> Self {
        Self { root, custom_library: None }
    }
    /// These folders with the device library at `library` (`None`, a relative path or the default: in the data
    /// folder).
    pub fn with_library(mut self, library: Option<PathBuf>) -> Self {
        let default = self.default_library();
        self.custom_library = library.filter(|p| p.is_absolute() && *p != default);
        self
    }
    /// The device library (records with their bases, state and assets).
    pub fn library(&self) -> PathBuf {
        self.custom_library.clone().unwrap_or_else(|| self.default_library())
    }
    /// The library's own place in the data folder, used unless the user chose another.
    pub fn default_library(&self) -> PathBuf {
        self.root.join("library")
    }
    /// The folder the user chose for their devices, if they did.
    pub fn custom_library(&self) -> Option<&Path> {
        self.custom_library.as_deref()
    }
    /// The chosen folder, when it isn't there as a library (a drive not connected, or only its empty mount point):
    /// it must not be opened, which would create an empty library in its place.
    pub fn missing_library(&self) -> Option<&Path> {
        self.custom_library().filter(|library| !library.join("records").is_dir())
    }
    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    /// Downloaded IPSWs, kept until a preparation succeeds. Beside a library the user moved: they're gigabytes too,
    /// and a preparation's output lands in the library.
    pub fn downloads(&self) -> PathBuf {
        match &self.custom_library {
            Some(library) => library.join(".downloads"),
            None => self.root.join("downloads"),
        }
    }
    /// Preparation jobs. On the library's filesystem so finished output is moved in, not copied.
    pub fn prepared(&self) -> PathBuf {
        self.library().join(".prepared")
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    /// Per-device guest USB pairing records.
    pub fn pairing(&self, record: &str) -> PathBuf {
        self.root.join("pairing").join(record)
    }
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }
    /// Detailed frame timing captures.
    pub fn profiles(&self) -> PathBuf {
        self.root.join("profiles")
    }
    /// Per-device guest web proxy identities (`network/<record id>`, owner-only).
    pub fn network(&self) -> PathBuf {
        self.root.join("network")
    }
    /// Frame inspector reports.
    pub fn reports(&self) -> PathBuf {
        self.root.join("reports")
    }
    pub fn screenshots(&self) -> PathBuf {
        pictures_dir().join("LightTouch")
    }
}

fn pictures_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        // Honour xdg-user-dirs when it is configured.
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
        if let Ok(text) = std::fs::read_to_string(config.join("user-dirs.dirs")) {
            for line in text.lines() {
                if let Some(value) = line.strip_prefix("XDG_PICTURES_DIR=") {
                    let value = value.trim_matches('"').replace("$HOME", &home().to_string_lossy());
                    return PathBuf::from(value);
                }
            }
        }
        home().join("Pictures")
    }
    #[cfg(not(target_os = "linux"))]
    {
        home().join("Pictures")
    }
}

/// `path` without Windows' `\\?\` prefix, which `canonicalize` adds there: the C runtime's `stat` (the engine library's
/// file checks) refuses such paths. Kept when the path needs it (longer than MAX_PATH).
pub fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if text.len() < 260 => match rest.strip_prefix(r"UNC\") {
            Some(share) => PathBuf::from(format!(r"\\{share}")),
            None if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
            None => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}

/// Bundled resources: the engine library, guest tools, firmwarekit and its helpers.
///
/// Looked up, in order: `LIGHTTOUCH_RESOURCES`; `resources/` next to the executable (dev builds); `../Resources`
/// (macOS app bundle); `../lib/lighttouch` (Linux AppImage / install prefix); the executable's own payload, unpacked
/// (the Windows single-file build, [`crate::bundle`]; unpacked here, without progress, when the app hasn't already).
#[derive(Debug, Clone)]
pub struct Resources {
    pub dir: PathBuf,
}

impl Resources {
    pub fn locate() -> Self {
        if let Some(dir) = std::env::var_os("LIGHTTOUCH_RESOURCES") {
            return Self { dir: PathBuf::from(dir) };
        }
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .map(|p| plain(&p))
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let candidates = [exe_dir.join("resources"), exe_dir.join("../Resources"), exe_dir.join("../lib/lighttouch")];
        if let Some(dir) = candidates.iter().find(|dir| dir.join(engine_file_name()).is_file()) {
            return Self { dir: dir.clone() };
        }
        if let Some(embedded) = crate::bundle::Embedded::current() {
            match embedded.unpack(&mut |_, _| {}) {
                Ok(dir) => return Self { dir },
                Err(e) => eprintln!("LightTouch couldn't unpack its resources: {e}"),
            }
        }
        let dir = candidates.iter().find(|dir| dir.is_dir()).cloned().unwrap_or_else(|| exe_dir.join("resources"));
        Self { dir }
    }

    /// The executable's resource payload, when [`Resources::locate`] would have to unpack it first (a single-file
    /// build's first start of this version), so the app can show that happening.
    pub fn pending_unpack() -> Option<crate::bundle::Embedded> {
        if std::env::var_os("LIGHTTOUCH_RESOURCES").is_some() {
            return None;
        }
        let exe_dir = std::env::current_exe().ok()?.canonicalize().ok().map(|p| plain(&p))?.parent()?.to_path_buf();
        if ["resources", "../Resources", "../lib/lighttouch"]
            .iter()
            .any(|d| exe_dir.join(d).join(engine_file_name()).is_file())
        {
            return None;
        }
        crate::bundle::Embedded::current().filter(|e| !e.ready())
    }

    /// The engine library. `LIGHTTOUCH_ENGINE` overrides it.
    pub fn engine(&self) -> PathBuf {
        std::env::var_os("LIGHTTOUCH_ENGINE").map(PathBuf::from).unwrap_or_else(|| self.dir.join(engine_file_name()))
    }

    /// `armv6.itpack` / `armv7.itpack` and the preparer's guest helpers.
    pub fn guest_tools(&self) -> PathBuf {
        std::env::var_os("LIGHTTOUCH_GUEST_TOOLS").map(PathBuf::from).unwrap_or_else(|| self.dir.join("guest-tools"))
    }

    pub fn firmwarekit(&self) -> PathBuf {
        std::env::var_os("LIGHTTOUCH_FIRMWAREKIT")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.dir.join(exe_name("firmwarekit")))
    }

    pub fn usbmuxd(&self) -> PathBuf {
        std::env::var_os("LIGHTTOUCH_USBMUXD")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.dir.join(if cfg!(windows) { "lighttouch-usbmuxd.exe" } else { "usbmuxd-qemu" }))
    }

    /// The open source notices, one folder per bundled component (`lighttouch_model::notices`).
    pub fn licenses(&self) -> PathBuf {
        self.dir.join("licenses")
    }
}

pub fn engine_file_name() -> &'static str {
    if cfg!(windows) {
        "libqemu-arm.dll"
    } else if cfg!(target_os = "macos") {
        "libqemu-arm.dylib"
    } else {
        "libqemu-arm.so"
    }
}

pub fn exe_name(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

/// This program, for starting worker processes. On Linux `/proc/self/exe` keeps working after the file on disk
/// is replaced (an update, or a rebuild while running).
pub fn worker_exe() -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    if Path::new("/proc/self/exe").exists() {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    std::env::current_exe().map_err(|e| e.to_string())
}

#[cfg(test)]
mod plain_tests {
    use super::plain;
    use std::path::{Path, PathBuf};

    #[test]
    fn drops_the_verbatim_prefix() {
        assert_eq!(
            plain(Path::new(r"\\?\C:\lt\LightTouch\lighttouch.exe")),
            PathBuf::from(r"C:\lt\LightTouch\lighttouch.exe")
        );
        assert_eq!(plain(Path::new(r"\\?\UNC\host\Data\x")), PathBuf::from(r"\\host\Data\x"));
        assert_eq!(plain(Path::new(r"\\?\Volume{1234}\x")), PathBuf::from(r"\\?\Volume{1234}\x"));
        assert_eq!(plain(Path::new("/opt/lighttouch")), PathBuf::from("/opt/lighttouch"));
    }
}

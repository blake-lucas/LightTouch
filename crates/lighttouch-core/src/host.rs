//! What this computer is, for the app log's header and an exported log's summary: OS, CPU, memory, free storage.
use std::path::Path;

/// (label, value) lines describing this computer. Anything that can't be read is left out. `library` is where the
/// devices are kept, when it isn't inside the data folder `data` (Settings → Storage).
pub fn describe(data: &Path, library: &Path) -> Vec<(&'static str, String)> {
    let mut lines = Vec::new();
    if let Some(os) = os() {
        lines.push(("OS", os));
    }
    lines.push(("Architecture", std::env::consts::ARCH.to_string()));
    if let Some(cpu) = cpu() {
        let threads = std::thread::available_parallelism().map_or(0, |n| n.get());
        lines.push(("CPU", format!("{cpu} ({threads} threads)")));
    }
    if let Some(bytes) = memory_bytes() {
        lines.push(("Memory", gib(bytes)));
    }
    if let Some(bytes) = free_bytes(data) {
        lines.push(("Free storage", format!("{} where LightTouch keeps its data", gib(bytes))));
    }
    if !library.starts_with(data) {
        if let Some(bytes) = free_bytes(library) {
            lines.push(("Free storage for devices", format!("{} where LightTouch keeps its devices", gib(bytes))));
        }
    }
    #[cfg(target_os = "linux")]
    {
        let session = [std::env::var("XDG_SESSION_TYPE").ok(), std::env::var("XDG_CURRENT_DESKTOP").ok()];
        let session: Vec<String> = session.into_iter().flatten().filter(|s| !s.is_empty()).collect();
        if !session.is_empty() {
            lines.push(("Desktop", session.join(", ")));
        }
        if std::env::var_os("APPIMAGE").is_some() {
            lines.push(("Package", "AppImage".into()));
        }
    }
    lines
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// The first line a command prints, trimmed; `None` if it fails.
#[allow(dead_code)]
fn command_line(program: &str, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

#[cfg(target_os = "linux")]
fn os() -> Option<String> {
    let release = std::fs::read_to_string("/etc/os-release").ok();
    let name = release.as_deref().and_then(|text| {
        text.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).map(|v| v.trim_matches('"').to_string())
    });
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|k| k.trim().to_string());
    match (name, kernel) {
        (Some(n), Some(k)) => Some(format!("{n} (Linux {k})")),
        (n, k) => n.or(k.map(|k| format!("Linux {k}"))),
    }
}

#[cfg(target_os = "macos")]
fn os() -> Option<String> {
    command_line("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}"))
}

#[cfg(windows)]
fn os() -> Option<String> {
    command_line("cmd", &["/c", "ver"])
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn os() -> Option<String> {
    Some(std::env::consts::OS.to_string())
}

#[cfg(target_os = "linux")]
fn cpu() -> Option<String> {
    let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    info.lines()
        .find(|l| l.starts_with("model name") || l.starts_with("Hardware"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}

#[cfg(target_os = "macos")]
fn cpu() -> Option<String> {
    command_line("sysctl", &["-n", "machdep.cpu.brand_string"])
}

#[cfg(windows)]
fn cpu() -> Option<String> {
    std::env::var("PROCESSOR_IDENTIFIER").ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn cpu() -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn memory_bytes() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib: u64 =
        info.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kib * 1024)
}

#[cfg(target_os = "macos")]
fn memory_bytes() -> Option<u64> {
    command_line("sysctl", &["-n", "hw.memsize"])?.parse().ok()
}

#[cfg(windows)]
fn memory_bytes() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullTotalPhys)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn memory_bytes() -> Option<u64> {
    None
}

#[cfg(unix)]
pub(crate) fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent()?;
    }
    let c = std::ffi::CString::new(existing.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut stat) } == 0).then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(windows)]
pub(crate) fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent()?;
    }
    let wide: Vec<u16> = existing.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(test)]
mod tests {
    #[test]
    fn this_computer_has_a_description() {
        let lines = super::describe(&std::env::temp_dir(), &std::env::temp_dir());
        assert!(lines.iter().any(|(k, _)| *k == "Architecture"));
        #[cfg(target_os = "linux")]
        assert!(lines.iter().any(|(k, _)| *k == "Memory"), "{lines:?}");
    }
}

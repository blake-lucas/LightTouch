//! The host monotonic clock in the core's time domain (CLOCK_MONOTONIC on Linux and macOS, the performance counter on
//! Windows), for touch and motion timestamps.

#[cfg(unix)]
pub fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(windows)]
pub fn monotonic_ns() -> u64 {
    use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let (mut count, mut frequency) = (0i64, 0i64);
    unsafe {
        QueryPerformanceCounter(&mut count);
        QueryPerformanceFrequency(&mut frequency);
    }
    ((count as i128 * 1_000_000_000) / frequency.max(1) as i128) as u64
}

/// CPU time this process has used, all threads.
#[cfg(unix)]
pub fn process_cpu_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(windows)]
pub fn process_cpu_ns() -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) };
    if ok == 0 {
        return 0;
    }
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    (ticks(kernel) + ticks(user)) * 100
}

/// This process's resident memory (working set on Windows); 0 where it can't tell.
#[cfg(target_os = "linux")]
pub fn process_resident_bytes() -> u64 {
    let pages = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|p| p.parse::<u64>().ok()))
        .unwrap_or(0);
    pages * unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as u64
}

#[cfg(target_os = "macos")]
pub fn process_resident_bytes() -> u64 {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    let got = unsafe {
        libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTASKINFO, 0, &mut info as *mut _ as *mut libc::c_void, size)
    };
    if got == size { info.pti_resident_size } else { 0 }
}

#[cfg(windows)]
pub fn process_resident_bytes() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    if ok == 0 { 0 } else { counters.WorkingSetSize as u64 }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn process_resident_bytes() -> u64 {
    0
}

/// Today's date on the host's calendar (local time): year, month, day.
#[cfg(unix)]
pub fn local_date() -> (i32, u32, u32) {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    (tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32)
}

#[cfg(windows)]
pub fn local_date() -> (i32, u32, u32) {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut now = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut now) };
    (now.wYear as i32, now.wMonth as u32, now.wDay as u32)
}

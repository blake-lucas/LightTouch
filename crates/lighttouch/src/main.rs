//! LightTouch desktop. One executable: run with no arguments for the app; `worker` runs one device session (started
//! by the app, one process per session because the engine starts once per process); the rest are developer tools.
// Windows: a GUI program, so starting it from Explorer opens no console; commands run from a terminal attach to it.
#![cfg_attr(windows, windows_subsystem = "windows")]
mod audio;
mod cli;
mod ui;
mod worker;

fn main() {
    #[cfg(windows)]
    attach_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("worker") => worker::main(&args[1..]),
        Some(command) if cli::handles(command) => cli::main(&args),
        _ => match ui::run() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("LightTouch couldn't open its window: {e}");
                1
            }
        },
    };
    std::process::exit(code);
}

/// Print to the console this was started from, if any, unless output is already redirected (a worker's pipes).
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if out.is_null() || out == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

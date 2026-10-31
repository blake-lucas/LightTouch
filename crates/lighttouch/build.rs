//! The app icon in the Windows executable (Explorer, the taskbar, Alt-Tab): a resource script compiled with MinGW's
//! windres and linked in. Other targets take the icon from their bundle (macOS) or the window (Linux).
use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/lighttouch.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let icon = manifest.join("assets/icon/lighttouch.ico");
    let script = out.join("lighttouch.rc");
    std::fs::write(&script, format!("1 ICON \"{}\"\n", icon.display().to_string().replace('\\', "/"))).unwrap();
    let object = out.join("lighttouch-icon.o");
    let windres = env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    match Command::new(&windres).arg(&script).arg("-O").arg("coff").arg("-o").arg(&object).status() {
        Ok(status) if status.success() => println!("cargo:rustc-link-arg-bins={}", object.display()),
        result => println!("cargo:warning=no icon in the executable: {windres} failed ({result:?})"),
    }
}

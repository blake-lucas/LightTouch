//! Builds the C activation library (csrc/activation.c) into this crate with `-DLT_ACTIVATION_LIBRARY`. It is kept in
//! C, not rewritten in Rust, so the audited pattern matcher is the one tools/activation tests; it cross-compiles for
//! Windows (x86_64-w64-mingw32-gcc-posix) and Android.
//!
//! Compiles the vendored iBoot32Patcher (vendor/iBoot32Patcher, LukeZGD's fork at the commit pinned in
//! build-support/dependencies.json with iBoot32Patcher-ltm.patch already applied) into the crate, so the iPad's
//! real-iBoot chain patches iBoot in-process on every platform instead of shelling out to a binary nothing ships for
//! Linux/Windows/Android. Its `main` is renamed to `lt_iboot32_main` so k48iboot::patch_iboot can call it with the
//! same argv the separate tool got (`--rsa --debug -b <boot-args>`, two temp files); the output is byte-identical.
//! csrc/iboot32_quiet.{h,c} send its printf diagnostics to stderr, keeping stdout (create's JSON Lines) clean.

use std::path::PathBuf;

fn activation() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("csrc");
    let c = src.join("activation.c");
    let include = src.join("include");

    println!("cargo:rerun-if-changed={}", c.display());
    println!("cargo:rerun-if-changed={}", include.join("CActivation.h").display());

    cc::Build::new()
        .file(&c)
        .include(&include)
        .define("LT_ACTIVATION_LIBRARY", None)
        .warnings(false)
        .compile("cactivation");
}

fn iboot32patcher() {
    let dir = "vendor/iBoot32Patcher";
    let sources = ["finders.c", "functions.c", "iBoot32Patcher.c", "patchers.c"];
    let mut build = cc::Build::new();
    let quiet = "csrc/iboot32_quiet.h";
    if build.get_compiler().is_like_msvc() {
        build.flag(format!("/FI{quiet}"));
    } else {
        build.flag("-include").flag(quiet);
    }
    build
        .include(dir)
        .define("main", "lt_iboot32_main")
        // The upstream Makefile's flags; the tool's own multichar literals and intentional int<->pointer casts.
        .flag_if_supported("-Wno-multichar")
        .flag_if_supported("-Wno-int-conversion")
        .flag_if_supported("-Wno-pointer-to-int-cast")
        .flag_if_supported("-Wno-int-to-pointer-cast")
        // Its arm32 instruction structs are packed bit-fields; GCC's compat note is just noise here.
        .flag_if_supported("-Wno-packed-bitfield-compat")
        .warnings(false)
        .file("csrc/iboot32_quiet.c");
    for s in sources {
        build.file(format!("{dir}/src/{s}"));
        println!("cargo:rerun-if-changed={dir}/src/{s}");
    }
    for h in ["arm32_defs.h", "finders.h", "functions.h", "iBoot32Patcher.h", "patchers.h", "rsa_result.h"] {
        println!("cargo:rerun-if-changed={dir}/include/{h}");
    }
    println!("cargo:rerun-if-changed=csrc/iboot32_quiet.h");
    println!("cargo:rerun-if-changed=csrc/iboot32_quiet.c");
    build.compile("iboot32patcher");
}

fn main() {
    activation();
    iboot32patcher();
}

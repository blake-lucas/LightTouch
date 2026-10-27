// Compiles the shared C++ core in Portable/ (the same sources Android and the Linux frontend use) into a static
// library. zlib comes from libz-sys, built from source, so every target links the same version. The guest web proxy
// (Portable/network) needs OpenSSL 3's headers, which openssl-sys passes here (system or vendored, see Cargo.toml).
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let portable = manifest.join("../../Portable");
    let engine_include = match env::var_os("LIGHTTOUCH_QEMU_SOURCE") {
        Some(source) => {
            let source = PathBuf::from(source);
            vec![source.join("contrib/ios-app"), source.join("contrib/macos-app")]
        }
        None => vec![manifest.join("engine-include")],
    };
    println!("cargo:rerun-if-env-changed=LIGHTTOUCH_QEMU_SOURCE");
    println!("cargo:rerun-if-changed={}", portable.join("src").display());
    println!("cargo:rerun-if-changed={}", portable.join("include").display());
    println!("cargo:rerun-if-changed={}", portable.join("network").display());
    println!("cargo:rerun-if-changed=src/network.cpp");
    println!("cargo:rerun-if-changed=engine-include");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(portable.join("include"))
        .includes(&engine_include)
        .warnings(false)
        // Full prepared-base integrity reads run in debug builds too (see Portable/CMakeLists.txt).
        .opt_level(2)
        .debug(false);
    if let Some(zlib) = env::var_os("DEP_Z_INCLUDE") {
        build.include(zlib);
    }
    for source in ["Storage.cpp", "Runtime.cpp", "GuestPackage.cpp", "Library.cpp", "Screenshot.cpp"] {
        build.file(portable.join("src").join(source));
    }
    // A system OpenSSL in /usr/include is already on the compiler's path; naming it again with -I would put it
    // ahead of the C++ library's own wrappers of the C headers.
    let openssl = PathBuf::from(env::var_os("DEP_OPENSSL_INCLUDE").expect("openssl-sys reports its include folder"));
    if openssl != std::path::Path::new("/usr/include") {
        build.include(openssl);
    }
    build
        .include(portable.join("network"))
        .file(portable.join("network/Network.cpp"))
        .file(manifest.join("src/network.cpp"));
    if target_os == "windows" {
        build.define("WIN32_LEAN_AND_MEAN", None).define("NOMINMAX", None);
        // MinGW's C++ runtime goes into the executable, so nothing has to ship beside it.
        build.cpp_link_stdlib(None);
    }
    build.compile("lighttouch_portable");
    cc::Build::new().file(manifest.join("src/layout.c")).include(portable.join("include")).compile("lighttouch_layout");

    match target_os.as_str() {
        "linux" | "android" => println!("cargo:rustc-link-lib=dl"),
        "windows" => {
            println!("cargo:rustc-link-lib=static:-bundle=stdc++");
            println!("cargo:rustc-link-lib=static:-bundle=winpthread");
            for lib in ["bcrypt", "kernel32", "advapi32", "ws2_32", "crypt32"] {
                println!("cargo:rustc-link-lib={lib}");
            }
        }
        _ => {}
    }
}

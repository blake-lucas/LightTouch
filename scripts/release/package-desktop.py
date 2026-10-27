#!/usr/bin/env python3
"""Assemble the Rust desktop app for Linux, Windows or macOS, with every component's notices for its licence page.

    package-desktop.py OUT --os linux|windows|macos --app BIN --firmwarekit BIN --engine LIB --usbmuxd BIN
        --guest-tools DIR [--lib FILE_OR_DIR]... [--notices GROUP=DIR]... [--group-source GROUP=URL]... [--source-reference URL] [--zip] [--appimage TOOLS]

OUT/                    lighttouch[.exe]; Linux also gets LightTouch, a launcher that puts lib/ on the library path
OUT/resources/          the engine, firmwarekit, the USB bridge, guest-tools/ (and on Windows, every --lib DLL)
OUT/lib/                (Linux) the --lib shared libraries the engine and the USB bridge need
OUT/resources/licenses/ frontend/ and firmware/ (stage-notices.py for the app and firmwarekit on this target),
                        plus each --notices GROUP (engine, usb), copied with a manifest.json in the schema
                        lighttouch_model::notices reads; its source link is --group-source, else
                        --source-reference (LightTouch's)

macOS (run on a Mac; OUT is LightTouch.app): the app in Contents/MacOS, the resources in Contents/Resources, and the
non-system dylibs the engine and the USB bridge load (Homebrew's) copied into Contents/Frameworks with their install
names rewritten; each one's licence files from its Homebrew keg join the engine group's notices. The bundle is signed
ad hoc.

The binaries are built beforehand: the app and firmwarekit with `cargo build --release` (packaged builds use
--features lighttouch-services/vendored-openssl, so the app needs no system OpenSSL), the engine with qemu-ios's
contrib/portable scripts, the USB bridge with its own build (scripts/release/build-desktop-linux.sh builds the Linux app
against glibc 2.31). --zip also writes OUT.zip. --appimage (Linux) also writes OUT.AppImage: an AppDir with the app in
usr/bin, the resources in usr/lib/lighttouch and the libraries in usr/lib, sealed by Linux/packaging/seal-appimage.py
with the pinned appimagetool and runtime in TOOLS.
"""
import argparse, hashlib, json, re, shutil, subprocess, sys, zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
NAME = re.compile(r"[A-Za-z0-9._-]+")  # what the licence page accepts in a notice path
TRIPLES = {"linux": "x86_64-unknown-linux-gnu", "windows": "x86_64-pc-windows-gnu", "macos": "aarch64-apple-darwin"}
INFO_PLIST = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>
	<string>lighttouch</string>
	<key>CFBundleIdentifier</key>
	<string>org.lighttouch.LightTouch</string>
	<key>CFBundleName</key>
	<string>LightTouch</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
"""
APPRUN = """#!/bin/sh
# The bundled libraries (libfdt, libimobiledevice) come after the system's; resources are found beside usr/bin.
APPDIR=${APPDIR:-$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)}
export LD_LIBRARY_PATH="$APPDIR/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$APPDIR/usr/bin/lighttouch" "$@"
"""
DESKTOP = """[Desktop Entry]
Type=Application
Name=LightTouch
Comment=Run iPod touch and iPhone systems from an IPSW
Exec=AppRun
Icon=lighttouch
Terminal=false
Categories=System;Emulator;
StartupNotify=true
"""
LAUNCHER = """#!/bin/sh
# Puts the bundled libraries (libfdt, libimobiledevice) after the system's on the library path.
HERE=$(dirname "$(readlink -f "$0")")
export LD_LIBRARY_PATH="$HERE/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/lighttouch" "$@"
"""


def copy(src, dst, mode=None):
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)
    if mode is not None:
        dst.chmod(mode)


def notices(group_dir, source, source_reference):
    """Copy a notice folder in, naming files the licence page accepts, and write its manifest."""
    if group_dir.exists():
        shutil.rmtree(group_dir)
    files = []
    for f in sorted(p for p in source.rglob("*") if p.is_file() and p.name != "manifest.json"):
        rel = "/".join(re.sub(r"[^A-Za-z0-9._-]", "_", part) for part in f.relative_to(source).parts)
        copy(f, group_dir / rel)
        files.append({"path": rel, "sha256": hashlib.sha256(f.read_bytes()).hexdigest()})
    if not files:
        raise SystemExit(f"package-desktop: no notices in {source}")
    manifest = {"schema": 1, "source_reference": source_reference, "files": files}
    (group_dir / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


def main():
    a = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    a.add_argument("out", type=Path)
    a.add_argument("--os", required=True, choices=sorted(TRIPLES))
    a.add_argument("--app", type=Path, required=True)
    a.add_argument("--firmwarekit", type=Path, required=True)
    a.add_argument("--engine", type=Path, required=True)
    a.add_argument("--usbmuxd", type=Path, required=True)
    a.add_argument("--guest-tools", type=Path, required=True)
    a.add_argument("--lib", type=Path, action="append", default=[], help="a library, or a folder of them")
    a.add_argument("--notices", action="append", default=[], metavar="GROUP=DIR")
    a.add_argument("--group-source", action="append", default=[], metavar="GROUP=URL")
    a.add_argument("--source-reference", default="https://github.com/blake-lucas/LightTouch")
    a.add_argument("--zip", action="store_true")
    a.add_argument("--appimage", type=Path, metavar="TOOLS", help="appimagetool-x86_64.AppImage and runtime-x86_64")
    args = a.parse_args()
    windows, macos = args.os == "windows", args.os == "macos"
    exe = ".exe" if windows else ""

    if args.out.exists():
        shutil.rmtree(args.out)
    res = args.out / ("Contents/Resources" if macos else "resources")
    copy(args.app, args.out / ("Contents/MacOS/lighttouch" if macos else f"lighttouch{exe}"), 0o755)
    copy(args.firmwarekit, res / f"firmwarekit{exe}", 0o755)
    engine = {"windows": "libqemu-arm.dll", "macos": "libqemu-arm.dylib"}.get(args.os, "libqemu-arm.so")
    copy(args.engine, res / engine)
    copy(args.usbmuxd, res / ("lighttouch-usbmuxd.exe" if windows else "usbmuxd-qemu"), 0o755)
    shutil.copytree(args.guest_tools, res / "guest-tools")
    libs = res if windows else args.out / ("Contents/Frameworks" if macos else "lib")
    given_from = {}
    for lib in args.lib:
        for f in sorted(lib.iterdir()) if lib.is_dir() else [lib]:
            if f.is_file():
                earlier = given_from.get(f.name.lower())
                if earlier and earlier.read_bytes() != f.resolve().read_bytes():
                    print(f"package-desktop: {f.name} from {f} replaces the one from {earlier}", file=sys.stderr)
                given_from[f.name.lower()] = f.resolve()
                copy(f.resolve(), libs / f.name)
    kegs = []
    if macos:
        version = re.search(r'^version = "(.+)"', (REPO / "Cargo.toml").read_text(), re.M).group(1)
        (args.out / "Contents/Info.plist").write_text(INFO_PLIST.replace("VERSION", version))
        given = sorted(libs.iterdir()) if libs.is_dir() else []
        kegs = bundle_dylibs([res / engine, res / "usbmuxd-qemu", *given], libs)
    elif windows:
        check_imports([args.out / "lighttouch.exe", *res.glob("*.exe"), *res.glob("*.dll")])
    else:
        (args.out / "LightTouch").write_text(LAUNCHER)
        (args.out / "LightTouch").chmod(0o755)

    licenses = res / "licenses"
    stage = REPO / "crates/lighttouch-firmware/stage-notices.py"
    for group, package, features in [("frontend", "lighttouch", "lighttouch-services/vendored-openssl"),
                                     ("firmware", "lighttouch-firmware", "")]:
        command = [sys.executable, str(stage), str(licenses / group), "--package", package,
                   "--target", TRIPLES[args.os], "--source-reference", args.source_reference]
        subprocess.run(command + (["--features", features] if features else []), check=True)
    sources = dict(spec.split("=", 1) for spec in args.group_source)
    groups = {}
    for spec in args.notices:
        group, _, folder = spec.partition("=")
        if not NAME.fullmatch(group) or not folder:
            raise SystemExit(f"package-desktop: --notices wants GROUP=DIR, not {spec}")
        groups[group] = Path(folder)
    if kegs:
        # The bundled Homebrew libraries' own licence files, gathered beside any engine notices given.
        merged = args.out.parent / (args.out.name + ".engine-notices")
        if merged.exists():
            shutil.rmtree(merged)
        if "engine" in groups:
            shutil.copytree(groups["engine"], merged)
        for name, keg in kegs:
            for f in sorted(keg.iterdir()):
                # Also SPDX-named texts, as glib's LGPL-2.1-or-later.txt.
                if f.is_file() and re.match(r"(LICEN[SC]E|COPYING|NOTICE|COPYRIGHT|.*(GPL|MIT|BSD|Apache|MPL).*\.txt$)",
                                            f.name, re.I):
                    copy(f, merged / name / f.name)
        groups["engine"] = merged
    for group, folder in groups.items():
        notices(licenses / group, folder, sources.get(group, args.source_reference))
    if kegs:
        shutil.rmtree(args.out.parent / (args.out.name + ".engine-notices"))
        subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(args.out)], check=True)

    if args.zip:
        archive = args.out.parent / (args.out.name + ".zip")
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as z:
            for f in sorted(args.out.rglob("*")):
                if f.is_file():
                    info = zipfile.ZipInfo.from_file(f, Path(args.out.name) / f.relative_to(args.out))
                    with f.open("rb") as s:
                        z.writestr(info, s.read(), zipfile.ZIP_DEFLATED)
        print(archive)
    if args.appimage:
        if windows:
            raise SystemExit("package-desktop: --appimage is for Linux")
        print(appimage(args.out, args.appimage))
    print(args.out)


def check_imports(binaries):
    """Fail when a bundled DLL imports a function another bundled DLL doesn't export: Windows refuses to load the
    engine then ("The specified procedure could not be found"), where Wine may find another copy. Two libwinpthread
    builds (the engine's MSYS2 one and Debian MinGW's) did this."""
    objdump = shutil.which("x86_64-w64-mingw32-objdump")
    if not objdump:
        print("package-desktop: no x86_64-w64-mingw32-objdump, DLL imports not checked", file=sys.stderr)
        return
    imports, exports = {}, {}
    for path in binaries:
        dll, table, own = None, False, set()
        for line in subprocess.run([objdump, "-p", str(path)], capture_output=True, text=True, check=True).stdout.splitlines():
            if m := re.match(r"\s*DLL Name: (\S+)", line):
                dll = m.group(1).lower()
            elif dll and (m := re.match(r"\s+[0-9a-f]+\s+\d+\s+(\S+)$", line)):
                imports.setdefault(path.name, set()).add((dll, m.group(1)))
            elif "[Ordinal/Name Pointer] Table" in line:
                dll, table = None, True
            elif table and (m := re.match(r"\s*\[\s*\d+\]\s+(\S+)$", line)):
                own.add(m.group(1))
        exports[path.name.lower()] = own
    missing = sorted(f"{name} needs {dll}!{symbol}" for name, wanted in imports.items() for dll, symbol in wanted
                     if dll in exports and symbol not in exports[dll])
    if missing:
        raise SystemExit("package-desktop: bundled DLLs don't fit together:\n  " + "\n  ".join(missing))


def bundle_dylibs(binaries, frameworks):
    """Copy the non-system dylibs `binaries` load (recursively) into `frameworks`, point every load command at the
    copies, and return (name, folder) for the licence files of what they came from: Homebrew kegs, or the
    share/licenses/<name> folders of another prefix."""
    def deps(path):
        lines = subprocess.check_output(["otool", "-L", str(path)], text=True).splitlines()
        # Universal binaries list each architecture under its own "FILE (architecture X):" header.
        names = {line.strip().split(" (")[0] for line in lines if line.startswith("\t")}
        return [n for n in names if not n.startswith(("/usr/lib/", "/System/", "@"))]

    kegs, todo, done = set(), list(binaries), set()
    while todo:
        path = todo.pop()
        for dep in deps(path):
            real = Path(dep).resolve()
            local = frameworks / Path(dep).name
            if not local.exists():
                copy(real, local, 0o644)
                subprocess.run(["install_name_tool", "-id", "@rpath/" + local.name, str(local)], check=True,
                               stderr=subprocess.DEVNULL)
                todo.append(local)
                parts = real.parts
                if "Cellar" in parts:
                    i = parts.index("Cellar")
                    kegs.add((parts[i + 1], Path(*parts[: i + 3])))
                else:
                    # A prefix built for the engine (build-engine.py --ffmpeg-archive) keeps them in share/licenses.
                    shared = real.parent.parent / "share/licenses"
                    for d in sorted(shared.iterdir()) if shared.is_dir() else []:
                        kegs.add((d.name, d))
            prefix = "@loader_path/" if path.parent == frameworks else "@loader_path/../Frameworks/"
            subprocess.run(["install_name_tool", "-change", dep, prefix + local.name, str(path)], check=True,
                           stderr=subprocess.DEVNULL)
        done.add(path)
    return sorted(kegs)


def appimage(out, tools):
    """OUT.AppImage from the folder in OUT, through the release pipeline's sealer."""
    work = out.parent / (out.name + ".appimage-work")
    if work.exists():
        shutil.rmtree(work)
    appdir = work / "LightTouch.AppDir"
    copy(out / "lighttouch", appdir / "usr/bin/lighttouch", 0o755)
    shutil.copytree(out / "resources", appdir / "usr/lib/lighttouch")
    if (out / "lib").is_dir():
        for f in (out / "lib").iterdir():
            copy(f, appdir / "usr/lib" / f.name)
    (appdir / "AppRun").write_text(APPRUN)
    (appdir / "AppRun").chmod(0o755)
    (appdir / "lighttouch.desktop").write_text(DESKTOP)
    copy(REPO / "Linux/packaging/lighttouch.svg", appdir / "lighttouch.svg")
    files = {str(f.relative_to(appdir)): hashlib.sha256(f.read_bytes()).hexdigest()
             for f in sorted(appdir.rglob("*")) if f.is_file()}
    (appdir / "runtime-manifest.json").write_text(json.dumps({"schema": 1, "files": files}, indent=2) + "\n")
    image = out.parent / (out.name + ".AppImage")
    if image.exists():
        image.unlink()
    subprocess.run([sys.executable, str(REPO / "Linux/packaging/seal-appimage.py"), "--appdir", str(appdir),
                    "--tool", str(tools / "appimagetool-x86_64.AppImage"), "--runtime", str(tools / "runtime-x86_64"),
                    "--output", str(image), "--evidence", str(work / "seal")], check=True, stdout=subprocess.DEVNULL)
    shutil.rmtree(work)
    return image


if __name__ == "__main__":
    main()

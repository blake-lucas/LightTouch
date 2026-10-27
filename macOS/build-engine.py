#!/usr/bin/env python3
"""Build a native macOS TCG/CGL engine from an existing qemu-ios checkout.

This recipe uses native Homebrew dependencies; scripts/release/package-desktop.py
later copies and relocates their dylib closure into LightTouch.app. No guest is run.
Use a memory/time supervisor on shared hosts. Requires ninja, meson, jq,
pkg-config, glib, pixman, libslirp, nettle, OpenSSL3 and the macOS Command Line Tools.
QEMU has no built-in AES any more: without nettle the guest's AES engine (the
4.x+ data-protection keybag boot) fails with "no crypto library enabled".
With --ffmpeg-archive the engine links an FFmpeg built here with only the
decoders it uses (aac, mp3, alac, h264), as the Windows build does, instead of
Homebrew's, which brings dozens of encoder and codec libraries along.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--qemu-source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--python", type=Path, required=True)
    parser.add_argument("--openssl-prefix", type=Path, required=True)
    parser.add_argument("--jobs", type=int, choices=[1, 2], default=2)
    parser.add_argument("--archive-revision", help="exact Git revision when using a recorded source archive")
    parser.add_argument("--ffmpeg-archive", type=Path, help="FFmpeg source tarball, built decoder-only into OUTPUT/codec")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("native macOS compiler/SDK required")
    source, output = args.qemu_source.resolve(), args.output.absolute()
    if output.exists() or output.is_symlink():
        parser.error("--output must be absent; existing build directories are preserved")
    if not (source / "contrib/macos-app/make-dylib-macos.sh").is_file():
        parser.error("qemu-ios source lacks its native macOS dylib adapter")
    if args.archive_revision:
        revision = args.archive_revision
    else:
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
        if subprocess.check_output(["git", "status", "--porcelain"], cwd=source, text=True):
            parser.error("commit source changes before building a recorded engine")
    output.mkdir(parents=True)
    prefix = args.openssl_prefix.resolve()
    env = dict(os.environ)
    env["PKG_CONFIG_PATH"] = str(prefix / "lib/pkgconfig") + ":" + env.get("PKG_CONFIG_PATH", "")
    commands = []
    if args.ffmpeg_archive:
        codec, ffmpeg = output / "codec", output / "ffmpeg-src"
        ffmpeg.mkdir()
        env["PKG_CONFIG_PATH"] = str(codec / "lib/pkgconfig") + ":" + env["PKG_CONFIG_PATH"]
        commands += [
            ["tar", "-xf", str(args.ffmpeg_archive.resolve()), "-C", str(ffmpeg), "--strip-components=1"],
            ["sh", "-c", "cd ffmpeg-src && ./configure --prefix=" + str(codec) +
             " --disable-everything --disable-autodetect --disable-programs --disable-doc"
             " --disable-avdevice --disable-avformat --disable-avfilter --disable-swscale --disable-swresample"
             " --enable-decoder=aac,mp3,alac,h264 --enable-shared --disable-static"
             " --install-name-dir=" + str(codec / "lib") +
             " && make -j" + str(args.jobs) + " && make install"
             " && mkdir -p " + str(codec / "share/licenses/ffmpeg") +
             " && cp LICENSE.md COPYING.LGPLv2.1 " + str(codec / "share/licenses/ffmpeg")],
        ]
    commands += [
        [str(source / "configure"), "--target-list=arm-softmmu", "--without-default-features",
         "--without-default-devices", "--with-devices-arm=portable", "--enable-tcg",
         "--enable-pixman", "--enable-slirp", "--disable-gnutls", "--enable-nettle", "--disable-docs", "--disable-tools",
         "--disable-werror", "--disable-pie", "--python=" + str(args.python.resolve()), "--audio-drv-list=",
         "--extra-cflags=-I" + str(prefix / "include"),
         "--extra-ldflags=-L" + str(prefix / "lib") + " -lcrypto"],
        ["ninja", "-j" + str(args.jobs), "qemu-system-arm"],
        ["bash", str(source / "contrib/macos-app/make-dylib-macos.sh"), str(output)],
    ]
    receipt = {"source_revision": revision, "argv": commands,
               "cpu_backend": "QEMU TCG", "guest_graphics_backend": "native macOS CGL legacy OpenGL",
               "guest_tested": False}
    try:
        for index, command in enumerate(commands):
            with (output / ("step-%d.log" % index)).open("w") as log:
                subprocess.run(command, cwd=output, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
        engine = output / "libqemu-arm.dylib"
        receipt["engine_sha256"] = hashlib.sha256(engine.read_bytes()).hexdigest()
        receipt["linked_libraries"] = subprocess.check_output(["otool", "-L", str(engine)], text=True)
        receipt["result"] = "compiled; no guest acceptance"
    except BaseException as exc:
        receipt["result"] = "failed"
        receipt["failure"] = str(exc)
        raise
    finally:
        (output / "native-engine-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(engine)


if __name__ == "__main__":
    main()

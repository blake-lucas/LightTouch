#!/usr/bin/env bash
# Linux -> Windows x86_64/MSVCRT, current engine + hardware-only WGL.
# Caller owns resource containment; never run simultaneous engine builds.
set -euo pipefail
qemu=$(realpath "${1:?usage: build-engine.sh QEMU_SOURCE DEPENDENCIES OUTPUT FFMPEG_ARCHIVE}")
deps=$(realpath "${2:?dependencies root}")
out=$(realpath -m "${3:?fresh output root}")
archive=$(realpath "${4:?verified ffmpeg9.0.1 archive}")
if [[ -e "$out" ]]; then echo "Refusing existing output: $out" >&2; exit 2; fi
if [[ -n $(git -C "$qemu" status --porcelain) ]]; then echo 'Commit engine source first' >&2; exit 2; fi
mkdir -p "$out"
python3 - "$archive" <<'PY'
import hashlib,sys
expected='cf38e0e28c7e5605942c4a77755349b0145804a397af37eb1fb4c77cb237f635'
assert hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest()==expected,'FFmpeg checksum mismatch'
PY
sysroot="$deps/sysroot"
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$sysroot/codec/lib/pkgconfig:$sysroot/mingw64/lib/pkgconfig"
export PKG_CONFIG_PATH=''
cat > "$out/target-pkg-config" <<'EOF'
#!/bin/sh
exec /usr/bin/pkg-config "$@"
EOF
chmod +x "$out/target-pkg-config"
export PKG_CONFIG="$out/target-pkg-config"
tar --no-same-owner -xf "$archive" -C "$out"
cd "$out/ffmpeg-9.0.1"
patch -p1 < "$qemu/contrib/ffmpeg/h264-chunk-er.patch"
patch -p1 < "$qemu/contrib/ffmpeg/h264-cavlc-pcm-offset.patch"
./configure --prefix=/codec --target-os=mingw32 --arch=x86_64 --enable-cross-compile \
  --cross-prefix=x86_64-w64-mingw32- --cc=x86_64-w64-mingw32-gcc-posix --cxx=x86_64-w64-mingw32-g++-posix \
  --disable-everything --disable-autodetect --disable-programs --disable-doc --disable-x86asm \
  --disable-avdevice --disable-avformat --disable-avfilter --disable-swscale --disable-swresample \
  --enable-decoder=aac,mp3,alac,h264 --enable-shared --disable-static
make -j"${LT_JOBS:-$(nproc)}"
make DESTDIR="$sysroot" install
mkdir "$out/engine"
cd "$out/engine"
"$qemu/configure" --target-list=arm-softmmu --without-default-features --without-default-devices \
  --with-devices-arm=portable --cross-prefix=x86_64-w64-mingw32- \
  --cc=x86_64-w64-mingw32-gcc-posix --cxx=x86_64-w64-mingw32-g++-posix \
  --host-cc=/usr/bin/cc --python=/usr/bin/python3 --enable-tcg --enable-pixman --enable-gcrypt \
  --enable-sdl --enable-slirp --disable-docs --disable-tools --disable-guest-agent --disable-plugins \
  --disable-werror --disable-pie --disable-containers --audio-drv-list=sdl \
  --extra-cflags="-I$sysroot/mingw64/include" --extra-ldflags="-L$sysroot/mingw64/lib" \
  -Dcpp_std=gnu++17
ninja -j"${LT_JOBS:-$(nproc)}" -d keeprsp qemu-system-arm.exe
x86_64-w64-mingw32-objdump -p qemu-system-arm.exe > "$out/PE-IMPORTS.txt"
sha256sum qemu-system-arm.exe > "$out/ENGINE-SHA256.txt"

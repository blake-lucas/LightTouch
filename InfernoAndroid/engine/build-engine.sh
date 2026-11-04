#!/usr/bin/env bash
# Builds Inferno (MakrSas/Inferno, branch ios: ChefKiss Inferno plus the
# embedding API in include/ui/inferno-embed.h) as libqemu-aarch64-softmmu.so
# for Android arm64, against the dependencies build-deps.sh put in $OUT.
#
#   ANDROID_NDK_HOME=/opt/android/android-ndk-r28c InfernoAndroid/engine/build-engine.sh
#
# The source is pinned to INFERNO_COMMIT and the files in patches/ are applied
# on top, in order. Output: $OUT/engine/libqemu-aarch64-softmmu.so (stripped)
# and $OUT/engine/qemu-bundle (the firmware/keymap data QEMU loads with -L).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME}"
: "${OUT:=$HERE/../build}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
: "${JOBS:=$(nproc)}"
: "${MESON:=meson}"
INFERNO_URL=https://github.com/MakrSas/Inferno.git
INFERNO_COMMIT=b76fa575831d94a80db169ffb3c76d9094a648bd
SRC="$OUT/inferno-src"
BUILD="$OUT/inferno-build"
CROSS="$OUT/cross-android-arm64.txt"
PREFIX="$OUT/deps-prefix"
[ -f "$CROSS" ] || { echo "run build-deps.sh first ($CROSS missing)" >&2; exit 1; }
export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig" PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"

if [ "$(git -C "$SRC" rev-parse HEAD 2>/dev/null)" != "$INFERNO_COMMIT" ] || [ -n "${REFRESH:-}" ]; then
    rm -rf "$SRC" "$BUILD"
    git init -q "$SRC"
    git -C "$SRC" fetch -q --depth 1 "$INFERNO_URL" "$INFERNO_COMMIT"
    git -C "$SRC" -c advice.detachedHead=false checkout -q FETCH_HEAD
    for p in "$HERE"/patches/*.patch; do
        [ -e "$p" ] || continue
        echo "== applying $(basename "$p")"
        git -C "$SRC" apply --whitespace=nowarn "$p"
    done
fi

if [ ! -f "$BUILD/build.ninja" ]; then
    "$MESON" setup "$BUILD" "$SRC" --cross-file "$CROSS" \
        -Dbuildtype=release -Dprefix=/usr/local -Dshared_lib=true -Db_staticpic=true -Dwerror=false \
        -Dkvm=disabled -Dhvf=disabled -Dwhpx=disabled \
        -Dcocoa=disabled -Dgtk=disabled -Dsdl=disabled -Dcurses=disabled \
        -Dcurl=disabled -Dlibssh=disabled -Dbzip2=disabled -Dpng=enabled \
        -Dvnc=enabled -Dvnc_jpeg=disabled -Dvnc_sasl=disabled \
        -Dtools=disabled -Dslirp=enabled \
        > "$OUT/inferno-meson.log" 2>&1 || { tail -n 40 "$OUT/inferno-meson.log" >&2; exit 1; }
fi
ninja -C "$BUILD" -j"$JOBS" > "$OUT/inferno-ninja.log" 2>&1 || { grep -m30 -B2 -A8 "error:" "$OUT/inferno-ninja.log" >&2; exit 1; }

LIB=$(find "$BUILD" -maxdepth 1 -name 'libqemu-aarch64-softmmu*.so' | head -1)
[ -n "$LIB" ] || { echo "no libqemu-aarch64-softmmu.so in $BUILD" >&2; ls "$BUILD" >&2; exit 1; }
mkdir -p "$OUT/engine"
"$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip" --strip-unneeded -o "$OUT/engine/libqemu-aarch64-softmmu.so" "$LIB"
rm -rf "$OUT/engine/qemu-bundle"
DESTDIR="$OUT/engine/qemu-bundle" "$MESON" install -C "$BUILD" --no-rebuild --tags runtime > /dev/null 2>&1 || true
echo "== $OUT/engine/libqemu-aarch64-softmmu.so ($(du -h "$OUT/engine/libqemu-aarch64-softmmu.so" | cut -f1))"

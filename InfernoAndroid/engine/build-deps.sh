#!/usr/bin/env bash
# Builds Inferno's host dependencies for Android arm64 with the NDK, as static
# PIC libraries in $PREFIX, and writes the meson cross file the engine build
# uses ($OUT/cross-android-arm64.txt).
#
#   ANDROID_NDK_HOME=/opt/android/android-ndk-r28c InfernoAndroid/engine/build-deps.sh
#
# Every source is pinned: tarballs by SHA-256, git checkouts by commit. GitHub
# release archives are fetched as git tags (the same bytes the tag names), so
# the script also works where only git reaches GitHub.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to an NDK r28 or later}"
: "${API:=29}"
: "${OUT:=$HERE/../build}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
: "${JOBS:=$(nproc)}"
: "${MESON:=meson}"
SRC="$OUT/deps-src"
PREFIX="$OUT/deps-prefix"
CROSS="$OUT/cross-android-arm64.txt"
mkdir -p "$SRC" "$PREFIX"

TC="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
TRIPLE=aarch64-linux-android
export CC="$TC/$TRIPLE$API-clang" CXX="$TC/$TRIPLE$API-clang++"
export AR="$TC/llvm-ar" RANLIB="$TC/llvm-ranlib" STRIP="$TC/llvm-strip" NM="$TC/llvm-nm"
export CFLAGS="-O2 -fPIC" CXXFLAGS="-O2 -fPIC"
export LDFLAGS="-Wl,-z,max-page-size=16384"
export PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig"   # never the build machine's .pc files
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
[ -x "$CC" ] || { echo "no NDK clang at $CC" >&2; exit 1; }

log() { printf '== %s\n' "$*"; }
done_mark() { [ -e "$PREFIX/.done-$1" ]; }
mark() { touch "$PREFIX/.done-$1"; }

# fetch NAME URL SHA256: a tarball, checked, unpacked into $SRC/NAME
fetch() {
    local name=$1 url=$2 sum=$3 file="$SRC/$(basename "$2")"
    [ -s "$file" ] || curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 -o "$file" "$url"
    echo "$sum  $file" | sha256sum -c --quiet - || { echo "checksum mismatch: $file" >&2; exit 1; }
    rm -rf "$SRC/$name"; mkdir -p "$SRC/$name"
    tar xf "$file" -C "$SRC/$name" --strip-components=1
}
# gitpin NAME URL TAG COMMIT: a shallow checkout of TAG that must be COMMIT
gitpin() {
    local name=$1 url=$2 tag=$3 commit=$4 dir="${5:-$SRC/$1}"
    rm -rf "$dir"
    git -c advice.detachedHead=false clone -q --depth 1 --branch "$tag" "$url" "$dir"
    [ "$(git -C "$dir" rev-parse HEAD)" = "$commit" ] || { echo "$name: $tag is not $commit" >&2; exit 1; }
}
autotools() { # autotools DIR [configure args...]
    local dir=$1; shift
    (cd "$dir" && ./configure --host=$TRIPLE --prefix="$PREFIX" --enable-static --disable-shared --with-pic "$@" \
        > configure.log 2>&1 && make -j"$JOBS" > make.log 2>&1 && make install > install.log 2>&1) ||
        { tail -n 30 "$dir"/*.log >&2; exit 1; }
}
mesonbuild() { # mesonbuild DIR [meson args...]
    local dir=$1; shift
    rm -rf "$dir/_b"
    "$MESON" setup "$dir/_b" "$dir" --cross-file "$CROSS" --prefix "$PREFIX" --libdir lib \
        --default-library static --buildtype release -Db_staticpic=true "$@" > "$dir/meson.log" 2>&1 &&
        ninja -C "$dir/_b" -j"$JOBS" install > "$dir/ninja.log" 2>&1 || { tail -n 40 "$dir"/*.log >&2; exit 1; }
}

cat > "$CROSS" <<EOF
# Meson cross file: Android arm64, API $API, NDK at $ANDROID_NDK_HOME. Written by build-deps.sh.
[binaries]
c = '$CC'
cpp = '$CXX'
ar = '$AR'
strip = '$STRIP'
ranlib = '$RANLIB'
nm = '$NM'
pkg-config = 'pkg-config'

[properties]
pkg_config_libdir = '$PREFIX/lib/pkgconfig'

[built-in options]
c_args = ['-fPIC', '-I$PREFIX/include']
cpp_args = ['-fPIC', '-I$PREFIX/include']
c_link_args = ['-L$PREFIX/lib', '-Wl,-z,max-page-size=16384']
cpp_link_args = ['-L$PREFIX/lib', '-Wl,-z,max-page-size=16384']

[cmake]
CMAKE_ANDROID_NDK = '$ANDROID_NDK_HOME'
CMAKE_ANDROID_API = '$API'
CMAKE_ANDROID_ARCH_ABI = 'arm64-v8a'
CMAKE_MAKE_PROGRAM = '$(command -v ninja)'

[host_machine]
system = 'android'
cpu_family = 'aarch64'
cpu = 'armv8-a'
endian = 'little'
EOF

# zlib is the NDK's own (libz.so, a stable NDK API); libpng's .pc asks for it by name.
mkdir -p "$PREFIX/lib/pkgconfig"
cat > "$PREFIX/lib/pkgconfig/zlib.pc" <<PC
Name: zlib
Description: the NDK's system zlib
Version: 1.3.0
Libs: -lz
Cflags:
PC

if ! done_mark gmp; then
    log gmp
    fetch gmp https://ftp.gnu.org/gnu/gmp/gmp-6.3.0.tar.xz a3c2b80201b89e68616f4ad30bc66aee4927c3ce50e33929ca819d5c43538898
    autotools "$SRC/gmp"
    mark gmp
fi
if ! done_mark nettle; then
    log nettle
    fetch nettle https://ftp.gnu.org/gnu/nettle/nettle-3.10.2.tar.gz fe9ff51cb1f2abb5e65a6b8c10a92da0ab5ab6eaf26e7fc2b675c45f1fb519b5
    autotools "$SRC/nettle" --disable-documentation --disable-openssl --libdir="$PREFIX/lib" \
        CPPFLAGS="-I$PREFIX/include" LDFLAGS="$LDFLAGS -L$PREFIX/lib"
    mark nettle
fi
if ! done_mark tasn1; then
    log libtasn1
    fetch libtasn1 https://ftp.gnu.org/gnu/libtasn1/libtasn1-4.20.0.tar.gz 92e0e3bd4c02d4aeee76036b2ddd83f0c732ba4cda5cb71d583272b23587a76c
    autotools "$SRC/libtasn1" --disable-doc --disable-valgrind-tests
    mark tasn1
fi
if ! done_mark lzfse; then
    log lzfse
    gitpin lzfse https://github.com/lzfse/lzfse.git lzfse-1.0 88e2d2788b4021d0b2eb9fe2d97352ae9190f128
    (cd "$SRC/lzfse" && for c in src/*.c; do [ "$c" = src/lzfse_main.c ] && continue; "$CC" $CFLAGS -Isrc -c "$c" -o "${c%.c}.o"; done &&
        "$AR" rcs liblzfse.a src/lzfse_decode.o src/lzfse_decode_base.o src/lzfse_encode.o src/lzfse_encode_base.o \
            src/lzfse_fse.o src/lzvn_decode_base.o src/lzvn_encode_base.o &&
        install -D -m644 liblzfse.a "$PREFIX/lib/liblzfse.a" && install -D -m644 src/lzfse.h "$PREFIX/include/lzfse.h")
    mark lzfse
fi
if ! done_mark pixman; then
    log pixman
    fetch pixman https://www.cairographics.org/releases/pixman-0.44.2.tar.gz 6349061ce1a338ab6952b92194d1b0377472244208d47ff25bef86fc71973466
    mesonbuild "$SRC/pixman" -Dtests=disabled -Ddemos=disabled -Dgtk=disabled -Dlibpng=disabled -Da64-neon=enabled
    mark pixman
fi
if ! done_mark glib; then
    log glib
    fetch glib https://download.gnome.org/sources/glib/2.84/glib-2.84.3.tar.xz aa4f87c3225bf57ca85f320888f7484901a17934ca37023c3bd8435a72db863e
    # pcre2's wrap downloads a GitHub release; check the tag out instead and lay wrapdb's meson overlay over it.
    sp="$SRC/glib/subprojects"
    gitpin pcre2 https://github.com/PCRE2Project/pcre2.git pcre2-10.44 6ae58beca071f13ccfed31d03b3f479ab520639b "$sp/pcre2-10.44"
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 -o "$SRC/pcre2-overlay.zip" https://wrapdb.mesonbuild.com/v2/pcre2_10.44-2/get_patch
    echo "4336d422ee9043847e5e10dbbbd01940d4c9e5027f31ccdc33a7898a1ca94009  $SRC/pcre2-overlay.zip" | sha256sum -c --quiet -
    (cd "$sp" && python3 -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall('.')" "$SRC/pcre2-overlay.zip")
    mesonbuild "$SRC/glib" -Dtests=false -Dintrospection=disabled -Dnls=disabled -Dlibmount=disabled -Dselinux=disabled \
        -Dxattr=false -Dman-pages=disabled -Ddocumentation=false -Dsysprof=disabled -Dglib_debug=disabled \
        --force-fallback-for=pcre2,libffi,proxy-libintl
    mark glib
fi
if ! done_mark slirp; then
    log libslirp
    fetch libslirp https://gitlab.freedesktop.org/slirp/libslirp/-/archive/v4.9.1/libslirp-v4.9.1.tar.gz 3970542143b7c11e6a09a4d2b50f30a133473c41f15ed0bdcc3b7a1c450d9a5c
    mesonbuild "$SRC/libslirp"
    mark slirp
fi
if ! done_mark lz4; then
    log lz4
    gitpin lz4 https://github.com/lz4/lz4.git v1.10.0 ebb370ca83af193212df4dcbadcc5d87bc0de2f0
    make -C "$SRC/lz4/lib" -j"$JOBS" CC="$CC" AR="$AR" CFLAGS="$CFLAGS" liblz4.a > "$SRC/lz4/make.log" 2>&1 ||
        { tail -n 20 "$SRC/lz4/make.log" >&2; exit 1; }
    install -D -m644 "$SRC/lz4/lib/liblz4.a" "$PREFIX/lib/liblz4.a"
    for h in lz4.h lz4hc.h lz4frame.h; do install -D -m644 "$SRC/lz4/lib/$h" "$PREFIX/include/$h"; done
    cat > "$PREFIX/lib/pkgconfig/liblz4.pc" <<PC
prefix=$PREFIX
Name: lz4
Description: LZ4 (static, for Inferno on Android)
Version: 1.10.0
Libs: -L\${prefix}/lib -llz4
Cflags: -I\${prefix}/include
PC
    mark lz4
fi
if ! done_mark png; then
    log libpng
    gitpin libpng https://github.com/pnggroup/libpng.git v1.6.58 3061454d980de7d53608f594194cfac722721d2a
    cmake -S "$SRC/libpng" -B "$SRC/libpng/_b" -G Ninja \
        -DCMAKE_TOOLCHAIN_FILE="$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake" \
        -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=android-$API -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DCMAKE_INSTALL_PREFIX="$PREFIX" \
        -DPNG_SHARED=OFF -DPNG_STATIC=ON -DPNG_TESTS=OFF -DPNG_TOOLS=OFF -DPNG_FRAMEWORK=OFF \
        > "$SRC/libpng/cmake.log" 2>&1 || { tail -n 30 "$SRC/libpng/cmake.log" >&2; exit 1; }
    ninja -C "$SRC/libpng/_b" install > "$SRC/libpng/ninja.log" 2>&1 || { tail -n 30 "$SRC/libpng/ninja.log" >&2; exit 1; }
    mark png
fi
if ! done_mark libyuv; then
    # Inferno's display pipe needs libyuv; its wrap follows libyuv's main branch through meson's CMake bridge, which
    # cannot drive an Android build. Build a pinned commit with the NDK's own CMake toolchain instead
    # (patches/0001 makes Inferno take it).
    log libyuv
    gitpin libyuv https://chromium.googlesource.com/libyuv/libyuv main c6700f48ddf3fdda4afb3cc7163f36427733aba5 2>/dev/null ||
        { rm -rf "$SRC/libyuv"; git init -q "$SRC/libyuv" &&
          git -C "$SRC/libyuv" fetch -q --depth 1 https://chromium.googlesource.com/libyuv/libyuv c6700f48ddf3fdda4afb3cc7163f36427733aba5 &&
          git -C "$SRC/libyuv" -c advice.detachedHead=false checkout -q FETCH_HEAD; }
    [ "$(git -C "$SRC/libyuv" rev-parse HEAD)" = c6700f48ddf3fdda4afb3cc7163f36427733aba5 ] || { echo "libyuv: wrong commit" >&2; exit 1; }
    cmake -S "$SRC/libyuv" -B "$SRC/libyuv/_b" -G Ninja \
        -DCMAKE_TOOLCHAIN_FILE="$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake" \
        -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=android-$API -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_POSITION_INDEPENDENT_CODE=ON -DCMAKE_INSTALL_PREFIX="$PREFIX" -DUNIT_TEST=OFF \
        > "$SRC/libyuv/cmake.log" 2>&1 || { tail -n 30 "$SRC/libyuv/cmake.log" >&2; exit 1; }
    ninja -C "$SRC/libyuv/_b" yuv > "$SRC/libyuv/ninja.log" 2>&1 || { tail -n 30 "$SRC/libyuv/ninja.log" >&2; exit 1; }
    install -D -m644 "$SRC/libyuv/_b/libyuv.a" "$PREFIX/lib/libyuv.a"
    mkdir -p "$PREFIX/include" && cp -r "$SRC/libyuv/include/libyuv" "$SRC/libyuv/include/libyuv.h" "$PREFIX/include/"
    cat > "$PREFIX/lib/pkgconfig/libyuv.pc" <<PC
prefix=$PREFIX
Name: libyuv
Description: libyuv (pinned for Inferno on Android)
Version: 0.0.0-c6700f48
Libs: -L\${prefix}/lib -lyuv
Libs.private: -lc++_static -lc++abi
Cflags: -I\${prefix}/include
PC
    mark libyuv
fi
log "dependencies in $PREFIX; cross file $CROSS"

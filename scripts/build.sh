#!/bin/bash
# One development build entry point: the engine, the Linux frontend, the Android APK, and (on a Mac,
# over ssh) the guest package and LT Bench. Outputs go to build/<target>/ in this repository, object
# trees to build/obj/ (incremental; --clean removes a target's). See docs/BUILDING.md.
#
#   scripts/build.sh [--clean] TARGET...
#
#   linux-engine   build/linux/libqemu-arm.so                  x86_64 engine (qemu-ios, portable board)
#   linux          build/linux/lighttouch-linux (+ engine)     the Linux frontend (Linux/CMakeLists.txt)
#   android-engine build/android/engine/arm64-v8a/              arm64 engine, libc++_shared.so, notices
#   android        build/android/LightTouch-debug.apk (+ engine) arm64 debug APK
#   portable-tests build/obj/portable-tests (ctest)            Portable/ unit tests
#   firmwarekit    build/firmwarekit/linux/                     firmwarekit (Swift) + lighttouch-oneshot, x86_64
#   firmwarekit-android build/firmwarekit/android/              the same for arm64 Android (Swift SDK for Android)
#   guest-package  build/guest-package/{armv6,armv7}.itpack   on the Mac ($LT_MAC), Xcode + old SDKs
#   firmware-tools build/firmware-tools/                       on the Mac: the helpers firmwarekit create bakes in
#                                                             (--guest-tools; contrib/export-guest-artifacts.sh)
#   ltbench        build/ltbench/LTBench.ipa                   on the Mac ($LT_MAC)
#   all            linux android portable-tests (+ guest-package ltbench when LT_MAC is set)
#
# Windows and macOS app builds are not here: Desktop/build.py (Desktop/README.md) runs those recipes on
# their own hosts.
#
# Machine-local inputs come from the environment, or from scripts/build.env if present (sourced; not
# tracked). The defaults are this project's development machine; docs/BUILDING.md says what each is.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
[ -f "$REPO/scripts/build.env" ] && . "$REPO/scripts/build.env"

: "${QEMU_IOS:=$HOME/git-projects/qemu-ios}"                                      # engine source
: "${LT_INPUTS:=$HOME/Documents/LightTouch-Development/port-workspace/builds}"   # prebuilt deps
: "${LT_ENGINE_KIT:=$HOME/Documents/LightTouch-Development/perf/engine-api29/arm64-v8a}"  # libc++, notices
: "${ANDROID_HOME:=$HOME/Android/Sdk}"
: "${ANDROID_NDK_VERSION:=28.2.13676358}"
: "${JAVA_HOME:=/usr/lib/jvm/java-17-openjdk-amd64}"
: "${LT_MAC:=}"                                                                   # user@host of the Mac
: "${LT_DEBUGGABLE:=true}"                       # false: a non-debuggable APK (release; GrapheneOS AOT)
: "${LT_PROFILEABLE:=false}"                     # true: simpleperf may attach from adb shell (perf builds)
: "${LT_ENGINE_PGO:=$LT_INPUTS/backend/pgo/engine-arm64.profdata}"   # Android engine PGO profile; off: none
# LT_ENGINE_CFLAGS / LT_ENGINE_LDFLAGS: added to the engine's configure (e.g. -fprofile-generate for a PGO
# profile run, which then skips LT_ENGINE_PGO); with LT_ENGINE_VARIANT=NAME the Android engine builds in its own
# android-engine-arm64-NAME.
: "${LT_MAC_DEVELOPER_DIR:=/Applications/Xcode.app/Contents/Developer}"
: "${LT_MAC_IPAD_SDK:=\$HOME/Developer/ipod2g-re/OldSDK/iPhoneOS3.2.sdk}"         # expanded on the Mac
: "${LT_MAC_ARMV6_SDK:=\$HOME/Developer/ipod2g-re/OldSDK/iPhoneOS3.1.3.sdk}"       # expanded on the Mac
: "${JOBS:=$(nproc 2>/dev/null || echo 4)}"
: "${LT_SWIFT_ENV:=$HOME/.local/share/swiftly/env.sh}"                           # swiftly's Swift 6.4+
: "${LT_SWIFT_ANDROID_SDK:=aarch64-unknown-linux-android29}"   # swift sdk install ...android.artifactbundle
: "${LT_SWIFT_ANDROID_BUNDLE:=$HOME/.swiftpm/swift-sdks/swift-6.4.1-RELEASE_android.artifactbundle}"
PCLIBDIR=
export ANDROID_HOME JAVA_HOME

OUT="$REPO/build"
OBJ="$OUT/obj"
NDK_BIN="$ANDROID_HOME/ndk/$ANDROID_NDK_VERSION/toolchains/llvm/prebuilt/linux-x86_64/bin"
log() { printf '== %s\n' "$*"; }
need() { [ -e "$1" ] || { echo "missing $2: $1 (see docs/BUILDING.md)" >&2; exit 1; }; }

# qemu-ios configured for the portable board, as every port's engine; $1 build dir, $2 deps prefix,
# $3 pkg-config path, then extra configure arguments. PKG_CONFIG_LIBDIR (cross builds) keeps the host's
# own .pc files out.
configure_engine() {
    local dir=$1 deps=$2 pcpath=$3; shift 3
    mkdir -p "$dir"
    [ -f "$dir/build.ninja" ] && return 0
    printf '#!/bin/sh\nexec pkg-config --static "$@"\n' > "$dir/target-pkg-config"
    chmod +x "$dir/target-pkg-config"
    (cd "$dir" && env PKG_CONFIG="$dir/target-pkg-config" PKG_CONFIG_PATH="$pcpath" \
        ${PCLIBDIR:+"PKG_CONFIG_LIBDIR=$PCLIBDIR"} "$QEMU_IOS/configure" --target-list=arm-softmmu --without-default-features --without-default-devices \
        --with-devices-arm=portable --enable-tcg --enable-pixman --enable-gcrypt --python=/usr/bin/python3 \
        --disable-docs --disable-tools --disable-werror --disable-pie --audio-drv-list= \
        "--extra-cflags=-fPIC -DQEMU_IOS_PORTABLE_CRYPTO${LT_ENGINE_CFLAGS:+ $LT_ENGINE_CFLAGS}" \
        "--extra-ldflags=-L$deps/lib${LT_ENGINE_LDFLAGS:+ $LT_ENGINE_LDFLAGS}" "$@" \
        > "$dir/configure.out" 2>&1) || { tail -20 "$dir/configure.out" >&2; exit 1; }
}
# Link qemu-system-arm keeping its response file, then turn it into libqemu-arm.so.
link_engine() {
    local dir=$1
    rm -f "$dir/qemu-system-arm"
    (cd "$dir" && ninja -j"$JOBS" -d keeprsp > "$dir/ninja.out" 2>&1) || { grep -m20 "error" "$dir/ninja.out" >&2; exit 1; }
    python3 "$QEMU_IOS/contrib/portable/make-shared.py" "$dir" > "$dir/make-shared.out" 2>&1 ||
        { tail -20 "$dir/make-shared.out" >&2; exit 1; }
}

build_linux_engine() {
    log "linux-engine"
    local deps="$LT_INPUTS/backend/deps-linux/prefix" dir="$OBJ/linux-engine${LT_ENGINE_VARIANT:+-$LT_ENGINE_VARIANT}"
    need "$deps" "Linux engine dependency prefix"
    configure_engine "$dir" "$deps" \
        "$LT_INPUTS/backend/crypto/linux/prefix/lib/pkgconfig:$deps/lib/pkgconfig"
    link_engine "$dir"
    if [ -n "${LT_ENGINE_VARIANT:-}" ]; then   # a variant stays in its build directory (LT_ENGINE= for runs)
        echo "$dir/libqemu-arm.so"
        return
    fi
    mkdir -p "$OUT/linux"
    cp "$dir/libqemu-arm.so" "$OUT/linux/"
    echo "$OUT/linux/libqemu-arm.so"
}

build_linux() {
    build_linux_engine
    log "linux frontend"
    cmake -S "$REPO/Linux" -B "$OBJ/linux-frontend" -DLIGHTTOUCH_QEMU_SOURCE="$QEMU_IOS" > "$OBJ/linux-frontend.cmake.out"
    cmake --build "$OBJ/linux-frontend" -j"$JOBS" --target lighttouch-linux > "$OBJ/linux-frontend.build.out" 2>&1 ||
        { tail -30 "$OBJ/linux-frontend.build.out" >&2; exit 1; }
    cp "$OBJ/linux-frontend/lighttouch-linux" "$OUT/linux/"
    echo "$OUT/linux/lighttouch-linux"
}

build_android_engine() {
    log "android-engine (arm64-v8a)"
    local deps="$LT_INPUTS/backend/deps/arm64-v8a/prefix" dir="$OBJ/android-engine-arm64${LT_ENGINE_VARIANT:+-$LT_ENGINE_VARIANT}"
    need "$deps" "Android engine dependency prefix"
    need "$LT_ENGINE_KIT/notices" "engine notice bundle"
    need "$NDK_BIN" "Android NDK $ANDROID_NDK_VERSION"
    PCLIBDIR="$LT_INPUTS/backend/crypto/arm64-v8a/prefix/lib/pkgconfig:$deps/lib/pkgconfig"
    # Profile-guided optimization from a gameplay profile (docs/BUILDING.md, "Engine profile"): clang lays the
    # engine's hot paths out together. A different profile (or none) reconfigures the engine.
    local pgo="" stamp="none"
    if [ "$LT_ENGINE_PGO" != off ] && [ -f "$LT_ENGINE_PGO" ] && [[ "${LT_ENGINE_CFLAGS:-}" != *-fprofile-generate* ]]; then
        pgo="-fprofile-use=$LT_ENGINE_PGO -Wno-profile-instr-unprofiled -Wno-profile-instr-out-of-date -Wno-backend-plugin"
        stamp=$(sha256sum "$LT_ENGINE_PGO" | cut -c1-16)
    fi
    if [ -f "$dir/build.ninja" ] && [ "$(cat "$dir/pgo-profile" 2>/dev/null || echo none)" != "$stamp" ]; then
        log "engine profile changed ($stamp): reconfiguring"
        rm -rf "$dir"
    fi
    mkdir -p "$dir"; echo "$stamp" > "$dir/pgo-profile"
    LT_ENGINE_CFLAGS="${LT_ENGINE_CFLAGS:+$LT_ENGINE_CFLAGS }$pgo" \
    LT_ENGINE_LDFLAGS="${LT_ENGINE_LDFLAGS:+$LT_ENGINE_LDFLAGS }${pgo:+${pgo%% *} -Wl,-z,keep-text-section-prefix}" \
    configure_engine "$dir" "$deps" "$PCLIBDIR" \
        --cross-prefix= --host-cc=/usr/bin/cc --cpu=aarch64 \
        --cc="$NDK_BIN/aarch64-linux-android29-clang" --cxx="$NDK_BIN/aarch64-linux-android29-clang++" \
        --disable-containers
    PCLIBDIR=
    link_engine "$dir"
    local e="$OUT/android/engine/arm64-v8a"
    rm -rf "$e"; mkdir -p "$e"
    cp "$dir/libqemu-arm.so" "$e/"
    cp "$LT_ENGINE_KIT/libc++_shared.so" "$e/"
    cp "$dir/portable-manifest.json" "$e/" 2>/dev/null || cp "$LT_ENGINE_KIT/portable-manifest.json" "$e/"
    cp -rL "$LT_ENGINE_KIT/notices" "$e/notices"
    # The APK build checks the notice bundle names this library by hash.
    python3 - "$e" <<'PY'
import hashlib, json, pathlib, sys
e = pathlib.Path(sys.argv[1]); m = e / "notices/manifest.json"; d = json.loads(m.read_text())
d["library_sha256"] = hashlib.sha256((e / "libqemu-arm.so").read_bytes()).hexdigest()
m.write_text(json.dumps(d, indent=2) + "\n")
PY
    echo "$e"
}

build_android() {
    build_android_engine
    log "android APK (arm64-v8a, debug)"
    local w="$LT_INPUTS" angle="$LT_INPUTS/graphics/angle-android-arm64-gl-build-01/candidate-01"
    need "$angle/lib" "ANGLE arm64 libraries"
    need "$w/mobile-android/arm64-v8a" "Android mobile services"
    # firmwarekit prepares devices from IPSWs on the phone (build/firmwarekit/android): rebuilt every time (SwiftPM is
    # incremental), or the APK carries whatever firmwarekit and lighttouch-oneshot were last built.
    build_firmwarekit_android
    # Guest tools the APK carries (new devices get the packages; firmwarekit bakes the helpers into the systems it
    # prepares); without them the APK still builds, but new devices get no guest tools and preparation cannot run.
    local tools="${LT_GUEST_TOOLS:-$OUT/firmware-tools}" guest=()
    if [ -f "$tools/armv6.itpack" ] && [ -f "$tools/armv7.itpack" ] && [ -f "$tools/it_seal" ]; then guest=(-PguestToolsDir="$tools")
    else echo "warning: no guest packages and helpers in $tools (scripts/build.sh firmware-tools, or set LT_GUEST_TOOLS); the APK will carry no guest tools" >&2; fi
    # A non-debuggable build runs lint-vital, which needs an artifact the offline Gradle cache lacks.
    local lint=()
    [ "$LT_DEBUGGABLE" = false ] && lint=(-x lintVitalAnalyzeDebug -x lintVitalReportDebug -x lintVitalDebug)
    (cd "$REPO/Android" && CMAKE_BUILD_PARALLEL_LEVEL="$JOBS" ./gradlew --offline --no-daemon --max-workers=8 \
        '-Dorg.gradle.jvmargs=-Xmx2g -XX:MaxMetaspaceSize=512m' -PnativeJobs="$JOBS" -PtargetAbis=arm64-v8a \
        -PportableDir="$REPO/Portable" -PqemuSource="$QEMU_IOS" \
        -PengineRoot="$OUT/android/engine" -PfirmwareKitDir="$OUT/firmwarekit/android" \
        -PengineNoticeArm64="$OUT/android/engine/arm64-v8a/notices" \
        -PsourceRevision="$(git -C "$REPO" rev-parse HEAD)" \
        -PsourceReference="${LT_SOURCE_REFERENCE:-https://github.com/blake-lucas/LightTouch/tree/$(git -C "$REPO" branch --show-current)}" \
        -PbuildRoot="$OBJ/android-gradle" -PmobileServicesArm64="$w/mobile-android/arm64-v8a" \
        -PfrontendNotices="$w/product/frontend-inputs-15" \
        -PfrontendNoticesSha256=88918185b1a3ba3ea1b4c910c8b7dde56391597574f53824a160fef45d2b76a7 \
        -PangleArm64Libraries="$angle/lib" -PangleArm64Manifest="$angle/elf-closure.json" \
        -PangleArm64Notices="$angle/notices" \
        -PangleArm64ManifestSha256=3abde58ea5d9863453b12e3d7f67eff7c96ee989fbcfa4c00a16859cfd694945 \
        -PangleArm64NoticesSha256=63d2221b0fab9bff7788dc0c4e9a94c74079b3ab5ed3c09bd3ef72c909d1f924 \
        -Pdebuggable="$LT_DEBUGGABLE" -Pprofileable="$LT_PROFILEABLE" "${guest[@]}" "${lint[@]}" :app:assembleDebug > "$OBJ/android-gradle.out" 2>&1) ||
        { grep -A12 "What went wrong" "$OBJ/android-gradle.out" >&2 || tail -30 "$OBJ/android-gradle.out" >&2; exit 1; }
    cp "$OBJ/android-gradle/app/outputs/apk/debug/app-debug.apk" "$OUT/android/LightTouch-debug.apk"
    echo "$OUT/android/LightTouch-debug.apk"
}

build_portable_tests() {
    log "portable-tests"
    cmake -S "$REPO/Portable" -B "$OBJ/portable-tests" -DLIGHTTOUCH_QEMU_SOURCE="$QEMU_IOS" \
        -DLIGHTTOUCH_BUILD_TESTS=ON > "$OBJ/portable-tests.cmake.out"
    cmake --build "$OBJ/portable-tests" -j"$JOBS" > "$OBJ/portable-tests.build.out" 2>&1 ||
        { tail -30 "$OBJ/portable-tests.build.out" >&2; exit 1; }
    (cd "$OBJ/portable-tests" && ctest --output-on-failure | tail -3)
}

# firmwarekit (Packages/FirmwareKit) builds a device from an IPSW off the Mac too; its build-time boots go through
# lighttouch-oneshot, which loads the engine library ($LTM_QEMU_DYLIB, else libqemu-arm.so beside it).
build_firmwarekit() {
    log "firmwarekit (linux)"
    . "$LT_SWIFT_ENV"
    (cd "$REPO/Packages/FirmwareKit" && swift build -c release --product firmwarekit 2>&1 | tail -3)
    mkdir -p "$OUT/firmwarekit/linux"
    cp "$REPO/Packages/FirmwareKit/.build/release/firmwarekit" "$OUT/firmwarekit/linux/"
    cc -O2 -Wall -Wextra -Werror -o "$OUT/firmwarekit/linux/lighttouch-oneshot" "$REPO/Portable/oneshot/lighttouch-oneshot.c" -ldl -lpthread
    ls -la "$OUT/firmwarekit/linux"
}

# The Swift SDK for Android's static runtime wants NDK r30's libc++ (setup-android-sdk.sh with ANDROID_NDK_HOME at
# r30); build-support/android-pkgconfig keeps the host's zlib off the link and takes libc++ statically.
build_firmwarekit_android() {
    log "firmwarekit (android arm64)"
    . "$LT_SWIFT_ENV"
    (cd "$REPO/Packages/FirmwareKit" && swift build -c release --product firmwarekit --swift-sdk "$LT_SWIFT_ANDROID_SDK" \
        --scratch-path .build-android --static-swift-stdlib --pkg-config-path build-support/android-pkgconfig \
        -Xlinker -lssl -Xlinker -lcrypto 2>&1 | tail -3)
    mkdir -p "$OUT/firmwarekit/android"
    cp "$REPO/Packages/FirmwareKit/.build-android/out/Products/Release-android-aarch64/firmwarekit" "$OUT/firmwarekit/android/"
    "$NDK_BIN/llvm-strip" "$OUT/firmwarekit/android/firmwarekit"
    "$NDK_BIN/aarch64-linux-android29-clang" -O2 -Wall -Wextra -Werror -o "$OUT/firmwarekit/android/lighttouch-oneshot" \
        "$REPO/Portable/oneshot/lighttouch-oneshot.c"
    python3 "$REPO/Packages/FirmwareKit/build-support/stage-notices.py" "$OUT/firmwarekit/android/licenses" \
        --checkouts "$REPO/Packages/FirmwareKit/.build-android/checkouts" \
        --swift-license "$LT_SWIFT_ANDROID_BUNDLE/swift-android/swift-resources/usr/share/swift/LICENSE.txt" \
        --source-reference "${LT_SOURCE_REFERENCE:-https://github.com/blake-lucas/LightTouch/tree/$(git -C "$REPO" branch --show-current)}"
    ls -la "$OUT/firmwarekit/android"
}

mac() { [ -n "$LT_MAC" ] || { echo "$1 builds on a Mac: set LT_MAC=user@host (docs/BUILDING.md)" >&2; exit 1; }; }

build_guest_package() {
    mac guest-package
    log "guest-package on $LT_MAC"
    local remote=ltpkg-build
    # The build reads contrib/, include/hw/arm (the GL name table) and tests/guest-package.
    ssh "$LT_MAC" "rm -rf $remote && mkdir -p $remote/src/include/hw/arm $remote/src/tests"
    rsync -az --exclude=.git "$QEMU_IOS/contrib" "$LT_MAC:$remote/src/"
    rsync -az "$QEMU_IOS/include/hw/arm/" "$LT_MAC:$remote/src/include/hw/arm/"
    rsync -az "$QEMU_IOS/tests/guest-package" "$LT_MAC:$remote/src/tests/"
    ssh "$LT_MAC" "cd $remote/src && DEVELOPER_DIR=$LT_MAC_DEVELOPER_DIR IPAD_SDK=$LT_MAC_IPAD_SDK \
        PATH=/opt/homebrew/bin:\$PATH /usr/bin/caffeinate -i -s bash contrib/guest-package/build.sh ~/$remote/out > ~/$remote/build.log 2>&1" ||
        { ssh "$LT_MAC" "tail -30 ~/$remote/build.log" >&2; exit 1; }
    mkdir -p "$OUT/guest-package"
    scp -q "$LT_MAC:$remote/out/armv6.itpack" "$LT_MAC:$remote/out/armv7.itpack" "$LT_MAC:$remote/build.log" "$OUT/guest-package/"
    cp "$QEMU_IOS/contrib/guest-package/VERSION" "$OUT/guest-package/"
    (cd "$OUT/guest-package" && sha256sum armv6.itpack armv7.itpack > SHA256SUMS)
    ls -la "$OUT/guest-package"
}

build_firmware_tools() {
    mac firmware-tools
    log "firmware-tools on $LT_MAC"
    local remote=ltfw-build
    # export-guest-artifacts.sh reads what guest-package/build.sh does, plus the app headers and its own script.
    ssh "$LT_MAC" "rm -rf $remote && mkdir -p $remote/src/include/hw/arm $remote/src/tests"
    rsync -az --exclude=.git "$QEMU_IOS/contrib" "$LT_MAC:$remote/src/"
    rsync -az "$QEMU_IOS/include/hw/arm/" "$LT_MAC:$remote/src/include/hw/arm/"
    rsync -az "$QEMU_IOS/tests/guest-package" "$LT_MAC:$remote/src/tests/"
    ssh "$LT_MAC" "/usr/bin/caffeinate -i -s zsh -lc 'cd $remote/src && DEVELOPER_DIR=$LT_MAC_DEVELOPER_DIR IPAD_SDK=$LT_MAC_IPAD_SDK \
        ARMV6_SDK=$LT_MAC_ARMV6_SDK PATH=/opt/homebrew/bin:\$PATH \
        bash contrib/export-guest-artifacts.sh ~/$remote/out > ~/$remote/build.log 2>&1'" ||
        { ssh "$LT_MAC" "tail -30 ~/$remote/build.log" >&2; exit 1; }
    rm -rf "$OUT/firmware-tools"; mkdir -p "$OUT"
    scp -qr "$LT_MAC:$remote/out/ipad-guest-tools" "$OUT/firmware-tools"
    scp -q "$LT_MAC:$remote/out/manifest.json" "$OUT/firmware-tools.manifest.json"
    ls -la "$OUT/firmware-tools"
}

build_ltbench() {
    mac ltbench
    log "ltbench on $LT_MAC"
    LT_MAC="$LT_MAC" QEMU_IOS="$QEMU_IOS" "$REPO/tools/guestbench/build.sh" "$OUT/ltbench"
}

clean=0; targets=()
for a in "$@"; do
    case "$a" in
    --clean) clean=1 ;;
    -h|--help) sed -n 2,25p "$0"; exit 0 ;;
    all) targets+=(linux android portable-tests); [ -n "$LT_MAC" ] && targets+=(guest-package ltbench) ;;
    linux-engine|linux|android-engine|android|portable-tests|firmwarekit|firmwarekit-android|guest-package|firmware-tools|ltbench) targets+=("$a") ;;
    *) echo "unknown target: $a (scripts/build.sh --help)" >&2; exit 2 ;;
    esac
done
[ ${#targets[@]} -gt 0 ] || { sed -n 2,25p "$0"; exit 2; }
for t in "${targets[@]}"; do
    if [ $clean = 1 ]; then
        case "$t" in
        linux-engine) rm -rf "$OBJ/linux-engine" ;;
        linux) rm -rf "$OBJ/linux-engine" "$OBJ/linux-frontend" ;;
        android-engine) rm -rf "$OBJ/android-engine-arm64" ;;
        android) rm -rf "$OBJ/android-engine-arm64" "$OBJ/android-gradle" ;;
        portable-tests) rm -rf "$OBJ/portable-tests" ;;
        esac
    fi
    "build_${t//-/_}"
done

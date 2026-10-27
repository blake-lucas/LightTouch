#!/bin/bash
# Build, package and publish a LightTouch release for every platform. Each step works in a release
# workspace ($LT_RELEASE_ROOT/release-X.Y.Z) from worktrees of the release tags, so the checkouts you
# work in are never built from. See docs/BUILDING.md, "Release builds".
#
#   scripts/build-release.sh VERSION [STEP...]        e.g. scripts/build-release.sh 0.0.9
#
#   prepare   worktrees app/ and engine/ at tags vX.Y.Z (here) and lighttouch-vX.Y.Z (qemu-ios)
#   android   non-debuggable arm64 APK carrying the guest tools ($LT_GUEST_TOOLS)
#   windows   the engine + FFmpeg cross-built with MinGW
#   linux     the engine in the Debian 11 image (Docker), with the USB bridge and the libraries both load
#   desktop   the desktop app (docs/DESKTOP.md) as a Linux AppImage and a Windows zip, from the linux and windows
#             steps' engines
#   macos     the engine and the desktop app on the Mac ($LT_MAC) over ssh, from the pushed commits
#   finish    source archives, guest tools, SHA256SUMS, RELEASE-MANIFEST.json (status from
#             LT_PRERELEASE), NOTES.md checksums
#   publish   push both tags and create the GitHub release from out/NOTES.md (a pre-release unless
#             LT_PRERELEASE=false)
#
# With no STEP it runs prepare through finish, skipping the steps already done (a .done-STEP file in
# the workspace); naming a step runs it again. publish only runs when named. Write out/NOTES.md
# (title on its first line, CHECKSUMS where the checksum block goes) before finish.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
[ -f "$REPO/scripts/build.env" ] && . "$REPO/scripts/build.env"

D=$HOME/Documents/LightTouch-Development
: "${QEMU_IOS:=$HOME/git-projects/qemu-ios}"                         # engine repository (tags, subprojects)
: "${LT_INPUTS:=$D/port-workspace/builds}"                           # prebuilt inputs, as for scripts/build.sh
: "${LT_RELEASE_ROOT:=$D}"                                           # workspaces go in release-X.Y.Z here
: "${LT_GUEST_TOOLS:=}"                                              # guest tools shipped and bundled; empty: built on $LT_MAC
: "${LT_LINUX_INPUTS:=$LT_INPUTS/linux-desktop/bullseye-build01}"    # deps-full/ crypto/ mobile-private-runtime-ui15/
: "${LT_LINUX_IMAGE:=lighttouch-bullseye-collector08}"               # Debian 11 build image
: "${LT_APPIMAGE_TOOLS:=$LT_INPUTS/linux-desktop/dependencies/appimagetool-pinned01}"
: "${LT_LINUX_CPUS:=$(nproc)}"                                       # the Linux build container's CPUs and jobs"
: "${LT_WIN_DEPS:=$D/release-0.0.10/win-deps}"                       # MinGW sysroot (copied; FFmpeg installs into it)
: "${LT_WIN_INPUTS:=$LT_INPUTS/windows-native}"                      # services-mobile01/ (the USB bridge)
: "${LT_FFMPEG_ARCHIVE:=$LT_INPUTS/backend/deps-linux/ffmpeg-9.0.1.tar.xz}"   # checked by its SHA-256
: "${LT_MAC:=}"                                                      # user@host of the Mac
: "${LT_MAC_ROOT:=Documents}"                                        # on the Mac, relative to home
: "${LT_MAC_PYTHON:=/opt/homebrew/bin/python3.13}"
: "${LT_MAC_OPENSSL:=/opt/homebrew/opt/openssl@3}"
: "${LT_MAC_VENDOR:=/Applications/Light Touch.app/Contents}"         # LightTouchMac app: its USB bridge (MacOS/usbmuxd)
: "${LT_PRERELEASE:=true}"
: "${LT_DESKTOP_NOTICES:=$D/desktop/notices}"                        # linux/ and windows/, each engine/ and usb/
: "${ANDROID_HOME:=$HOME/Android/Sdk}"
export QEMU_IOS LT_INPUTS ANDROID_HOME LT_PRERELEASE

[ $# -ge 1 ] || { sed -n 2,22p "$0"; exit 2; }
case "$1" in -h|--help) sed -n 2,22p "$0"; exit 0 ;; esac
VER=${1#v}; V=v$VER; shift
R=$LT_RELEASE_ROOT/release-$VER
OUTD=$R/out
GH_APP=https://github.com/blake-lucas/LightTouch.git
GH_ENGINE=https://github.com/blake-lucas/qemu-ios.git
# Subprojects meson would download; copied in from $QEMU_IOS (git ignores them).
SUBPROJECTS="berkeley-softfloat-3 berkeley-testfloat-3 keycodemapdb dtc slirp"
log() { printf '== %s: %s\n' "$V" "$*"; }
need() { [ -e "$1" ] || { echo "missing $2: $1" >&2; exit 1; }; }
rev() { git -C "$1" rev-parse HEAD; }

step_prepare() {
    local app_rev engine_rev
    app_rev=$(git -C "$REPO" rev-parse --verify -q "$V^{commit}") || { echo "tag $V first (git tag $V)" >&2; exit 1; }
    engine_rev=$(git -C "$QEMU_IOS" rev-parse --verify -q "lighttouch-$V^{commit}") ||
        { echo "tag lighttouch-$V in $QEMU_IOS first" >&2; exit 1; }
    # The Mac fetches the commits from GitHub.
    [ -n "$(git -C "$REPO" branch -r --contains "$app_rev")" ] || { echo "push $V's commit first" >&2; exit 1; }
    [ -n "$(git -C "$QEMU_IOS" branch -r --contains "$engine_rev")" ] || { echo "push lighttouch-$V's commit first" >&2; exit 1; }
    mkdir -p "$OUTD"
    [ -e "$R/app" ] || git -C "$REPO" worktree add -q --detach "$R/app" "$app_rev"
    [ -e "$R/engine" ] || git -C "$QEMU_IOS" worktree add -q --detach "$R/engine" "$engine_rev"
    [ "$(rev "$R/app")" = "$app_rev" ] && [ "$(rev "$R/engine")" = "$engine_rev" ] ||
        { echo "$R/app or $R/engine is not at the tags; remove the worktree and rerun" >&2; exit 1; }
    for p in $SUBPROJECTS; do
        need "$QEMU_IOS/subprojects/$p" "subproject $p (configure the engine once in $QEMU_IOS)"
        rm -rf "$R/engine/subprojects/$p"; cp -a "$QEMU_IOS/subprojects/$p" "$R/engine/subprojects/"
    done
    # meson applies a wrap's diff_files only when it fetches the subproject, so the copy must be patched already.
    grep -q udp_refuse_dns "$R/engine/subprojects/slirp/src/udp.c" ||
        { echo "$QEMU_IOS/subprojects/slirp lacks libslirp-restricted-dns.patch: configure the engine there afresh" >&2; exit 1; }
    for w in app engine; do
        [ -z "$(git -C "$R/$w" status --porcelain --untracked-files=all)" ] || { echo "$R/$w is not clean" >&2; exit 1; }
    done
    echo "app $app_rev, engine $engine_rev"
}

step_android() {
    # The itpacks new devices get, plus the helpers firmwarekit installs when the app prepares an IPSW (it_seal …),
    # built from this release's engine.
    if [ -z "$LT_GUEST_TOOLS" ]; then
        QEMU_IOS="$R/engine" bash "$R/app/scripts/build.sh" firmware-tools
        LT_GUEST_TOOLS=$R/app/build/firmware-tools
    fi
    for f in armv6.itpack armv7.itpack it_seal; do need "$LT_GUEST_TOOLS/$f" "guest tools"; done
    QEMU_IOS="$R/engine" LT_GUEST_TOOLS="$LT_GUEST_TOOLS" LT_DEBUGGABLE=false \
        LT_SOURCE_REFERENCE="https://github.com/blake-lucas/LightTouch/tree/$V" \
        bash "$R/app/scripts/build.sh" --clean android
    local apk=$OUTD/LightTouch-$V-android-arm64.apk aapt
    cp "$R/app/build/android/LightTouch-debug.apk" "$apk"
    aapt=$(ls -d "$ANDROID_HOME"/build-tools/*/aapt | tail -1)
    if "$aapt" dump xmltree "$apk" AndroidManifest.xml | grep -q 'android:debuggable.*0xffffffff'; then
        echo "$apk is debuggable" >&2; exit 1
    fi
    "$aapt" dump badging "$apk" | grep -E "^package:" | grep -q "versionName='$VER'" ||
        { echo "$apk is not versionName $VER" >&2; exit 1; }
    echo "$apk"
}

step_windows() {
    need "$LT_WIN_DEPS/sysroot" "Windows dependency sysroot"; need "$LT_FFMPEG_ARCHIVE" "FFmpeg 9.0.1 archive"
    rm -rf "$R/win-cross" "$R/win-deps"
    cp -a "$LT_WIN_DEPS" "$R/win-deps"
    bash "$R/app/Windows/tools/build-engine.sh" "$R/engine" "$R/win-deps" "$R/win-cross" "$LT_FFMPEG_ARCHIVE" \
        > "$R/windows-engine.log" 2>&1 || { tail -30 "$R/windows-engine.log" >&2; exit 1; }
    need "$R/win-cross/engine/qemu-system-arm.exe.rsp" "the engine's link (keeprsp)"
    echo "$R/win-cross/engine"
}

step_linux() {
    local b=$R/linux-build mounts=() d p
    # The prebuilt prefixes go where they were built: their pkg-config files name /build/<dir>.
    for p in deps-full crypto mobile-private-runtime-ui15; do
        need "$LT_LINUX_INPUTS/$p" "Linux input $p"
        mounts+=(--mount "type=bind,src=$LT_LINUX_INPUTS/$p,dst=/build/$p,readonly")
    done
    # The worktrees' .git files point into these repositories (and their object alternates).
    for d in "$(git -C "$R/app" rev-parse --path-format=absolute --git-common-dir)" \
             "$(git -C "$R/engine" rev-parse --path-format=absolute --git-common-dir)"; do
        mounts+=(--mount "type=bind,src=$d,dst=$d,readonly")
        if [ -f "$d/objects/info/alternates" ]; then
            while read -r a; do mounts+=(--mount "type=bind,src=$a,dst=$a,readonly"); done < "$d/objects/info/alternates"
        fi
    done
    # Earlier runs leave root-owned files.
    [ -d "$b" ] && docker run --rm -v "$b:/b" "$LT_LINUX_IMAGE" sh -c 'rm -rf /b/* /b/.[!.]*'
    mkdir -p "$b"
    docker rm -f "lt-$V-linux" > /dev/null 2>&1 || true
    timeout 5400 docker run --name "lt-$V-linux" --rm --network none --memory=8g --memory-swap=8g --cpus="$LT_LINUX_CPUS" --pids-limit=256 \
        --cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE --cap-add FOWNER --security-opt no-new-privileges \
        --mount "type=bind,src=$R/app,dst=/src/app,readonly" \
        --mount "type=bind,src=$R/engine,dst=$R/engine,readonly" \
        --mount "type=bind,src=$b,dst=/build" "${mounts[@]}" \
        -e "LT_JOBS=$LT_LINUX_CPUS" -e "QEMU_SOURCE=$R/engine" -e "EXPECTED_QEMU_REV=$(rev "$R/engine")" -e "EXPECTED_APP_REV=$(rev "$R/app")" \
        "$LT_LINUX_IMAGE" /bin/sh /src/app/scripts/release/linux-inner.sh > "$R/linux.log" 2>&1 ||
        { tail -30 "$R/linux.log" >&2; exit 1; }
    ls -1 "$b/runtime" "$b/runtime/lib"
}

step_desktop() {
    local d=$R/desktop a=$R/linux-build/runtime sysroot=$R/win-deps/sysroot
    local tools=${LT_GUEST_TOOLS:-$R/app/build/firmware-tools} n=$LT_DESKTOP_NOTICES
    need "$a/libqemu-arm.so" "Linux engine (run the linux step)"
    need "$R/win-cross/engine/qemu-system-arm.exe.rsp" "Windows engine build (run the windows step)"
    need "$R/engine/contrib/portable/make-shared-windows.py" "the engine's Windows DLL relink (qemu-ios dev/desktop)"
    need "$tools" "guest tools"
    rm -rf "$d"; mkdir -p "$d"
    bash "$R/app/scripts/release/build-desktop-linux.sh" > "$R/desktop-linux.log" 2>&1 ||
        { tail -30 "$R/desktop-linux.log" >&2; exit 1; }
    # The engine and the USB bridge from the linux step, with the libraries they load beyond the base system.
    python3 "$R/app/scripts/release/package-desktop.py" "$d/LightTouch-$V-Linux-x86_64" --os linux \
        --app "$R/app/target/bullseye/release/lighttouch" --firmwarekit "$R/app/target/bullseye/release/firmwarekit" \
        --engine "$a/libqemu-arm.so" --usbmuxd "$a/usbmuxd-qemu" --guest-tools "$tools" \
        --lib "$a/lib" --notices "engine=$n/linux/engine" --notices "usb=$n/linux/usb" --group-source "engine=$GH_ENGINE" \
        --appimage "$LT_APPIMAGE_TOOLS" > "$R/desktop-linux-package.log" 2>&1 ||
        { tail -30 "$R/desktop-linux-package.log" >&2; exit 1; }
    # Windows: the windows step's engine build relinked as a DLL, its DLL closure, and the app cross-built.
    python3 "$R/engine/contrib/portable/make-shared-windows.py" "$R/win-cross/engine" "$d/win-engine" \
        > "$R/desktop-windows.log" 2>&1 || { tail -30 "$R/desktop-windows.log" >&2; exit 1; }
    python3 "$R/app/Windows/tools/package-runtime.py" --engine "$d/win-engine/libqemu-arm.dll" \
        --dll-dir "$sysroot/mingw64/bin" --dll-dir "$sysroot/codec/bin" --destination "$d/win-runtime" \
        >> "$R/desktop-windows.log" 2>&1 || { tail -30 "$R/desktop-windows.log" >&2; exit 1; }
    (cd "$R/app" && CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc-posix CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++-posix \
        AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar cargo build --release --locked --target x86_64-pc-windows-gnu \
        -p lighttouch -p lighttouch-firmware --features lighttouch-services/vendored-openssl) >> "$R/desktop-windows.log" 2>&1 ||
        { tail -30 "$R/desktop-windows.log" >&2; exit 1; }
    # The engine's runtime goes last, so its libwinpthread (which the engine and libstdc++ import from) wins over the
    # older one beside the USB bridge; package-desktop checks the bridge still finds what it imports.
    local w=$R/app/target/x86_64-pc-windows-gnu/release services=$LT_WIN_INPUTS/services-mobile01/bin
    python3 "$R/app/scripts/release/package-desktop.py" "$d/LightTouch-$V-Windows-x86_64" --os windows \
        --app "$w/lighttouch.exe" --firmwarekit "$w/firmwarekit.exe" --engine "$d/win-engine/libqemu-arm.dll" \
        --usbmuxd "$services/lighttouch-usbmuxd.exe" --guest-tools "$tools" --lib "$services" --lib "$d/win-runtime" \
        --notices "engine=$n/windows/engine" --notices "usb=$n/windows/usb" --group-source "engine=$GH_ENGINE" --zip \
        > "$R/desktop-windows-package.log" 2>&1 || { tail -30 "$R/desktop-windows-package.log" >&2; exit 1; }
    cp "$d/LightTouch-$V-Linux-x86_64.AppImage" "$d/LightTouch-$V-Windows-x86_64.zip" "$OUTD/"
    ls -1 "$OUTD"/*.AppImage "$OUTD"/*Windows*.zip
}

step_macos() {
    [ -n "$LT_MAC" ] || { echo "the macOS build runs on a Mac: set LT_MAC=user@host" >&2; exit 1; }
    local m="$LT_MAC_ROOT/LightTouch-Release-$VER" out=LightTouch-$V-macOS-arm64 app_rev engine_rev
    local tools=${LT_GUEST_TOOLS:-$R/app/build/firmware-tools} n=$LT_DESKTOP_NOTICES ff
    need "$tools" "guest tools"; need "$LT_FFMPEG_ARCHIVE" "FFmpeg 9.0.1 archive"
    need "$n/linux/engine/qemu-ios" "engine notices"; need "$n/linux/usb" "USB bridge notices"
    app_rev=$(rev "$R/app"); engine_rev=$(rev "$R/engine"); ff=$(basename "$LT_FFMPEG_ARCHIVE")
    ssh "$LT_MAC" "set -e; rm -rf $m; mkdir -p $m/notices/engine; cd $m
        git init -q app; git -C app fetch -q --depth 1 $GH_APP $app_rev; git -C app checkout -q --detach FETCH_HEAD
        git init -q qemu; git -C qemu fetch -q --depth 1 $GH_ENGINE $engine_rev; git -C qemu checkout -q --detach FETCH_HEAD"
    for p in $SUBPROJECTS; do rsync -a "$R/engine/subprojects/$p" "$LT_MAC:$m/qemu/subprojects/"; done
    rsync -a "$tools/" "$LT_MAC:$m/guest-tools/"
    rsync -a "$n/linux/engine/qemu-ios" "$LT_MAC:$m/notices/engine/"
    rsync -a "$n/linux/usb/" "$LT_MAC:$m/notices/usb/"
    rsync -a "$LT_FFMPEG_ARCHIVE" "$LT_MAC:$m/"
    # The engine links a decoder-only FFmpeg; the USB bridge is the LightTouchMac app's ($LT_MAC_VENDOR). The
    # packager bundles the Homebrew libraries both load, with their licence files, and signs the app ad hoc.
    ssh "$LT_MAC" "export PATH=\$HOME/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin; cd $m
        caffeinate -s $LT_MAC_PYTHON app/macOS/build-engine.py --qemu-source \$PWD/qemu --output \$PWD/engine --python $LT_MAC_PYTHON \
            --openssl-prefix $LT_MAC_OPENSSL --ffmpeg-archive \$PWD/$ff > engine.log 2>&1 || { tail -30 engine.log; exit 1; }
        (cd app && caffeinate -s cargo build --release --locked -p lighttouch -p lighttouch-firmware \
            --features lighttouch-services/vendored-openssl) > app.log 2>&1 || { tail -30 app.log; exit 1; }
        $LT_MAC_PYTHON -B app/scripts/release/package-desktop.py \$PWD/LightTouch.app --os macos --app app/target/release/lighttouch \
            --firmwarekit app/target/release/firmwarekit --engine engine/libqemu-arm.dylib --usbmuxd '$LT_MAC_VENDOR/MacOS/usbmuxd' \
            --guest-tools guest-tools --notices engine=\$PWD/notices/engine --notices usb=\$PWD/notices/usb \
            --group-source engine=$GH_ENGINE > package.log 2>&1 || { tail -30 package.log; exit 1; }
        codesign --verify --deep --strict LightTouch.app
        ditto -c -k --keepParent LightTouch.app $out.zip"
    scp -q "$LT_MAC:$m/$out.zip" "$OUTD/"
    echo "$OUTD/$out.zip"
}

step_finish() {
    python3 "$R/app/scripts/release/finish.py" "$R" "$V" "${LT_GUEST_TOOLS:-$R/app/build/firmware-tools}"
    (cd "$OUTD" && sha256sum -c --quiet SHA256SUMS)
    if [ ! -f "$OUTD/NOTES.md" ]; then echo "next: write $OUTD/NOTES.md (with CHECKSUMS), then rerun finish"
    elif grep -q CHECKSUMS "$OUTD/NOTES.md"; then echo "NOTES.md still has CHECKSUMS" >&2; exit 1; fi
}

step_publish() {
    local notes=$OUTD/NOTES.md title assets=()
    need "$notes" "release notes"
    grep -q CHECKSUMS "$notes" && { echo "run finish first: NOTES.md has no checksums" >&2; exit 1; }
    title=$(head -1 "$notes" | sed 's/^# *//')
    for f in LightTouch-$V-android-arm64.apk LightTouch-$V-Linux-x86_64.AppImage LightTouch-$V-Windows-x86_64.zip \
             LightTouch-$V-macOS-arm64.zip armv6.itpack armv7.itpack LightTouch-$V-source.tar.gz qemu-ios-$V-source.tar.gz \
             SHA256SUMS RELEASE-MANIFEST.json LICENSE.txt; do need "$OUTD/$f" "package"; assets+=("$OUTD/$f"); done
    (cd "$OUTD" && sha256sum -c --quiet SHA256SUMS)
    git -C "$REPO" push -q origin "$V"
    git -C "$QEMU_IOS" push -q origin "lighttouch-$V"
    local pre=(); [ "$LT_PRERELEASE" = true ] && pre=(--prerelease)
    (cd "$REPO" && gh release create "$V" --verify-tag "${pre[@]}" --title "$title" --notes-file "$notes" "${assets[@]}")
}

steps=("$@"); auto=0
if [ ${#steps[@]} = 0 ]; then steps=(prepare android windows linux desktop macos finish); auto=1; fi
for s in "${steps[@]}"; do
    case "$s" in prepare|android|windows|linux|desktop|macos|finish|publish) ;; *) echo "unknown step: $s" >&2; exit 2 ;; esac
done
mkdir -p "$R"
for s in "${steps[@]}"; do
    if [ $auto = 1 ] && [ -f "$R/.done-$s" ]; then log "$s already done"; continue; fi
    log "$s"; rm -f "$R/.done-$s"
    "step_$s"
    date -u +%FT%TZ > "$R/.done-$s"
done

#!/bin/bash
# Build LTBench.ipa (docs/GUEST-BENCHMARKS.md). Needs macOS (Xcode's clang and ld, ldid) for the
# armv6 link; from Linux, LT_MAC=user@host runs the same build there over ssh.
# usage: build.sh OUT_DIR    (writes OUT_DIR/LTBench.ipa)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$(mkdir -p "$1" && cd "$1" && pwd)"
TOOLCHAIN="${QEMU_IOS:-$HOME/git-projects/qemu-ios}/contrib/armv6-toolchain"

if [ "$(uname)" != Darwin ]; then
    : "${LT_MAC:?not on macOS: set LT_MAC=user@host of a Mac with Xcode and ldid}"
    remote=ltbench-build-$$
    ssh "$LT_MAC" "mkdir -p $remote"
    scp -q "$HERE"/ltbench.c "$HERE"/start.S "$HERE"/Info.plist "$HERE"/build.sh "$TOOLCHAIN"/machotool.c "$LT_MAC:$remote/"
    ssh "$LT_MAC" "cd $remote && LTB_CFLAGS=$(printf %q "${LTB_CFLAGS:-}") QEMU_IOS=. MACHOTOOL_SRC=machotool.c PATH=/opt/homebrew/bin:\$PATH /usr/bin/caffeinate -i -s bash build.sh out"
    scp -q "$LT_MAC:$remote/out/LTBench.ipa" "$OUT/"
    ssh "$LT_MAC" "rm -rf $remote"
    echo "$OUT/LTBench.ipa"
    exit 0
fi

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
xcrun clang -std=c11 -O2 -o "$W/machotool" "${MACHOTOOL_SRC:-$TOOLCHAIN/machotool.c}"
# ARM state (the ARM1176 has no Thumb-2); clang only links armv7 now, so the objects are
# presented as armv7 and the result is put back to armv6 (contrib/armv6-toolchain/README.md).
for src in ltbench.c start.S; do
    xcrun clang -target armv6-apple-ios5.0 -marm -O2 -ffreestanding -fno-builtin -fno-stack-protector \
        -Wall ${LTB_CFLAGS:-} -c "$HERE/$src" -o "$W/$src.o" 2> >(grep -v incompatible-sysroot >&2)
    "$W/machotool" subtype "$W/$src.o" 9 >/dev/null
done
# No 3.1.3 SDK needed: a link stub naming just the libSystem symbols these objects import.
{
    printf -- '--- !tapi-tbd\ntbd-version: 4\ntargets: [ armv7-ios ]\n'
    printf "install-name: '/usr/lib/libSystem.B.dylib'\ncurrent-version: 111\ncompatibility-version: 1\n"
    printf 'exports:\n  - targets: [ armv7-ios ]\n    symbols: [ dyld_stub_binder'
    xcrun nm -u "$W/ltbench.c.o" "$W/start.S.o" | grep '^_' | sort -u | sed 's/^/, /' | tr -d '\n'
    printf ' ]\n...\n'
} > "$W/libSystem.tbd"
xcrun ld -arch armv7 -platform_version ios 5.0 5.0 -e _start -o "$W/LTBench" \
    "$W/ltbench.c.o" "$W/start.S.o" "$W/libSystem.tbd"
"$W/machotool" mkold "$W/LTBench" >/dev/null
ldid -S "$W/LTBench"
mkdir -p "$W/Payload/LTBench.app"
cp "$W/LTBench" "$HERE/Info.plist" "$W/Payload/LTBench.app/"
(cd "$W" && zip -qr "$OUT/LTBench.ipa" Payload)
echo "$OUT/LTBench.ipa"

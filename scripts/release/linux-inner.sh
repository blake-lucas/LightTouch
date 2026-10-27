#!/bin/sh
# The Linux release build, run by scripts/build-release.sh inside the Debian 11 (glibc 2.31) build
# image. /src/app and $QEMU_SOURCE are read-only release worktrees and /build the output. The prebuilt
# prefixes are mounted read-only at /build/{deps-full,crypto,mobile-private-runtime-ui15}, where they
# were built: their pkg-config files name those paths.
set -eu
LT_JOBS=${LT_JOBS:-4}
export LC_ALL=C.UTF-8 MAKEFLAGS=-j$LT_JOBS CMAKE_BUILD_PARALLEL_LEVEL=$LT_JOBS
git config --global --add safe.directory '*'
Q=$QEMU_SOURCE
[ "$(git -C "$Q" rev-parse HEAD)" = "$EXPECTED_QEMU_REV" ]
[ -z "$(git -C "$Q" status --porcelain=v1 --untracked-files=all)" ]
[ "$(git -C /src/app rev-parse HEAD)" = "$EXPECTED_APP_REV" ]
[ -z "$(git -C /src/app status --porcelain=v1 --untracked-files=all)" ]
# QEMU's configure writes into its source tree, so build from a private clone of the same commit.
rm -rf /build/qemu-source
git clone -q --shared --no-checkout "$Q" /build/qemu-source
git -C /build/qemu-source checkout -q --detach "$EXPECTED_QEMU_REV"
# The subprojects meson would download (no network here), slirp with its patches already applied.
for p in berkeley-softfloat-3 berkeley-testfloat-3 keycodemapdb dtc slirp; do cp -a "$Q/subprojects/$p" /build/qemu-source/subprojects/; done
[ -z "$(git -C /build/qemu-source status --porcelain=v1 --untracked-files=all)" ]
echo "=== ENGINE (LDFLAGS=-ldl: glibc 2.31 keeps dladdr in libdl)"
LDFLAGS=-ldl python3 /build/qemu-source/contrib/portable/build-engine.py /build/engine --cpp-standard=gnu++17 \
    --prefix /build/deps-full/prefix --crypto-prefix /build/crypto/prefix
echo "=== RUNTIME (the engine, the USB bridge and the libraries they load beyond the base system)"
rm -rf /build/runtime; mkdir -p /build/runtime/lib
cp /build/engine/libqemu-arm.so /build/mobile-private-runtime-ui15/prefix/bin/usbmuxd-qemu /build/runtime/
export LD_LIBRARY_PATH=/build/deps-full/prefix/lib:/build/crypto/prefix/lib:/build/mobile-private-runtime-ui15/prefix/lib
for l in libfdt.so.1 libimobiledevice-1.0.so.6 libimobiledevice-glue-1.0.so.0 libplist-2.0.so.4 libusbmuxd-2.0.so.7 \
         libssl.so.1.1 libcrypto.so.1.1; do
    p=$(ldd /build/runtime/libqemu-arm.so /build/runtime/usbmuxd-qemu | awk -v l="$l" '$1 == l { print $3; exit }')
    [ -n "$p" ] || { echo "neither the engine nor the USB bridge loads $l" >&2; exit 1; }
    cp -L "$p" /build/runtime/lib/
done
# Build-tree search paths must not reach the package; the app's launcher puts lib/ on the library path.
for f in /build/runtime/libqemu-arm.so /build/runtime/usbmuxd-qemu /build/runtime/lib/*; do patchelf --remove-rpath "$f"; done
printf '\nLINUX_RELEASE_BUILD_COMPLETE\n'

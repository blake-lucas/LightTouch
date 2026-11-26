#!/bin/bash
# linux-engine.sh MODE OUT [PROFILE_DIR]: the Linux release engine (Debian 11 image, GCC 10, /build paths as scripts/release/linux-inner.sh)
# at the engine checkout $ENGINE's HEAD. MODE base | gen (-fprofile-generate=/pgo, atomic counters) | use (PROFILE_DIR mounted at /pgo).
set -euo pipefail
MODE=$1; OUT=$(realpath -m "$2"); PROF=${3:-}
ENGINE=${ENGINE:-${QEMU_IOS:-$HOME/git-projects/qemu-ios}}
IN=${LT_LINUX_INPUTS:-${LT_INPUTS:-$HOME/Documents/LightTouch-Development/port-workspace/builds}/linux-desktop/bullseye-build01}
REV=$(git -C "$ENGINE" rev-parse HEAD); GD=$(git -C "$ENGINE" rev-parse --path-format=absolute --git-common-dir)
case $MODE in
  base) CF=""; LF="" ;;
  gen) CF="-fprofile-generate=/pgo -fprofile-update=atomic"; LF="-fprofile-generate=/pgo" ;;
  use) CF="-fprofile-use=/pgo -fprofile-correction -fprofile-partial-training -Wno-missing-profile -Wno-coverage-mismatch"; LF="" ;;
esac
mkdir -p "$OUT"; B=$OUT/build; mkdir -p $B
docker run --rm -v "$B:/b" ${LT_LINUX_IMAGE:-lighttouch-bullseye-collector08} sh -c 'rm -rf /b/* /b/.[!.]*' || true
M=(--mount "type=bind,src=$IN/deps-full,dst=/build/deps-full,readonly" --mount "type=bind,src=$IN/crypto,dst=/build/crypto,readonly"
   --mount "type=bind,src=$ENGINE,dst=/engine,readonly" --mount "type=bind,src=$GD,dst=$GD,readonly" --mount "type=bind,src=$B,dst=/build")
[ -f "$GD/objects/info/alternates" ] && while read -r a; do M+=(--mount "type=bind,src=$a,dst=$a,readonly"); done < "$GD/objects/info/alternates"
[ -n "$PROF" ] && M+=(--mount "type=bind,src=$(realpath "$PROF"),dst=/pgo,readonly")
docker run --rm --network none --cpus=6 "${M[@]}" -e LT_JOBS=6 ${LT_LINUX_IMAGE:-lighttouch-bullseye-collector08} /bin/sh -c "
set -eu; export LC_ALL=C.UTF-8; git config --global --add safe.directory '*'
git clone -q --shared --no-checkout /engine /build/qemu-source; git -C /build/qemu-source checkout -q --detach $REV
for p in berkeley-softfloat-3 berkeley-testfloat-3 keycodemapdb dtc slirp; do cp -a /engine/subprojects/\$p /build/qemu-source/subprojects/; done
LDFLAGS=-ldl python3 /build/qemu-source/contrib/portable/build-engine.py /build/engine --cpp-standard=gnu++17 \
  --prefix /build/deps-full/prefix --crypto-prefix /build/crypto/prefix --cflags='$CF' --ldflags='$LF' > /build/engine.log 2>&1 || { tail -20 /build/engine.log; exit 1; }
patchelf --remove-rpath /build/engine/libqemu-arm.so; cp /build/engine/libqemu-arm.so /build/libqemu-arm.so"
cp "$B/libqemu-arm.so" "$OUT/libqemu-arm.so"; echo "$MODE $REV" > "$OUT/what"; ls -la "$OUT/libqemu-arm.so"

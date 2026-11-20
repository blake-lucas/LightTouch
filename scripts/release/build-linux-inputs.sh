#!/bin/bash
# Build the Linux release's dependency prefixes from their pinned recipes, in the Debian 11 image
# (linux-build.Dockerfile): OUTPUT/deps-full (contrib/portable/build-deps.py: glib, pixman, FFmpeg, OpenSSL ...),
# OUTPUT/crypto (build-crypto-deps.py: libgcrypt) and OUTPUT/mobile-private-runtime-ui15
# (Linux/packaging/build-mobile-deps.py: the libimobiledevice stack and usbmuxd-qemu). That is the LT_LINUX_INPUTS
# folder scripts/build-release.sh's linux step mounts; each prefix is built at /build/<name>, the path its pkg-config
# files name, as the linux step mounts it. CI caches OUTPUT (.github/workflows/release-build.yml).
#
#   build-linux-inputs.sh ENGINE_SOURCE OUTPUT [IMAGE]
set -euo pipefail
APP="$(cd "$(dirname "$0")/../.." && pwd)"
Q=$(realpath "${1:?usage: build-linux-inputs.sh ENGINE_SOURCE OUTPUT [IMAGE]}")
OUT=$(realpath -m "${2:?output folder}")
IMAGE=${3:-lighttouch-linux-build}
JOBS=${LT_JOBS:-$(nproc)}
mkdir -p "$OUT/deps-full" "$OUT/crypto" "$OUT/mobile-private-runtime-ui15"
# Sources are downloaded, checked and unpacked here: Debian 11's Python 3.9 tarfile has no extraction filter.
python3 "$Q/contrib/portable/build-deps.py" "$OUT/deps-full" --prepare-only
python3 "$Q/contrib/portable/build-crypto-deps.py" "$OUT/crypto" --prepare-only
python3 "$APP/Linux/packaging/build-mobile-deps.py" "$OUT/mobile-private-runtime-ui15" --prepare-only
# The recipes ask make and ninja for -j2; shims on PATH let them use the machine.
docker run --rm -e "LT_JOBS=$JOBS" -e "LT_TRIM=${LT_TRIM:-0}" \
    --mount "type=bind,src=$Q,dst=/src/qemu,readonly" --mount "type=bind,src=$APP,dst=/src/app,readonly" \
    --mount "type=bind,src=$OUT/deps-full,dst=/build/deps-full" --mount "type=bind,src=$OUT/crypto,dst=/build/crypto" \
    --mount "type=bind,src=$OUT/mobile-private-runtime-ui15,dst=/build/mobile-private-runtime-ui15" \
    "$IMAGE" bash -euc '
        mkdir -p /tmp/shims
        for t in make ninja; do
            printf "#!/bin/bash\nargs=(); for a in \"\$@\"; do case \"\$a\" in -j2) args+=(-j$LT_JOBS) ;; *) args+=(\"\$a\") ;; esac; done\nexec %s \"\${args[@]}\"\n" \
                "$(command -v $t)" > /tmp/shims/$t
            chmod +x /tmp/shims/$t
        done
        export PATH=/tmp/shims:$PATH LC_ALL=C.UTF-8
        git config --global --add safe.directory "*"
        python3 /src/qemu/contrib/portable/build-deps.py /build/deps-full
        python3 /src/qemu/contrib/portable/build-crypto-deps.py /build/crypto
        python3 /src/app/Linux/packaging/build-mobile-deps.py /build/mobile-private-runtime-ui15
        # LT_TRIM=1 (CI) keeps only the prefixes and their records: the source and build trees are only for rebuilding.
        if [ "${LT_TRIM:-0}" = 1 ]; then
            for d in /build/deps-full /build/crypto /build/mobile-private-runtime-ui15; do
                find "$d" -mindepth 1 -maxdepth 1 ! -name prefix ! -name "*.json" -exec rm -rf {} +
            done
        fi
        # The prefixes stay readable by whoever restores them from a cache.
        chmod -R a+rX /build'
ls "$OUT"/*/prefix/lib | head -40

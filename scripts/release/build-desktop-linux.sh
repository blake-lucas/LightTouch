#!/bin/sh
# Build the desktop app and firmwarekit for Linux against glibc 2.31 in Debian 11 (desktop-linux.Dockerfile), with
# OpenSSL compiled in. Output: target/bullseye/release/{lighttouch,firmwarekit}, for package-desktop.py --os linux.
# The host's cargo registry is shared so crates download once.
set -eu
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
IMAGE=lighttouch-desktop-linux
docker build -q -t "$IMAGE" -f "$REPO/scripts/release/desktop-linux.Dockerfile" "$REPO/scripts/release" > /dev/null
mkdir -p "$HOME/.cargo/registry"
docker run --rm --user "$(id -u):$(id -g)" -v "$REPO:/src" -v "$HOME/.cargo/registry:/opt/cargo/registry" -w /src \
    -e CARGO_TARGET_DIR=/src/target/bullseye "$IMAGE" \
    cargo build --release --locked -p lighttouch -p lighttouch-firmware --features lighttouch-services/vendored-openssl
for f in lighttouch firmwarekit; do
    need=$(objdump -T "$REPO/target/bullseye/release/$f" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)
    echo "$f needs $need"
done

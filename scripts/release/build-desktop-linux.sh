#!/bin/sh
# Build the desktop app and firmwarekit for Linux against glibc 2.31 in Debian 11 (desktop-linux.Dockerfile), with
# OpenSSL compiled in. Output: target/bullseye/release/{lighttouch,firmwarekit}, for package-desktop.py --os linux.
# The host's cargo registry is shared so crates download once.
set -eu
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
IMAGE=lighttouch-desktop-linux
docker build -q -t "$IMAGE" -f "$REPO/scripts/release/desktop-linux.Dockerfile" "$REPO/scripts/release" > /dev/null
mkdir -p "$HOME/.cargo/registry"
# The commit the logs name (crates/lighttouch-core/build.rs): a worktree's git metadata is outside the mount, so git
# can't answer in the container; work it out here the way build.rs does.
COMMIT=$(git -C "$REPO" rev-parse --short=10 HEAD 2>/dev/null || echo unknown)
[ "$COMMIT" = unknown ] || [ -z "$(git -C "$REPO" status --porcelain --untracked-files=no)" ] || COMMIT="$COMMIT-modified"
docker run --rm --user "$(id -u):$(id -g)" -v "$REPO:/src" -v "$HOME/.cargo/registry:/opt/cargo/registry" -w /src \
    -e CARGO_TARGET_DIR=/src/target/bullseye -e LIGHTTOUCH_COMMIT="$COMMIT" "$IMAGE" \
    cargo build --release --locked -p lighttouch -p lighttouch-firmware --features lighttouch-services/vendored-openssl
# SDL2 for game controllers, loaded at run time (lighttouch_model::gamepad::sdl): Debian 11's, with its copyright file
# for the licence page. The AppImage carries it in usr/lib.
rm -rf "$REPO/target/bullseye/sdl" "$REPO/target/bullseye/sdl-notices"
mkdir -p "$REPO/target/bullseye/sdl" "$REPO/target/bullseye/sdl-notices"
docker run --rm --user "$(id -u):$(id -g)" -v "$REPO:/src" "$IMAGE" sh -c \
    'cp -L /usr/lib/x86_64-linux-gnu/libSDL2-2.0.so.0 /src/target/bullseye/sdl/ &&
     cp /usr/share/doc/libsdl2-2.0-0/copyright /src/target/bullseye/sdl-notices/SDL2-copyright.txt'
for f in lighttouch firmwarekit; do
    need=$(objdump -T "$REPO/target/bullseye/release/$f" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)
    echo "$f needs $need"
done

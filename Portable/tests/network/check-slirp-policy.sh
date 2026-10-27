#!/bin/sh
# Native host-only verification of the engine's actual vendored libslirp.
set -eu
engine=${1:?qemu-ios source required}
output=${2:?new native slirp build directory required}
meson_command=${3:-meson}
fixture=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ ! -f "$output/build.ninja" ]; then
    "$meson_command" setup "$output" "$engine/subprojects/slirp" --default-library=static
fi
"$meson_command" compile -C "$output" -j 4
"$meson_command" test -C "$output" --print-errorlogs
${CC:-cc} -std=gnu11 -Wall -Wextra -Werror -UNDEBUG \
    -I"$engine/subprojects/slirp/src" -I"$output" \
    $(pkg-config --cflags glib-2.0) "$fixture/SlirpPolicy.c" \
    "$output/libslirp.a" $(pkg-config --libs glib-2.0) -o "$output/lighttouch-policy-test"
"$output/lighttouch-policy-test"

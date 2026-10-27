Copies of the engine's exported ABI headers, so `cargo build` needs no engine checkout:

- `qemu-ios-ui.h` from `contrib/ios-app/`
- `qemu-macos-extras.h` from `contrib/macos-app/`

Taken from qemu-ios `1f5b965e33`. Set `LIGHTTOUCH_QEMU_SOURCE=/path/to/qemu-ios` to build against a checkout's
headers instead, and refresh these copies when the engine's API version changes.

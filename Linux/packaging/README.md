# Linux packaging pieces

The desktop app's Linux AppImage is built by `scripts/build-release.sh` (its `linux` and `desktop` steps) and
`scripts/release/package-desktop.py`. This folder keeps what those use and the USB bridge's own build:

| File | What it is |
|---|---|
| `build-mobile-deps.py` | Builds the hash-pinned libplist, libimobiledevice, libusbmuxd and OpenSSL stack and the private `usbmuxd-qemu` daemon (the USB bridge between the engine and the app's services), in the Debian 11 (glibc 2.31) build image. Pins come from `build-support/dependencies.json`. `--prepare-only` fetches and checks the sources; `--daemon-only-from DIR` rebuilds only the daemon against a stack built before. Its output is the `mobile-private-runtime-*` input the release's `linux` step mounts. |
| `patches/` | The daemon's patches: standard Linux headers, and the private QEMU socket configuration. Android's USB bridge build (`Android/packaging/build-mobile-android.py`) applies them too. |
| `refresh-private-daemon.py` | Rebuilds only the daemon (`build-mobile-deps.py --daemon-only-from`), runs `test-private-daemon.py` on it, then the command given after `--`. |
| `test-private-daemon.py` | Exercises the built daemon without a guest or a physical USB device. |
| `seal-appimage.py` | Seals an AppDir into an AppImage with the pinned appimagetool and runtime (`package-desktop.py --appimage`). |
| `lighttouch.svg` | The app icon in the AppImage. |

The AppImage needs glibc 2.31 or later and X11 or Wayland. The host supplies the GPU drivers (EGL/GL), glibc
and the C++ runtime; the AppImage bundles the engine, the USB bridge and the seven libraries they load beyond the
base system. Without FUSE, run it with `--appimage-extract` and then `squashfs-root/AppRun`.

# Desktop app (Rust)

One app for Linux, Windows and macOS, built with `cargo build --release`. It replaced the Python/Tk launcher, the Linux
SDL worker, the Windows C# helper and the macOS Swift host, which are gone from the repository. It aims for parity with the Android app: preparing
devices from an IPSW (ipsw.me / archive.org), the Legacy Store with specific app versions, IPA installs, and the same
look. Work happens on `dev/desktop` (app) and the engine's `dev/desktop`.

## Layout

| Crate | What it is |
| --- | --- |
| `lighttouch-sys` | The shared C++ core (`Portable/`, also used by Android) compiled by `cc`, plus raw bindings to `lighttouch.h`. The engine's two ABI headers are vendored in `engine-include/`. |
| `lighttouch-core` | Safe wrappers: device library, engine runtime, the worker line protocol, frames over shared memory, app paths. |
| `lighttouch-catalog` | The firmware catalog (built in), IPSW and boot ROM downloads, guest tools selection, Legacy Store client, IPA checks. |
| `lighttouch-services` | Guest services over the emulated USB cable: the usbmuxd-qemu bridge, usbmux, lockdown (TLS 1.0 via OpenSSL), AFC, installation proxy. |
| `lighttouch-model` | All app behaviour without a UI toolkit: devices, sessions, background tasks, settings. |
| `lighttouch` | The executable: the egui frontend (`src/ui/`), `worker` mode, developer commands (`list`, `prepare`, `run`). |

Processes: the app, plus one `lighttouch worker` per running device (the engine starts once per process). The worker
loads the engine library through the core, plays audio with cpal, writes frames into shared memory the app created,
and speaks JSON lines on stdin/stdout. The UI only renders `Model` state and calls its methods, so the toolkit can be
replaced without touching behaviour.

## Running from a checkout

```sh
cargo build
LIGHTTOUCH_ENGINE=/path/to/libqemu-arm.so LIGHTTOUCH_GUEST_TOOLS=/path/to/guest-tools target/debug/lighttouch
target/debug/lighttouch run <record id> --seconds 60 --shot frame.ppm   # headless session
LIGHTTOUCH_FIRMWAREKIT=/path/to/firmwarekit target/debug/lighttouch prepare n72ap-7E18 [--ipsw FILE]
LIGHTTOUCH_USBMUXD=/path/to/usbmuxd-qemu target/debug/lighttouch run <record id> --store "doodle jump"   # or --install App.ipa, --services
```

Windows, cross-built from Linux with the posix-threads MinGW toolchain (`.cargo/config.toml` picks the linker):

```sh
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc-posix CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++-posix \
       AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
cargo build --release --target x86_64-pc-windows-gnu --features lighttouch-services/vendored-openssl
```

The engine DLL is qemu-ios's MinGW build relinked by `contrib/portable/make-shared-windows.py`; it and its DLLs,
`lighttouch-usbmuxd.exe` and `guest-tools/` go in `resources\` beside `lighttouch.exe`.

`scripts/release/package-desktop.py` puts a folder together from those pieces, including the notices the licence
page shows (`--notices engine=DIR --notices usb=DIR` for the engine's and the USB bridge's licence texts; the app's
and firmwarekit's are staged from `cargo metadata`):

```sh
python3 scripts/release/package-desktop.py out/LightTouch-windows-x86_64 --os windows \
    --app target/x86_64-pc-windows-gnu/release/lighttouch.exe \
    --firmwarekit target/x86_64-pc-windows-gnu/release/firmwarekit.exe \
    --engine libqemu-arm.dll --usbmuxd lighttouch-usbmuxd.exe --guest-tools guest-tools --lib dlls/ \
    --notices engine=notices/engine --notices usb=notices/usb \
    --group-source engine=https://github.com/blake-lucas/qemu-ios --zip
```

`LIGHTTOUCH_DATA` overrides the data folder (default `~/.local/share/LightTouch`, `%LOCALAPPDATA%\LightTouch\App`,
`~/Library/Application Support/LightTouch`); `LIGHTTOUCH_RESOURCES` the bundled resources folder.
To check the UI where nothing can capture the screen (over ssh), `LIGHTTOUCH_UI_SHOT=file.png` saves the window after
`LIGHTTOUCH_UI_SHOT_AFTER` seconds and closes it; `LIGHTTOUCH_UI_START=1` starts the selected device first.

## Status

| Area | State |
| --- | --- |
| Device library, rename, delete | Done. Devices are added only by preparing them from an IPSW (`.itbase` import is gone, on Android too) |
| Start, stop, recover, pause, mute, screenshot (PNG), frames, touch, buttons, keys | Done (Linux, checked under Xvfb). Like Android: tilt from the arrow keys (accelerometer, plus gyro where the board has one), and a finger dragged past the screen edge holds there. For mouse users, Alt-drag (Option on a Mac) adds a mirrored second finger to pinch or rotate, Alt+Shift moves two fingers together; the ? button beside Settings opens a Controls dialog listing them (Settings has a summary too). Rotate (dock, full screen menu) turns the device for landscape apps: portrait, Home on the right, Home on the left; clicks and tilt arrows follow the turn. While iOS's display sleeps the screen is black and the drawn Home and Sleep/Wake (or H, P) wake it |
| Theme, device frame, overlay (FPS/basic/full), full screen, settings | Done. The Full overlay has Android's rows: guest CPU, GL, lock and run time per frame from the live detailed-timing capture, and memory. Settings previews the overlay as set and explains the headphones switch |
| Frame inspector (Settings > Diagnostics > Inspect button) | Done like Android's: pauses the guest and charts its last two minutes of frames (overview with range selection, per-frame bars, stats, histogram), saves a report folder under `reports/` in the data folder. With Detailed frame timing on (Settings > Diagnostics) an engine capture runs while the guest does (`profiles/`, newest two kept plus any the inspector claimed), and the inspector splits each frame into GL, lock waits and guest CPU, with the slowest engine events. Reports are named after the app in front (asked of the guest when it pauses). Also from full screen; with detailed timing off, Settings can record a profile for tools/perf/profile.py |
| Prepare from an IPSW (catalog, download, firmwarekit sidecar, adoption) | Done (Linux) with the Rust firmwarekit: n72ap-7E18 prepared in 15 s and n81ap-10B500 in 126 s (keybag and seal boots included), both boot to the lock screen. Closing the window mid-preparation asks first; if LightTouch ends anyway, firmwarekit ends with it and the next start clears the half-built job and reopens the same preparation (a download resumes). While it runs the device list shows the new device with its progress (click it for the details); the last step before preparing is laid out like Android's |
| Legacy Store, specific versions, IPA install, last crash (Rust usbmux/lockdown/AFC/instproxy over usbmuxd-qemu) | Done (Linux), checked on iPod touch 2G 3.1.3 and 4.2.1 and iPhone 3GS 5.1.1. Matches Android's dev/perf store: tabs for the device's list, Installed (with newer versions), Featured, Charts, Categories and Collections, app pages with screenshots, and an install queue behind a header button (cancel until iOS commits, retry). Following the guest when it reattaches under a new usbmux id is written but not yet exercised |
| macOS | Everything works on an M-series Mac with an engine built from source (`macOS/build-engine.py`, now with nettle: QEMU has no built-in AES, and without a crypto library the guest's AES engine failed the 4.x+ keybag boot with a CDMA error). The Rust app and firmwarekit build there; the iPod touch 4G 6.1.6 prepares in 41 s and boots to the lock screen, the iPod touch 2G 3.1.3 in 10 s; sessions, the UI (Retina, system theme) and Legacy Store installs work. `package-desktop.py --os macos` makes LightTouch.app (ad-hoc signed, Homebrew dylibs in Contents/Frameworks, their licences in the engine notices) |
| Windows | Builds by cross-compiling from Linux (below). On real Windows 10/11 (user's RTX 2080 Ti): preparation with the Rust firmwarekit, sessions, GPU drawing and audio work for the iPod touch 2G 3.1.3, 3G 4.2.1, 4G 6.1.6, iPhone 3GS 5.1.1 and iPhone 4 7.1.2. The bundled `lighttouch-usbmuxd.exe` is built without libimobiledevice, so unlike usbmuxd-qemu it never pairs with the guest; the app pairs itself there (`lockdown.rs`, `pairing.rs`, as libimobiledevice's `lockdownd_pair`) and stores the record with the daemon. Legacy Store installs and guest networking checked by the user on real Windows with v0.0.12. Under Wine app services can't run (the daemon checks who owns each loopback connection). Prepare works under Wine end to end: the iPod touch 4G 6.1.6 prepares in about 5 minutes (keybag and setup boots included), boots to SpringBoard and stops cleanly. That needed two Windows fixes: flushing firmwarekit's read-only outputs when the device is added, and an engine that sizes new NAND files sparsely instead of through MinGW's `ftruncate` (which refuses to grow a file past the free disk space and crashed under Wine). One prepare out of three timed out in the keybag boot with the engine still running after the guest halted; not reproduced since |
| Guest networking (Settings > Network), like Android's (docs/NETWORK.md) | Done (Linux): off by default; Direct internet or the Wayback Machine at a chosen day, LAN opt-in, every change live (turning networking on included) and a Current session card. The worker makes the device's proxy (`<data>/network/<record>`) and installs its CA and Wi-Fi proxy through the guest tools. Checked on an iPod touch 2G 4.2.1: setup Ready at about 30 s, Safari loads Google directly, Yahoo as archived on 9 September 2009, then (live) 1 March 2000, then live again directly. `lighttouch run --network direct|archive[:yyyyMMdd] [--lan]` for headless tests. Windows works too (AF_UNIX forward), checked by the user with Wayback and direct internet. macOS (keychain roots) unchecked |
| Open source licenses (Settings > About), like the Android licence dialog | Done: lists `resources/licenses/<group>/manifest.json` (frontend, engine, firmware, usb) and shows a file once its SHA-256 matches. `stage-notices.py --package lighttouch` stages the app's crate notices |
| Packaging | `scripts/release/build-desktop-linux.sh` builds the app and firmwarekit in Debian 11 (they need glibc 2.30; the engine 2.30), and `scripts/release/package-desktop.py` assembles the Linux or Windows folder (plus a zip, or with `--appimage` an AppImage sealed by the release pipeline's sealer) or the macOS LightTouch.app, each with every component's notices. Checked: the AppImage prepares an iPod touch 2G 3.1.3 and boots it; LightTouch.app prepares and runs devices with only its bundled resources; the Windows folder runs under Wine. `scripts/build-release.sh VERSION` publishes these: its `desktop` step builds the Linux AppImage and Windows zip from the release's own engine and USB pieces (it needs the engine's Windows DLL relink, on qemu-ios dev/desktop), and its `macos` step builds LightTouch.app on the Mac. `macOS/build-engine.py --ffmpeg-archive` builds the decoder-only FFmpeg the Windows build uses instead of linking Homebrew's (LightTouch.app 102 MB to 73 MB; checked: the iPod touch 4G boots to its lock screen). Still to do: a notarized Mac build (needs a Developer ID) |
| `firmwarekit` in Rust (`crates/lighttouch-firmware`; Windows prep, no Swift toolchain) | Done for every board recipe: K48 family (iPad 1, iPod touch 3G/4G, iPhone 3GS/4), iPod touch 2G and iPhone OS 1.x (iPod touch 1G, original iPhone). Checked against the Swift tool: identity, kboot, NOR, iBoot and GID blobs byte-identical; every fit check, the guest package and activation records equal; the volumes equal file for file except binary plists' object order. 1.x now prepares on Linux (Swift needed a Mac) and boots to SpringBoard; the iPod touch 2G's 4.x keybag boot and sibling keybag ramdisks work. The same inputs give a byte-identical device on Linux, macOS and Windows (Wine). `scripts/build.sh firmwarekit-android` builds it for the APK; on a Poco F3 it prepared the iPod touch 2G 3.1.3 in 32 s with the APK's engine, byte-identical to the Linux run (NAND, NOR, boot files; the lock differs only in its timestamp and input paths). Through the APK's UI on the Poco (2026-10-26): iPod touch 2G 3.1.3 in 30 s and iPod touch 4G 6.1.6 in 131 s (keybag boot 19 s, first start 79 s), both start to the lock screen. The Mac prepares too, with an engine built from source. The APK bundles this firmwarekit; the Swift one is gone |
| Rust tcp_usb host replacing usbmuxd-qemu | Later |

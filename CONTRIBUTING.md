# Contributing to LightTouch

LightTouch runs a library of emulated legacy iOS devices (iPhone, iPhone 3GS, iPhone 4, iPod touch 1G to 4G and
iPad) on Android, Linux, Windows and macOS. Start with [README.md](README.md), [docs/DESKTOP.md](docs/DESKTOP.md) (the
desktop app) and [docs/BUILDING.md](docs/BUILDING.md).

## Layout

| Path | What |
|---|---|
| `crates/` | The Rust workspace: the desktop app and `firmwarekit` (below) |
| `Portable/` | The shared C/C++ core both apps link: runtime, storage, device library, network proxy, guest package and screenshot code, with its tests; `firmware/firmware-catalog.json` is the firmware catalog |
| `Android/` | The Android app (Kotlin, JNI over `Portable/`), its packaging and phone test scripts |
| `Linux/packaging/`, `Windows/`, `macOS/` | What the desktop release still needs per OS: the USB bridge builds, the engine builds, the AppImage sealer |
| `scripts/` | `build.sh` (development builds), `build-release.sh` and `release/` (release builds) |
| `build-support/` | `dependencies.json` (pinned source archives), licence texts and build patches |
| `tools/` | `activation/`, `perf/` (benchmark and profiling harness), `guestbench/` (LT Bench), `compat/` |
| `tests/` | `activation/` and `port/` (the Android package check) |

The crates:

| Crate | What |
|---|---|
| `lighttouch` | The desktop app (egui), one executable: the app, the per-device `worker`, and developer commands (`list`, `prepare`, `run`) |
| `lighttouch-model` | Everything the app does without a UI toolkit: devices, preparing, sessions, settings, the Store |
| `lighttouch-core`, `lighttouch-sys` | Safe Rust over `Portable/include/lighttouch.h`, and the raw bindings |
| `lighttouch-catalog` | The firmware catalog, downloads (IPSWs, boot ROMs), guest tools, the Legacy Store and IPA checks |
| `lighttouch-services` | Guest services over the emulated USB cable: pairing, lockdown, app installs, AFC |
| `lighttouch-firmware` | `firmwarekit`: prepares a device from an IPSW for every board. Android runs the same binary |

## Firmwares in the catalog

The [firmware catalog](Portable/firmware/firmware-catalog.json) is the source of truth for builds, availability, keys
and preparation recipes. `firmwarekit` reads it ([Portable/firmware/README.md](Portable/firmware/README.md)):

```sh
firmwarekit create --catalog Portable/firmware/firmware-catalog.json \
  --id n72ap-7E18 --ipsw /path/to/stock.ipsw --bootrom bootrom.bin --guest-tools GUEST-TOOLS --out /path/to/new-device
```

Catalog availability is distinct from measured acceptance ([docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)).

## Repositories

| Repo | What it is |
|---|---|
| This repository | The apps, the shared core, the preparer and the packaging recipes |
| [qemu-ios](https://github.com/blake-lucas/qemu-ios) | The emulator (`hw/arm/ipod_touch_*.c`, `hw/arm/ipad1.c`, `hw/arm/s5l89*`) and the guest tools under `contrib/`; every app builds against it |
| [usbmuxd](https://github.com/samhenrigold/usbmuxd) (fork) | The usbmuxd that bridges the emulated USB device to the app's services; its commit is pinned in `Linux/packaging/build-mobile-deps.py` and `Windows/usbmux/SOURCES.json` |

## Building

`cargo build` builds the desktop app and `firmwarekit`; [docs/DESKTOP.md](docs/DESKTOP.md) says how to run it.
`scripts/build.sh` builds the engines, the Android APK, `firmwarekit` for Android and (on a Mac, over ssh) the guest
tools and LT Bench, with outputs in `build/`: see [docs/BUILDING.md](docs/BUILDING.md). `scripts/build-release.sh`
builds a release for every platform.

## Tests

No runner wraps these; run what covers the code you changed.

```sh
cargo test                                      # the Rust workspace
scripts/build.sh portable-tests                 # Portable/tests (CTest)
python3 -m unittest discover -s tests/port      # the Android package check
sh tests/activation/run.sh                      # the activation recognizer
cd Android && ./gradlew testDebugUnitTest       # Android unit tests (the properties Android/BUILD-AND-RUN.md lists)
```

Headless boots for checking a change end to end: `lighttouch prepare ID` and `lighttouch run ID --seconds N
--script FILE --shot FILE.ppm` (docs/DESKTOP.md).

## Style

**Rust.** `rustfmt` with the repository's `rustfmt.toml` (120 columns), on the files you touched. No warnings.
`///` documents items, `//!` a module; comments say why, not what. American spelling, except cancelled, cancelling
and cancellable.

**C and C++.** `clang-format` (`.clang-format`).

**Kotlin.** Match the surrounding code.

**Files.** A file whose role isn't obvious from its name opens with a short comment saying what it is and why. No
created-by or license headers.

## Boards

A board's facts have two homes. The emulator's: qemu-ios's `qemu_ios_device_info` table
(`contrib/ios-app/qemu-ios-ui.c`): the `-M` machine, the board ID, the screen, the modem, the USB host and the
`panel=` limits; `firmwarekit oneshot '{"mode":{"machines":{}}}'` lists them. The preparer's: `Board` in
`crates/lighttouch-firmware/src/boot.rs` and its recipe (`k48_recipe.rs` for the S5L8920 and S5L8930 boards,
`n72_recipe.rs`, `n45_recipe.rs`). Names and models for the UI are in `crates/lighttouch-catalog` (and
`FirmwareCatalog.kt` on Android). A new board's firmware is catalog entries.

`device.lock.json` is written by `firmwarekit` and read by the core when a prepared device is added and started.

## Rules

- **Activation is maintained separately.** `firmwarekit` runs it as a built-in preparation step
  (`crates/lighttouch-firmware/csrc/activation.c`, `tools/activation/`). Treat it as a black box.
- **No firmware in any repo.** IPSWs, decrypted components, NAND images, SDKs and prepared devices stay outside the
  repositories.
- **Engine changes.** A qemu-ios change an app needs lands there first, on the matching branch.

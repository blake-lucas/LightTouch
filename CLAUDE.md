# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

LightTouch runs classic iPhone OS / iOS devices (iPod touch 1G–4G, iPhone to iPhone 5, iPad) in an emulator on
Android, Linux, Windows and macOS. Start with `README.md`, `CONTRIBUTING.md` (layout, tests, style, rules),
`docs/DESKTOP.md` and `docs/BUILDING.md`; prefer them over the older `Portable/README.md`, `Android/README.md` and the
`Android/*.md` acceptance notes, which describe earlier states.

## Two repositories

- **This repo**: the apps, the shared C++ core, the preparer (`firmwarekit`) and the packaging.
- **The engine, qemu-ios** (`$QEMU_IOS`, default `~/git-projects/qemu-ios`): QEMU with the iOS boards
  (`hw/arm/ipod_touch_*.c`, `hw/arm/ipad1.c`, `hw/arm/s5l89*`) and the guest tools under `contrib/`. It is built as a
  shared library (`libqemu-arm.so` / DLL) that the core loads at runtime. An engine change an app needs lands there
  first, on the matching branch.

Both repos have `main` (releases), `develop` (the integration branch for the next version) and feature branches
`dev/*`. Feature branches merge `develop` in and are merged back into `develop`. A user-facing feature is merged only
after the owner has reviewed a release (non-debuggable) build of it; test builds come from `develop`.

## Commands

```sh
cargo build                                  # desktop app (target/debug/lighttouch) + firmwarekit
cargo test                                   # Rust workspace
cargo test -p lighttouch-firmware NAME       # one crate / one test

scripts/build.sh linux-engine                # build/linux/libqemu-arm.so (from $QEMU_IOS)
scripts/build.sh android                     # engine + firmwarekit-android + build/android/LightTouch-debug.apk
scripts/build.sh android-test                # Android unit tests (Gradle needs the properties this script passes)
scripts/build.sh portable-tests              # Portable/ C++ tests (CMake + CTest, needs $QEMU_IOS)
(cd build/obj/portable-tests && ctest -R lighttouch_nand_disk --output-on-failure)   # one Portable test (ctest -N lists them)
scripts/build.sh --clean TARGET              # reconfigure from scratch after a configure-level change

python3 -m unittest discover -s tests/port   # Android package check
sh tests/activation/run.sh                   # activation recognizer
```

There is no single test runner: run what covers the code you changed. Outputs go to `build/` (object trees in
`build/obj/`, incremental). Machine-local inputs (`QEMU_IOS`, `LT_INPUTS` prebuilt deps/ANGLE/notices, `ANDROID_HOME`,
NDK, `LT_GUEST_TOOLS`, `LT_MAC` for the Mac-only guest-package/ltbench/firmware-tools targets) come from the
environment or an untracked `scripts/build.env`; `docs/BUILDING.md` lists them.

**macOS CI replaces the Mac for Mac-only builds**, so no local Mac is needed:

```sh
gh workflow run guest-package.yml -R blake-lucas/LightTouch -f engine_ref=<qemu-ios sha or branch>   # guest tools
gh run download <run id> -R blake-lucas/LightTouch -n guest-tools -D DIR   # the set LT_GUEST_TOOLS takes
gh workflow run release-build.yml -R blake-lucas/LightTouch --ref develop -f engine_ref=<sha> \
    -f guest_tools_run=<guest-package run id> -f platforms=android,desktop,macos
gh workflow run ltbench.yml -R blake-lucas/LightTouch     # LTBench.ipa
```

`guest-package.yml` builds `armv6.itpack`/`armv7.itpack` and the full guest-tools set (artifact `guest-tools`) from
any engine commit on a macOS runner; a release or test build takes a run's set through `guest_tools_run` (CI) or
`LT_GUEST_TOOLS` (local, after `gh run download`). A guest-side change is: engine commit (bump
`contrib/guest-package/VERSION`'s serial), push, a `guest-package.yml` run, then app builds with that set.

End-to-end checks without the UI (see `docs/DESKTOP.md`):

```sh
LIGHTTOUCH_FIRMWAREKIT=... target/debug/lighttouch prepare n72ap-7E18 [--ipsw FILE]
LIGHTTOUCH_ENGINE=build/linux/libqemu-arm.so target/debug/lighttouch run <record id> --seconds 60 \
    [--script FILE] [--shot frame.ppm] [--network direct|archive[:yyyyMMdd]]
LIGHTTOUCH_USBMUXD=... target/debug/lighttouch run <record id> --store "doodle jump"   # or --install App.ipa
```

On phones, `tools/perf/fcbench.py APK SECONDS GAME` (with `ANDROID_SERIAL`) installs, boots and measures a game;
`docs/PERFORMANCE.md` describes the benchmark set and A/B method. Engine A/B switches are environment variables (a
debug APK reads them from the app's `files/debug-env`). Release builds: `scripts/build-release.sh VERSION [STEP...]`
(`docs/BUILDING.md`, "Release builds").

## Architecture

**One native core, two frontends.** `Portable/` is the C++ core both apps link, with a single C ABI in
`Portable/include/lighttouch.h`: the device library (records, assets, locks, overlays), the engine runtime (loading
the engine library, boot assembly, frames, audio, input, sensors), storage/boot routes (`boot_route` in
`Portable/src/Storage.cpp`), guest package handling, network proxy, transfer/import. Android reaches it through JNI
(`Android/app/src/main/cpp/lighttouch_jni.cpp`); the desktop through `lighttouch-sys` (compiles `Portable/` with `cc`,
raw bindings) and `lighttouch-core` (safe wrappers).

**Desktop app** (`crates/`, one Rust workspace, edition 2024):
- `lighttouch` is one executable with three roles: the egui app, `lighttouch worker` (one process per running device;
  the engine can start only once per process), and developer commands (`list`, `prepare`, `run`). The worker speaks
  JSON lines on stdin/stdout, writes frames into shared memory the app created, and plays audio with cpal.
- `lighttouch-model` holds all behaviour without a UI toolkit (devices, preparing, sessions, settings, the Store);
  the UI in `crates/lighttouch/src/ui/` only renders `Model` state and calls its methods. Put behaviour in the model.
- `lighttouch-catalog`: firmware catalog, IPSW/boot ROM downloads, guest tool selection, Legacy Store, IPA checks.
- `lighttouch-services`: guest services over the emulated USB cable (usbmuxd-qemu bridge, lockdown over TLS 1.0,
  AFC, installation proxy, pairing).

**Android app** (`Android/`, Kotlin/Compose, package `org.lighttouch.android`): the UI process talks over Binder (AIDL
in `src/main/aidl`) to `EngineService` in a separate `:engine` process, the only one that loads JNI and QEMU, and
`FirmwareService` runs preparation in `:firmware`. Video renders directly into a transferred Surface.

**Preparing a device** (`crates/lighttouch-firmware`, the `firmwarekit` binary, shipped on every platform including
inside the APK): turns a stock IPSW into a prepared device. `Portable/firmware/firmware-catalog.json` is the source of
truth for builds, availability, keys and preparation recipes (`Portable/firmware/README.md`). `firmwarekit` writes
`device.lock.json`, which the core reads when the device is added and started. Prepared base images are never
modified; each device's writable state is a separate overlay.

**Board facts live in two places**: the engine's `qemu_ios_device_info` table (qemu-ios
`contrib/ios-app/qemu-ios-ui.c`: machine, board ID, screen, modem, USB, panel limits), and the preparer's `Board` in
`crates/lighttouch-firmware/src/boot.rs` plus its recipe (`k48_recipe.rs` for S5L8920/8930 boards, `n72_recipe.rs`,
`n45_recipe.rs`, `n42_recipe.rs` with the `a6_*` modules for the iPhone 5). UI names/models are in
`crates/lighttouch-catalog` and `FirmwareCatalog.kt` on Android. A new board's firmware is catalog entries.

**Guest package**: the tools inside iOS (GL shim, agent, `it_boot`, …) are armv6/armv7 `.itpack` files built on macOS
from qemu-ios (by the `guest-package.yml` CI workflow, or `scripts/build.sh guest-package` with `LT_MAC`). A device gets one only through its record (`records/ID/assets/guest.itpack`, pinned by
`guest_package_sha256` in `record.json`); the runtime's verdicts are in `Portable/src/GuestPackageJudge.hpp`.

## Rules and style

- **Activation is a black box**, maintained separately (`crates/lighttouch-firmware/csrc/activation.c`,
  `tools/activation/`).
- **No firmware in any repo**: IPSWs, decrypted components, NAND images, SDKs and prepared devices stay outside.
- **Don't patch iOS binaries.** Modifying the guest's system files is a last resort that needs the project owner's
  approval each time; look for the fix in the engine, the host side or configuration (launchd plists, prefs) first.
  The one standing exception is the iPhone 5 (n42ap, iOS 10), where code signing makes system edits unavoidable for
  booting and app installs; keep those minimal and listed. Likewise no hacks that fake guest time (e.g. idle
  fast-forward) to hide guest waits.
- **Performance changes are recorded in `docs/PERFORMANCE.md` in the same commit**: what changed, before → after
  numbers labelled in-guest or host-side, and rejected experiments as one row each.
- Don't publish built guest packages without the project owner's go-ahead.
- Rust: `rustfmt` (repo `rustfmt.toml`, 120 columns) on the files you touched only, never a workspace-wide
  `cargo fmt`. No warnings. `///` on items, `//!` on modules; comments say why, not what. American spelling, except
  cancelled / cancelling / cancellable.
- C/C++: `clang-format` (`.clang-format`). Kotlin: match the surrounding code.
- A file whose role isn't obvious from its name opens with a short comment saying what it is and why. No created-by
  or license headers.
- `docs/` is mostly git-ignored local notes; only the files whitelisted in `.gitignore` are tracked. Keep tracked docs
  short and coherent (a few hundred lines each, one table row per experiment); detail lives in git history.
- Commit subjects are plain sentences prefixed by area (`Android: …`, `Desktop: …`, `CI: …`,
  `Merge dev/x: …`) describing the user-visible effect.

## Lessons learned

- **Preparation stays lean**: `firmwarekit create` does only what the result needs. No backup copies, retry loops or
  extra verification boots; a final hash is the one check worth keeping. If preparation fails, the user prepares again.
- **No silent waits**: import, preparation and Start show a real progress bar (phase + bytes) and use multiple threads;
  never a long spinner. Test both the IPSW-download route and the prepared-folder/import route.
- **UI**: buttons size to their label, not full width; dialog titles stay small; one control per action (no duplicates
  in a dock); show "<device> <iOS>", never firmware ids like `n72ap-7E18`; don't capture phone hardware keys by default
  (opt-in setting); every screen shares the app's styling.
- **Measuring performance**:
  - Measure on real phones, and measure games, not just boot.
  - A/B with interleaved pairs from one session. Phones drift 1–2 fps between sessions and throttle on the charger.
  - Judge a game against its own frame cap. When a game is capped (often 30 fps), compare vCPU busy time instead of fps.
  - New engine switches are environment variables (`LT_X=off` to disable), so one build can be measured both ways.
- **Stale engine build tree**: `build/obj/android-engine-arm64` (and `linux-engine`) stays configured against
  whichever `QEMU_IOS` it was first configured with. After building from another engine checkout, run
  `scripts/build.sh --clean android-engine`. Before a phone A/B, check
  `grep -o 'qemu-ios[a-z-]*/configure' build/obj/android-engine-arm64/config.status`.
- **PGO matches by relative source path**: the engine profile only applies fully when the build directory layout
  matches the one it was recorded with. Release worktrees (`release-X/app` + `release-X/engine`) differ from
  `~/git-projects/qemu-ios`, so build A/B or tester APKs with the release layout.
- **`LT_INPUTS` is not disposable**: it holds the prebuilt deps, ANGLE, PGO profile and notices both build scripts
  read. Rebuilding it is slow, so never delete it when freeing disk.
- **Wayback proxy** (`Portable/network/Network.cpp`): at most 6 kept-alive connections to web.archive.org. Many
  separate HTTP/1.1 connections get the IP refused for hours, so load-test with ≤6 parallel and few repeats.
- **Release notes** are written for newcomers: plain words, no internal names (BQL, commit hashes outside the source
  section), and no hard line wraps (GitHub renders each newline as a break).
- In zsh, write `${PKG}:engine`, never `$PKG:engine` (`:e` is a modifier).

# Building (development)

The desktop app and `firmwarekit` are a Rust workspace: `cargo build` (see [DESKTOP.md](DESKTOP.md) for running
it against an engine). `scripts/build.sh` builds the rest of what day-to-day work needs, with outputs saved under
`build/` in this repository (ignored by git). Object trees go in `build/obj/` and build incrementally.

Networking needs an engine with slirp and fresh firmware tools, including `ittrust`/`itproxy`. See
[Guest networking](NETWORK.md) for OpenSSL inputs and portable verification.

```sh
scripts/build.sh linux-engine          # build/linux/libqemu-arm.so
scripts/build.sh android               # build/android/engine/arm64-v8a/ + build/android/LightTouch-debug.apk
scripts/build.sh portable-tests        # Portable/ unit tests (ctest)
LT_MAC=user@mac scripts/build.sh guest-package ltbench   # build/guest-package/*.itpack, build/ltbench/LTBench.ipa
scripts/build.sh all                   # linux-engine android portable-tests (+ the Mac targets when LT_MAC is set)
scripts/build.sh --clean android       # reconfigure from scratch (after a configure-level change)
```

| Target | Output | Built from |
| --- | --- | --- |
| `linux-engine` | `build/linux/libqemu-arm.so` | qemu-ios (`$QEMU_IOS`), portable board, x86_64 |
| `android-engine` | `build/android/engine/arm64-v8a/` (`libqemu-arm.so`, `libc++_shared.so`, manifest, notices) | qemu-ios with NDK clang, API 29 |
| `android` | the engine, `firmwarekit-android` and `build/android/LightTouch-debug.apk` (arm64-v8a, debug) | `Android/` Gradle |
| `firmwarekit` | `build/firmwarekit/linux/firmwarekit` | `crates/lighttouch-firmware` (Rust; links only libc) |
| `firmwarekit-android` | `build/firmwarekit/android/` (the same, arm64, plus `licenses/`) | the same for `aarch64-linux-android` (`rustup target add aarch64-linux-android`) with NDK clang; `licenses/` from `crates/lighttouch-firmware/stage-notices.py` |
| `portable-tests` | test results | `Portable/` with `-DLIGHTTOUCH_BUILD_TESTS=ON` |
| `guest-package` | `build/guest-package/{armv6,armv7}.itpack`, `build.log`, `VERSION`, `SHA256SUMS` | qemu-ios `contrib/guest-package/build.sh`, run on the Mac over ssh |
| `ltbench` | `build/ltbench/LTBench.ipa` | `tools/guestbench/build.sh`, on the Mac over ssh |

The desktop app's packages for Linux, Windows and macOS come from `scripts/build-release.sh` (below).

## Engine changes

An engine change happens in the qemu-ios checkout (`$QEMU_IOS`, default `~/git-projects/qemu-ios`,
branch `dev/perf`). `scripts/build.sh android` (or `linux-engine`) rebuilds it incrementally and packages it
again. Known issue: the first configure of a fresh Android tree has once stopped at QEMU's meson
postconf with `FileNotFoundError: .../pyvenv/bin/meson` although the file exists afterwards; running
the same command again built normally. The engine is QEMU's `qemu-system-arm` link turned into a shared library by qemu-ios
`contrib/portable/make-shared.py`. The build keeps the link's response file (`ninja -d keeprsp`), which
that step reads. The Android APK checks that the engine notice bundle names the library by SHA-256, so
the script rewrites `notices/manifest.json` after each engine build.

Engine A/B switches are environment variables. On Android a debug APK reads them from the app's
`files/debug-env` (`KEY=VALUE` lines), so one APK can be measured both ways (`docs/PERFORMANCE.md`).

### Engine profile (PGO)

The Android engine is built with profile-guided optimization when `LT_ENGINE_PGO` (default
`$LT_INPUTS/backend/pgo/engine-arm64.profdata`) exists: clang lays the engine's hot code out together,
which gave Jetpack Joyride on the Poco F3 about 8% more frames (`docs/PERFORMANCE.md`). `LT_ENGINE_PGO=off`
builds without it, and a different profile reconfigures the engine on the next build. The profile comes
from gameplay on a phone:

```
LT_ENGINE_VARIANT=pgogen LT_ENGINE_CFLAGS=-fprofile-generate LT_ENGINE_LDFLAGS=-fprofile-generate \
  scripts/build.sh android                      # an instrumented engine in build/obj/android-engine-arm64-pgogen
tools/perf/pgo-collect.sh build/android/LightTouch-debug.apk engine-arm64.profdata \
  n81-jetpack-play n81-doodlejump n81-realracing n81-flightcontrol n81-epiccitadel:190
```

`pgo-collect.sh` runs each `tools/perf/fcbench.py` preset once and has the engine write its counters for a
window of gameplay (`LT_PGO_RESET`, `LT_PGO_DUMP`; the default is 150 to 250 s after start). Functions
changed since the profile was taken are compiled without it, so refresh the profile after large engine
changes. Configure options must match those of the instrumented build. The raw profile of each game stays in `OUT.d`, so a game can be weighted when merging (`llvm-profdata merge -weighted-input=3,jetpack.profraw ...`); the shipped profile weights Jetpack Joyride 3×.

## Inputs that are not in this repository

Set these in the environment or in `scripts/build.env`, which the script sources if present (not
tracked). The defaults are the development machine's paths.

| Variable | Default | What it is, and where it came from |
| --- | --- | --- |
| `QEMU_IOS` | `~/git-projects/qemu-ios` | the engine source (github.com/samhenrigold/qemu-ios fork, `dev/perf`) |
| `LT_INPUTS` | `~/Documents/LightTouch-Development/port-workspace/builds` | prebuilt inputs: `backend/deps/arm64-v8a/prefix` and `backend/deps-linux/prefix` (glib, pixman and the rest), plus `backend/crypto/*` (gcrypt), built by qemu-ios `contrib/portable/build-deps.py` and `build-crypto-deps.py` (see that directory's README); `mobile-android/arm64-v8a` (the USB bridge helpers `libusbmuxd_qemu.so` and `liblt_appservice.so`, from `Android/packaging/build-mobile-android.py`); `graphics/angle-android-arm64-gl-build-01/candidate-01` (ANGLE for arm64, with its ELF closure and notices); `product/frontend-inputs-15` (the frontend notice bundle). The APK build checks the ANGLE and frontend bundles against the SHA-256 pins in the script. |
| `ANDROID_HOME`, `ANDROID_NDK_VERSION`, `JAVA_HOME` | `~/Android/Sdk`, `28.2.13676358`, JDK 17 | Android SDK, NDK and JDK (`Android/README.md` lists the pinned tool versions) |
| `LT_GUEST_TOOLS` | `build/firmware-tools` | the guest tools the APK carries (`-PguestToolsDir`): `armv6.itpack` and `armv7.itpack`, which new devices get, and the helpers IPSW preparation installs (`it_seal` and the rest; `scripts/build.sh firmware-tools` on the Mac) |
| `LT_ANDROID_ENGINE` | unset (build it) | an `android-engine` output folder (holding `arm64-v8a/`) for `android` to package instead of building the engine |
| `LT_APP_ID_SUFFIX`, `LT_APP_LABEL` | none, `LightTouch` | a test APK that installs beside the regular app (e.g. `.fwkit`; its own data) and its launcher name |
| `LT_MAC` | unset | `user@host` of a Mac for `guest-package` and `ltbench` |
| `LT_MAC_DEVELOPER_DIR`, `LT_MAC_IPAD_SDK` | Xcode.app; `~/Developer/ipod2g-re/OldSDK/iPhoneOS3.2.sdk` on the Mac | see the Mac below |
| `JOBS` | `nproc` | parallel jobs |

## The Mac (guest package and LT Bench)

The guest package (the tools inside iOS: the GL shim, the agent, `it_boot` and the rest) and LT Bench
are armv6 iOS binaries. They link with Xcode's `ld` on a Mac. qemu-ios
`contrib/armv6-toolchain/README.md`, "Setting up a Mac to build the guest package", lists what that Mac
needs, where each piece came from, and what does not work: Xcode 27.1 (not Command Line Tools alone),
the iPhoneOS 3.1.3 and 3.2 SDKs, and `ldid`. LT Bench needs no SDK. The script copies the sources over
with `rsync` (to `~/ltpkg-build` on the Mac), builds there, and copies the results back.

Remote guest-package, firmware-tools and LT Bench builds, and the macOS release checkout/build,
run under the Mac's `/usr/bin/caffeinate -i -s`. This prevents idle sleep and, on AC power, system
sleep for the command's lifetime, including child compilers, signing and packaging. The assertions
are released on success or failure; the scripts do not change persistent power settings or keep the
display awake. Keep the Mac connected to power for release builds.

The iPhoneOS 3.1.3 SDK (headers and link stubs for the guest link) also ships in Apple's original
installer, which the Internet Archive keeps in its iOS Firmware Exhibit Collection:
[`iphone_sdk_3.1.3_with_xcode_3.2.1__snow_leopard__10m2003a.dmg`](https://archive.org/download/iOS_Firmware_Exhibit_Collection/iphone_sdk_3.1.3_with_xcode_3.2.1__snow_leopard__10m2003a.dmg).
Not yet tried as a replacement for the third-party mirror the current SDK came from (docs/TODO.md).

A guest package reaches a device only through its record: `records/ID/assets/guest.itpack`, pinned by
`guest_package_sha256` in `record.json`. A newer serial installs over the existing system at the next
boot, and the runtime's verdicts (`Portable/src/GuestPackageJudge.hpp`) keep it installed. Don't
publish built packages without the project owner's go-ahead.

## Checking a build

- `scripts/build.sh portable-tests`.
- Desktop, headless: `lighttouch run ID --seconds 120 [--script FILE] [--shot FILE.ppm]` with `LIGHTTOUCH_ENGINE`
  naming the engine (docs/DESKTOP.md). It boots the record, prints the runtime's status lines and shuts iOS down
  cleanly.
- Phones: `tools/perf/fcbench.py build/android/LightTouch-debug.apk 30 jetpack` (with `ANDROID_SERIAL`
  set) installs the APK, boots, starts the game and measures it. `docs/PERFORMANCE.md` describes the
  benchmark set and the A/B method, and `docs/GUEST-BENCHMARKS.md` describes LT Bench.

## Release builds

`scripts/build-release.sh` builds every platform's package from worktrees of the release tags, in a
workspace `$LT_RELEASE_ROOT/release-X.Y.Z` (default `~/Documents/LightTouch-Development`):

```sh
git tag v0.0.12; git -C ~/git-projects/qemu-ios tag lighttouch-v0.0.12   # both commits pushed, tags not yet
LT_MAC=user@mac scripts/build-release.sh 0.0.12         # prepare android windows linux desktop macos finish
# write release-0.0.12/out/NOTES.md (title on line 1, CHECKSUMS for the checksum block)
scripts/build-release.sh 0.0.12 finish publish          # checksums into the notes; push tags, GitHub release
```

Set `QEMU_IOS` to an engine checkout whose `subprojects/` were configured with the current patches (slirp's
restricted-DNS patch is checked); `prepare` copies them into the release's engine worktree.

| Step | Output | How |
| --- | --- | --- |
| `android` | `out/LightTouch-vX-android-arm64.apk` | `scripts/build.sh android` from the worktree, `LT_DEBUGGABLE=false`, with the guest tools and firmwarekit; checked non-debuggable and versioned |
| `windows` | the MinGW engine build in `win-cross/` | `Windows/tools/build-engine.sh` (engine and FFmpeg) |
| `linux` | `linux-build/runtime/`: the engine, `usbmuxd-qemu` and the libraries they load | `scripts/release/linux-inner.sh` in the Debian 11 Docker image `$LT_LINUX_IMAGE` |
| `desktop` | `out/LightTouch-vX-Linux-x86_64.AppImage`, `out/LightTouch-vX-Windows-x86_64.zip` | the app built against glibc 2.31 (`scripts/release/build-desktop-linux.sh`) and cross-built for Windows, the Windows engine relinked as a DLL, then `scripts/release/package-desktop.py` |
| `macos` | `out/LightTouch-vX-macOS-arm64.zip` | on `$LT_MAC`: fetches both commits from GitHub, `macOS/build-engine.py`, `cargo build`, `package-desktop.py --os macos` (signed ad hoc), `ditto` |
| `finish` | source archives, `armv6.itpack` and `armv7.itpack`, `LICENSE.txt`, `SHA256SUMS`, `RELEASE-MANIFEST.json` | `scripts/release/finish.py`; checks the APK carries `$LT_GUEST_TOOLS` |

A run skips steps already done; name a step to run it again. The prebuilt inputs (dependency prefixes, the MinGW
sysroot, the Windows USB bridge, the notices, the Mac's LightTouchMac app for its USB bridge) are variables at the top
of the script, settable in `scripts/build.env`. The `android` step builds the guest tools from the release's engine on
`$LT_MAC` (`firmware-tools`) unless `LT_GUEST_TOOLS` names a folder, and `firmwarekit-android` with the Rust Android
target and the NDK. `Android/BUILD-AND-RUN.md` has the sealed two-ABI Android recipe.

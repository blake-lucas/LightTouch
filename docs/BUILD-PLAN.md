# Build pipeline and CI plan

Scoped 2026-10-31 from the current scripts and the Un1box fork's CI. Nothing here is built yet.

## Decisions needed first

| Decision | Options | Recommendation |
|---|---|---|
| Android signing key | Releases are signed with this machine's debug key (`CN=Android Debug`, `~/.android/debug.keystore`). A new key forces every user to uninstall and lose their devices. | Keep the debug key as a CI secret for now; back it up today. Switch to a real key once, at a release that says so. |
| ANGLE in the APK | 9.9 MB arm64 build, only described in `Portable/firmware/ANGLE-SOURCE-ROUTE.md`. | Keep it; publish the existing build as a pinned input. |
| PGO profile | Collected on a phone; ~8% of frames without it. | Publish as a pinned input; recollect by hand when the engine changes a lot. |
| macOS | Guest tools and the macOS app need a Mac. | One GitHub-hosted macOS job (free for public repos), cached so it rarely runs. Try a Linux-only guest build later as a spike. |

## Straighten out first (in order)

1. **Linux release image**: `lighttouch-bullseye-collector08` exists only in local Docker (made with `docker commit`; its script was deleted in `c198b393`). Recreate it as a Dockerfile in the repo, publish to GHCR.
2. **Pinned inputs**: one versioned "inputs" GitHub release with a sha256 manifest and a `fetch-inputs.py`, for what CI can't rebuild: ANGLE, PGO profile, the MSYS2 packages behind the Windows sysroot, desktop notices, appimagetool, the macOS USB bridge. FFmpeg now needs the AAC encoder too (`--enable-encoder=aac`, for the AMC encoder): the Android and Linux dev prefixes and the bullseye `deps-full` were rebuilt in place on 2026-11-01 (old `libavcodec.a` kept as `.before-aacenc`); Windows and macOS build it from the archive with the flag.
3. **Signing**: back up the keystore; add it as a secret.
4. **No machine paths**: remove the `~/Documents/LightTouch-Development` defaults (`build.sh:33`, `build-release.sh:28-47`); add `scripts/build.env.example`; `build-deps.py` honours `LT_JOBS` instead of `-j2`; slirp joins the pinned deps instead of coming from a configured checkout.
5. **Engine pin**: an `ENGINE_REF` file (qemu-ios SHA) used by `build.sh` and the release; drops the paired-tag requirement.
6. **Versions in one place**: `rust-toolchain.toml` (1.99.1), NDK 28.2, JDK 17, Xcode 27.1.
7. **One engine recipe**: four configure recipes today (`build.sh:60-73`, `contrib/portable/build-engine.py`, `Windows/tools/build-engine.sh`, `macOS/build-engine.py`); converge on `build-engine.py`. Fold `tools/perf/build-apk.sh` into `build.sh`.
8. **Recipes for the hand-made inputs**: Windows sysroot staging, Windows and macOS USB bridges (or replace them, below), desktop notices.
9. **Docs**: `BUILDING.md:74` (old fork, `dev/perf`), `Android/BUILD-AND-RUN.md` (old Gradle properties).

## CI layout (GitHub Actions)

| Workflow | Runner | Builds | Runs on |
|---|---|---|---|
| `inputs.yml` | Linux (bullseye image) | Android/Linux deps, crypto, mobile helpers; cached by recipe hash | pin file changes, manual |
| `guest.yml` | macOS, Xcode pinned | guest itpacks and helpers; cached by `contrib/guest-package/build-inputs.sh sources` | guest source changes |
| `android.yml` | Linux | engine (cached object tree), firmwarekit, APK, `apksigner`, `check-android-package.py` | push to `develop`, PRs (after approval) |
| `desktop.yml` | Linux + macOS | AppImage, Windows zip (MinGW cross), macOS zip (ad-hoc signed) | push to `develop`, tags |
| `release.yml` | Linux | `finish.py` → **draft** release, published by hand | `v*` tags |

PRs get the cheap checks (Gradle preflight, `cargo test`, `portable-tests`, `android-test`) at once; heavy jobs wait for approval. From the fork, take: cache keys from a fingerprint of the build recipe, engine object cache per SHA with a cold fallback, Gradle preflight before the engine, logs uploaded on failure, sha-checked legacy SDK fetch. Avoid: pulling binaries out of published APKs, polling other runs for artifacts (use `needs:`), the keystore in `actions/cache`.

## Dropping macOS

- **Guest tools**: blocked on the linker's output, not just armv6 support (Xcode's newer ld output was rejected by `machotool mkold`, and its OpenGLES crash-looped SpringBoard). A Linux route (clang + cctools-port ld64 or ld64.lld + ldid) needs a spike: LT Bench first (no SDK), pass = `machotool mkold` accepts it and the OpenGLES loads on a device. The Apple SDK headers already come from a third-party mirror; public CI makes that visible.
- **macOS app**: stays on a Mac (bundling, codesign; the SDK licence only allows Apple hardware).

## Moving code into Rust

| # | What | Today | Rust side | Removes | Effort |
|---|---|---|---|---|---|
| 1 | Android app-services helper | `lt_appservice.c` (1.2k lines) on libimobiledevice | `lighttouch-services` already has info, list, install, uninstall, crash log; needs syslog and an SSLv3 path for iPhone OS 1.x | libimobiledevice, libusbmuxd, libtatsu, curl stub for Android | S–M |
| 2 | QEMU usbmuxd daemon (all platforms) | C daemon + patches; Windows `Windows/usbmux/`; macOS borrowed from another app | client and pairing exist; the daemon side is new (`qemu-ios/docs/tcp-usb-protocol.md`) | `build-mobile-android.py` and its 7 C packages, Linux shared libs, Windows/macOS bridge builds | L |
| 3 | Kotlin that mirrors Rust | ~1.25k lines: Legacy Store client, IPA preflight, firmware catalog, install queue, guest tools | `lighttouch-catalog`, `lighttouch-model` | duplicate logic | M–L, needs a JNI library pattern |
| 4 | Portable library and storage | `Storage.cpp`, `Library.cpp`, `NandDisk.hpp` (~2.8k lines) | `nand_disk.rs` already duplicates `NandDisk.hpp` | duplicated writers | L |

Rust reaches Android today only as a program the app runs (firmwarekit as `libfirmwarekit.so`); items 1–2 fit that pattern. Add `-z max-page-size=16384` to that build before shipping more. Keep in C/C++: the JNI display path (`lighttouch_jni.cpp`, presenter headers), `Runtime.cpp` and `Network.cpp` (already shared), guest-side code, the engine.

Suggested order: straighten-out items 1–6 → `android.yml` → item 1 (Rust app services) → `desktop.yml` and `guest.yml` → item 2 (Rust usbmuxd), which removes most remaining hand-built inputs.

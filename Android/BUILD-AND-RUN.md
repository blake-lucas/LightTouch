# Build and run the Android development candidate

The current production-engine baseline is [candidate29](CANDIDATE29.md). Its exact source
pins, tested recipe, APK hashes and runtime qualification limits are documented
there. Experimental native-TLS builds have no demonstrated speedup. The
candidate20 recipe below remains a historical reproducibility record.

This guide follows the verified candidate20 build. It is a Debug development APK, with arm64-v8a and x86_64, API29 minimum and API35 compile/target. The main/test APKs are built together; running Android instrumentation is a separate step.

| Source | Candidate20 revision |
| --- | --- |
| Android UI, Binder and tests | `3409cce3b21d9617723c80ac88665944f51553c3` |
| Portable domain, firmware and CActivation | `53ea530954411460d915aace69249398801c4101` |
| QEMU engine and JNI bridge headers/helper, both ABIs | `b59b3620b01da9285dea3a9ee6f58c1bb3cecfc2` |

All62 tracked native/JNI/CActivation files at the two app revisions were checked byte-identical. Candidate20 includes provider frame readiness and a post-capture freshness/coherence check; the Portable/JNI source remains unchanged. Keep `Packages/FirmwareKit/Sources/CActivation` beside `Portable` when exporting native source. Omitting it makes firmware CMake fail. Source export must also retain the catalog at `Portable/firmware/firmware-catalog.json`. (That applies to those revisions; today the preparer is the Rust `crates/` workspace, CActivation included, and `Portable` builds without it.)

For an archived Android build, export the catalog explicitly alongside Android:

```sh
LT_PROJECT_REPO=/path/to/verified-LightTouch-checkout
LT_ENGINE_REPO=/path/to/verified-QEMU-checkout
LT_APP_REVISION=3409cce3b21d9617723c80ac88665944f51553c3
LT_NATIVE_REVISION=53ea530954411460d915aace69249398801c4101
LT_ENGINE_REVISION=b59b3620b01da9285dea3a9ee6f58c1bb3cecfc2
git -C "$LT_PROJECT_REPO" archive "$LT_APP_REVISION" Android Portable/firmware/firmware-catalog.json \
  > android-source.tar
git -C "$LT_PROJECT_REPO" archive "$LT_NATIVE_REVISION" \
  Portable Packages/FirmwareKit/Sources/CActivation > native-source.tar
git -C "$LT_ENGINE_REPO" archive "$LT_ENGINE_REVISION" \
  contrib/ios-app/qemu-ios-ui.h contrib/macos-app/qemu-macos-extras.h \
  contrib/portable/engine-qmp-snapshot.c > qemu-bridge-source.tar
```

Extract each archive into its own empty source directory, preserving relative
paths. The bridge archive supplies JNI/domain headers and the fixed QMP helper;
the complete engine sources/relink inputs belong to the separate engine kits.

An Android-only archive silently makes Gradle's `prepareFirmwareCatalog` task
`NO-SOURCE`. Candidate19's APK lacked the asset; preparation could rely on a
previously provisioned app-private catalog. Candidate20 exports the tracked
catalog from its Android revision `3409cce3b21d9617723c80ac88665944f51553c3` and
verifies `assets/firmware-catalog.json` against that source before handoff. A
successful APK build alone does not catch this omission. Check the archive and
packaged asset even when using a warm build cache.

## Inputs and build

Use the tool versions in [README](README.md#pinned-tools): JDK17, NDK28.2.13676358, CMake3.22.1, SDK/build-tools35, pinned Gradle8.11.1/AGP8.9.2/Kotlin2.1.20. Obtain the pinned sources and a verified artifact cache before an offline build. Keep sources, compiled outputs and guest inputs separate.

The verified build requires these supplied local inputs:

- Real Bionic engine inputs for **both** ABIs: each directory contains `libqemu-arm.so`, its producer manifest, the matching NDK `libc++_shared.so`, and its audited notice bundle. Build/relink recipes are in QEMU's `contrib/portable/README.md`; host/glibc libraries cannot substitute.
- `build/firmwarekit/android` from `scripts/build.sh firmwarekit-android` (firmwarekit, arm64), passed as `-PfirmwareKitDir`.
- The frozen candidate02 x86 ANGLE three-library/closure/notice inputs and frontend notice bundle. Use their sealed producer manifest SHA256 pins; keep these legal build inputs separate from firmware/ROM/guest assets. ARM uses the platform provider; packaging ANGLE does not enable it automatically.

Set the following placeholders to your verified source/input directories and to the publisher's sealed manifest pins. The recipe below is the candidate20 command with machine-local paths parameterized; all properties were used in the tested build.

```sh
export ANDROID_HOME=/path/to/Android/Sdk
export JAVA_HOME=/path/to/jdk-17
export CMAKE_BUILD_PARALLEL_LEVEL=2
LT_ANDROID_SRC=/path/to/android-3409-source
LT_NATIVE_SRC=/path/to/native-53ea-source
LT_QEMU_SRC=/path/to/qemu-b59-source
LT_INPUTS=/path/to/verified-build-inputs
LT_BUILD=/path/to/private/new-android-build
LT_ANGLE_CLOSURE_SHA=b7b6988e39c227d5aa20143102fb98fce8c1a181c5dd0955b2a522c2d3199d8f
LT_ANGLE_NOTICE_SHA=7d28941a751e15a2f63c060b67d0c549435f87b96d527b7c2a8ff08d831bfd57
LT_FRONTEND_NOTICE_SHA=88918185b1a3ba3ea1b4c910c8b7dde56391597574f53824a160fef45d2b76a7
cd "$LT_ANDROID_SRC/Android"
./gradlew --offline --no-daemon --max-workers=2 \
  '-Dorg.gradle.jvmargs=-Xmx768m -XX:MaxMetaspaceSize=384m' \
  -PnativeJobs=2 -PtargetAbis=arm64-v8a,x86_64 \
  "-PportableDir=$LT_NATIVE_SRC/Portable" \
  "-PfirmwareKitDir=$LT_NATIVE_SRC/build/firmwarekit/android" \
  "-PqemuSource=$LT_QEMU_SRC" "-PengineRoot=$LT_INPUTS/engine" \
  "-PengineNoticeArm64=$LT_INPUTS/engine/arm64-v8a/notices" \
  "-PengineNoticeX86=$LT_INPUTS/engine/x86_64/notices" \
  -PsourceRevision=3409cce3b21d9617723c80ac88665944f51553c3 \
  -PsourceReference=https://github.com/blake-lucas/LightTouch/tree/3409cce3b21d9617723c80ac88665944f51553c3 \
  "-PbuildRoot=$LT_BUILD" \
  "-PangleLibraries=$LT_INPUTS/angle/lib" \
  "-PangleManifest=$LT_INPUTS/angle/closure.json" \
  "-PangleManifestSha256=$LT_ANGLE_CLOSURE_SHA" \
  "-PangleNotices=$LT_INPUTS/angle/notices" \
  "-PangleNoticesSha256=$LT_ANGLE_NOTICE_SHA" \
  "-PfrontendNotices=$LT_INPUTS/frontend" \
  "-PfrontendNoticesSha256=$LT_FRONTEND_NOTICE_SHA" \
  :app:assembleDebug :app:assembleDebugAndroidTest \
  :app:testDebugUnitTest --tests 'org.lighttouch.android.*'
```

The actual candidate20 build passed in61.6s and passed all ten JVM cases (Stop3, timestamp order4, session delivery3). It compiled the real-View, dirty-admission, concurrent-media and six synthetic `GuestAngleFrameReadinessTest` cases. Those six Android cases were compiled only; device execution and guest rendering remain separate. `GuestUnlockTest` retains the sealed main19 APK identity and intentionally rejects candidate20; do not invoke it as a candidate20 unlock test. Preserve source archives, exact argv/environment, producer manifests and input-to-packaged strip mappings. Different local paths/debug signing keys can change APK bytes; compare the actual payload provenance rather than assume every new build matches a sealed APK hash.

These verification commands use explicit paths and can run from the app root or another directory after the variables above are set:

```sh
LT_APK="$LT_BUILD/app/outputs/apk/debug/app-debug.apk"
LT_TEST_APK="$LT_BUILD/app/outputs/apk/androidTest/debug/app-debug-androidTest.apk"
python3 "$LT_ANDROID_SRC/Android/scripts/check-apk.py" "$LT_APK"
"$ANDROID_HOME/build-tools/35.0.0/apksigner" verify --verbose "$LT_APK"
"$ANDROID_HOME/build-tools/35.0.0/apksigner" verify --verbose "$LT_TEST_APK"
"$ANDROID_HOME/build-tools/35.0.0/zipalign" -c -P 16 -v 4 "$LT_APK"
"$ANDROID_HOME/build-tools/35.0.0/zipalign" -c -P 16 -v 4 "$LT_TEST_APK"
unzip -p "$LT_APK" assets/firmware-catalog.json | sha256sum
```

Candidate20's sealed checks covered all17 DSOs, uncompressed16KiB APK/ELF alignment, ABI/SONAME/platform-or-packaged dependency closure,247 notice payload hashes and exact input-or-NDK-strip mappings. The packaged catalog SHA256 must be `d83227ee3aa4ea509dfa09b0f68e8830b03312fcceeb18f60e85281501fdeed1`. The firmware corresponding-source kit covers its declared subset; complete app/engine sources, static-library relink material, source availability and whole-APK licensing eligibility remain separate reviews. Use the matching producer receipts rather than reuse a kit for a different binary.


The sealed candidate20 main APK SHA256 is
`233ba90f40add8dfcb71ef727a0000bf76ae56e4ee10ac224c450dca063e4c3d`;
the matching instrumentation APK is
`cc089629d7ec65afcd1224941aabd25583a2da9e777f1f9a2176f79e11669621`.
The complete private producer receipt is sealed by
`aff3b72bff643fd3180e8e47fe25ab7b2ff8026cff1c31f0425d9233d3f3a8c2`;
it retains actual compile commands, source archives and all input-to-strip maps.
These hashes identify the tested artifacts, not a promise that a rebuild with
another checkout path or signing key produces identical APK bytes.

| ABI | Engine input SHA256 | Packaged engine SHA256 | Engine notice manifest SHA256 |
| --- | --- | --- | --- |
| arm64-v8a | `bc5694ab7f2d6bf390186f4b2ce9fadf5b1511a50d7d022d1096729fc80efb65` | `b09f134a50fe2274d14e58e1ed70f80ec96904cfdea8af735bb5727f92872ca1` | `1386240b37299ec0a15e4485a1b94b5a0188eb43c4d15a268e852cdd04f99e0c` |
| x86_64 | `2c1f4212b4a7c4e8d4f537b85317134e7f7b82085305947e2a7f4798fe75279b` | `1ee55a240b824555a0de1275a9069f2972271d0c1ef220ac1290f9abdc36a698` | `b0462d076020dcb2c0533ad02048f9e04c52f4e57902c28c8bff625567ed9100` |

Both engine producers are clean source `b59b3620b01da9285dea3a9ee6f58c1bb3cecfc2`.
The matching NDK runtimes have SHA256
`ab4e6c71b96b851de45a8a9bd86369e7dbc2130a44b3b4520564be94847910f2`
(arm64) and `e4cd73c8a3607269f3be58d15c21f78bff112e27f9398d6261e5f965668f8746`
(x86). Keep the producer manifests with the engine inputs; do not compare a
stripped APK engine directly to its unstripped input hash.

The local `upstream20` engine source kits have bundle manifest SHA256
`a6dcdcb424fc4ae361c23c25763bd289daf1183b718f76e0a5631a8fb929222c`
(arm64) and `30a476e6bdefcf5e2587f2b72ffd8d338b3fc7992dc4176aaf834b72cd0bae1c`
(x86). Their checks are source-package validation, not source replay/relink or
whole-APK licensing approval. The `corresponding-source-attempt20-a` firmware kit
has source-manifest SHA256
`e30f68fd2cd61624e1ad06fdebcebc657d05331ca7b6c0b07060931bb8303153`
and correspondence SHA256
`2a9d149d9c87e79f4bff8938c2d8edc633cd46eae9f1764a9c0ffe139a76abc1`.
It binds the actual20 firmware/JNI producers, catalog export and packaged bytes;
its source-export replay is unverified. No source-kit hash establishes public
source availability or complete app licensing clearance. Keep old19 kits intact.

## Install and supply private guest inputs

These are device-owner steps; stop/retain any existing session before updating. Installation and data preservation depend on the same package/signing key. Choose one authorized device explicitly:

```sh
LT_DEVICE_SERIAL=your-authorized-device
adb -s "$LT_DEVICE_SERIAL" install -r "$LT_APK"
adb -s "$LT_DEVICE_SERIAL" install -r "$LT_TEST_APK"
adb -s "$LT_DEVICE_SERIAL" shell am start -W -n org.lighttouch.android/.MainActivity
```

Normal use: **Get started** or **Add device** prepares a device from an IPSW on the phone (RUNNING.md, "Add a device"). Each preparation creates a new record and independent state. Start, Home, Power, Pause/Resume, Screenshot and Stop are semantic controls. Backgrounding pauses/cancels input; reopen and explicitly Resume. Guest Home/Photos readiness requires inspecting real guest pixels and response.

Keep guest sources, prepared outputs, app-private state, logs and captures outside Git and cloud uploads. Do not delete `.dirty` or replace a baseline to recover a session. Ordinary Start rejects interrupted state; **Recover retained state** explicitly cold-boots preserved writable data in a fresh worker. Confirmed guest halt and successful engine cleanup, not timeout/exit0 alone, clear dirty state.

## Preparation, tests and current limits

**Prepare from an IPSW** runs the APK's `firmwarekit` in a separate same-UID firmware service. The app downloads or copies the stock IPSW and checks it against the catalog; the guest tools come from the APK (`-PguestToolsDir`). The output is moved into the library as a new record. Completion is distinct from boot acceptance.

The desktop app uses the same preparer and Portable domain; see [firmware preparation](../Portable/firmware/README.md) and [the desktop app](../docs/DESKTOP.md). There is no Android-specific firmware recipe or boot-argument list. A locally prepared base must still boot, accept input, halt cleanly and preserve state on each required target.

Foreground MainActivity before every `am instrument --no-restart` test. Inspect JUnit output and private receipts: shell exit0 is not a JUnit pass. Ordinary instrumentation completion can terminate the target; choose session holds/stop/recovery deliberately. The following owner invocation is planned against candidate20. Run it before starting a guest. It exercises three real-View/fake-endpoint cases and six synthetic frame-policy cases; it does not exercise a guest:

```sh
adb -s "$LT_DEVICE_SERIAL" shell am start -W -n org.lighttouch.android/.MainActivity
adb -s "$LT_DEVICE_SERIAL" shell am instrument --no-restart -w -r \
  -e class org.lighttouch.android.GuestDisplayAdmissionTest,org.lighttouch.android.GuestAngleFrameReadinessTest \
  org.lighttouch.android.test/androidx.test.runner.AndroidJUnitRunner
```

Historical evidence, separate from candidate20: candidate18 passed all three real View/fake Binder cases on Poco, including unowned resize preserving an unrelated button pulse; this is not native button delivery or guest input proof. Candidate17b passed actual dirty admission with unchanged metadata and explicit recovery/QMP diagnosis to a lit lock screen. Its later wake diagnosis remained black, and Stop disposed the worker after the deadline with dirty state retained. Shutdown receipts are best effort: a missing receipt does not establish a blocked call or a clean halt. See [wake diagnosis](WAKE-DIAGNOSIS.md) for the optional150–500ms host hold and its separate guest-clock limits.

Required guest multi-contact response, concurrent touch/render/nonzero audio, full lifecycle, sustained20-minute session, clean same-state reboot/persistence, prepared-stock boot and both AVD page-size gates remain subject to their own retained acceptance. See [candidate17 sequence](CANDIDATE17-ACCEPTANCE.md) and [concurrent/media recipes](CONCURRENT-ACCEPTANCE.md) for opt-ins and manual checks.

At this guide checkpoint, candidate20 is installed on Poco and its explicit retained-state recovery/QMP diagnosis reaches a visually inspected boot logo. This is not Home, unlock, touch, audio, lifecycle or prepared-stock boot acceptance. Historical candidate15 Home/Photos and later partial wake evidence do not qualify candidate20. The first helper19b attempt ended on a fresh Lock frame; its second attempt had stale copied captures and cannot establish the final guest screen.

Tilt controls (on by default, switch beside the guest) feed the phone's accelerometer to boards that model one, and the gyroscope to boards that model one (the iPod touch 2G has no gyroscope). The value last sent is shown under the switch. Android display/audio/touch/lifecycle and durability remain required.

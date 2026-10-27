# APK dependency and corresponding-source review

This records identified coverage and remaining work, not a distribution grant.
It applies to the native/Kotlin APK; private guest firmware, ROMs, NAND and state
are excluded. Recheck the exact conveyed artifact rather than reusing an older
attempt's matrix. No unresolved license incompatibility has been established
by this Android review; notice/source coverage remains incomplete.

| Conveyed component | Evidence and notice coverage | Corresponding-source / remaining identified work |
| --- | --- | --- |
| Android activity, Compose UI, Binder services and JNI adapters | Project source; repository root carries GPL2 license text, while artifact source revisions are recorded outside Git per attempt. | Include this exact source and Android build/pin/verification files. Record the applicable grant for new frontend/adapter files explicitly; the root license text alone does not resolve an "or later" grant or the complete combined-work classification. |
| Portable runtime linked into JNI | Authoritative native sources, engine/runtime ABI and build recipes; source revision recorded per attempt. | Include the exact Portable source and licenses with the conveyed JNI revision; do not substitute another worktree's current HEAD. |
| QEMU and selected static engine dependencies | Backend manifest identifies library SHA, source revision, selected-source notices, FFmpeg, GLib, PCRE2, pixman, proxy-libintl, libgcrypt, libgpg-error, libfdt and NDK runtime. Verified per-ABI manifests are consumed by `stage-engine-notices.py`. | Preserve each ABI's actual revisions and patches. A QEMU repository URL alone does not provide all linked dependency sources. Applicable static LGPL relink inputs/build instructions remain to be delivered and verified by the publisher. |
| Firmware preparation executable (`libfirmwarekit.so`, the Rust firmwarekit) | `crates/lighttouch-firmware/stage-notices.py` stages the licence of every Rust crate it links (from `cargo metadata`), iBoot32Patcher, SQLite's notice and LightTouch's activation.c, into `assets/licenses/firmwarekit`. | Include the exact crates/ source and `Cargo.lock`. |
| NDK libc++ and AndroidX native graphics path DSO | The APK contains one NDK28 libc++ per ABI; engine bundles bind its input SHA to NDK notice files. AndroidX graphics-path is supplied by the selected AAR. | Record the actual graphics-path AAR/native sources and its upstream notices in the frontend runtime audit; NDK notices alone are not an attribution for that library. |
| Main APK AndroidX / Kotlin / coroutine runtime inputs | Direct pins live in `app/build.gradle.kts`; strict verification metadata records downloaded artifacts. Inspected primary artifact POMs for Activity1.10.1, Compose UI1.7.8, Material3 1.3.2, DocumentFile1.0.1, Kotlin stdlib2.1.20 and coroutines-android1.7.3 declare Apache2. These inspections do not establish the entire selected transitive graph. | Export the actual selected variant graph with `recordApkRuntimeArtifacts`, then preserve upstream LICENSE/NOTICE/attribution for every shipped artifact (including nested `classes.jar` resources). Inspect the original artifacts, not only merged APK resources. Frontend offline bundle coverage remains open. |
| Instrumentation and debug-only probe/client | AndroidTest APK and debug native probes have separate inputs; fixed QMP/TCG helpers appear only in debug production artifacts. | Keep these identities separate from release APK runtime inventory. If a debug APK/test APK is conveyed, include their own selected licenses/source. |
| Gradle/AGP/Kotlin compiler/JDK/CMake/NDK build executables | Pinned reproducible build instructions and dependency verification. | Preserve reproducible recipes and required compiler/plugin identities; do not count every downloaded build-tool artifact as an APK runtime dependency. |

The matrix's concrete frontend packaging gap is observable: frozen attempt09
contains ten notice-like ZIP entries, all under `assets/licenses/firmware`.
The original Activity1.10.1 AAR contains
`META-INF/androidx/activity/activity/LICENSE.txt` (SHA256
`809fa1ed21450f59827d1e9aec720bbc4b687434fa22283c6cb5dd82a47ab9c0`),
but that entry is absent from the APK. Neither artifact verification hashes nor
POM declarations replace the missing offline notice coverage. The subsequent
engine-notice source change is not retroactive to attempt09.

Run the explicit inventory task in the allocated build slot, using the same
build properties/pins as the intended APK:

```sh
./gradlew --no-daemon --max-workers=1 [same native/source/buildRoot properties] \
  -PauditConfiguration=debugRuntimeClasspath :app:recordApkRuntimeArtifacts
```

Use `releaseRuntimeClasspath` for release. The report is generated under the
private `buildRoot/app/reports`; it contains resolved artifact file paths for
local audit and must not be copied into public Git, offline assets or source
notices. Only coordinates, upstream references and checked bytes belong in a
public notice bundle. `gradle/verification-metadata.xml` includes build-tool,
metadata, historic version and test downloads as well as runtime inputs; it is
not the selected graph. DEX shrinking can further change a release's actual
contents, so correlate the selected inputs with that artifact.

The offline viewer's per-group source links are provenance, not proof of source
availability. Complete corresponding source and applicable relinking material
remain separate from inclusion of license texts. The unresolved grant/version and combined-work classifications above are review
questions, not an assertion that the dependency combination is incompatible.
Publisher review should use
the actual grants preserved in the bundles: [GPL3 text](https://www.gnu.org/licenses/gpl-3.0.html),
[LGPL2.1 text](https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html), and
[Apache2 text](https://www.apache.org/licenses/LICENSE-2.0).

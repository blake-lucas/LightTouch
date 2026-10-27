# Running the Android port

The APK and public source do not provide Apple firmware, boot ROMs, prepared NAND, SDKs or personal device state:
the app downloads the IPSW and boot ROM and prepares each device on the phone, without a Mac, root or an Apple SDK.

## Current artifact and compatibility

Status recorded on 2026-10-14 for candidate 25:

| Component | Identity |
| --- | --- |
| Android and Portable source | `be428ddd18bcf76fea6776258c2e80d867f0eefa` |
| QEMU engine | `259e4ad31702f4927ed8d06a06b64219121bc0a0` |
| Main APK SHA256 | `ddf8ea5b0066ae5c3a432d4e663436f251b60be42462fae8072177619a7d66c3` |
| Android package | `org.lighttouch.android` |
| Packaged host ABIs | `arm64-v8a`, `x86_64`; no 32-bit Android host claim |
| Android versions | API 29 minimum; compiled/targeted against API 35 |

The ARMv6/ARMv7 names on guest packages describe the **guest**, not the Android
host ABI. API 29 minimum is an installation boundary, not a claim that every
Android version, GPU driver or device has passed guest acceptance.

| Environment | Actual coverage and limits |
| --- | --- |
| Poco F3, ARM64, Android 16/API 36,4 KiB pages, Adreno 650 | Candidate 25 has compiled/static checks; its guest qualification is pending. Historical candidate 24 reached an inspected Lock Screen and touch-unlocked Home, but its latest Stop was unconfirmed, retained dirty state and produced no terminal receipt. Historical 21b/23 clean Stop and persistence results belong to those artifacts. |
| x86_64 Android emulators,4 KiB/16 KiB pages | APK/library packaging and selected app/provider loading checks passed. Complete current guest boot, input, audio and clean halt qualification remains open. Static16 KiB ELF/ZIP alignment is not guest execution proof. |
| Physical Pixel 8/9/10 and other devices | Pixel 8/9 are untested. On Pixel 10/Tensor G5, the user reports APK25's smooth bootloader progress bar followed by a flickering Apple logo and eventual idle display. Reported worker readings were approximately 140% of one host core and 482 MiB PSS. No physical-device ADB evidence was captured; a crash loop or OOM cause is unproven. Physical Pixel 10 remains unqualified; emulator profiles do not qualify Tensor hardware or its GPU. |
| Prepared n81 route | Native kboot/NAND/NOR/ARMv7 routing exists. Supplemental Linux/Android guest/GLES2/input qualification remains open; this does not expand the n72 stock-preparation coverage. |

Gyro/motion is deferred and unqualified. Current Linux final-engine guest
acceptance is also separate from historical generated-base results. A successful
import, frame counter or agent response alone does not establish a working guest.

## Install the main APK

Use the main APK for ordinary use. Verify its hash and signing identity against
the supplied artifact receipt. Before updating an existing installation, finish
an orderly Stop and retain its data. Updates require the same package/signing
key; a locally rebuilt Debug APK can have a different signer. Do not uninstall
an existing app as an automatic response to a signing mismatch.

The Android package installer can install the APK. An authorized owner can also
use these commands, substituting the selected device and verified file:

```sh
LT_DEVICE_SERIAL=your-authorized-device
LT_MAIN_APK=/path/to/verified/app-debug.apk
sha256sum "$LT_MAIN_APK"
adb -s "$LT_DEVICE_SERIAL" install -r "$LT_MAIN_APK"
adb -s "$LT_DEVICE_SERIAL" shell am start -W -n org.lighttouch.android/.MainActivity
```

The test APK is optional instrumentation, not a runtime dependency. Use candidate
25's matching test artifact and receipt. Its Pixel diagnosis helper is copied from
the same source revision into the test source set; it is not an external revision
overlay. Historical 24/24b test artifacts have different provenance and are not
substitutes. Do not run a historical guest-control test against an arbitrary newer
main APK. See the test guidance in
[BUILD-AND-RUN.md](BUILD-AND-RUN.md).

## Add a device

**Get started** (first launch) or **Add device** picks a device, then an iOS version; every catalog firmware
prepares on the phone. The app downloads the IPSW (the link ipsw.me lists, else the catalog's mirrors and the
Internet Archive) or copies one the user chooses, checking it against the catalog's SHA-1 either way. The iPod touch
1G and 2G and the original iPhone also need their boot ROM: it comes from another device on the phone, a download
(securerom.fun, its GitHub mirror or devos50's qemu-ios release, pinned by SHA-256), or a file the user chooses. The
guest tools come from the APK.

Preparation runs the APK's `firmwarekit` in a separate same-UID process, with no root, mounts or macOS utilities; its
iOS boots run the APK's engine with performance hints. The completed output is moved into the library as a new
record and selected, then the IPSW copy is deleted; if adding it fails the output is kept for **Try adding it
again**. Cancellation or failure leaves no usable output. Devices are only added this way: importing a prepared
`.itbase` is gone. [Portable/firmware](../Portable/firmware/README.md) documents the preparer.

## Controls, stopping and retained state

Use **Home**, **Power**, touch, **Pause/Resume** and **Stop** on the selected guest.
Backgrounding pauses the guest and cancels owned contacts; reopen and choose
**Resume**. The screen can sleep while the engine is running; **Power** requests a
wake, which still needs an actual guest response. Avoid using repeated blind
pulses or instrumentation completion as proof of guest behavior.

**Screenshot** exports a host image to `Pictures/LightTouch`. The guest's
Home+Power screenshot gesture creates a guest Saved Photo. A host export is not
proof that guest media persisted.

Stop requests guest shutdown and bounded cleanup. Process disappearance, exit0 or
a missing receipt does not prove a clean halt. An unconfirmed shutdown retains
dirty state. **Recover retained state** explicitly cold-boots retained writable
data in a fresh worker; it is not a snapshot restore or proof of clean shutdown.
Never delete `.dirty`, replace NOR/overlay files or re-import a base to make a
failed persistence check appear successful.

To assess persistence, retain an identifiable guest change, obtain a confirmed
clean Stop/storage outcome, then ordinarily Start the **same record/state** in a
new worker and inspect the same change. Candidate 24's latest unconfirmed Stop
does not satisfy that condition. Retained-state history and the nondestructive
comparison limits are explained in [STATE-CONTROL.md](STATE-CONTROL.md).

## Performance observations

Open **Performance** to show cached observations for the current worker. Sampling
is requested only while the panel is open and the activity is foregrounded; worker
samples are limited to one every two seconds. Allow the CPU reading to warm up and
check each reading's age. Missing measurements remain unavailable.

**Emulator CPU** measures the worker process, with 100% meaning one CPU core; it
can exceed 100% when multiple threads run. It includes emulator, audio, rendering,
Binder and sampling work. **Emulator memory** is proportional set size (PSS), not
guest RAM or a complete GPU-memory measurement. **Android RAM** is available/total
device memory. Android whole-device CPU and iOS guest CPU/RAM are unavailable in
this artifact. These observations do not establish a performance bottleneck.

Candidate 25 changes display ownership and touch mapping, and avoids empty PCM
polling work. Those source changes and static tests do not prove a speedup or fix
an observed device issue. A resized viewport can cancel an in-flight gesture;
begin a fresh gesture after the view settles rather than continuing an old contact.

## Building and source material

[BUILD-AND-RUN.md](BUILD-AND-RUN.md) documents pinned host tools, source exports,
engine/crypto/renderer inputs and builds. Its candidate 20/22 recipes are historical;
do not silently label their binaries or pins as 25. Reproducing 25 requires the app and
engine revisions above, the actual matching per-ABI input manifests and build
records, and the declared same-revision test-source staging if building that test
APK. Native exports
must retain all `Portable`, `crates` (firmwarekit, with its C), Android JNI
and helper/header inputs, the tracked firmware catalog, and root README/LICENSE.
Host compilation does not require private guest firmware, ROMs or Apple SDKs.

Read [APK dependency coverage](DEPENDENCY-NOTICES.md)
and [frontend notices](FRONTEND-NOTICES.md).
The app's **Licenses** viewer preserves offline notices and source references;
those references and the firmware component kit do not establish complete APK
source/relink availability, a full source rebuild or public distribution clearance.
Keep user assets, writable state, signing keys and private captures outside public
source delivery. The earlier private 21b demo bundle remains an older artifact.

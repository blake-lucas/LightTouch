# Android frontend

User scope D12 (2026-10-14): gyro/motion qualification is deferred, unqualified
and excluded from initial acceptance. Automatic experimental motion listeners
are disabled by default. Preserve existing APIs/adapters without further gyro
integration, probe/SDK work or claims of working guest gyro. Multi-touch, display,
audio, durability and lifecycle remain required.

Kotlin/Compose UI binds to a same-UID `:engine` process. Only that process loads
JNI and QEMU. Binder carries controls/status and timestamped bounded input batches;
video renders directly into a transferred Surface. Surface references are acquired
and released in native code, frames copy under the runtime/engine frame lock,
latest frame wins, and no video frame travels in a Binder parcel. A native CPU
conversion from BGRA8888 to Surface RGBA8888 preserves the first correctness path.

The shared `Portable/include/lighttouch.h` owns validation, device state, writer
leases, board discovery, boot arguments, touch transforms and motion conversion.
Android passes stable MotionEvent pointer IDs; never pointer indices as identities.
Rejected whole touch batches trigger native release of all owned contacts and
abort the remainder of that Android gesture. Async guest rejection is polled and
reported; queue admission alone is never displayed as delivered input.
View-local pixel positions map through the same native letterbox function used by
Linux. Guest portrait remains upright inside the current Android view; touch must
not apply physical display rotation a second time. Sensor vectors remain Android
natural-device coordinates and use the shared display rotation conversion. Sensor
units are m/s² (accelerometer including gravity) and rad/s (calibrated gyroscope).
The shared mapper translates SensorEvent BOOTTIME timestamps to host monotonic
age, rejects stale samples and advertises guest capabilities separately. Host
sensor availability never implies guest gyro availability.

## Pinned tools

- Gradle 8.11.1 (wrapper distribution SHA256 verified on download)
- Android Gradle Plugin 8.9.2; compile/target API 35, build-tools 35.0.0
- Kotlin + Compose compiler plugin 2.1.20; UI/foundation 1.7.8, Material3 1.3.2
- JDK 17; NDK 28.2.13676358; CMake 3.22.1
- AndroidX Activity Compose 1.10.1; DocumentFile 1.0.1
- Native ABIs arm64-v8a and x86_64, minimum Android API 29

Official platform references, checked 2026-10-14:
[AGP 8.9 compatibility](https://developer.android.com/build/releases/agp-8-9-0-release-notes)
requires Gradle 8.11.1 and JDK17 and supports API35;
[Compose compiler setup](https://developer.android.com/develop/ui/compose/setup-compose-dependencies-and-compiler)
uses a compiler plugin matching Kotlin;
[16 KiB native support](https://developer.android.com/guide/practices/page-sizes)
requires aligned native dependencies and uncompressed APK entries. AGP≥8.5.1 and
NDK≥r28 provide compatible packaging/linking defaults. This app additionally
links JNI with max-page-size 16384. Verify the entire engine dependency closure,
not only JNI.

## Build and run

Follow the concise [verified build/install/run guide](BUILD-AND-RUN.md) for
the current baseline's exact source revisions, required engine/firmware/ANGLE/frontend
notice inputs, complete Gradle properties and APK checks. It separates tested
build/host admission commands from manual guest readiness and open acceptance.
Keep build outputs and guest material outside the checkout. Build both real
Bionic ABI closures; missing engine libraries fail explicitly, and an arm64-only
build cannot qualify the AVD gates. Review pinned dependency verification changes
when intentionally updating versions/checksums.

## Devices and state

Devices are added only by preparing them from an IPSW on the phone (RUNNING.md, "Add a device"); the native domain
validates the preparer's locked outputs and metadata and atomically publishes them as a new record. Firmware
is never bundled. Every native record under `files/library` receives its own
immutable base, matching package/optional ROM copies, writable state and serial
log. The native library generates record IDs, persists selection atomically and
exposes board/build/capabilities to the device selector. Adding a second device
never replaces the first. A legacy
`files/base`/`files/state` pair is transactionally adopted only into an empty
library, preserving its dirty marker, overlay and source copies.
Prepared-kernel routes use the base’s validated kboot.bin and do not require a
hardware boot ROM. The native catalog/recipe owns compatibility checks; adding a device is not boot
acceptance. The first UI deliberately preserves an existing device rather than
replacing its state.

PNG screenshots use the actual copied guest frame, retain a private copy in
`files/screenshots/`, and publish to MediaStore Pictures/LightTouch without broad
storage permission. A debug build can export them with `adb exec-out run-as
org.lighttouch.android cat files/screenshots/NAME.png` using an authorized device.

## Lifecycle policy

Backgrounding pauses the guest and cancels held input; foreground return requires
an explicit Resume. Audio focus loss also pauses and cancels input; focus gain
resumes only a guest paused by focus, never a guest paused by the user or activity. Sensors unregister while inactive.
Surface loss releases contacts and native Surface ownership. The local foreground
service is `specialUse`, with explicit subtype local user-controlled legacy guest
emulation; notification permits return to the app. See
[foreground service types](https://developer.android.com/develop/background-work/services/fgs/service-types).
No Google services, root privileges or isolated-UID storage exceptions are needed.
The foreground notification permission can be denied without affecting user input.

Stop requests guest shutdown under the shared 35-second disposal budget.
The domain alone decides clean versus dirty storage. The independent watchdog is
armed before native admission; Stop runs off the UI/Binder path. Own-PID terminal
receipts/errors are best effort, and a missing receipt never confirms clean halt.
Native state metadata remains authoritative. Restart binds a newly created worker
and never invokes QEMU twice in one process. Ordinary Start refuses a dirty state;
**Recover retained state** explicitly reuses the existing overlay in a fresh
worker without clearing markers. Confirmed guest halt plus engine cleanup decides
clean state. See [recovery controls](recovery-controls.md). No snapshot capability
is claimed.

Audio drains only actual bounded signed-16-bit PCM into an AudioTrack configured
from native sample rate/channel metadata. Unsupported audio remains unsupported;
no generated PCM can stand in for guest audio acceptance.

## Acceptance

`tests/semantic-smoke.yaml` checks the Android shell through semantic IDs. Private
local acceptance must additionally boot the real guest, exercise production touch
batches, compare rendered output, hear captured guest
audio, and restart with persistent data on the phone and both AVD page sizes.
An APK build or shell test does not prove those gates. Record native/backend/asset
revisions with logs, measured frame cadence, boot time and process IDs. The n72ap
baseline has no guest gyro. Gyro/motion acceptance is explicitly deferred and
not a completion gate. Modeled board flags are not qualified functionality.

## Debug instrumentation

The Debug variant alone contains a separate same-UID `:tcgProbe` service and
`lighttouch_probe` JNI library. It executes four legal ARM instructions on the
actual packaged QEMU `virt` machine and verifies RAM word42 through private QMP.
The worker exits after the probe; production UI never accepts custom guest argv.
This establishes app-UID CPU/JIT execution, not iOS boot or graphics.

```sh
./gradlew [same pinned build properties] :app:assembleDebugAndroidTest
adb install -r /path/to/app-debug.apk
adb install -r /path/to/app-debug-androidTest.apk
adb shell am instrument -w -e class org.lighttouch.android.TcgSmokeTest \
  org.lighttouch.android.test/androidx.test.runner.AndroidJUnitRunner
```

AndroidX Test Runner1.6.2 and JUnit1.2.1 are pinned; see the
[official AndroidX Test releases](https://developer.android.com/jetpack/androidx/releases/test).
`GuestInputTest` is opt-in (`-e exerciseGuest true`) and requires a legitimately
imported/selected device. Instrumentation may restart the app and its worker;
never launch it assuming a prior running guest stays alive. `settleSeconds`
(default60) gives the device owner time to confirm guest Home and open the chosen
built-in gesture workload. `gesture` selects pinch/cross/cancel/all. Pointer IDs7
and23 travel via real MotionEvents into GuestDisplay, Binder, JNI and native
controller input; independent lift changes pointer index without changing ID.
Screenshots and a trajectory manifest stay in private app storage. The retained motion
test is separately disabled unless experimentalMotion=true and is excluded from
initial acceptance. It does not establish working guest gyro. No gyro probe/SDK
build is performed. The production adapter remains disabled by default.
Both harness manifests explicitly require guest-response review; successful host
execution is not accepted as a guest-visible gesture/gyro result.

For a fresh debug worker, `GuestInputTest` accepts `-e touchWireTrace true` with
the normal `exerciseGuest` opt-in. The production Binder/JNI method selects only
the approved native `ipod_touch_multitouch_log` event before start; it accepts no
event name, path or command. Selection is closed at the first start attempt,
and release builds reject it. An already running guest must have configured this
trace before boot; the test never silently restarts it. The optional trace hook
requires the shared native implementation of `lt_runtime_debug_touch_trace_enable`.
Bounded private `touch-wire.log`/JSON reads retain FRAME_READ, serialized contact
and finger-edge records alongside indexed host MotionEvents. Raw output is capped
at4MiB and selected records at8192. Host uptime/read intervals and guest virtual
timestamps have separate epochs; pending reads plus nonempty contact records
do not establish per-batch collection, release ordering or visible response.
The test requests cancellation in `finally` even when a held-contact screenshot
fails, retaining cleanup errors without erasing the original failure.

## Local userspace preparation

The preparation dialog reads firmware IDs from the original upstream catalog,
bundled as a generated build asset. Native `lt_firmware_supports` labels implemented
versus unsupported routes; it does not imply measured target acceptance. Pick a
legitimate stock IPSW and verified private handoff root containing `inventory.json`
and `release-guest-unpacked/guest-tools`. Only the required guest-tools subtree, inventory and IPSW are copied into
app-private storage; unrelated NAND fixtures, SDKs and captures stay at their
original location.
The local inventory is trusted hash evidence, not the unavailable original exporter
manifest; the historical Apple SDKs are not bundled or replaced. Native validates
stock firmware/catalog identity, local guest tool hashes and package payload tables.

A separate same-UID `:firmware` foreground service loads only the firmware JNI
library, performs native userspace preparation with progress/cancel, and exposes
explicit completion/failure/restart-required status. It executes no extracted
programs and mounts no filesystem. A complete prepared output is validated/imported
into a fresh shared native device record; its declared matching guest.itpack is
selected by native code. Recipes, architecture/tool selection and boot arguments
remain native. Each new job uses a fresh firmware worker. Interrupted staging trees
are never admitted as devices, and an existing device is preserved on failure.

Preparation runs `firmwarekit` (Rust, crates/lighttouch-firmware) from the APK's nativeLibraryDir; its
boots load the APK's engine themselves (`firmwarekit oneshot`). `scripts/build.sh firmwarekit-android`
builds it and `-PfirmwareKitDir` packages it. Preparation is not G6 acceptance until the resulting base boots on
Linux and unrooted Android with actual guest rendering and usable input.

UI status polling uses one background request at a time; native startup does
integrity admission outside the frame/control mutex and publishes handles only
when ready. Stop during admission is retained and applied when startup returns.

Real Android userspace preparation acceptance is also opt-in:
`-e class org.lighttouch.android.FirmwareAcceptanceTest -e preparePrivateFirmware true`.
Privately stage `files/acceptance-fw/stock.ipsw`,
`files/acceptance-fw/handoff/inventory.json`, and its
`release-guest-unpacked/guest-tools` directory. Supply
`files/acceptance-fw/input-sha256.json`, mapping every staged input path relative
 to `acceptance-fw` to its verified SHA256. The test first cancels at the native
`write NAND` checkpoint after system edits, checks cleanup and unchanged inputs,
waits for Binder death, retries in a fresh same-UID worker, then validates and
imports the completed base through production IEngine. Its cancellation hook
exists only in Debug native builds and requires an explicit non-exported service
bind extra. Timings/results/stage traces are under `files/firmware-evidence-*`;
native preparer diagnostics remain at `files/prepared/job-*.native.log`.
Default timeout is 1800 seconds per job, overridden with
`firmwareTimeoutSeconds`. This establishes preparation/import only; guest boot
and the normal picker flow still require independent acceptance.

Production service status includes an `observations` Bundle for private
acceptance: shared native producer/PCM queue counts, successful Surface posts,
local CLOCK_MONOTONIC presentation intervals and work time, copy/window failures,
and real AudioTrack pulled/accepted sample counts, nonzero counts, S16 RMS,
short/failed writes, underruns and playback-head advancement. Guest frame
sequence/timestamp and shared publication timestamp retain their documented
clock semantics; PCM capture timestamps have a separate generation-relative
epoch. Presentation intervals include intentional/static guest gaps and do not
alone measure jank. Queued/accepted samples do not prove audible output.

`GuestMediaLifecycleTest` requires `-e exerciseMedia true` and an already booted
real guest. `mediaSeconds` controls the running measurement (30 by default;
1200 for a 20-minute observation). Choose a real animation and use
`requireFrameProgress=true`, or a known guest sound and
`requireGuestAudio=true`, to require those measured deltas. The test records user
pause/resume, background/surface loss and foreground return; optional
`exerciseAudioFocus=true` temporarily requests real Android audio focus without
synthesizing sound. It writes private `files/media-evidence-*/observations.json`
and `result.json`, leaving visible/audible qualification false until independent
inspection/listening. Metrics follow the official
[AudioTrack API](https://developer.android.com/reference/android/media/AudioTrack).
Each engine worker redirects its own native stdout/stderr before engine start
into an append-only mode0600 `engine-PID.log` in the selected private state;
`status.engineLog` identifies it. Prior worker logs and serial logs are retained.
Worker start failure and terminal/timed-out shutdown additionally retain
`files/worker-observations-PID.json` before worker disposal, including error and
native log paths. Audio-focus denial pauses the guest and reports a recovery
message; user Resume requests focus again.
Unexpected worker death preserves an actionable error instead of presenting a
clean stop; terminal dirty/unconfirmed native exits are failed even for exit0.
Start from a consumed worker waits for its Binder death and binds a fresh process.
On API30+, only this app's `:engine` historical exit record is captured via
ActivityManager/ApplicationExitInfo, with at most 4 MiB from its own trace stream,
under private `files/worker-exits`. No other process or system logs are read.
To retrieve a specific prior native crash without reproducing it, run only
`OwnWorkerExitTest` with `-e workerPid PID`. It requests this app's exact :engine
PID, records the platform exit reason/status and retains available bounded raw trace bytes
for integration-owner tombstone/protobuf decoding. Default suites skip it.
Android instrumentation uses nonsequential pointer IDs7/23 within the platform
0..31 range ([AOSP Input.h](https://android.googlesource.com/platform/frameworks/native/+/master/include/input/Input.h)).
A cancelled/focus-lost/surface-lost gesture ignores subsequent moves until a new
DOWN. Worker death during Surface/input Binder calls reports recovery text and
does not throw through the activity event dispatcher. Touch evidence is written
incrementally so failures preserve the preceding trajectory.
Shutdown disposal uses shared LT_SHUTDOWN_GRACE_MS (35seconds), copied from the
authoritative header into BuildConfig at configuration time. The service arms an
independent timer before any Stop JNI call and admits native shutdown off Binder;
the UI also sends Stop off its main thread. The deadline reads only cached
progress and queues a bounded private `worker-shutdown-PID.json` receipt/error
to a nonwaiting daemon writer before disposing its own worker process. It never queries Native status or
observations, waits for AudioTrack cleanup or changes native state markers.
Terminal native status may dispose earlier; the single winner does not infer
clean shutdown when no native confirmation was observed. Expiry reports
unconfirmed shutdown and retained writable state, including startup that never
marked a device dirty. The policy deadline begins at service Stop admission;
it does not prove that Binder dispatch, OS scheduling or storage writes have a
hard realtime guarantee. Existing presenter/platform blocking needs separate
target evidence and is not identified by this source change.
Receipt/error persistence is best effort: a blocked filesystem cannot delay
process disposal, and the process can die before the writer completes. The UI
reports unconfirmed shutdown when a valid terminal receipt is unavailable;
missing/partial bytes are never taken as clean shutdown proof.
Host `ShutdownWatchdogTest` blocks each native callback in turn and checks timer
disposal and late-completion single-winner behavior without loading/killing QEMU
or operating a phone. Run with the configured native/source input properties and
`:app:testDebugUnitTest --tests org.lighttouch.android.ShutdownWatchdogTest`.

Gradle consumes the shared firmware `package-notices.py` generator and pinned
NDK notice into offline assets. `sourceRevision`/`sourceReference` override the
publisher's corresponding-source identity; local default checkout references
record development provenance only. Backend-owned engine bundles are consumed
with `-PengineNoticeArm64=/verified/bundle/arm64-v8a` and
`-PengineNoticeX86=/verified/bundle/x86_64`; supply every packaged ABI when using
these properties. `stage-engine-notices.py` checks each manifest against the
actual unstripped engine and libc++ input hashes, verifies all listed notice
hashes, rejects traversal/symlink paths, and copies only declared files into
separate offline engine groups. Verification finishes before replacing prior
generated groups. The Licenses dialog accepts checked nested relative paths,
verifies each selected notice hash, and offers the manifest's source link.
These firmware/engine grants do not establish complete APK distribution
clearance. Frontend dependencies, complete corresponding source availability,
and applicable static LGPL relink materials still need publisher review.

Debug builds alone can arm derived private QMP before a fresh worker start.
`GuestQmpSnapshotTest` requires `-e debugGuestQmp true` and a fresh unused worker;
optional recordId identifies a native library record. It invokes only the fixed
backend client (capabilities/status/CPUs/info registers), with parent-FD aliases
for Unix pathname limits, same-UID peer verification, bounded responses/timeouts,
and no caller commands or socket paths. The native frame/control lock is released
before socket I/O. Diagnosis and private log paths are retained, never guest-boot
qualification. Release builds disable the endpoint and native client.
Worker-owned Java Surface parcel copies are released after native acquisition;
replacement now releases the previous CPU producer deterministically. Window
observations retain precise geometry/lock/post codes, format/size/stride and the
first-error host timestamp; at most8 error lines per worker enter its native log.
Target execution must confirm recovery; source corrections alone prove no frames.
`qmpHoldSeconds` (0default, atmost3600) keeps the production activity resumed
and instrumentation active after initial snapshots, recording additional
read-only register snapshots every10seconds. `snapshotOnly=true` reuses an
existing already-enabled worker; `resumeExisting=true` explicitly resumes it.
No debug test stops/disposes the worker itself. Normal instrumentation completion
can force-stop the package: observed reason10 '[FORCE STOP] ...finished inst'.
On AndroidS+ `am instrument --no-restart` prevents restart/finished-instrumentation
force-stop ([AOSP Android16 policy](https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-16.0.0_r2/services/core/java/com/android/server/am/ActivityManagerService.java));
the target main process must already run. ActivityScenario closure still invokes
background pause, so reopen and Resume for continued foreground use. Exit reason
and trace availability remain actual platform evidence, not a service-failure claim.
For a fresh worker, `GuestQmpSnapshotTest -e touchWireTrace true` admits the fixed
typed trace before `debugArmQmp` and guest start, retaining configuration in the
receipt and each snapshot. This combines copied-buffer metrics/QMP with guest
MT wire logging from boot. `snapshotOnly=true` with this flag requires the
existing worker's `touchTraceConfigured` status; it never configures a running
guest. No event name, general command or caller path is accepted. Trace admission
does not prove that any touch frame was collected or visible.

`GuestOrientationLifecycleTest` is opt-in (`-e exerciseOrientationLifecycle true`)
against an already booted legitimate guest. It uses the production activity and
Surface, recreates the activity, requests landscape then portrait (restoring its
original own-activity request in `finally`), backgrounds/returns the Surface,
and cancels two held contacts before reusing valid IDs7/23. Semantic Pause/Resume
also ends the production Android gesture, suppresses stale MOVE and exercises a
fresh two-contact move, independent lift, cancellation and ID reuse after every
interruption. Android-side admission
and cancellation counters are separate from guest delivery/outcome evidence.
The test clicks semantic production Home/Power controls, checks paired native
admissions, retains exact worker/media/window observations and actual guest
screenshots, and releases contacts/buttons on failure. It fails if the platform
does not apply the requested orientation or if the worker changes; it never
changes system rotation settings. `hostChecksPassed` does not qualify a visible
guest, gesture response, or audible playback; manual flags remain false.
The presenter keeps guest portrait pixels upright, aspect-fitted to the current
Surface. Native touch maps view-local coordinates using the same viewport and
rotation0; display rotation is not applied a second time. Sensors have their
separate deferred rotation mapping. A fresh or cancelled view ignores MOVE/UP
until a new DOWN, including after recreation. For continuation root uses the
instrumentation `--no-restart` policy described above; closing the scenario
normally pauses the session and ordinary instrumentation may force-stop it.
Both gesture harnesses construct their stimuli inside the actual aspect-fit
image using observed guest frame dimensions and current View dimensions. Their
traces retain normalized guest coordinates, injected View pixels and expected
image bounds. They fail when actual dimensions are unavailable; whole-View
fractions could otherwise silently reach landscape letterbox margins and cause
native cancellation. This test geometry does not replace the production native
input mapper or prove guest consumption.
If this already booted worker configured the fixed trace, `touchWireTrace=true`
retains bounded wire records around these interruptions. The orientation test
does not start or reconfigure an existing worker.

[The APK notice/source matrix](DEPENDENCY-NOTICES.md) separates conveyed components,
verified per-ABI notice coverage, selected frontend runtime inputs and outstanding
source/relink materials. `recordApkRuntimeArtifacts` exports the actual main
variant graph without building; its private paths stay outside public assets/Git.

When root continues a worker with `--no-restart`, foreground the normal
`MainActivity` immediately before **each** instrumentation invocation, including
back-to-back tests. A measured second test remained pending before test entry
while Android had frozen the cached target process; normal foreground activity
launch unfroze it and the test completed. No system freezer/security setting was
changed. Absence of the test's evidence folder indicates it has not entered the
test body (the wake folder is created before activity launch/Binder observers).
Retain the initial pending-command log and foreground-launch receipt separately;
this observation does not establish a native observer deadlock or guest pass.

## Debug x86 packaged graphics provider

The optional ANGLE path is an unqualified debug candidate. A fresh `:engine`
worker accepts only the closed choice platform0 or angle-gl1; ANGLE is rejected
on ARM and release builds. The selected choice is fixed before start/context
creation; a failed library preflight also requires a fresh worker. The native
adapter uses fixed suffixed SONAMEs inside the app's linker namespace, with no
user paths, environment names or settings changes. Platform remains default.
A choice recorded in status alone does not prove renderer dispatch.

Supply all five controlled build properties together:

```sh
-PangleLibraries=/verified/provider/lib \
-PangleManifest=/verified/provider/elf-closure.json \
-PangleManifestSha256=ACTUAL_64_HEX_SHA \
-PangleNotices=/verified/provider/notices \
-PangleNoticesSha256=ACTUAL_64_HEX_SHA
```

`prepareAngleRuntime` verifies manifest pins, the exact three library hashes,
x86 ELF identity, SONAME/closed public Android dependency set, 16KiB LOAD
alignment and every declared notice before copying into owned debug outputs.
Notice paths use the offline viewer's checked component grammar. Provider DSOs
are packaged without stripping to preserve their pinned hashes; `check-apk.py`
verifies uncompressed ZIP alignment and packaged library/notice identities.
Release source sets omit these staged libraries/assets. The engine artifact
must also contain the optional dispatch implementation; older a95/c22 inputs
cannot satisfy the opt-in experiment.

`GuestAngleProviderTest` requires `exerciseAngleGuest=true`, a fresh unused x86
worker and a legitimately imported native device record. It checks packaged
hashes, same normal app UID, choice immutability and the actual engine's retained
public entry-owner/verified-dispatch diagnostics. `angleObserveSeconds` defaults
to180 (bounded10..1200); `requireNonzeroGuestFrame=true` can require a nonzero
sample for the chosen workload. The screenshot stays in `files/screenshots`.
The test preserves diagnostics and false manual guest/visible/audio flags;
actual app-UID loading and normal guest operation remain target acceptance work.
Source and selected-license notices do not establish complete application
compatibility or source/relink coverage. Root chooses the qualified library
candidate and controls the subsequent build/device execution.

`GuestFrontmostDiagnosisTest` uses only attempt10's existing interface and accepts
`diagnoseGuestFrontmost=true`. It explicitly resumes an existing worker, waits at
most15seconds for observed agentALIVE, and requests only fixed FRONTMOST1 with an
empty argument. It retains the entire native reply/status and paired captures
under `files/screenshots/<evidence>/`; no launch/button/start/stop query occurs.
Root can isolate this one test file on the frozen attempt10 source for a test-only
APK. Foreground the normal activity before each `--no-restart` invocation.
Launch guestStatus3 is preserved directly from SpringBoardServices' nonzero launch
return, distinct from native request-state3. The matched source provides no named
code3 enum. Existing Swift launch handling checks lock status separately; this
read-only FRONTMOST response reports `com.apple.springboard`/`Lock Screen` only
when the actual guest lock-status API says locked. Do not infer locked from3 alone.

The frontend extractor remains separately owned. Its verified schema1 bundle can
be consumed using `-PfrontendNotices=/verified/frontend-bundle` and
`-PfrontendNoticesSha256=ACTUAL_64_HEX_SHA`. The consumer exports the intended
`auditConfiguration` main runtime graph (debug default; choose release explicitly
for release), rehashes selected artifact bytes, validates the portable graph hash,
manifest pin/source metadata/complete declared payloads, and stages only public
manifest-listed bytes. Private artifact/cache paths never enter assets. Old assets
are replaced after full verification. No license text is generated or inferred
by the consumer. Preserve the bundle's own source identity unchanged; the publisher
may need a freshly generated bundle for the complete conveyed source reference.
Notice inclusion alone does not establish source availability or application
license/relink clearance.

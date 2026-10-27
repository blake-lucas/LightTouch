# Open work

Tick items off here as they land (with the commit), so nothing is lost between sessions.
Performance results go in [PERFORMANCE.md](PERFORMANCE.md).

## Support targets

Performance and graphics work aims at one good iOS per device first. Upstream Light Touch for Mac marks as
tested only the iPad on 3.2, 3.2.2 and 4.2.1 and the iPod touch 2G on 2.2, 2.2.1 and 3.1.3 (catalog
`available`); everything below starts from its `experimental` set.

| Device | Target iOS | Why | Today (Android) |
| --- | --- | --- | --- |
| iPod touch 2G (n72ap) | 3.1.3 (7E18) | upstream's tested pick, fastest on phones | Tested, recommended |
| iPod touch 2G | 4.2.1 (8C148) | its last iOS; apps that need iOS 4 | Tested |
| iPod touch 4G (n81ap) | 6.1.6 (10B500) | its last iOS, widest app range of the iPods | Tested; most games at or near 60 fps on a Poco F3 (Jetpack Joyride's title ~55, Epic Citadel ~30) |
| iPhone 4 (n90ap) | 6.1.3 (10B329) | its last iOS 6 (6.1.6 shipped only for the 3GS and iPod touch 4G); wide app range | not run off the Mac |

Later: the iPhone 4 on 4.2.1/4.3.5 and the 3GS on 5.1.1 (Tested, slow), the iPod touch 3G 4.2.1 (Tested), the
iPad, the iPod touch 1G and the original iPhone (never booted off the Mac), iOS 7 (skipped for now).

- [ ] Bring up the iPhone 4 on 6.1.3: prepare on Linux and the Poco, `compat.py` and `services.py` to `clean`.
- [ ] A validation run per target that a person doesn't have to watch, extending LT Bench and the
  compat harness (`tools/compat/`):
  1. Machine-readable results: LT Bench writes its results as JSON to its Documents folder, and the
     harness launches it through the guest tools and fetches the file over the USB bridge (house_arrest), in
     place of reading screenshots.
  2. Graphics correctness: fixed scenes drawn into an offscreen framebuffer, read back with `glReadPixels` and
     compared with reference images within a tolerance. ES 1.1 on every device: texture formats (PVRTC,
     RGB565/4444/5551, A8/L8/LA88), blend modes, texture environment and multitexture, point sprites, depth,
     stencil, mipmaps, framebuffer objects. ES 2.0 on SGX devices (iPod touch 3G and later, not the 2G's MBX):
     shaders, render to texture, non-power-of-two textures, depth and stencil formats. Reference images from
     a real device where possible, else from the macOS host; the harness keeps the diff image of a failure.
  3. Performance at a fixed workload, frame times as median and 1% low: a 2D sprite scene (Jetpack's
     title, ~230 sprites), a 3D ES 1.1 mesh scene (Asphalt 5's race, ~125 draws), an ES 2.0 shader scene on
     SGX devices. Plus boot to Home, app launch and Home-screen swipe fps. The current microbenchmarks stay.
  4. A short list of real games per target with the frame rate each should hold, for checks by hand.
  5. Run on Linux, the Poco and the Pixel; results per target in COMPATIBILITY.md and PERFORMANCE.md.

## Performance (current focus)

- [ ] Storage space: give back the space iOS frees. A device's NAND page files only grow: pages iOS
  erases or no longer maps keep their bytes in the app's storage. Options: punch holes
  (`fallocate(FALLOC_FL_PUNCH_HOLE)`) for pages an erase clears, the way QEMU discards; or compact a
  stopped device's page files. Check what Android's file systems (f2fs, ext4) support first.
- [ ] Android: steady ~60 Hz guest UI. Poco F3, engine 64d111110b, Home ↔ Spotlight swipes
  (`inappfps.py --swipes 10`, two runs): 55.3 / 56.3 fps, 94% of frames on the 16.7 ms cadence (was
  44.8–49.2). Pixel not re-measured.
- [ ] Android: steady target frame rate in common period games. Jetpack Joyride's title is the default
  benchmark on both phones, the heaviest scene found on the iPod 2G (Flight Control, Doodle Jump and
  JellyCar hold their targets on both phones and have left the set). Poco, guest tools serial 27: 53.4 fps without the BQL spin (ceiling 58, median 54, 1% low ~28, 8–13
  hitches per 60 s); 57.5 with a 50 µs spin and 58.4 with the 150 µs budget, now the Android default (median 58–60, 1% low
  ~29–30, 1–8 hitches per 60 s). Desktop Jetpack 59. The 1% low (~33 ms frames) is what is left; see
  the hitch leads below.
- [ ] iPod touch 4G 6.1.6 on the Poco (PERFORMANCE.md, "iPod touch 4G"): Doodle Jump's title 45–49 fps
  (ceiling 54–60), Asphalt 5's intro movie ~16 of 30, Jetpack's title ~23–24 (median 24; desktop
  ~27–28, vCPU-bound), Epic Citadel ~6–8 (runs since `d3768ae49c`; vCPU-bound). Next: Flight Control
  (desktop 54–58; on the Poco its PLAY! button ignores `adb input` taps that UIKit alerts take: find out
  why before measuring), Real Racing (desktop attract demo 35–45), the Asphalt 5 race. Open option:
  report a headset plugged in (CS42L59 register 0x33 bit 7) so iOS skips speaker protection (~10% of
  the vCPU in games); a product decision, since the guest then routes audio as to headphones.
- [x] IPA installs (PERFORMANCE.md, "App installs"): unpacking progress from the guest's free space,
  limits that follow it, 1 MiB AFC writes, NAND FIFO bulk moves. Asphalt 5 on the Poco 4G: timed out
  after ~10 min → done in 240 s. Left: the 2G's unpack is guest-CPU-bound (~148 s on the desktop); the
  4G's IOP round trip per NAND request (~1.6 ms on the Poco). Check the Pixel.
- [x] BQL spin on by default on Android with the 150 µs budget (engine `b5708085da`): +7.6% and
  hitches 30 → 8 on the Poco, then +2.1% for 150 µs (PERFORMANCE.md).
- [ ] BQL spin follow-ups: interleaved pairs on the Pixel (JellyCar and a 60 fps scene) and one more
  Poco game with `LT_BQL_SPIN=off` as the A arm, idle engine CPU with the spin on (it burns up to
  150 µs per contended take), and a desktop measurement before turning it on there.
- [ ] Confirm the two open engine leads (both default off, PERFORMANCE.md): the indirect miss-helper
  cache (`LT_IBTC_L2=on`, +2.2% in 3 of 3 pairs) and the inlined TB lookup hit (`LT_TB_LOOKUP_INLINE=on`,
  ahead in 6 of 6 pairs, flat with the array cache). Measure each on top of the BQL spin (now the Android default).
  Both together on the 4G's Jetpack title (2026-10-25): flat, 29.7 → 29.4 Mcycles a frame. Epic Citadel's tour on the Poco (vCPU 95% busy, ABBAAB): 60.1 → 61.9 Mcycles a frame, no gain; the lead is closed for the 4G.
- [ ] Assassin's Creed: Altair's Chronicles holds ~19–20 fps on the Pixel 10, which may be the game's
  own cap (as Asphalt 5's 25 is). The overlay shows the guest CPU at 50–60% while the engine reports
  95–100%: find where the engine time goes that the guest does not account for (frame inspector
  capture `lighttouch-hitch-20261019-225636.zip`, then a simpleperf profile), and confirm the cap on the
  desktop. An audio loop was seen once; reopening the app cleared it.
- [ ] Jetpack hitches on the Poco (PERFORMANCE.md, "Hitches in Jetpack's title"; capture tools in
  PROFILING.md). Leads, by evidence:
  1. Held refreshes recur 88–92 ticks (~1.5 s) apart in four sparse captures (`LT_FRAME_PATH`). Follow
     the guest present and compositor queue boundaries before each one, including the work that starts
     before the visible gap, and compare with ordinary intervals.
  2. Audio: the AAC decoder runs before 48 of 87 gaps over 30 ms, and its converter's 2-mod-4 output
     batches take ~420 µs a call through ARM1176 alignment traps (25 µs aligned). Join exact converter
     batches to the recurring misses. Keep the traps and iOS's handler (architectural).
  3. Logging: the two longest gaps in one capture follow ~26 ms of launchd + syslogd work. Find which
     stderr or IPC request it serves before changing logging again (the tally experiments were rejected).
  4. Bluetooth: a 166 ms gap with 84 ms of BTServer work, and a 10 s BTServer timeout and relaunch at
     one boot (`LT_BT_PATH` shows every HCI reply injected). Capture UART FIFO drain, timeout and IRQ
     state around a reproduced failure. Don't log firmware payloads.
  5. The rest of the BQL waits: the 150 µs spin removes most short sleeps, but longer waits and slow
     wakeups from guest idle remain. Find the lock owners and main-loop work first; the PL080's
     per-element audio dispatch and the LCD's dirty tracking are the known holders.
- [ ] Pixel gameplay pauses: a 214 ms Jetpack gap had 206 ms of execution wall time and 83 ms of vCPU
  CPU time. Capture a scheduler trace with CPU samples during the scene. Don't assume the Poco's causes.
- [ ] Recording overhead: the Android Start/Stop profile costs 6–7% of the Jetpack frame rate on the
  Poco (54.0 → 50.0 and 52.7 → 48.6 fps in pairs). Make it cheaper, or say in the UI that recorded rates
  run low. Consider Android 15's `ProfilingManager` for host scheduling traces next to it.
- [ ] Indirect branches with many targets (`objc_msgSend`'s final jump goes to every method): LT Bench
  `icall` is 3–6× a direct call. Rejected so far (PERFORMANCE.md): 2-way and 64K jump caches,
  revalidation across TLBIASID, an inline jump-cache probe in generated code (`icall` 32 → 15 ns, no game
  gain) and an eight-bucket per-site cache (+96 bytes per TB). Untried: promoting an inline probe's hit
  into the site's two-entry cache. `LT_IBTC_L2` (above) is the open lead.
- [ ] LT Bench `thread-switch`: a pipe round trip between two threads takes 65 µs on the Poco (26 µs on
  the desktop), 4× a Mach RPC to another process. Not a wait: during the test the vCPU runs ~95% of
  the time and is halted <1%, and a desktop profile is flat (no emulator hotspot), so it is guest
  kernel work: four BSD syscalls and two scheduler switches per round trip, against two Mach traps
  with a direct handoff for the RPC. Left open only to compare with a real device.
- [x] Install the quiet GL shim in place (engine `7f744e80cc`, guest tools serial 27): Linux
  `lighttouch-linux --library LIB --record ID --set-guest-package ITPACK`, Android "Guest tools" on a
  stopped device. Both benchmark phones have it; the Poco has the private serial 29 from the rejected
  tally experiment, which behaves as 27 against the current host.
- [x] Prepare guest tools for public distribution (serial 30, engine `e95888e9cb`). AppSync's signer
  certificate was Apple's "iPhone Activation" certificate; it is now a self-signed LightTouch one.
  The `contrib/` sources carry no per-file headers, which qemu-ios's LICENSE covers as
  GPL-2.0-or-later. Tested on the Poco: serial 30 installed at boot (good 30), and Enigmo 4.1
  installed through AppSync and opened. The armv7 package was built but not run. The helpers still
  build against Apple's iPhoneOS 3.1.3/3.2 SDK headers and link stubs from a third-party mirror.
  Apple's own 3.1.3 SDK installer is on the Internet Archive (docs/BUILDING.md, "The Mac"); try
  building from it instead.
- [x] Guest package verdicts on Android and Linux. The portable runtime never told the guest's loader
  (qemu-ios `contrib/it-boot`) that a package was good, so `it_boot` reverted any package newer than
  the record's after two boots (`MAX_TRIES`) and refused that serial from then on. That is why serial
  27 installed by hand lasted two boots. `Portable/src/GuestPackageJudge.hpp` now judges each boot as
  the macOS app's `GuestPackage.verdict` does: good after 10 s with the agent alive and a loader
  report, bad after 300 s of running time without that. Time paused does not count. The verdicts go to
  `state/guest-verdicts`, and each offer carries them as `verdict good N` / `verdict bad N` lines.
- [x] Update a record's guest tools in place, keeping the device. `lt_library_set_guest_package(library,
  id, path, ...)` refuses while the record is open (`state/.lease`) or when the file is not an
  `ITPACK01` package, then stages and swaps `assets/guest.itpack` and rewrites `guest_package_sha256` in
  `record.json`. The guest's `it_boot` installs a newer serial over the existing system at the next
  boot, keeping apps and data. Linux: `lighttouch-linux --library LIB --record ID --set-guest-package
  ITPACK`. Android: "Guest tools" on a stopped device applies the file to the selected device. Tested:
  desktop (serial 26 record → 27, two boots judged good) and the Pixel (26 ↔ 27 through the picker).
- [ ] Guest tools update, follow-up: check that the package has a family for the record's board and
  build before swapping (today an older or mismatched package is accepted; `it_boot` then keeps the
  installed serial).
- [x] LT Bench GL tests (`gl-get`, `gl-state`, `gl-draw`, `gl-sprite`, `gl-upload`, `gl-present`), run on
  the desktop, the Poco and the Pixel (docs/GUEST-BENCHMARKS.md, PERFORMANCE.md).
- [x] Fewer host GL calls per draw: the client-array cache (engine `6762325419`, on by default) skips
  pointer, enable and client-unit calls that change nothing (setup + cleanup 0.619 → 0.228 µs per draw on
  the Pixel). Strict `glGetError` checks stay (`IT_GLES_STRICT=0` was rejected).
- [ ] Dynamic TLS in the engine. `libqemu-arm.so` is `dlopen`ed, so each `__thread` access in QEMU's
  hot paths (`current_cpu`, the RCU reader) goes through `tlsdesc_resolver_dynamic`: 1.2% of the vCPU
  in Jetpack, 3.2% in `gl-sprite` on the Poco. Find the hot accessors (a call-graph profile), then cache
  them or move them out of TLS.
- [ ] Texture upload on the Pixel: `gl-upload` (16 KiB `glTexSubImage2D`) is 51–57 µs against 12 µs on
  the Poco and 3–4 µs on the desktop. It looks like the Pixel's GL stack (ANGLE or the IMG driver).
  Profile it before testing a game that streams textures.
- [ ] The guest kernel is 22% of the vCPU in Jetpack on the Poco (half the time in translated code).
  It is spread thin: locks, Mach IPC, the scheduler, timers, IOKit work loops. `0addc3e156` made its
  lock and interrupt-mask instructions cheaper. `LT_EXCEPTION_PROF=on` counts exceptions, SVCs and
  Mach messages; next, find which of them the emulator makes expensive.
- [x] Pixel: the AHardwareBuffer present (engine 4de4ae5ba1) takes the `IOSurface` path on the Pixel's
  PowerVR driver and renders correctly (LT Bench).
- [ ] Pixel: the user sees Doodle Jump's menu swing between fast and stuttering. Per-second fps
  (`FC_SERIES=1`, 60 s, 974c213352 + b11ef28b): ADPF on 31.3 and 29.5 mean, each second 19–44; ADPF off
  17.3 and 18.6, 11–29. The swing is the same proportion (about ±25%) either way and no second
  reaches 60, so ADPF is not causing it (it is worth 1.7× here). Next: per-frame gaps against the
  BQL hold log, to see whether the slow seconds line up with main-loop work (audio DMA, LCD).
- [ ] Try the engine's upstream merge (`qemu-ios` `dev/upstream-sync`, 7ab545390b) on a phone;
  verified on Linux only.
- [x] Document every performance change and its measurement: [PERFORMANCE.md](PERFORMANCE.md).
- [x] Guest fps readout and 60 s graph in the Android app (b11ef28b).
- [x] Manual Start/Stop profile in the Android app, next to the fps graph (engine `50a08eb2d2`):
  frontmost app, per-frame GL totals, slow GL calls, BQL waits and holds, Android present stages
  ([PROFILING.md](PROFILING.md)).

## Firmware and assets

- [x] Rewrite [ASSETS.md](ASSETS.md) easiest route first: `tools/get-assets.py` (downloads the
  upstream app ZIP and unpacks it on any OS, including guest.aar's LZFSE Apple Archive), then by
  hand, then IPSW preparation. `--verified-tools` makes the folder IPSW preparation needs
  (the n72-only `lighttouch-prepare` was verified with it; `firmwarekit` replaced it).
- [x] Boot the bases `get-assets.py` unpacks. LightTouchMac prepares them with `aes-uid=hardware`
  (its NOR wraps the SHSH with iBoot's encrypting UID derivation), and the v0.0.5 engine accepted only
  `engine` or `legacy`, so every port failed at engine start with "aes-uid must be 'engine' or
  'legacy'" (reported on Windows). Engine dev/perf now carries upstream's `cb400606d2`. Checked on Linux:
  the v0.0.5 engine fails with that error on an imported `n72ap-7E18.itbase`, and the new engine boots
  it to SpringBoard. Users need a new release to get it.
- [x] Prepared-device (`.itbase`) import removed from every app, with `get-assets.py`: each app prepares from an
  IPSW it downloads (or the user chooses), with the boot ROM downloaded too, so nothing needs rehosting.

## Cleanup

- [x] Prune the upstream Mac app code (branch `dev/cleanup`; the macOS port still builds with SwiftPM
  from the root `Package.swift` via `macOS/build-shared.py`):
  1. [x] Xcode project/workspace, xctestplans, `Packages/BuildTools`, `Packages/ReleaseChecks`,
     `Models/`, the upstream `scripts/` (kept `check-android-package.py`, `measure-android-session.py`),
     `tests/offline`, `tests/sessions` and the Xcode-only `Configuration/` files (b40cb574).
  2. [x] `firmware-catalog.json` now lives in `Portable/firmware/` (2a9a518f). `CActivation` stays in
     `Packages/FirmwareKit/Sources`: SwiftPM cannot take a target path outside its package and FirmwareKit
     stays for the macOS port; `Portable/firmware` already compiles it from there.
  3. [x] `LightTouchMac/`, `LightTouchServices/`, `macOS/build-native.py`, the `macos-arm64` dispatcher
     target and the `LightTouch`/`CLockdown`/`LightTouchServices` targets (57bb9aaa).
     `Packages/DeviceServices` stays: `LightTouchCore` (linked by `LightTouchDesktopHost`) depends on
     `HostServiceWire`/`HostServiceClient`.
  4. [x] `CONTRIBUTING.md` rewritten without Xcode; stale handoff/branch notes and the stopped TCG
     profiling proposals removed.
- [x] Remove the Swift packages, `Package.swift`, `LightTouchDevice/`, the Python/Tk `Desktop/`, the Linux SDL
  frontend, the Windows C# helper and the macOS shared host: the Rust desktop app and firmwarekit replace them
  (v0.0.12). `CActivation` moved to `crates/lighttouch-firmware/csrc`.
- [ ] Port `tools/compat` to `lighttouch prepare`/`run` and Android's own preparation (docs/COMPATIBILITY.md).
- [ ] The core still reads `.itbase` archives (`lt_import_archive*` in `Portable/src/Storage.cpp`), which only its
  tests use now: move those tests' fixtures to prepared folders, then drop the archive reader.

## Android bugs seen, not yet fixed

- [x] The first touch after the guest wakes was often ignored: the "Guest screen asleep" label sat
  above the guest view and the view jumped down on wake. It is now an overlay on the guest view.
  (Harness note: the guest re-sleeps ~5 s after a Power wake; a second Power tap before the wake
  shows puts it back to sleep.)
- [ ] Stop was seen disabled once after Store use; not reproduced.
- [ ] The Store's install queue keeps the guest running with LightTouch off screen (then pauses it when the
  queue is empty); not yet tried on a phone. Guest audio is not muted meanwhile, and Android 14+ ends a
  `dataSync` foreground service after six hours a day.
- [x] Store overlay lacks `testTagsAsResourceId`, so UI automation cannot find its controls.
- [ ] Some catalog apps marked compatible need iOS 4 APIs (Doodle Jump 3.x: GCD; the n72 search row
  is 3.6.1). Consider flagging apps that weak-link iOS 4 symbols, or surfacing Last crash
  automatically after a quick quit. Linux/Windows Apps now has a version picker; it has no Last crash.

- [x] Battle Bears: Zombies! was guest-CPU-bound (desktop ~24 fps, Pixel 22): its OpenFeint 2.9.1 `_mainDefault:`
  thread spins on an empty run loop. Guest tools serial 31 make that wait sleep (gles-public); 29.6–29.7 fps, the
  game's cap (PERFORMANCE.md). Check on the phones.

- [ ] Installing a very large app over the USB bridge: profiled and sped up (the IPA installs item
  above). The guest "crash" was likely a killed engine's NAND overlay (fixed in `b704f259fa`); not
  reproduced on the 4G or 2G desktop runs of 2026-10-22.

- [ ] Doodle Jump on the iPod touch 3G (5.1.1) plays at double speed: physics run twice as fast as they should.
  An accuracy bug (timer, display-link or vsync rate), not a performance one.

- [x] iPod touch 4G (6.1.6) on the Pixel 10: in gyro driving games (Real Racing with Gyroscope on), "down"
  re-centred every so often. The accelerometer's z was upside down (face up read as face down), so iOS's motion
  fusion disagreed with the gyro on every forward or back tilt. Fixed in the engine (cdd01aec99); confirm on the
  Pixel in Real Racing.

## Android front end (dev/ui)

- [ ] Save & Stop: keep the guest's state across app close and phone reboot. The engine has
  `qemu_ios_snapshot_save2` and `-incoming`, but `gles_save_blocker` refuses migration while GL
  contexts are live on EGL hosts, and the GL serializer (`hw/arm/gles-host-snapshot.c.inc`) is
  CGL-only. Needs: an ES1/ES2 state capture (no `glGetTexImage`, so read textures back through an
  FBO), a runtime save + "suspended" marker per record, and `-incoming` restore on start. The UI
  hook exists: `MainActivity.canSaveState` turns the Stop dialog's primary action into Save & Stop.

- [ ] Web proxy (port of the Mac's, per device in Settings: Off / Direct / Archive with a date). The guest
  half ships in the guest tools: `itproxy on|off` points iOS's HTTP/HTTPS proxy at 10.0.2.100:3128 and
  `ittrust` trusts a CA. The Mac's host half to port: `LightTouchDevice/WebProxy.swift` (HTTP/1.0 replies, TLS 1.0
  terminated with a per-host leaf; direct, archive from the Wayback Machine, off) with `WebProxyAdapters.swift`
  (Weather, Wi-Fi location, retired hosts), `DeviceRuntime/WebProxyCA.swift` (the device CA and leaves, hand-written
  DER), `LightTouchCore` `WebProxyConfiguration`/`WebProxySetup`/`DeviceWebProxy` (routing file, then route and
  trust through the guest agent). Android needs: (1) an engine guestfwd that reaches the app (the Mac's
  `guestfwd=...-cmd:/usr/bin/nc -U SOCK` needs `/bin/sh` and `nc`; a per-connection Unix-socket target in
  `net/slirp.c`, passed as `-netdev user,id=wifi0,...` in place of the boards' default); (2) the proxy in the
  engine service (check that Conscrypt still serves TLS 1.0 with a cipher iOS 3 offers, else the engine's OpenSSL); (3) the CA and leaves
  (port the DER writer; Android has no public certificate builder); (4) a guest agent client to run `itproxy` and
  `ittrust` (Android has none yet); (5) the Settings row. (Battle Bears' spinning thread waits on an empty run
  loop, not on the network, so this is unrelated to it.)

## Guest graphics

- [x] Untextured white or grey geometry on Android (engine 4ae00eaca8): Jetpack Joyride's lab
  background, Battle Bears' zombies and menu art, Real Racing's opponent cars and scenery. Cause:
  ES `glTexImage2D` does not convert between formats, and the bridge uploaded opaque PVRTC (decoded
  to RGBA) as `GL_RGB`, so each such upload failed with `GL_INVALID_OPERATION` (engine log:
  `REFUSED upload-error:0xde1:0x502`) and the texture stayed empty. Desktop GL converts, which is
  why Linux and macOS never showed it. The upload now does the conversion itself. Confirmed on the
  Poco F3 (Jetpack: white before, correct after, same NAND) and by the user on the Pixel 10
  (Jetpack, Battle Bears, Real Racing).

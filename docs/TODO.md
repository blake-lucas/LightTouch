# Open work

Remove items once they land (the commit and git history keep them), so this list stays what's still open.
Performance results go in [PERFORMANCE.md](PERFORMANCE.md).

## Support targets

Performance and graphics work aims at one good iOS per device first. Upstream Light Touch for Mac marks as
tested only the iPad on 3.2, 3.2.2 and 4.2.1 and the iPod touch 2G on 2.2, 2.2.1 and 3.1.3 (catalog
`available`); everything below starts from its `experimental` set.

| Device | Target iOS | Why | Today (Android) |
| --- | --- | --- | --- |
| iPod touch 2G (n72ap) | 3.1.3 (7E18) | upstream's tested pick, fastest on phones | Tested, recommended |
| iPod touch 2G | 4.2.1 (8C148) | its last iOS; apps that need iOS 4 | Tested |
| iPod touch 4G (n81ap) | 6.1.6 (10B500) | its last iOS, widest app range of the iPods | Tested; most games at or near 60 fps on a Poco F3 (Jetpack Joyride's title ~25, Epic Citadel ~43) |
| iPhone 4 (n90ap) | 6.1.3 (10B329) | its last iOS 6 (6.1.6 shipped only for the 3GS and iPod touch 4G); wide app range | Tested (Linux: lock screen, services, clean stop) |
| iPad (k48ap) | 5.1.1 (9B206) | its last iOS | Tested (Linux: lock screen, services, clean stop) |

Also Tested: the iPhone 4 on 4.2.1/4.3.5, the 3GS on 5.1.1 and the iPod touch 3G on 4.2.1. Not starting: the iPod
touch 1G and the original iPhone (white screen, see COMPATIBILITY.md). iOS 7 skipped for now.

- [ ] iPhone 4 6.1.3 and iPad 5.1.1 on a phone. The iPhone 4 ran the game benchmark on the Poco (docs/GAME-BENCHMARKS.md).
  The iPad on the Poco: Epic Citadel gameplay ~35-40 fps.
- [ ] iPod touch 1G / original iPhone white screen: iBoot never boots the kernel after preparation.
- [ ] iPhone 5 (n42ap, A6, iOS 10.3.4): upstream qemu-ios added it in November 2026 (s5l8950 machine, generic timer, AIC,
  TLB and jump-cache work for iOS 10's process counts, `0ad2a354b5` and the commits around it; reviewed in the v0.0.14
  upstream pull). Left out of our engine for now; pull it in, with its catalog and firmwarekit side, when we add the
  device.
- [ ] iPhone 3GS microphone records silence (the setting attaches the mic only to the A4 devices).
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
  4. Run on Linux, the Poco and the Pixel (the game matrix in `tools/bench/` has run on Linux and the Poco); results per target in COMPATIBILITY.md, GAME-BENCHMARKS.md and PERFORMANCE.md.

## Performance (current focus)

- [ ] Games that pace themselves with a restarted timer: find what sets `w`. Real Racing HD 1.04 on the iPad 5.1.1
  races at ~20–21 fps on every host (desktop 21.2, Pixel 10 20.2; the menu reaches 57). From its binary (symbols
  kept): `-[EAGLView initWithCoder:]` sets `m_bUseDisplayLink` and then clears it, so it always draws from an
  NSTimer; `CGlobal::game_UpdatePlay(dt)` keeps the last 16 samples of `1000 / dt` (integer ms), averages them,
  floors that at 20, adds 2 and calls `updateAnimationInterval(1 / that)`, which stops and restarts the timer every
  frame. A frame is then the interval plus `w`, the time from the timer firing to that restart, and the rate settles
  where `f (f + 2) = 2000 / w`. Reconstructed from the desktop capture's presents (45–49 ms apart, not vsync steps):
  `w` ≈ 3.5 ms (p10 2.1, p90 5.1). `w` ≈ 2 ms would give ~30 fps, 1 ms ~44. The Pixel runs the guest CPU ~3× slower
  yet lands at the same rate, so `w` looks like fixed latency rather than guest work: measure, in a race, the guest
  timer's wake-up (PMGR event timer deadline → FIQ → the run-loop thread) and how long `presentRenderbuffer` takes
  to return. Unknown: what a real iPad 1 gets (likely the 20s, GPU-bound near 30). Other games that restart an
  NSTimer each frame would gain the same way.
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
  (ceiling 54–60), Asphalt 5's intro movie ~16 of 30, Jetpack's title ~25–27 (23.7–23.9
  before the headphones route; desktop ~27–28, vCPU-bound), Epic Citadel's tour ~43–44 cool, 1% low ~20 (runs since
  `d3768ae49c`; vCPU ~97% busy). Next: Flight Control
  (desktop 54–58; on the Poco its PLAY! button ignores `adb input` taps that UIKit alerts take: find out
  why before measuring), Real Racing (desktop attract demo 35–45), the Asphalt 5 race.
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
- [ ] Pixel: the user sees Doodle Jump's menu swing between fast and stuttering. Per-second fps
  (`FC_SERIES=1`, 60 s, 974c213352 + b11ef28b): ADPF on 31.3 and 29.5 mean, each second 19–44; ADPF off
  17.3 and 18.6, 11–29. The swing is the same proportion (about ±25%) either way and no second
  reaches 60, so ADPF is not causing it (it is worth 1.7× here). Next: per-frame gaps against the
  BQL hold log, to see whether the slow seconds line up with main-loop work (audio DMA, LCD).
- [ ] iOS's "Connect to iTunes to Use Push Notifications" alert pauses each game on an unactivated iPad until OK is
  tapped (all versions). Decide whether to suppress it; the bench's alert taps miss on the iPad (OK sits at scan
  0.5,0.60 for landscape apps, 0.57,0.52 for portrait).
- [ ] Controller motion on Windows (Switch Pro, 2G 3.1.3 and 4G 6.1.6 Doodle Jump): the readout looked frozen and
  neither a motion nor a stick tilt binding leaned the device. On Linux a stick tilt leans Doodle Jump (3G) and motion
  reaches the 2G's accelerometer. Suspected: the controller stops sending sensor reports after SDL turns them on
  (Steam Input?), and a motion binding then overrode the stick tilt. `3110750a` re-enables quiet sensors and falls
  back to the stick; `8901ff19` logs sensor rates (`logs/controllers.log`) and motion the engine turned away
  (`logs/engine*.log`, formerly `logs/session-*.log`). Read those from the user's next run before calling it fixed.

- [ ] Checks still owed from fixes that landed:
  - IPA installs: check the Pixel. What's left is the 2G's unpack, which is guest-CPU-bound (~148 s on the desktop), and the 4G's IOP round trip per NAND request (~1.6 ms on the Poco).
  - Battle Bears: Zombies! at its 29.6–29.7 fps cap (guest tools serial 31): check on the phones.
  - The 4G's gyro "down" re-centring (engine `cdd01aec99`): confirm on the Pixel in Real Racing.

## Firmware and assets

- [x] "firmwarekit's emulator library has no machine for n18ap": the `machines` listing unloaded the engine before
  exit, and Android 10's exit() then called the engine's exit handlers in unmapped code (SIGSEGV; issue #11, every
  preparation on a HUAWEI P40 Lite E). The listing now keeps the library loaded. (unverified on Android 10)
- [ ] Jailbreak on the iPod touch 2G. A jailbroken 2G (3.1.3 checked, on main's firmwarekit too) starts once and then
  never again: Cydia's Startup job installs Substrate from its AutoInstall folder on the first boot, Substrate's install
  script writes `/etc/launchd.conf` (`bsexec .. /usr/bin/cynject 1 …/SubstrateLauncher.dylib`), and on every later
  boot launchctl stops right after "Couldn't stat /etc/mach_init.d" (kernel `-v`: no further I/O, the engine near
  idle). Without Substrate the same device restarts fine. The 3G, 4G, 3GS and iPhone 4 boot jailbroken kernels
  (sandbox patch, `amfi_get_out_of_my_way`); the 2G's is stock with only the AMFI boot args. Until this is solved
  the prepare screens show the Jailbreak switch locked on the 2G and firmwarekit refuses it. Dropping Substrate (and
  HTTPatch, a Substrate extension) isn't enough on its own: on 3.x it_typein then goes back into SpringBoard's
  `DYLD_INSERT_LIBRARIES`, which cydo refuses (see `cydia::install_jailbreak`), and a user installing Substrate from
  Cydia would hang the device the same way.

## Cleanup

- [ ] Port `tools/compat` to `lighttouch prepare`/`run` and Android's own preparation (docs/COMPATIBILITY.md).
- [ ] The core still reads `.itbase` archives (`lt_import_archive*` in `Portable/src/Storage.cpp`), which its
  tests use and `lt_library_import` still falls back to for a source that isn't a folder (`Portable/src/Library.cpp`):
  move those tests' fixtures to prepared folders, then drop the archive reader.

## Android bugs seen, not yet fixed

- [ ] Stop was seen disabled once after Store use; not reproduced. (unverified)
- [ ] The Store's install queue keeps the guest running with LightTouch off screen (then pauses it when the
  queue is empty); not yet tried on a phone. Guest audio is not muted meanwhile, and Android 14+ ends a
  `dataSync` foreground service after six hours a day. (unverified)
- [ ] Some catalog apps marked compatible need iOS 4 APIs (Doodle Jump 3.x: GCD; the n72 search row
  is 3.6.1). Consider flagging apps that weak-link iOS 4 symbols, or surfacing Last crash
  automatically after a quick quit. Linux/Windows Apps now has a version picker and "Why did the last app quit?".

- [ ] Installing a very large app over the USB bridge: profiled and sped up (PERFORMANCE.md, "App
  installs"). The guest "crash" was likely a killed engine's NAND overlay (fixed in `b704f259fa`); not
  reproduced on the 4G or 2G desktop runs of 2026-10-22.

## Android front end (dev/ui)

- [ ] Save & Stop: keep the guest's state across app close and phone reboot. The engine has
  `qemu_ios_snapshot_save2` and `-incoming`, but `gles_save_blocker` refuses migration while GL
  contexts are live on EGL hosts, and the GL serializer (`hw/arm/gles-host-snapshot.c.inc`) is
  CGL-only. Needs: an ES1/ES2 state capture (no `glGetTexImage`, so read textures back through an
  FBO), a runtime save + "suspended" marker per record, and `-incoming` restore on start. The UI
  hook exists: `MainActivity.canSaveState` turns the Stop dialog's primary action into Save & Stop.

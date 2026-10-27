# Performance log

Every performance change, what it fixed, and how it was measured. Newest first within each
section. Test phones: **Pixel 10** (Tensor G5: 2×A520, 5×A725, 1×X925; GrapheneOS) and
**Poco F3** (Snapdragon 865-class; LineageOS, Android 16), the low-end target. Guest: iPod touch 2G, iOS 3.1.3
(`n72ap-7E18`).

## How to measure

The harness is in [`tools/perf/`](../tools/perf):

| Tool | Measures |
| --- | --- |
| `inappfps.py` | In-app boot to lock screen, then Home ↔ Spotlight swipes; delivered fps from SurfaceFlinger `--timestats` for the guest SurfaceView (`--idle SECS` adds idle engine CPU). |
| `gamefps.py SECS` | Delivered fps while a game runs on screen. |
| `fcbench.py APK SECS [GAME]` | Installs an APK, boots, unlocks, starts a game from Home page 2; presets follow each phone's layout (Poco: `jetpack`, `flightcontrol`; Pixel: `jellycar`, `doodlejump`; a game that holds a steady 60 on both phones leaves the benchmark set, so Flight Control, Doodle Jump and JellyCar are no longer run; check with `FC_SHOT=file.png`) and runs `gamefps.py`. Debug builds read `files/debug-env` (`KEY=VALUE`), e.g. `LT_CROSS_PAGE_CHAIN=off` or `LT_ADPF=off`, for same-APK A/B runs. |
| `version-apk.sh VERSION` | Builds a released version as a side-by-side APK (`org.lighttouch.android.vXYZ`, "LightTouch VERSION", non-debuggable) laid out like a release build, so its engine matches the release's (the engine profile matches functions by source path). For A/B tests between versions on one phone. |
| `versionab.py OUT GAME SECS ROUNDS PKG...` | Game fps between installed versions: rounds alternate the order, one guest runs at a time (Home, then Stop, after each run), with a rest between runs; per-second series, screenshots and a summary in `OUT`. The tools take `LT_PKG` for a side-by-side install; `fcbench.py -` measures what is installed. |
| `framestats.py` | Frame pacing from a per-frame log: the **ceiling** (95th percentile of 500 ms windows: what the game sustains when nothing gets in the way), median, **low** (5th percentile: how deep the dips go), 1% low, **hitches** (frames over 50 ms), share of time at ≥55 / <30 fps, and a chart. `fcbench.py` with `FC_FRAMES=chart.png` turns on the runtime's `LT_FRAME_LOG` and reports it for the same window. Given the engine's `IT_FRAME_TRACE` (with `IT_VCPU_ACCT` and `IT_GLES_PROF`) it also splits each class of frame (on time / late / hitch) into guest code, GL, BQL wait, idle and off-CPU time. |
| `androidsyslog.py` | Android: the guest's syslog for 1–600 s through the app's USB bridge, with receive times on the same clock as the frame logs. |
| `guestprocs.py` | Guest process, task and pmap pointers read with LLDB through `LT_GUEST_GDB_PORT`, to name the guest threads in a capture ([PROFILING.md](PROFILING.md)). |
| `gameprof.sh` / `inappprof.sh` | `simpleperf` of the `:engine` process during play / swipes: per-thread split, vCPU DSOs and symbols. |
| `profile.py` and the other analyzers in `tools/perf/` | Read the Android app's Start/Stop profile captures and the engine's opt-in hitch diagnostics; see [PROFILING.md](PROFILING.md). |
| `abtimer.sh`, `appab.py`, `scrollab.sh` | Interleaved A/B runs. |

"Delivered fps" is frames the guest surface actually presented, from SurfaceFlinger, not a
counter inside the app. It counts guest screen updates, not a fixed refresh: a static screen
publishes almost nothing.

Keep the measured frame samples with `framestats.py --device 30 --save frames.log`; the saved
window can be analysed again with `framestats.py frames.log --secs 30`. Use `fcbench.py APK 0 ltbench`
to launch LT Bench without sampling fps on its static results screen.

**What full speed is.** The iPod touch 2G panel refreshes at 60 Hz, and a game's own cap is
60 or 30 fps. Find a game's cap on a host fast enough to reach it: the desktop app runs a record
headless (`lighttouch run ID --seconds N --script FILE`, with timed touch and button commands) and its
status lines carry the produced frame count. On a Ryzen 7 3700X, Flight
Control 1.9.2 holds **60 fps** throughout play, so it targets 60: the phones' ~18–25 fps are
30–40% of full speed.

Measure in-app (not the standalone engine): the Android scheduler
treats the app's worker process differently, and that turned out to be the largest effect.

**Run-to-run spread on the Poco.** Identical runs of Flight Control land anywhere from ~51 to
~56 fps. It is not core placement: sampling the vCPU thread every 250 ms over six runs found it on
the prime core (cpu7) 95–100% of the time in every run. That core's clock, chosen by schedutil
under the ROM's 2.75 GHz cap, averaged 2.42–2.51 GHz from run to run, and fps roughly follows it
(2.51 GHz: 55.0 fps; 2.42 GHz: 53.1). A change worth less than ~3% needs many interleaved pairs, or
a direct measure of the work it removes (time per call, per frame) alongside the frame rate.

## Android

| Change | Commit | Before → after |
| --- | --- | --- |
| Present guest frames on the display's vsync: the engine stamps each frame with the vsync it was latched on (engine `13b651b479`), and the presenter posts it at that time plus a steady latency, moved to just after a display vsync from Choreographer; it also asks for 60 Hz fixed-source content and turns off Android 15's touch boost | `daa3b042` | On a 120 Hz panel each 60 Hz guest frame has to land on the right one of two refreshes; posting on arrival handed the engine's ±2 ms jitter to SurfaceFlinger, which held ~30% of frames for one refresh or three: stutter at a full 60 fps. Pixel 10 at 120 Hz, Doodle Jump menu, SurfaceFlinger present gaps: **1232 at 16 ms, 273 at 8 ms, 270 at 24/25 ms (30% mis-paced) → 1739–1754 at 16 ms, 8–14 mis-paced (0.3–0.8%)**, 59.5–59.9 fps both. Spacing posts evenly without the vsync snap left 9%; the 60 Hz request alone left 27% (the Pixel keeps the panel at 120 Hz, which it counts as compatible). The Pixel 10 has no adaptive-refresh support (`hasArrSupport false`), so the presenter follows the panel's fixed vsync instead. |
| Run the panel at 60 Hz while LightTouch is in front (the window's preferred display mode, as Google Maps does) | `b33726c7` | With Smooth Display at 120 Hz the Pixel 10 kept 120 Hz for the 60 Hz fixed-source request alone; the preferred mode switches it to 60 (`renderFrameRate 60`). Doodle Jump menu, SurfaceFlinger present gaps: **1796 at 16 ms, 5 at 17, 9 at 33, none at 8 or 24** (vs 0.3–0.8% mis-paced with vsync snapping at 120 Hz); 59.8 fps, 99% of the time at ≥ 55 fps. |
| Release the 60 Hz cap and touch-boost lock while the Store or a dialog covers the guest | `21c96bba` | The cap applied to the whole window, so the Legacy Store scrolled at a fixed 60 Hz with no touch boost on 120 Hz phones and looked choppy (reported on the Pixel 10: `activeMode 60 Hz` with the Store open). Poco F3, the same 10 flings of the Store's results: panel 60 → 120 Hz, 651 → 1088 frames drawn, 99th percentile 17 ms both; back to 60 Hz on Done. |
| Ask for vCPU performance through ADPF (`APerformanceHint`, API 33+): each published frame reports the vCPU threads' CPU time against a 16.67 ms target | `44cfb147` | Pixel 10, JellyCar's live physics menu, same APK with ADPF switched off by `LT_ADPF=off` (three interleaved rounds): 5.7/5.5/6.0 → 13.2/13.4/13.3 fps (2.3×); screenshots confirm the same scene. No effect on the Poco: its LineageOS power HAL has no hint sessions (`dumpsys performance_hint`: `Hint Session Support: false`), and its 4.19 kernel has no uclamp, so an app has no portable substitute there. |
| Guest timer 4 at the hardware's 6 MHz instead of 10 MHz (Android only) | `e0d81871` | Interleaved A/B ×2 per phone: Poco animation 43.7/45.0 → 50.9/47.2 fps, frames on the 16.7 ms cadence 62% → 75%; Pixel 47.5/50.1 → 50.7/50.1 fps. Idle engine CPU −5 to −13%. Desktop keeps 10 MHz: an older desktop measurement found worse pacing there. |
| Bind the engine service with `BIND_IMPORTANT` | `25d12df7` | The `:engine` worker ran in the *foreground* cgroup: on the Pixel that excludes the X925 prime core and leaves the vCPU's cores near 400 MHz. Now it inherits the activity's *top-app* group. Pixel boot to lock screen 55.8 s → 20.9 s (same as the standalone engine); Poco 42.6 s → 37.4–38.5 s. |
| Deliver frames when the engine publishes them | `09381ce2` | Frames crossed two polling timers (the runtime publisher's 10 ms poll, then a fixed 16 ms sleep *after* each present), so present gaps were never under ~16.7 ms + work and frames were dropped. Now the engine's `qemu_ios_ui_attach` callback wakes the publisher and a new `lt_runtime_set_frame_listener` wakes the presenter. Home ↔ Spotlight: Pixel 29–32 → 50.2–50.6 fps, Poco 36–37 → 44.8–49.2 fps. |
| Keep engine threads on fast cores | `3509e0db` | Android parked the bursty vCPU thread on little cores. Engine threads now get affinity to CPUs with ≥ 50% of the maximum `cpu_capacity`. Standalone boot: Pixel 57–74 s → 19–20 s; Poco 55 s → 36–40 s. |
| Cheaper presenter scaling | `3509e0db` | The Android presenter's per-pixel nearest-neighbour upscale was rewritten to copy runs per source pixel and duplicate rows. |

### iPod touch 4G, iOS 6.1.6 (engine `dev/perf`, 2026-10-22)

Guest `n81ap-10B500` on the Poco F3 (`fcbench.py APK 30 n81-doodlejump`, `n81-jetpack`,
`n81-asphalt5-intro`), interleaved against the build before each step; desktop numbers are the headless
Linux frontend on the Ryzen 7 3700X VM. Before this work the Poco ran Doodle Jump's title at 25–35 fps,
Jetpack Joyride's at 13–21 and Asphalt 5's intro movie at ~4.

| Change | Commit | Before → after |
| --- | --- | --- |
| Empty the code cache at a new app's first frame (`LT_FLUSH_ON_APP=off` for A/B) | `b83cb1f003` | By the time a game starts the cache holds ~400 MB of boot and launch translations with the game's code spread among them, and its blocks that run a few times a frame miss in the host's caches (see "Jetpack Joyride, instruction by instruction"). Flushing once while the app is still loading packs its code together. **Poco, interleaved: Jetpack play 29.8/29.7/29.6 → 31.3/29.9/31.7 fps, frames slower than 30 fps 49% → 33%; Real Racing's attract demo 39.2/42.7 → 42.3/45.2; Flight Control 58.1 → 58.5 and Doodle Jump 59.9 → 59.7 (at its cap).** Desktop level. |
| Copy a GL present into the guest's surface on a worker thread (Android; `LT_PRESENT_ASYNC=off` for A/B) | `948df7f1b3` | The present waited for the GPU and copied the 2.4 MB frame into guest memory on the emulated CPU. Now the CPU takes a GPU fence and moves on; a worker waits and copies, and whatever reads the frame next (the scaler, the display, the game's next GL call) waits for it. **Jetpack Joyride: Pixel 10 present 1.26 → 0.28 ms a frame; Poco GL time on the emulated CPU 3.19 → 2.51 ms a frame.** About one frame in five, the game's next GL call still waits ~1.4 ms for the copy. |
| Keep each translated block's bookkeeping record away from its code (`LT_TB_META_APART=off` for A/B) | `8d2732ea4f` | QEMU stored a 256-byte record in front of every translated block, so the code a frame runs was spread over far more cache lines than it needs; the Poco spends most of its cycles waiting on instruction fetch. Code now grows up from the bottom of 1 MB chunks and the records down from the top (arm64 must reach a block's record within 1 MB). **Jetpack Joyride gameplay: desktop cycles per frame −14%, L2 instruction misses −20%; Poco 31.3 → 34.9 fps, frames under 30 fps 24% → 8%** (3 pairs each). |
| Move each block's memory-access slow paths (run only on a TLB miss) out of the hot code too (`LT_TB_COLD_STUBS=off` for A/B) | `41862bccc0` | They were about a quarter of a block's bytes and sat between one block's code and the next. They now go below the block's record in the same chunk, so every branch to and from them stays in range. **Jetpack Joyride gameplay: Poco 32.0 → 35.1 fps, frames under 30 fps 26% → 4%; desktop cycles per frame −3.6%, L2 instruction misses −7%** (3 pairs each). |
| Move each block's exception-restore data out of the hot code as well (`LT_TB_COLD_SEARCH=off` for A/B) | `4b2e4dafe5` | About 30 bytes a block, read only when a guest exception lands in the middle of a block; it now sits below the block's record. **Jetpack Joyride gameplay: Poco 34.0 → 35.2 fps, frames under 30 fps 16% → 6%; desktop L2 instruction misses about −4%** (3 pairs each). |
| Load and store multiple registers (push, pop) two words at a time (`LT_LDM_PAIR=off` for A/B) | `2f955da6a1` | Each word paid the full guest-TLB check (about 9 host instructions on arm64), and in the iOS kernel's frequently run code these instructions move more words than single loads and stores do. Pairs now go as one 64-bit access, still checked for 4-byte alignment and with each word atomic. **Jetpack Joyride gameplay: Poco 35.9 → 36.8 fps, desktop cycles per frame −2.8%** (3 pairs each). Letting 4-byte-aligned LDRD/STRD stay on the fast path made no measurable difference and was dropped. |
| Rejected: move each block exit's unchained path and far-jump fallback (12 of its 16 bytes on arm64) below the record too, leaving one branch inline (patch `perf/n81/rejected-cold-exit.patch`, `LT_TB_COLD_EXIT`) | — | Neutral within one APK: Poco Jetpack 39.1 → 39.3 fps (3 pairs). Unlike the slow paths and unwind data, these bytes sit after an unconditional branch at the end of a block, so they cost little cache. |
| Rejected: skip the per-instruction "can do I/O" stores and let a block's device access find its instruction by address instead (patch `perf/n81/rejected-io-by-pc.patch`, `LT_IO_BY_PC`) | — | No gain: Poco Jetpack, 3 pairs, the runs with it were not faster. The stores are about 4% of the host instructions, but they are cheap and never miss. |
| Rejected: translate without position-independent blocks (`CF_PCREL` off), saving the PC update on every chained block transition (patch `perf/n81/rejected-no-pcrel.patch`) | — | Correct (iOS maps its code at one address), but neutral: Poco Jetpack 36.3 → 36.6 fps, desktop within noise (3 pairs each). |
| Empty the code cache once more when an app has finished loading (`LT_FLUSH_SETTLE=off` for A/B) | `8c94663488` | The flush at an app's first frame leaves its loading code mixed in with the code that runs every frame. Once translation settles (16 MB since that flush, then 5 s under 1 MB/s), a second flush packs the steady-state code together; in Jetpack it lands about 30 s after the first frame. **Jetpack Joyride, 30 s of play: Poco 28.6/31.0 → 31.2/39.7 fps (2 pairs; a fixed-time flush in the menu or in play gave 29.3 → 31.4 over 3 pairs); desktop cycles per frame −4%, IPC 1.01 → 1.06 (3 pairs).** It costs one retranslation burst, which did not show among the worst frames. |
| Paced audio reaches the I2S FIFO in one copy instead of one register write per sample (`LT_I2S_BULK=off` for A/B) | `a363209dfe` | Each 44.1 kHz stereo sample went through the memory system with the big lock held, about 44,000 times a second, roughly 8% of the main loop's time. Poco Jetpack: main loop −9% CPU, fps unchanged (41.5 vs 41.5, 3 pairs). Pixel overlay: lock wait 1.0 → 0.4 ms a frame. Captured audio is identical on the iPod touch 4G and 2G. |
| Rejected: answer an idle GL context's finish, flush or error query without switching the host to it (patch `perf/n81/rejected-gl-quiet.patch`, `LT_GL_QUIET`) | — | backboardd finishes its context once a frame, and the switch there and back cost the Pixel's vCPU about 4%. Skipping it only moved that cost: the GPU driver now submitted the game's frame at the present's fence instead (GL share of the vCPU 15.1 → 14.5%). Pixel Jetpack title 42.0/40.5 vs 43.5/45.0 fps, Poco unchanged. |
| Guest GL calls run on a thread of their own (`LT_GL_THREAD=off` for A/B); the app's performance hint covers that thread too | `57fb8a115f` | The GPU driver's work for each GL call ran on the emulated CPU's thread, about 15% of its time in Jetpack Joyride and more in Epic Citadel. A worker thread now runs the calls. Calls that only set state or draw are queued and the game goes on; the vCPU copies the guest memory they need first. Calls that return something still wait. The idle-context finish rejected above is included, together with ES 2.0 queueing; Jetpack on the Pixel is the same with and without the two (48.3/48.0 vs 47.6/50.2), while Epic Citadel, an ES 2.0 game, has 98.6% of its calls queued only with the ES 2.0 part. Epic Citadel: Pixel 32.0/33.5 → 37.1/39.4 fps, Poco 13.4/13.4 → 27.5/26.6. Jetpack: Pixel title 45.0/42.6 → 50.8/51.4 (about 80% of frames at 16 ms), Poco title 36.7/37.7 → 48.5/45.5. Desktop unchanged: its present is a synchronous readback. SpringBoard, Jetpack, Epic Citadel and the iPod touch 2G render as before. |
| ↳ The present is queued on the GL thread too (`LT_PRESENT_QUEUE=off` for A/B) | `429e0a3551` | The present was the one call a frame the emulated CPU still waited for, while the GL thread finished the frame's queued calls (about 0.6 ms a frame on the Poco). The emulated CPU now checks the present and finds the surface's memory itself, then moves on. The display and scaler still wait for a frame before they read it. Draws that may re-read an IOSurface now wait, because that check walks the emulated CPU's page tables. Jetpack title: Poco 50.0/52.9 → 52.9/55.8 fps (frames at 55 fps or better 23/37% → 52/62%), Pixel 50.8/49.6 → 53.9/54.7 (87% of frames at 16 ms). Epic Citadel: Pixel 40.1/37.4 → 42.2/41.0, Poco level. |
| The M2 scaler's modelled transfer time is 1 ns a pixel instead of 5 (`LT_SCALER_PS_PER_PIXEL`) | `1f5b965e33` | CoreAnimation turns every frame of a landscape game with the M2 scaler, and backboardd swaps the frame only once the transfer is done. The model held the done interrupt for 3 ms (640×960), an estimate from when it moved out of the start write. A guest trace on the Poco put 109 of 434 transfers past the next screen refresh, and each one cost Jetpack Joyride a whole frame. At 0.6 ms the interrupt still comes well after the driver code the earlier fix protects. Poco Jetpack title 51.9/51.5 → 55.0/55.9 fps; Pixel unchanged. |
| ↳ A second thread does half of each large scaler transfer (`LT_SCALER_SPLIT=off` for A/B) | `f924ae1b67` | On the Pixel 10 the turn itself took 3.3 ms of a mid core and was often still running at the refresh. Now two threads each read half the source and turn and write half the destination. Jetpack title: Poco 50.2/51.0 → 54.8/56.4 fps (frames at 55 fps or better 23/29% → 57/75%); Pixel 33 ms frames 119/112 → 77/62 in 20 s (92–94% of frames at 16 ms). |
| Rejected: a tighter performance-hint target (10 ms instead of 16.7 ms) on the Pixel | — | The vCPU's average clock rose from 2.55 to 3.07 GHz but Jetpack's title stayed at 43 fps: frames are not limited by the clock. |
| Build the Android engine with profile-guided optimization (`scripts/build.sh`, profile from gameplay; see `docs/BUILDING.md`) | — | Clang groups the functions and branches that gameplay runs and moves the rest out of the way, so the engine's own code, about a quarter of the phone's instruction-fetch stalls, fits the caches better. **Jetpack Joyride gameplay: Poco 37.1 → 40.2 fps (+8%), frames under 30 fps 4% → 2%** (3 pairs). The shipped profile is Jetpack weighted 3× plus one run each of Jetpack, Doodle Jump, Real Racing, Flight Control and Epic Citadel: the five-game profile alone gave Jetpack about 2.5% less than Jetpack's alone (38.4 vs 39.4 fps), the weighted one 40.1 vs 40.9 (within the spread), and Real Racing's attract demo was the same with either (fcbench's window there now includes a scene load, so compare the share of 60 Hz frames: about 93% with any of these builds, 68% before today's changes). |
| Rejected: link-time optimization on top of the profile (`LT_ENGINE_CFLAGS=-flto=thin LT_ENGINE_LDFLAGS=-flto=thin`) | — | Neutral: Poco Jetpack 40.7 → 40.6 fps (3 pairs). |
| Rejected: build without QEMU's object-cast checks (`--disable-qom-cast-debug`) | — | Neutral with the profile applied: Poco Jetpack 40.5 → 39.9 fps (3 pairs). |
| Not taken: a fresh five-game profile (Jetpack 3×) from the GL-thread and scaler-split tree, in place of the shipped one | — | About even: **Poco Jetpack title 54.1/56.2 → 55.2/56.4 fps (ABBA), Epic Citadel 29.8 → 28.9 (one pair)**. The shipped profile stays. |
| Scaler: NV12 → NV12 (a movie frame turned upright) from index tables, not two 64-bit divides a pixel | `83a50a5c4d` | **Desktop, Epic Citadel's loading movie: `scaler_write` 8.3% of the vCPU → under 1%.** Poco, Asphalt 5 intro: neutral (15.9/15.3 → 15.5/14.0 fps); arm64's divide is cheap. |
| NAND: CDMA moves a run of H2FMI FIFO words in one call instead of an MMIO dispatch per word (`LT_CDMA_BULK=off`) | `bec13dcdbf` | The A4's NAND goes through the IOP's firmware, whose CDMA channels fed the flash controller's FIFO 4 bytes per register access: ~1.5M MMIO dispatches/s on the IOP during file I/O, contending with the vCPU for host time. **Desktop: IOP MMIO 1.5M → 0.25M/s. Poco: Jetpack, two pairs, 20.8/21.4 → 23.4/22.1 fps; Asphalt 5 upload 75 → 58 s (see App installs).** |
| A sequential run of stores into a code page invalidates to the page end at once (`LT_SMC_STREAM=off`) | `9eeedfb270` | Recycled code pages are zeroed or copied a word at a time, and each word took the not-dirty slow write and a TB lookup: ~30K/s at Jetpack's title. After 8 stores in a row (up or down) the rest of the page is invalidated in one go. **Desktop: code-page slow writes ~30K → ~500/s, their host time 6 → 0.9%.** Poco: neutral within the spread. |
| Keep the last movie frame across VBLs; convert a movie smaller than the panel at its own size, then scale | `0cdfe20796` | A movie decodes at ~30 fps against a 60 Hz VBL, so every other latch converted and blended the same frame again. **Desktop, Asphalt 5 intro: 23.0 → 31.6 fps (the movie's own rate); VBL tick 9.8 → 4.5 ms.** |
| Compose a movie frame without the BQL, a table lookup per pixel instead of a divide | `c441e15853` | The video layer was converted (a 64-bit divide per pixel) and blended under the BQL in every VBL: the vCPU waited for the lock 43.6% of wall time. **Desktop: BQL wait 43.6 → 4.0%, movie 15.4 → 23.0 fps.** With the gyro fix, **Poco, Asphalt 5 intro, two pairs: 3.8/4.9 → 16.5/16.1 fps** (median 4 → 20, ceiling 10–12 → 30). Its Done button now responds (the first launch shows a push-notification alert over the movie; OK it first). |
| Gyro: set the FIFO watermark flag above the watermark, not at it | `b1e30b672a` | With the level at the watermark the L3G4200D model held INT2 up, so CoreMotion (running the gyro during the movie) read FIFO_SRC ~2.8K times/s, each a PMGR clock-gate read-modify-write and a GPIO ack under the BQL. **Desktop: PMGR accesses 50K → 8K/s, movie 11.3 → 15.4 fps.** |
| Scaler: rotate RGB frames in 32×32 blocks from index tables, without the BQL | `7c94d7c243` | CoreAnimation turns each landscape frame 960×640 → 640×960 through the scaler, which converted it a pixel at a time, strided, under the lock. Desktop `scaler_write` 4.1 → 3.4% of wall. Measured with the next row. |
| softfloat: keep exact zeros (x × 0, x − x) and NaN/Inf operands in default-NaN mode on the hardfloat path | `c9fa361f20` | AudioToolbox's speaker protection (`AUSpeakerProtectionKernelT<float>::Process`) feeds NaN/Inf through ~1.75M float ops/s, and each fell to the slow software path. Same results as before on 39 936 differential cases and `fp-test -l 2 -r all`. **Desktop, Jetpack: `soft_f32_*` 3.0 → 0.16% of the vCPU.** With the scaler and the rows below, **Poco, Doodle Jump, two pairs: 35.5/40.9 → 49.3/44.8 fps (ceiling 44–46 → 54–60); Jetpack 13.1/13.8 → 17.6/21.5.** |
| Arm each VBL a period after the last one, not after the tick's work (`LT_VBL_DEADLINE=off`) | `00af4441d0` | The tick's own work (milliseconds) was added to every 16.7 ms period, so the panel ran at ~50 Hz and capped games at 48 fps (the 2G's LCD already re-armed from its deadline). **Desktop, Doodle Jump: 48.1 → 55.1 fps.** |
| No hflags rebuild for a mode switch within an EL on boards without EL2/EL3 | `123b6da3c2` | iOS 6's exception vectors switch modes several times per syscall. **Desktop LT Bench `syscall` 2040 → 1725 ns.** |
| PMU: no EL-change hook work while PMCR.E is clear and there is no EL2 | `4bbcdff605` | Two hook calls per exception entry and return that count nothing. **`syscall` 2550 → 2040 ns.** |
| Keep the user TLB across an ASID switch until user code runs under another ASID (`LT_EL0_TLB_DEFER=off`) | `3178e8cc16` | iOS 6 separates the kernel's address space: every syscall and IRQ from user mode loads the kernel's TTBR0 with CONTEXTIDR 0 and restores both on return. Each write flushed the user TLB, which the kernel does not use (except LDRT/STRT, which now sync it first). The flush is now done when hflags are rebuilt for EL0 under another ASID, or by a TLBIASID naming the ASID it holds. **`syscall` 3900 → 2550 ns.** |
| Test a pending interrupt against the CPU's masks before taking the BQL (`LT_MASKED_IRQ_FAST=off`) | `c830044470` | With a device line up and IRQs masked, every TB exit took the BQL only to find nothing to deliver. Desktop: interrupt-site BQL takes 11.8K → 1.2K/s. |
| A display refresh with the latched frame skips the live compose | `44ef6d0a4f` | Desktop vCPU BQL wait 5.2 → 2.6% of wall. With the five rows above, **Poco, Doodle Jump: 32.7 → 39.8 fps** (one pair; a second landed on the Camera). |
| No `CF_PARALLEL` for CPUs the board marks `tcg_unshared` (`LT_TCG_PARALLEL=on` restores it) | `dc8e9d5526` | The A4 and S5L8920 boards have two vCPUs (the application processor and the IOP), so every TB was translated for parallel execution: barriers became host fences, exclusives cmpxchg, and the per-site branch cache (`6c789b3e83`) was off. The cores share no code or atomics. **Desktop, Doodle Jump: TB lookups 1.67M → 0.40M/s.** With the two rows below, **Poco, Doodle Jump, two pairs: 33.4/24.9 → 42.8/39.1 fps, hitches 67–74 → 5–7 per 30 s.** |
| Compose the latched frame without the BQL | `a4547e958a` | Every swap the VBL tick read both UI layers (~5 MiB at 640×960), compared and blended them under the lock; the 2G's LCD had the same problem (`d7c4927685`). |
| Guest GL calls without the BQL on the A4, S5L8920 and iPod 1G boards | `1424d3ccb3` | `76ace3a65b` covered only the 2G. On the 4G the vCPU waited 6.6% of wall time at that site (55 µs mean, ~1.2K calls/s at Doodle Jump's title). |
| Raise the display's frame end (swap done, VBL) before the latch compose (`LT_VBL_IRQ_FIRST=off`) | `b19afd3b6e` | The VBL tick copied and blended the new layers (2–3 ms a frame at 640×960, up to 7) before telling the guest its swap had completed, and the vCPU then waited for the BQL the rest of the tick held to take the interrupt. **GPU host, Jetpack's title: vsync to swap-done 1.1 → 0.2 ms, 45.0/39.5 → 45.7/44.9 fps.** |
| Fire a due PMGR event timer when the guest reads the timebase past it (`LT_PMGR_SYNC_EXPIRY=off`) | `8434d63525` | The timer expired on the main loop tens of µs after its deadline; meanwhile iOS 6's `cpu_idle` saw the deadline passed (`SetIdlePop`) and returned without a WFI, ~10–12K times a second for ~150 WFIs. Now ~118 idle entries/s, almost all WFIs. Timer FIQs 853 → 1596/s: the ack re-arms with iOS's 10-tick minimum count, which now expires as on hardware. Desktop Doodle Jump vCPU 57.7 → 56.8% (one clean pair); **Poco Doodle Jump 55.9/49.3 → 56.4/53.8 fps, Jetpack 25.1/25.2 → 26.0/24.9 (neutral).** |
| Copy the latched frame to the console and publish it without the BQL (`LT_FRAME_COPY_UNLOCKED=off`); the timebase read fires a due timer only if the BQL is free | `b5005f79f4` | A Pixel 10 Jetpack profile: main-loop holds 1 ms mean, 7.5 ms worst; a vCPU waited up to 6.6 ms in the synchronous expiry. Desktop Jetpack (holds and waits ≥0.5 ms, 65 s of play): holds 228 → 93 ms, vCPU waits 440 → 213 ms (worst 10.4 → 3.8 ms); fps is llvmpipe-bound there. **Poco (hot, 38 °C), interleaved: Jetpack 21.3/21.4/22.3 → 24.2/24.7 fps (one more "on" run had a 1 s stall), Doodle Jump 41.7/43.8/53.7 → 55.8/56.6/58.0.** |
| Headphones in the iPod touch 4G's jack (`headphones=on`, the Android app's default; `LT_HEADPHONES=0/1` overrides) | `e3ece88a47`, `bbef4e86a1` | On the speaker route iOS runs speaker protection, EQ and a limiter on every sample (mono, bass cut), about 9% of the vCPU's samples in float helpers. The modeled jack detect (CS42L59 0x33/0x34/0x3a, interrupt on GPIO 0x0b) routes to Headphones: those disappear and sound is stereo. Proxmox Doodle Jump vCPU busy 59.3 → 50.9% (3 pairs). **Poco Jetpack, through the app's setting: 23.9/23.9/23.7 → 25.6/25.5/27.1 fps** (Doodle Jump at its cap either way). |
| float32 add/sub/mul stay on the host FPU while inexact is clear, by testing the result for exactness (`LT_HARDFLOAT_EXACT=off`) | `6dbde37800` | The hardfloat shortcut needed inexact already raised; exact guest math (silent buffers, small integers) never raises it, so ~85% of float32 adds went to softfloat (13 → 4.5 ns an op). 200M-case differential test and Berkeley fp-test clean. Desktop Doodle Jump (speaker route) vCPU busy −2.4 points, instructions −5%; 2G neutral. |
| PMGR timebase ticks without `muldiv64`'s 128-bit divide (both A4 and S5L8920) | `5bac54ee8a`, `ad6f571d54` | ~46–64K timebase reads/s at Doodle Jump, each paying `__divti3`; 24 MHz is 3/125 ticks per ns. Within noise on its own. |
| Neon float add, subtract, multiply and multiply-accumulate on host vectors when the result is exact (`LT_NEON_HOST_FLOAT=off`) | `2a16bb0665` | The Neon helpers ran softfloat per lane (speaker protection and games' matrix code issue them all the time). The vector path is taken only where it gives softfloat's bits: round to nearest, inexact already raised, operands and results normal or zero, NaNs handled as default NaN, the multiply rounded before the add. D-register operands are built a lane at a time: an 8-byte copy into a 16-byte vector stalled its load and made the first version slower than softfloat. **Desktop, Jetpack's title: float helpers 12.1 → 8.9% of the vCPU.** |
| One host clock sample per timebase read: the TICKS_HI read right after TICKS_LO answers from LO's sample | `3f36d251bf` | The kernel reads HI, LO, HI; with LO's due-timer check that was four host clock reads a timestamp, ~20K timestamps/s in Jetpack. A clock read costs ~200 ns on the Poco (`clock_gettime`; the raw counter ~160 ns), and `__kernel_clock_gettime` was 2.5% of its vCPU. Desktop Jetpack vCPU 63–64 → 62.3% (one run). |
| Scaler work buffers kept between transfers | `81376f17aa` | CoreAnimation turns every frame of a landscape game (Jetpack: 960×640 → 640×960) through the scaler, which allocated two 2.4 MiB buffers per transfer; on Android each was a fresh mapping (`munmap` 1.4% of the Poco's vCPU, plus page faults). With the row above, **Poco Jetpack 28.0/27.3/27.6 → 29.3/29.7/29.1 fps** (3 pairs; its cap at this cadence is 30). |
| A scaler transfer's pixel work on a worker thread, the done interrupt waiting for it too (`LT_SCALER_ASYNC=off`) | `602fa5c393` | The rotate ran on the vCPU inside the start write. Desktop Jetpack vCPU busy 61.8 → 56.9%; **Poco 29.4/28.9/28.9 → 29.8/30.6/29.7 fps at 92.1/90.3/91.2 → 88.5/89.5/89.0% busy** (3 pairs); **Flight Control 50.6/50.7/52.7 → 52.8/53.9/54.0** (phone at 38 °C). |
| TLB resize window on the coarse clock | `3a89b0b15d` | Every full flush read the host clock (~200 ns on the Poco; coarse ~12 ns) for a 100 ms window. **Poco Flight Control 55.7/52.7/55.3 → 54.5/50.9/53.6 fps (noise).** |
| Page and range flushes skip clean TLB indexes (`LT_TLB_FLUSH_DIRTY_ONLY=off`) | `3a89b0b15d` | A clean index holds nothing, but at minimum size a page flush flushed it whole: ~15K full flushes/s of empty tables on iOS 6. Desktop Jetpack vCPU 62.0/56.6 → 58.0/52.4%, 1% low 19.0/20.1 → 25.5/26.4. **Poco no gain: Jetpack busy 87.8/86.4/88.3 → 88.5/88.6/87.5%, Flight Control 57.1/56.1/54.7 → 56.6/54.3/53.6 fps.** Kept. |
| Per-ASID user TLB tables (`LT_TLB_ASID_BANKS=off`), page-walk TLB kept across ASID switches (`LT_TLB_KEEP_PHYS=off`), dirty-tracking resets queued for parked tables | `3daee348ab`, `67e9f0ecee` | A process switch emptied the user index (~700/s in Jetpack, ~109 entries each): 80K user refills/s, now 13K. 8 tables by ASID, LRU; every invalidation reaches the parked ones, a returning table is resized, and `tlb_reset_dirty` queues its range for a parked table instead of walking it under the lock (what sank the 2026-10-18 attempt below). The physical-address indexes refilled ~26K/s for nothing. Desktop Jetpack vCPU 54.1 → 53.0%. **Poco Jetpack busy 86.5/88.5/91.9 → 84.0/87.6/84.3%; Flight Control 54.5/56.6/52.5 → 56.5/57.5/57.3 fps** (3 pairs each, APK without the queued resets; the 4G resets ~0.1/s in both games). |
| GL requests trust the thread's record of its current EGL context (`LT_EGL_CURRENT_CHECK=on` asks EGL again) | `a1d63b3284` | `CGLSetCurrentContext` asked `eglGetCurrentContext` on every guest GL request; only it binds contexts on the host. Profile share on the Poco 2.7% of Jetpack's vCPU. **Poco Jetpack busy 84.8/86.2/84.6 → 82.1/85.7/84.8%; Flight Control 57.7/57.0/57.3 → 57.5/57.4/57.3 fps.** |
| No hflags rebuild after CP15 c2/c7/c8/c13 writes (`LT_CP_HFLAGS_SKIP=off`) | `7f34ed45ba` | No A32 TB flag reads the translation-table registers, cache/TLB maintenance or the process and thread IDs, which iOS writes on every context switch; the TB still ends. Profile share ~1%. **Poco Jetpack busy 84.3/83.6/85.7 → 79.2/83.7/81.3%; Flight Control 56.8/56.8/57.6 → 57.5/57.3/57.5 fps.** |
| Return address stack (`LT_RAS=off`) | `7765a1ac09` | ~95% of the indirect-branch site cache's misses were returns. Calls push their TB's slot on a 32-entry ring; a return pops it and jumps straight to the cached host code when key and state signature match. Desktop Jetpack: TB lookups 610K → 190K/s, 31.1/31.0 → 32.9/32.1 fps at 50.7/51.9 → 50.1/50.8% vCPU. What is left is mostly the same target under a renewed context: iOS issues ~34 TLBIALL/s, each emptying every jump cache and site cache (a 2-entry slot did not help). **Poco Jetpack busy 84.8/86.4/85.4 → 86.7/83.0/82.6%; Flight Control 57.2/57.6/55.5 → 57.2/56.8/57.8 fps** (noise-level there). |
| A32 coprocessor accesses carry their register info as a constant (`LT_CP_RI_CONST=off`) | `d21f6af351` | Generated code looked the register up in a hash table on every access (and again in the access check), 0.6–0.8% of the Poco's vCPU in Jetpack. Desktop Jetpack vCPU 50.4 → 49.8% (one pair). **Poco Jetpack busy 83.6/85.0/84.3 → 84.5/86.3/84.9%; Flight Control 57.6/57.3/57.4 → 57.3/57.4/59.6 fps** (no measurable change; kept, the lookup it drops shows in the profile). |
| CPSR, SPSR, SRS and CP15 c2/c7/c8/c13 writes chain to the next instruction instead of returning to the main loop (`LT_PSR_CHAIN=off`, `LT_CP_CHAIN=off`) | `fcec753a68` | iOS restores the CPSR at every lock release, switches modes four times per syscall entry and writes TTBR0 and CONTEXTIDR at every kernel entry and exit; each ended the TB with a main-loop return and a full lookup. Jetpack gameplay: ~167K → ~48K main-loop returns/s (the rest are exception entries and returns and FPEXC writes). A CPSR write that changes the TB flags or unmasks a pending interrupt still leaves, before the chain. Desktop LT Bench, two pairs: `syscall` 1524/1587 → 1183/1242 ns, `time` 930/993 → 787/762, `thread-switch` 28.4/31.0 → 25.7/25.3 µs, `stm-unaligned` −20%, `mmap` −10%. **Poco Jetpack busy 85.4/84.1/85.6 → 83.7/85.2/83.5%; Flight Control 57.5/54.5/56.5 → 56.7/57.4/57.1 fps; Jetpack gameplay (fcbench `n81-jetpack-play`) 28.9/28.1/29.3 → 31.1/29.3/29.1 fps.** |
| A guest `glFinish` flushes; the surface readback is the wait (`LT_FINISH_FLUSH=off`); the writeback's read buffer is kept (`LT_WRITEBACK_SCRATCH=off`) | `178b771d8a` | CoreAnimation's compositor finishes every frame. The host ran `glFinish`, then read each dirty surface back with `glReadPixels`, which waits for that work anyway: a second full GPU wait on the vCPU, ~1.1 ms a frame on the Pixel 10 (PowerVR), 4.5 ms under llvmpipe. **Desktop Jetpack play 43.3/45.7/46.9 → 48.3/48.2/47.4 fps; Poco 29.1/29.8/28.7 → 29.1/28.7/29.5** (Adreno's finish was already short). Pixel 10 (r8): no change, the readback now waits the same ~1.0 ms a frame. |

LT Bench on the desktop, best ns/op before → after these: `syscall` 4212 → 1746, `time` 1708 → 907,
`thread-switch` 53.0 → 36.6 µs, `thread-switch-64p` 87.0 → 38.4 µs, `rpc` 194.6 → 139.9 µs, `mmap` 13.2 →
7.8 µs, `gl-sprite` 21.7 → 15.2 µs. `rpc` is ~25× the 2G's because iOS 6's launchd runs a sandbox check
for each lookup from a sandboxed app: guest work, not emulation.

Jetpack's title is now ~23–24 fps on the Poco (median 24; it is vsync-quantized, so a vCPU gain shows only
once a frame drops under 33 ms; desktop ~27–28, vCPU-bound with 1.5% BQL wait). Its guest
time is ~30% kernel, spread thin; ~10% AudioToolbox speaker protection and its NEON float; 6% the game;
2% QuartzCore. Speaker protection runs only on the speaker route: with a headset reported plugged in
(CS42L59 register 0x33 bit 7, read by `AppleCS42L59Mikey`) iOS would skip it, but that changes what the
guest believes about its hardware, so it is an option to weigh, not a default.

Doodle Jump on the desktop (60 fps at ~57% vCPU busy; the Poco needs ~20% less work per frame for 60): the game
23% of a core, backboardd 21%, mediaserverd 10% (`it_ps`). Kernel episodes are 41% of the busy time. The guest idles
~8.8K times/s, each entry re-arming the PMGR decrementer; it reads the timebase ~46–64K times/s (three MMIO reads per
`mach_absolute_time`). Each frame's `kern_SwapEnd` (io_connect_method selector 5 from backboardd, ~520 µs) is mostly
IOMobileFramebuffer building an IODMACommand over the whole surface: `getPhysicalSegment` ~560 times a frame, Apple's
code that a real 4G runs too. The present copy of the GL frame into the guest surface is 3.3% of the vCPU. Setting
codec reg 0x33 bit 7 alone does not change the route: the driver powers the speaker path down and starts headset-type
detection, then gives up waiting for the codec's interrupt, which the model does not raise.

### App installs (2026-10-22)

Asphalt 5 (281 MB IPA, 4510 files, 410 MB unpacked, one 308 MB file). installd reports nothing while it
unpacks, so the Poco sat on ExtractingPackage for ~10 minutes and the helper's timeout fired first.

| Change | Commit | Before → after |
| --- | --- | --- |
| Unpacking progress from the guest's AFC free space every 2 s, against the zip's uncompressed total; any change counts as activity for the 90 s idle limit; the overall limit is 600 s + unpacked size at 256 KiB/s | `2ddb6026` | The desktop window and the Android Store show "Unpacking · X MB of Y MB" with a real bar. |
| Uploads write 1 MiB per AFC packet instead of 64 KiB | `2ddb6026` | libimobiledevice sends one packet per write and waits for the guest's reply (~20 ms on the 2G, whose AP is ~90% idle during an upload). 4 MiB is no faster. **2G desktop upload 137 → 73 s.** |
| NAND FIFO runs in one call (engine) | `bec13dcdbf` | See the iPod touch 4G table. |
| NAND pages read and programmed with pread/pwrite on the page files instead of the block layer's thread pool (engine, `LT_NAND_DIRECT=off`; not on Windows) | `b4db28d554` | Each page went through a block-layer worker with the IOP waiting: reads 31–42 → 3–4.5 µs, programs 41–45 → 7–17 µs, engine write syscalls ~45K → 7–10K/s. The stop handler fdatasyncs the page files before publishing ownership; a SIGKILL mid-unpack left 0 orphaned pages and recovered. **Infinity Blade II (1.12 GB IPA, 1.34 GB unpacked), desktop: unpack 170 → 101–108 s, install 300 → 215–223 s; GPU host (interleaved): 229–238 → 195–213 s.** |

| Asphalt 5 install | Before | After |
| --- | --- | --- |
| iPod touch 4G, Poco | upload 75 s, unpack ~10 min (timed out) | upload 34.5 s, unpack ~165 s, sandbox setup ~30 s: done in 240 s |
| iPod touch 4G, desktop | upload 50 s, unpack 126 s | upload 31.5 s, unpack 98 s |
| iPod touch 2G, desktop | upload 137 s | upload 73 s, unpack ~148 s |

The 4G unpacks small files at ~1 MB/s and the large one at ~5.5 MB/s. The 2G's unpack is CPU-bound in
the guest (AP ~90%, jump-cache misses ~10%). A remaining 4G cost is the IOP round trip per NAND request
(~0.5 ms desktop, ~1.6 ms Poco): about 7 IRQs per request and the IOP's cache-control MMIO, not wake latency
(a spinning IOP did not help).

### Fixes found while measuring the iPod touch 4G (2026-10-22)

| Fix | Commit | What it was |
| --- | --- | --- |
| A4 NAND overlay ownership reaches the `.dirty` file at once (mapped shared on POSIX) | `b704f259fa` | Ownership was written only at a clean stop, so a killed engine left pages first written that session reading the base again: the Poco's 4G record then panicked every boot (`yaFTL.blockArray[oldBlock].validPagesDNo`), which looked like games hanging (Flight Control's menu, Jetpack's splash). Desktop, Asphalt 5 install killed mid-unpack: orphaned overlay pages 140814 → 0; the next boot runs games (before: stuck at the lock screen). Unpack rate unchanged (4.17 → 4.11 MB/s; a write per page instead cost 14%). |
| Native GLES path: packed depth/stencil texels are 4 bytes | `d3768ae49c` | Epic Citadel's 960×640 `GL_DEPTH_STENCIL_OES` upload was copied at 1 byte a texel and Adreno read past the copy: the engine died after the logo (the "green screen"). It now runs, ~6–8 fps on the Poco. |
| PVRTC uploads to cube-map faces | `a51635216a` | Epic Citadel's environment map was refused (six faces). |

### Engine (`qemu-ios`, branch `dev/perf`)

| Change | Commit | Effect |
| --- | --- | --- |
| The display's vsync latch waits for the GL thread's presents without the BQL (`LT_LATCH_WAIT_UNLOCKED=off` for A/B) | `d20877104e` | The latch waited for every issued present to reach guest memory before planning its layers, with the BQL held, so the vCPU waited as long as the GL thread took to get through the game's queued calls: in Epic Citadel's guided tour on the Poco, `vbl_tick` held the lock for 2 ms or more on ~5% of vsyncs, up to 20 ms. Measured with the next row. |
| AMC (hardware audio decoder): libavcodec decodes without the BQL (`LT_AMC_DECODE_UNLOCKED=off` for A/B) | `7ba03a85fd` | A game's music decoded up to 64 KiB of PCM at a time on the main loop with the BQL held, up to 6.5 ms. **With the row above, Poco, Epic Citadel guided tour, last 120 s of an engine profile, both off → both on: vCPU BQL waits of 0.5 ms or more 2.01 → 0.78 s (1474 → 794; 4 ms or more 28 → 7), frames of 50 ms or more 820 → 679, 27.9 → 29.4 fps, p99 frame 67.5 → 67.1 ms** (a hot phone, ~1.9 GHz). Plain fcbench pairs with the latch change alone (phone at 47 °C): 30.2/30.8 vs 30.0/30.0 fps, 1% low 15 either way, hitch time 11.9/11.2 vs 12.4/12.4 s per minute. |
| Scaler: an exact 1:1 quarter turn goes through a 4×4 NEON transpose and is written to the guest a 32-row band at a time (arm64 hosts; `LT_SCALER_TURN4=off` for A/B) | `d58ecd7e9c` | CoreAnimation has the scaler turn every frame of a landscape game and backboardd swaps only once the turn is done, so a turn still running at the vblank loses the game a refresh. A turn of 640×960 took the Poco 2.8 ms at the median (p90 4.3) against the 0.6 ms the model allows; the per-pixel offset tables are gone for the 1:1 case, about a third less time on one mid core (1.6 → 1.05 ms with the copies). **Poco, Jetpack title, 8 interleaved 60 s runs at the same clock (1.76–1.83 GHz): 51.6 → 55.7 fps (49.8–53.4 → 55.0–56.6), frames shown after two refreshes 16.2% → 7.7%, time at ≥55 fps 26% → 66%.** The 1% low stays 28–29: the late frames left are still more than 1%. Other geometries and x86 hosts (no faster there) keep the old path. **Pixel 10, the user's hitch bundles of the same title (one session each, not interleaved): 50.1 and 53.3 fps with 19.4% and 11.9% of frames ≥25 ms (engine `7ba03a85fd`) → 56.7 fps, 5.8%, worst frame 40 ms (was 69 and 172), 1% low 27 → 28.4.** |
| ↳ The turn reads and writes the surfaces in guest RAM directly (arm64 hosts; `LT_SCALER_DIRECT=off` for A/B) | `e15a9a95e2` | The turn copied the source in (0.8 ms on a Poco mid core) and its result out through the bus a page at a time. When both surfaces are guest RAM it now works on them in place and marks the written pages dirty. **Poco, Jetpack title, ABBAAB 60 s at the same clock (1.77–1.82 GHz): 55.7 → 57.1 fps, frames shown after two refreshes 7.4% → 4.8% (every direct run below every copy run).** 1% low 28–29 → 29. |
| PL192: an input call that leaves a line's level unchanged does nothing (`LT_PL192_SAME_LEVEL=off` for A/B) | `56505fbfdb` | The iPod touch 4G's CDMA re-asserts its unchanged interrupt level on every register write, thousands per storage request on the IOP; each ran a full priority sort and lowered the CPU's FIQ, ~20% of the IOP thread during reads. Desktop, 4G 6.1.6, LT Bench, three interleaved pairs (best, on vs off): `fs-read` 14.4/15.5/13.1 vs 19.0/15.9/15.6 µs/KiB (~15%), `fs-read-16k` 661/837/601 vs 902/777/754 µs (~14%), IOP time per request 122/173/123 vs 183/151/150 µs. |
| iPod touch 2G flash: a transmit's busy bit clears at once (`LT_FMSS_TX_DONE=off` for A/B) | `3b4f4a67c3` | The stock write program polls FMC 0x40 bit 0 after loading each page's spare bytes; the model never cleared it, so every write program ran to the 100 000-step limit (~4 ms on the vCPU, ~250 times a second while iOS writes). Desktop, 2G 3.1.3, LT Bench: `fs-write` 381 → 148–198 µs/KiB, storage pass 19.1 → 12.1 s, data intact after a reboot. Not yet measured on a phone. |
| `glDrawElements` fetches only the vertices between its smallest and largest index, with the indices rebased (`LT_GLES_INDEX_RANGE=off` for A/B) | `f4eeeab129` | The fetch ran from vertex 0 to the largest index, a bug from the first GLES bring-up. Asphalt 5 keeps a whole track in one array and makes ~125 draws a frame that reach ~70 of its ~15 000 vertices: ~173 MB of vertex data a frame was copied (a third of it widened from `GL_FIXED`) where ~1 MB was needed, and the Adreno driver allocated and filled GPU memory for all of it per draw (`kgsl` ioctl, mmap/munmap). **Poco, Asphalt 5 Aspen race (`fcbench.py APK 30 asphalt5`): 8.2–8.6 → 24.8 fps; GL 54–66 → 5.4 ms a frame.** |
| Spin up to 150 µs before vCPU 0 sleeps on a contended BQL. On by default on Android; `LT_BQL_SPIN=off` turns it off, `LT_BQL_SPIN_EXTENDED=off` gives the first 50 µs version, and `LT_BQL_SPIN=on` turns it on elsewhere | `23041226e7`, `5547634eeb`, `b5708085da` | A short BQL wait that sleeps pays the host's wakeup, which on the Poco is often a millisecond or more. Spinning first acquires the lock in ~94% of contended takes with 50 µs and ~99% with 150 µs. Other threads, guest time, IRQs and audio are unchanged. **Poco, Jetpack title, three interleaved 60 s pairs (one reversed), off → 50 µs: 53.62/53.30/53.35 → 57.57/57.68/57.17 fps (53.42 → 57.47, +7.6%, 3/3); ceiling 58 → 60, median 54 → 58, low 48–50 → 52–54, 1% low 28.0–28.2 → 29.0–29.2, hitches 30 → 8 per 180 s (1570 → 411 ms).** A separate session, 50 → 150 µs: 57.16 → 58.36 fps (+2.1%, 3/3), low 52–54 → 54–58, hitches 16 → 11. Not yet measured on the Pixel or in other games (TODO). Desktop hosts were not measured and keep it off. |
| Keep each GL context's host client-array enables, pointers and client-active unit, and skip host calls that change nothing (`LT_GLES_ARRAY_CACHE=off` for A/B) | `6762325419` | A two-array draw made 2 pointer, 4 enable/disable and 4 client-unit host calls, each paying driver state validation. Guest data is still refreshed every draw and changed pointers are still checked. **Pixel, array setup + cleanup per draw 0.619 → 0.228 µs (`IT_GLES_PROF=1`, 50 000 draws); the calls drop to ~0 after warm-up. LT Bench `gl-draw` best time −10% in all five pairs (Poco 7.81/8.92/7.93 → 7.49/7.38/7.05 µs); `gl-sprite` within the spread. Poco, Jetpack title, three pairs: 51.6 → 52.5 fps (+1.7%, 2 of 3), ceiling 56–58 either way, hitches 6–7 → 1–8.** Kept on for the per-call saving; no game-level change is claimed. |
| Guest GL shim: report each CoreAnimation surface once instead of on every frame (guest package serial 27) | `7f744e80cc` | `surface_capture` in the guest's OpenGLES logged two lines per surface CA handed out, and CA rotates its buffers, so every frame cost ~12 `write(2)`s plus ~12 `GLES_OP_LOG` traps, and launchd relayed each write to syslogd as a message of its own, which syslogd parsed with regexes (`asl_parse_time` → `regexec`: the top shared-cache page in a Poco Flight Control profile). Found by sampling guest stacks at IRQs, naming the addresses with LT Bench `-DSYMS`, and reading the guest syslog through `syslog_relay` (6348 lines in 66 s of Flight Control, all but ~300 these). Measured with the shipped OpenGLES binary-patched to skip the lines vs unpatched, each installed into a copy of the same record. **Desktop, Flight Control at 57.4–57.8 fps either way: vCPU busy 72.2/69.3/71.3/73.5% → 50.9/54.3/52.1% of wall (~3.3 ms less guest work per frame). Desktop, Jetpack title: 55.7/56.9/53.6/56.0 → 59.2/58.9/58.8/58.9 fps (vCPU 90–94% → 73–77%). Poco, Flight Control, four interleaved pairs: 55.4/57.5/56.1/57.1 → 59.0/57.2/58.6/58.1 fps (56.5 → 58.2); low 46–48 → 52–56, hitches 9–16 → 1–9 per 30 s, ≥55 fps 63–83% → 85–97% of the time.** **Poco, Jetpack title, three interleaved pairs: 39.6/36.1/35.5 → 51.3/50.9/48.6 fps (37.1 → 50.3, +36%); ceiling 44–48 → 54–56, low 26–32 → 42–46, hitches 117–121 → 2–11 per 30 s** (screenshots: the same title scene). Jetpack draws more and presents the same, so the fixed per-frame cost was a larger share of its frame, and its hitches were the syslog work bunching up. **The real build (serial 27, Xcode 27.1, installed into the record and kept by the new package verdicts), Poco, verdict APK (`995ce258`) on engine `64d111110b`, two runs each against the record's own package: Jetpack title 39.4/39.4 → 50.4/50.7 fps (ceiling 48 → 56, low 30–32 → 44–46, 1% low 12 → 21–28, hitches 114 → 6–7 per 30 s); Flight Control 55.1/55.4 → 58.8/59.1 (median 56 → 60, ≥55 fps 58–66% → 98–100% of the time, hitches 12–15 → 1).** **Pixel, same build and APK: JellyCar menu 30.0/30.0 → 30.0/30.0/30.0 (its 30 fps target either way; hitches 147/41 → 8/45 per 30 s, too noisy to call); Doodle Jump menu 58.8/59.7 → 58.0/57.6 (no hitches either way; ≥55 fps 95–100% → 87–90% of the time, a small loss or noise, not chased: Doodle Jump has left the benchmark set).** Serial 27 stayed installed through six boots on the Pixel and five on the Poco (`state/guest-verdicts`: `good 27`). Serial 27 can now be installed in place through Linux `--set-guest-package` or Android "Guest tools" on a stopped device. It remains private pending the certificate and source license work in TODO. |
| CPSR writes that change only the interrupt masks or flags skip the hflags rebuild; `cpsid` no longer ends the TB; `VMRS APSR_nzcv` sets the flags inline (`LT_PSR_FAST=off` for A/B) | `0addc3e156` | iOS's kernel masks and restores interrupts around every lock (`hw_lock_try`: `cpsid if` … `msr CPSR_fc`). A Jetpack profile on the Poco put the hflags rebuild at ~3% of the vCPU and the guest kernel at 22%. **LT Bench, Poco, two interleaved pairs, best ns/op off → on: `syscall` 1165 → 991, `thread-switch` 79.6 → 70.2 µs, `rpc` 18.2 → 14.4 µs, `fault` 22.0 → 16.1 µs; compute tests unchanged.** Jetpack alone: 49.6 fps either way (within the spread). |
| QEMU_CALL request blocks (every GL call) read and written through the TLB instead of a debug page-table walk (`LT_QCALL_FAST=off`) | `2df170ca78` | A Poco `gl-sprite` profile had ~12% of the vCPU in `get_phys_addr`, `S1_ptw_translate`, `address_space_*` and the like: two full walks per GL call. **LT Bench, best ns/op off → on: Poco `gl-get` 3778/3608 → 1823, `gl-sprite` 19 736/20 569 → 15 731; Pixel `gl-get` 1024/915 → 619/377, `gl-sprite` 5557/5470 → 3755/3515.** **With `0addc3e156`, Poco Jetpack title, three interleaved pairs, both off → both on: 51.4/48.6/48.1 → 51.9/50.9/53.8 fps (49.4 → 52.2, +6%, ahead in every pair; low 36–46 → 42–50).** |
| GL calls that carry guest pointers copy through the TLB's host pointers instead of a page-table walk per page | `924e6839d5` | `gles_guest_rw` probed each page through the caller's TLB, then re-read it with `cpu_memory_rw_debug`, which walks the page tables again. Plain-RAM pages are now copied through the host pointers the probe returns. Poco, Jetpack Joyride title (232 client-array draws a frame): binding a draw's arrays 4.5 → 2.0 µs. Measured with the next row. |
| ES hosts: skip the rectangle-texture setup per draw when no rectangle texture is in use | `1997c1a488` | Every ES1 draw re-read, re-bound and re-enabled `GL_TEXTURE_2D` on each fixed-function unit and restored it afterwards (~10 driver calls per unit), dirtying texture state the Adreno driver then revalidated. Host `glDrawArrays` 8.8 → 5.9 µs; with the row above a draw costs ~9.4 µs instead of ~14.7 (~1.2 ms less per Jetpack frame). **Poco, interleaved 30 s runs: Jetpack 34.3/34.7/34.1/34.6 → 30.8/37.3/34.2/38.2 fps (34.4 → 35.1, ceiling 40 → 42–46); Flight Control 54.5/56.9/52.1 → 56.8/56.2/55.3 (54.5 → 56.1, hitches 17–42 → 8–23 per 30 s).** |
| ES hosts: cache texture name lookups | `64d111110b` | Each bind walked the list of every texture in the share group. Jetpack title, ~90 binds a frame: `glBindTexture` 4.5 → 1.6 µs. |
| GLES on Android: render the drawable into an AHardwareBuffer and copy the present out of it (`LT_GLES_AHB=0` for A/B) | `4de4ae5ba1` | The Android counterpart of the iOS host's IOSurface path: the color texture gets gralloc BGRA storage through an EGLImage (RGBA plus a per-row byte swap where BGRA is refused), so a present waits for the GPU, locks the buffer and copies its rows, instead of `glReadPixels` into a staging buffer and copying that. **Poco, Flight Control, per present: glReadPixels ~0.85–1.0 ms + copy 0.34 ms → lock 0.14 ms + copy 0.37 ms (~1.3 → ~0.5 ms of vCPU time per frame).** Frame rate, four interleaved pairs: 53.4 → 53.8 fps (55.2/51.0/55.8/51.4 vs 51.0/55.8/54.4/53.9), inside the run-to-run spread. Not yet tried on the Pixel (Tensor G5); it falls back to the readback if its gralloc or EGL refuses. |
| GLES on ES hosts: read the frame back as BGRA without a byte loop (`LT_GLES_BGRA_READ=0` for A/B) | `16c09e7fe3` | Every present read the drawable back by reading RGBA into a fresh 600 KB allocation and swizzling it a byte at a time (~2% of the vCPU in a game, plus first-touch faults on the new pages). Now `GL_EXT_read_format_bgra` straight into the destination, else a kept buffer swapped a word at a time. **Poco, Flight Control, same APK, 4 interleaved pairs: 51.9/54.7/51.7/54.3 vs 49.1/53.2/49.6/45.0 fps (53.2 vs 49.2, +8%, 4 of 4); ≥55 fps 48% vs 28% of windows; hitches 34 vs 53 per 30 s.** |
| GLES on ES hosts: convert uploads whose internal format differs from the data's | `4ae00eaca8` | Not a speed change: the white/grey untextured geometry in Jetpack Joyride, Battle Bears and Real Racing on Android (opaque PVRTC uploaded as `GL_RGB` from RGBA data, refused by ES). |
| LCD: convert and compose frames without the BQL | `d7c4927685` | The 60 Hz vsync push converted (or composed and blitted) the whole scanout holding the BQL: 1.1 ms per refresh on the Poco, 5.7% of wall time, and the vCPU, which takes the BQL thousands of times a second for the timer, VIC and interrupts, kept landing behind it (its waits over 1 ms were 13% of wall). Now only the dirty snapshot and `dpy_gfx_update` hold the lock. Same app, engine before (`ad729e45fe`) vs the blit unlock alone, four interleaved 30 s rounds per phone, all 8 pairs won: **Poco, Flight Control 37.1/38.5/36.1/37.3 → 40.4/43.4/45.5/43.4 fps (37.3 → 43.2, +16%); ceiling 46 → 52, hitches 111 → 77 per 30 s. Pixel, Doodle Jump menu 32.8/30.6/34.3/33.2 → 38.7/40.8/36.9/37.7 (32.7 → 38.5, +18%); ceiling 45 → 51.** The same commit also unlocks plane composition (Doodle Jump's scanout is two composed planes) and copies opaque pixels instead of blending them; desktop vCPU BQL wait in Doodle Jump 6.9% → 1.4% of wall. With that and the PL080 change below (`a62f05bae3`) vs the blit unlock alone, four interleaved rounds per phone: **Pixel, Doodle Jump menu 36.1/39.6/40.8/35.2 → 52.6/58.0/51.7/59.5 fps (37.9 → 55.5, +46%, 4/4); frames over 50 ms ~94 → 0–3 per 30 s, low 26 → 44–56, up to 97% of the time at ≥ 55 fps. Poco, Flight Control 40.0/39.8/40.8/42.2 → 43.8/48.5/45.3/41.0 (40.7 → 44.7, +10%, 3/4).** |
| PL080: move a paced FIFO's burst in one push | `a62f05bae3` | The I2S pacing timer (every 2 ms) let the PL080 DMA move audio one 4-byte element per MMIO dispatch: 129 µs a tick under the BQL on the Poco, 4.7% of wall while a game plays music. The DMA now reads the run from guest memory once and hands it to the I2S FIFO in one call (the last element still takes the normal path, which lowers the request line and finishes the descriptor). Desktop: the pacing callback 9.9 µs; the PCM tap still carries the audio. |
| Keep the kernel's global TLB entries across an ASID switch | `ad729e45fe` | QEMU's TLB is not ASID-tagged, so each CONTEXTIDR write (~1.5K/s under iOS) flushed everything, including the kernel's global mappings: EL1 high-half refills were about half of all TLB fills. The short-descriptor walk now reports nG and the switch drops only the few non-global pages the EL1 index picked up. Desktop: kernel fills −79%, all fills −42%. Interleaved 30 s rounds: Poco, Flight Control 36.5/36.7/36.6/34.2 → 36.6/38.9/38.8/33.9 fps (36.0 → 37.1, +3%; 2 won, 2 even); Pixel, Doodle Jump menu 32.0/32.4/32.1 → 35.1/34.6/30.3 (32.2 → 33.3, +3.6%; 2 of 3). |
| **Get the vCPU off the BQL** (four commits, below). The vCPU waited for QEMU's big lock ~26% of wall time in Flight Control on the Poco, ~80% of it in waits over 100 µs behind main-loop holds; it took the lock ~35K times/s. After: ~5K/s, 9% of wall. | engine `80c5b311f9`, `6622f7ab69`, `76ace3a65b`, `974c213352` | Same engine before/after (`256d81f9fb` vs `974c213352`), interleaved 30 s rounds. **Poco, Flight Control: 24.0/24.0/25.4/27.1/26.2 → 34.7/36.1/35.3/34.8/36.0 fps (25.3 → 35.4, +40%), 5/5 pairs. Pixel, Doodle Jump menu: 17.3/19.5/17.6 → 30.7/31.9/34.1 (18.1 → 32.2, +78%), 3/3.** **Pixel, JellyCar menu: 14.8/14.7/14.7 → 23.6/23.3/23.1 (14.7 → 23.3, +58%), 3/3** (its full speed is 30). Desktop full-speed runs still hold 60/60/30 fps (Flight Control/Doodle Jump/JellyCar) with no GL refusals. Per-fix split: one APK per commit, Poco Flight Control, two interleaved rounds: before 25.3 (the five rounds above) → exceptions 27.1/26.4 (+1.5) → tick counter 25.6/25.9 (±0, within noise; kept, as it removes ~18K lock takes/s) → GL calls 33.0/32.8 (+7.1, the big one) → audio clock 34.3/36.7 (+2.6). |
| ↳ Exceptions and exception returns without the BQL | `80c5b311f9` | iOS takes an exception per syscall, page fault and IRQ. `cpsr_write_eret` locked twice per return for EL-change hooks the ARM1176 does not have; `cpu_handle_exception` locked for `do_interrupt`, which is CPU-local without hooks (new `TCGCPUOps.do_interrupt_needs_bql`); `cpu_handle_interrupt` locked only to clear the EXITTB every exception entry sets (now an atomic clear). |
| ↳ Tick counter reads without the BQL | `6622f7ab69` | cputlb now honours `lockless_io` like physmem; timer1 opts in and only its TICKSHIGH/LOW reads (~18K/s) skip the lock. |
| ↳ Guest GL calls without the BQL | `76ace3a65b` | Every `gl*` call (`QC_GLES` via the `QEMU_CALL` cp15 register, ~5K/s) was `ARM_CP_IO`: the vCPU waited for the lock per call, then held it through the host GL work, delaying the main loop's timers. Only plain GL calls skip it; the bridge state other threads read got its own small locks. |
| ↳ No clock read per audio FIFO element | `974c213352` | The I2S model read the virtual clock for each 4-byte element the PL080 DMA pushed (~44K/s), for a stall-trace timestamp: a fifth of the main loop's CPU, all under the BQL, and the main loop's long holds are what the vCPU waited behind. |
| Keep two targets per indirect branch site | engine `bd7ceb2003` | A return or table jump alternating between two targets missed the one-entry cache every other time. **Poco, Flight Control, four interleaved pairs: 49.3 → 49.9 fps (+1%), 3 of 4 ahead.** Within noise; kept because it is never behind and costs one more compare on a miss. |
| Cache each indirect branch's last target in its TB (`-accel tcg,indirect-cache=on`, passed by every port; `LT_INDIRECT_CACHE=off` for A/B) | engine `6c789b3e83` | Every ARM TB ending in BX / a PC load called `helper_lookup_tb_ptr` (~2M/s in Flight Control on the Poco, ~20% of vCPU time, mostly a cache miss on the jump-cache entry and another on the TB it names). A desktop simulation found 59% of those jumps go where the same site went last time. Each such exit now keeps a one-entry cache in its own TB, keyed by target PC, Thumb bit and the address space's context id (renewed whenever that jump cache empties or any TB is invalidated); a hit is a load, a compare and a jump. **Poco, Flight Control, same APK, interleaved: 42.6/43.1/46.3/43.5/45.8 → 45.4/47.9/45.4/49.0/49.6/50.5 fps (44.3 → 48.0, +8%), 5 of 6 pairs; ceiling 52–56 → 54–60, hitches 60–92 → 43–79 per 30 s.** |
| Chain direct jumps across guest pages (`-accel tcg,cross-page-chain=on`, passed by every port) | engine `53744be61d` | System-mode QEMU never chains a jump to another guest page, because the page could be remapped to other code while the TB found there stays valid. iOS 3 never does that (static kernel text, code-signed user code at fixed addresses, no JIT/ASLR/aliases; a reused page is written first, which invalidates its TBs). A third of TB lookups in Flight Control were such jumps (0.66M of 2M/s). Flight Control, interleaved: Poco 17.3/21.3/19.3/19.0/19.4/22.2 → 27.5/23.9/21.4/25.3/23.7/25.2 fps (means 19.8 → 24.5, +24%); Pixel (Doodle Jump's animated menu; the Pixel's page 2 differs) 17.6/14.0/14.2 → 18.8/17.4/17.8 (+18%); won all 9 pairs. Kernel-only chaining gave about half of it. JellyCar's menu on the Pixel did not move (13.7 → 14.0), so that scene is bound elsewhere. |
| Hash cs_base into the jump cache; TLBIASID empties only that ASID's jump cache (and the TLB only if it is current) | `68d4df35e4` | Jump-cache misses in Flight Control 20.1% → 10.2% of lookups; whole-cache flushes ~20/s → 0. Poco, three interleaved rounds: 19.3/16.6/20.0 → 20.8/18.8/20.5 fps (+7%). A third of the misses were one PC with two TBs (iOS lazy VFP flips FPEXC.EN, which is in cs_base) evicting each other. |
| Look up the accelerometer/multitouch devices once instead of walking the QOM tree per event | `15c120f16f` | Each 100 Hz tilt sample ran `object_resolve_path_type` under the BQL: about a quarter of the main loop's CPU in Flight Control. |
| 16K-entry TB jump cache (128 page groups) | `0584dbc9cc` | Flight Control, Poco, three interleaved rounds: 4K 18.8/19.5/20.7 → 16K 22.0/20.9/21.9 fps (+10%); 8K was in between. |
| One TB jump cache per ARM ASID (16 per CPU, lazy epoch clearing) instead of emptying the only one on every context switch | `f9fe5bfd74` | Flight Control play on the Poco (`fcbench.py`, three interleaved 30 s rounds): 18.1/18.1 → 20.1/19.7/20.2 fps, +10%. No change to boot time, which is why it was first parked. A 64K-entry geometry was worse in play (16.8/16.8/18.0). |
| Android builds default to API 29 native ELF TLS | `5323ba7cfc` | Removes emulated-TLS calls from every TCG helper that touches thread-locals: ~5–8% faster boot. |
| PKE: keep the Montgomery radix inverse for the loaded modulus | `a41eb8e07b` | The boot's RSA work recomputed it on every operation: ~7% faster boot. |
| Already in the fork before this work (upstream adopted them later) | 909d3ccb62; cdbcff9724, 38642ff7a2, a4749f1ba6 | 4K target pages; learned per-TB I/O instruction limits (no re-translation after mid-block device reads). |

Jetpack Joyride's title screen (Poco) is the heaviest scene measured so far: 863 GL calls and 232
`glDrawArrays` a frame, host GL ~16–18% of wall time before the three GL rows above. Its best 500 ms
windows reach 40–46 fps, so it is mostly bound by guest code, not GL. It does not hold 60 on the
desktop either: the headless Linux frontend on the Ryzen 7 3700X VM runs the same title screen
(Legacy Store copy 5997, version 1.3.5) at ~54 fps, so the Poco's ~35–36 is about two thirds of that.
In it the guest kernel is 21% of the vCPU (44% of the time in translated code): Mach IPC, the
scheduler and locks, ~5 300 syscalls a second.

### Engine experiments of 2026-10-18/19

A second working branch (`dev/perf-codex`, merged into `dev/perf` on 2026-10-19) tried about twenty
engine changes behind environment switches. Each was measured against the same frozen APK on the
Snapdragon 865-class Poco, Jetpack title, in interleaved pairs with all diagnostics off. The two
wins are in the engine table above (BQL spin, client-array cache). Two more are **open leads, still
in the engine behind default-off switches**:

| Switch | Engine | Result | Next |
| --- | --- | --- | --- |
| `LT_IBTC_L2=on`: a 16K-entry side table behind each indirect branch site's two-entry cache, keyed by the site and its full generated key; a hit refills the site cache without a TB lookup (single vCPU only; `LT_IBTC_L2_PROF=on` samples one helper call in 256) | `6a71c1842f` | Miss-helper call 526 → 498 ns (−5%, hit rate ~67%). **Three ordinary pairs: 52.39 → 53.53 fps (+2.2%, 3/3), 1% low 20.3–28.1 → 26.8–28.6, hitches 28 → 21 per 90 s.** Pair 1 nearly flat. | A longer, independent confirmation, then a default |
| `LT_TB_LOOKUP_INLINE=on`: inline the fully validated jump-cache hit into `tb_lookup`'s callers; misses and accounting use the existing lookup | `164ec930cc` | **Two sets of three pairs, one reversed: 53.14 → 54.43 (+2.4%) and 52.74 → 53.47 (+1.4%), ahead in 6 of 6; hitches 16 → 16 and 22 → 25.** Together with the array cache, against both off: 53.39 → 53.69 (+0.6%, 2 of 3). | Measure on top of the BQL spin |

Removed after measurement. Their code stays on `dev/perf-codex` at the commit given (`git show`), with
full tables in that branch's `docs/PERFORMANCE.md` (app `55498790`). Don't retry one unchanged.

| Experiment (switch) | Engine | Result |
| --- | --- | --- |
| Deliver ARMv6 physical IRQs without the BQL (`LT_IRQ_DELIVERY_FAST`) | `5b956fc6ae` | 53.74 → 51.69 fps (−3.8%), hitches 12 → 50 per 70 s |
| Skip the BQL for interrupts the CPU has masked (`LT_MASKED_IRQ_FAST`) | `e82b9e770b` | 53.09 → 53.00, hitches 16 → 19; the masked check itself got cheaper |
| Bounded idle spin before vCPU 0 sleeps for guest work (`LT_IDLE_SPIN`, 250 µs) | `357fc83aba` | 57.83 → 57.88 with the BQL spin on, hitches 5 → 16 per 180 s |
| vCPU at Android's display nice level −4 (`LT_VCPU_DISPLAY_PRIORITY`) | `6971278387` | 59.05 → 59.13 with the 150 µs BQL spin on, hitches 2 → 5 per 180 s |
| ARMv6-only hflags rebuild (`LT_HFLAGS_V6_FAST`, with an equivalence checker) | `e414b9d74a` | 53.24 → 53.38 (+0.3%), hitches 24 → 24 per 180 s |
| Drop the duplicate hflags rebuild on exception return (`LT_ERET_HFLAGS_FAST`) | `9171922768` | LT Bench `fault` −8 to −13% best, `syscall` −1 to −3%; no game gain |
| Cache the last host-PC → TB lookup for fault restoration (`LT_HOST_PC_CACHE`) | `c146d54fad` | Lookup 380 → 289 ns, LT Bench `stm-unaligned` −4%; Jetpack 53.71 → 53.48 (−0.4%), hitches 29 → 31 |
| TLB refill RAM pointer without a second RCU lock (`LT_TLB_RAM_PTR_FAST`) | `54da21c185` | Refill call 365–373 → 348–351 ns; 59.16 → 59.07, hitches 2 → 4 |
| Lazy full-entry reads in the LCD's TLB dirty reset (`LT_TLB_DIRTY_FAST`) | `42d50e5d2e` | Snapshot 509.1 → 509.0 µs; 52.38 → 53.00, reversed pair −1.07 fps |
| Skip the dirty snapshot on forced LCD repaints | `9400ab1b68` | 52.73 → 51.77 (−1.8%), hitches 23 → 54 per 90 s (removed in `197f75d7ae`) |
| Single-page dirty-bit queries (`LT_DIRTY_QUERY_FAST`) | `cc6063a40f` | Query faster; 53.33 → 53.67 (+0.6%), one pair −1.3 |
| Exact-zero VFP single multiply (`LT_VFP_MUL_ZERO_FAST`) | `f43fbe4986` | 53.36 → 53.06 (−0.6%) |
| One helper for non-fused VFP multiply-add (`LT_VFP_MLA_FAST`) | `f235c93372` | 53.91 → 52.94 (−1.8%), slower in 3 of 3 (removed in `3cc8fa6127`) |
| Eight-bucket indexed indirect-site cache (`LT_IBTC_INDEXED`) | `7b4a86c9f1` | `icall` 42 → 15 ns; 49.66 → 50.86, carried by one hitch-heavy baseline, 2 of 3 lose; +96 bytes per TB (removed in `fd92f34185`) |
| Inline jump-cache probe in generated code (`LT_INLINE_JC`) | patch only | `icall` 32 → 15 ns (Poco), 21 → 7 ns (Pixel); Jetpack 52.93 → 52.60 |
| GL batch buffers through the TLB's RAM pointer (`LT_GLES_BATCH_FAST`) | `d99e32c8eb` | Copy 0.20 → 0.04 µs; 53.07 → 53.50 (+0.8%), pacing unchanged |
| Android ES contexts without dummy pbuffers | `e9e13d795e` | None of the Poco's ES1 contexts has `GL_OES_surfaceless_context` (removed in `2e79c09b2e`) |
| Runtime publisher keeps a frame notice that arrives during its work (`LT_PUBLISH_WAKE_EARLY`, app) | app `5fdf8e94` | 53.49 → 53.39 |
| Guest GL shim: no periodic success-only present tallies (`LT_GLES_TALLY_QUIET`, guest serial 28) | `14cead9c5e` | 53.06 → 53.66 (+1.1%), hitches 38 → 24 per 180 s, carried by one pair |
| Guest GL shim: each tally in one stderr write (`LT_GLES_TALLY_BATCH`, guest serial 29) | `20857608bb` | The first pair's shorter post-tally intervals did not repeat in reverse order |

The two tally experiments bumped the guest package to serials 28 and 29, which were built only
privately and are reverted (the source is serial 27 again). A device with 28 or 29 installed keeps
working: the host answers the unknown tally op with 0, and the shim then logs as serial 27 does.

### Upstream review of 2026-10-19

[samhenrigold/qemu-ios](https://github.com/samhenrigold/qemu-ios) (343 commits past our merge base, to
`6e869d1b33`) and [LightTouchMac](https://github.com/samhenrigold/LightTouchMac) (101, to `08f44357`) were
checked for speed and bug fixes. **Upstream has no speedup we lack**: its performance commits of 2026-10-18/19
are our own changes, adopted (PKE inverse, I2S clock, PL080 burst, GL copies through the TLB, lockless
exceptions and timebase, per-ASID 16K jump caches, cs_base hashing, global TLB entries, CPSR fast paths,
QEMU_CALL through the TLB, the quiet GL shim). Its refinement of our I/O insn limits (`2e1e7e611e`) and
publishing frames on the panel's vsync (`97860b36b0`) were already here. LightTouchMac's changes are Mac
app features.

Taken into engine `dev/perf` (`b6db569a58`..`f736ef8a20`):

| Change | Engine |
| --- | --- |
| QEMU v11.0.5 (TCG optimizer fixes; CBZ/CBNZ, IT and the Thumb hint space UNDEF before v6T2, as on an ARM1176) | `b6db569a58` |
| Windows gets the touch report queue (no lost quick taps) and the 25 ms boot-args scan; both were Linux-only by an `__linux__` guard | `99ede499c9` |
| n72 AES key/IV writes land in their byte lane, PMU clock follows QEMU's RTC, SPI receive-only RUN with AGD | `09ca794c58` |
| Upstream's bare-metal checks of the TCG speedups (ASID switches, PSR, QEMU_CALL across pages) and the paced I2S DMA qtest; all pass on ours | `f736ef8a20` |

**Poco, Jetpack title, v0.0.6 APK vs the same app on this engine, 7 interleaved 60 s pairs: no change.**
First set (A B B A A B, phone 32 → 37 °C): 58.9/57.4/56.6 → 56.9/57.5/54.9 fps. Second set (B A A B B A A B, phone
charging at ~38 °C, ceiling capped at 52–56): 48.4/49.2/48.4/49.4 → 54.9/50.9/49.0/50.5. The sets disagree
and both sit inside the session's thermal drift; mean 52.6 → 53.5.

Not taken: upstream's ISL29003 light sensor model (a new guest interrupt source with no host light input),
its MBX GPU model (only stock OpenGLES composites on it; with our GL front end SpringBoard sends it no work),
the NAND image store (a storage format change for the Mac app's records) and the n72 boot-args rework (7E18
boots unchanged).

### Un1box review of 2026-10-25

[KlugKlugTG/Un1box](https://github.com/KlugKlugTG/Un1box), a fork of this repository, carries its engine
changes as patch files applied in CI, all unmeasured there. Each was checked against the iPod touch 4G on the
Poco F3 and written into our own sources where it helps:

| Their change | What we did |
| --- | --- |
| The Android presenter swaps R and B four pixels at a time on arm64 (`978bbefe`) | Taken, app `e1ade586`, for any unscaled row (not only a buffer whose stride equals its width). A 960x640 frame: 0.58 → 0.17 ms on a big core, 0.71 → 0.19 ms on a mid core, identical output. The presenter thread is ~5% of a core in Jetpack. |
| `APPLE_fence` as EGL fence syncs instead of a `glFinish` per call (`478fbe51`) | Taken, engine `b6d540caf9` (`LT_GLES_APPLE_FENCE=glfinish` for A/B). No measured gain: Jetpack makes no fence calls, and Epic Citadel, Infinity Blade II, Asphalt 5 and 8 and Battle Bears do not import the extension. It only helps a game that uses fences. |
| The inlined TB lookup and the indirect side table on by default (`a02def4c`) | Not yet. Jetpack title, ABBAAB 60 s at 2.24 GHz: 58.9/58.9/58.9 → 59.0/58.3/58.8 fps, 29.7 → 29.4 Mcycles a frame, late frames and p99 unchanged. Still an open lead in [TODO.md](TODO.md). |
| Huge pages for guest RAM (same commit) | Not taken: the Poco's kernel has no transparent huge pages, so it would do nothing there. The Pixel is not checked yet. |
| A misaligned `LDM`/`STM` performed instead of faulting (`2f0c5ac5`) | Not taken. Their evidence is our iPod touch 2G measurement ("Hitches in Jetpack's title" below). Jetpack's title on the 4G takes no alignment faults, with audio playing (150 s on the desktop), so there is nothing to win there, and it departs from the hardware. |
| Presenter and vsync threads at display priority (`478fbe51`) | Not taken yet: unmeasured there, and the presenter is ~5% of a core here. |
| The synchronous present waits on a native fence; a frame's pages mapped once; no `glFinish` before the readback (`7ea1b8a8`, `59da7ef0`) | Not taken: these are fallback paths (`LT_PRESENT_ASYNC=off`, 16-bit surfaces, drivers without AHardwareBuffer). The 4G's default present is the asynchronous one. |
| "Fewer pauses when opening iOS apps" (`f84b1bec`) | Not taken: it turns off the two code-cache flushes measured above as gains (`LT_FLUSH_ON_APP`, `LT_FLUSH_SETTLE`). |

### Measured and rejected

**iPod touch 4G, 2026-10-25 (TLB and jump-cache sizes).** Poco, Epic Citadel's tour, 120 s windows, eight runs
interleaved ABCDDCBA, vCPU cycles per frame (vCPU time × core clock, so a hot phone's lower clock cancels out) and
frames ≥60 ms per minute. Default sizing 59.5/58.9 Mcycles, 39/36 slow frames a minute. A 4096-entry floor on every
softmmu TLB (it halves desktop refills, 32K → 14K/s with 16K entries) 62.0/60.6 and 58.5/34: the bigger tables miss the
phone's caches more than the refills cost. A 256-entry cap 75.3/74.0 and 175/164. A 4K-entry TB jump cache instead of
16K 61.4/58.4 and 46/41. Patch: `perf/ec/rejected-tlb-min-max-bits.patch`. Delivering IRQs and FIQs without the BQL when nothing else is
pending (`perf/ec/rejected-irq-unlocked.patch`; desktop: 373K lock acquisitions → 1): Poco, six runs ABBAAB, on
59.6/61.9/61.9 vs off 59.5/58.7/61.6 Mcycles a frame. iOS's handler reads the interrupt controller at once, and
that read waits for the lock instead. Same-engine runs still differ by up to
~4% across a session, so compare interleaved arms only.

**iPod touch 4G, 2026-10-25.** The IOP's flash page `pread`/`pwrite` without the BQL (`perf/ec/rejected-nand-io-unlocked.patch`
in the development folder): Poco, Real Racing's attract demo with its scene load, four runs ABBA, vCPU BQL waits 0.37/0.32 s
with it vs 0.34/0.25 s without in 40 s, the load as long either way. The phone's page cache serves the reads quickly.

**iPod touch 4G, 2026-10-22.** A 2-way TB jump cache (a miss also probes slot `h ^ 1`): desktop
jump-cache misses 32.8K → 22.0K/s at Jetpack's title, fps flat (28.1 vs 28.3); Poco Jetpack 24.0/24.1 vs
23.4/24.0, Doodle Jump 44.6/46.4 vs 47.4/51.7 (lost 2 of 2). The IOP's cache-control registers as a
subregion without the BQL: Poco Jetpack 23.4/22.1 vs 23.1/22.7 with the BQL, Asphalt 5 unpack 171 vs 162 s.

**Next-TB lookup from generated code after CP15 c2/c7/c8/c13 writes** (2026-10-23, `LT_CP_NOCHAIN`, patch `perf/n81/rejected-cp-nochain.patch`): these writes end the TB, and instead of returning to the main loop the next TB was looked up from generated code. Slower: LT Bench `syscall` 1463 → 1889 ns and `time` 892 → 1085 ns; desktop Jetpack vCPU 49.6/49.7 → 50.4/50.9%. The site cache misses after a context change, and its helper path costs more than the main loop's.

**Timer notification only for an earlier deadline** (2026-10-23, `LT_TIMER_NOTIFY_EARLIER`, patch `perf/n81/rejected-timer-notify-earlier.patch`): `timer_mod` wakes the main loop (a bottom half and an eventfd write, 0.75% of the Poco's vCPU in Jetpack play) whenever the timer ends up first, even when its deadline moved later. Skipping those: desktop neutral, Poco Jetpack play 29.1/29.1/29.3 → 28.9/28.4/29.1 fps.

**Shared stubs for the TLB check on guest loads and stores** (2026-10-23, `LT_TCG_THUNK=1`, x86-64 and arm64 backends, patch `perf/n81/rejected-tcg-thunk-pad.patch`): each guest load or store called one stub per access type instead of carrying the ~10-instruction TLB check and its slow path inline, to shrink the generated code (see "Jetpack Joyride, instruction by instruction" below). Desktop Jetpack play, three interleaved pairs: 391/390/381 → 373/373/382 frames per 8 s, misses from instruction fetch only 3% fewer. Poco: 29.3/28.6/29.0 → 28.9/26.6/29.7 fps.

**One translation of kernel code for both FPU-enable states** (2026-10-24, `LT_TB_FP_SHARED`, patch `perf/n81/rejected-tb-fp-shared.patch`): iOS turns the FPU on and off per thread (FPEXC.EN), and the translation key includes that bit, so a quarter of the kernel's blocks in Jetpack play were translated twice and a quarter of its instructions ran in the less used copy. Blocks with no FPU instruction were filed under one key and shared, chaining only to other shared blocks. Duplicates went from 6,429 to 3, but each copy was hot only while threads in its own state ran: desktop 40.4 → 40.3 Mcycles a frame (three pairs), Poco 32.7/32.3/31.7 → 31.9/31.0/32.1 fps. Sharing user code too was worse (its FPU-free blocks lost their chains into FPU blocks: desktop instructions per frame +12%).

**A smaller code cache** (2026-10-24, `LT_TB_SIZE_MB=128`, TEMP; the default is 1 GB, an eighth of RAM): the idea was to keep a game's translations packed together (see the flush below). The cache then fills during play and the whole thing is retranslated in the middle of a level: Poco Jetpack play 29.3/28.7/29.4 → 24.1/28.3/24.9 fps, each run with a stall of a second or more. Desktop 344 → 328 frames.

**One flush of the code cache at a fixed time, in the game's menu** (2026-10-24, `LT_TB_FLUSH_AT=100`, TEMP experiment): by the time a game starts the cache holds ~430 MB of translations, mostly boot and launch code that the game's code is interleaved with. Flushing once in Jetpack's menu, so play is retranslated into a compact cache: Poco play 29.7/30.3/29.3/30.4 → 32.5/30.2/30.9/30.7 fps (29.9 → 31.1, +4%, 3 of 4 pairs), frames under 30 fps 36–54% → 24–39%. Desktop, 100 s into play: cycles per instruction 1.13 → 1.08, frames 330 → 340. Translation keeps running for 20 s or more after a flush, so a flush in play costs more than it gains. That led to the kept change above: a flush at a new app's first frame.

**Per-address-space TLB banks, first attempt** (2026-10-18; redone and kept 2026-10-23, row above; patch kept as `perf/rejected-tlb-banks.patch`): the user TLB index
set aside per ASID at a context switch and swapped back when that address space runs again (8 banks;
every invalidation also applied to the inactive banks). On the desktop it cut user TLB fills per
switch from 82 to 58 in Flight Control (77% of switches found their bank), and LT Bench's `rpc-64p`
refill cost from ~730 to ~430 ns. But Flight Control on the Poco lost: banks off 55.7/56.5/54.7/55.0
vs on 53.2/54.9/53.1/54.3 fps (55.5 vs 53.9, −3%, 4 of 4 pairs), with lower medians and more hitches. Each LCD frame's
dirty-tracking reset (`tlb_reset_dirty`) walks every TLB entry under the TLB lock, and with banks
that is nine tables instead of one (2.6% of the main loop on the desktop), contending with the vCPU.
QEMU also sizes a TLB only when it is emptied, so a bank that keeps coming back stayed at its first
size and thrashed until it was flushed once on return over 70% full.

**Two more 4G experiments, 2026-10-22.** The scaler's conversion on a worker thread instead of the vCPU (patch kept
as `perf/n81/rejected-scaler-worker.patch`): Poco neutral. Re-tested 2026-10-23 and kept (row above): Jetpack runs at a 30 fps
cadence, so vCPU busy shows what fps hides. Rotating in place from mapped guest RAM (no copies) was slower on the desktop:
the column reads miss the cache, where the copy streams. Rebuilding
`hflags` on exception entry and return is ~0.7% of Jetpack's vCPU on the 4G (the ~10% was the 2G), so not cached. `CA_DISABLE_DETACHED=1` in backboardd's environment, to see
whether CoreAnimation's detached (direct scan-out) layers cost more than they save: Jetpack's vCPU 59% → 89% busy, so
the detached path is already the cheap one.

**More disabled launchd jobs** (iPod touch 4G 6.1.6, 2026-10-22; the recipe's `quiet_services` already disables 14, among
them iMessage, iCloud, push and OTA). A guest tool printing each process's CPU time (`proc_pidinfo`) showed the daemons use
under 1% of a core during Jetpack or Doodle Jump (backboardd ~30%, the game ~22%, mediaserverd ~13%) and the whole userland
1.5% on the awake home screen. Disabling 27 more jobs (iTunes Store, accounts, Apple ID, Game Center, geod, social, search,
powerlog and others), interleaved on the desktop: Jetpack's vCPU 59.9/58.6 → 59.1/59.4% busy at 29.9 fps either way, Doodle
Jump 57.2/59.1 → 55.7/56.3/56.3% at 59.7–60.0 fps, idle 5.8/5.4 → 6.0/5.3/5.3%. Only boot moved: guest CPU to the home
screen −8% (~2.6 s, spread ~2 s). Not built as an option.

No gain, so not kept: skipping the jump-cache clear on a single-page TLB flush (`tb_jmp_cache_clear_page`;
Poco, Flight Control 48.3 → 47.3 fps), a 64K-entry TB jump cache (slower than 4K in play), a lockless timer counter,
LTO for the Android engine, and the vCPU alone on the prime core. Unpinned, the vCPU spends only ~29%
of a game on the Poco's 3.19 GHz prime core, which looks like a win to take, but pinned there
(re-pinned each second, because core_ctl drops single-core masks) schedutil held the prime core
at 1.6–1.9 GHz and Flight Control fell to 9 fps (Pixel animation was also worse earlier).
Capturing the published frame (`ios_publish_bh`'s compare-and-copy, ~0.3 ms on the Poco) without the
BQL was tried after the LCD change and dropped: JellyCar on the desktop went from 22–28 frames over 50 ms
(two runs) to 220, at the same 30 fps mean, as its presents fell into a 50/16 ms alternation around the
vsync instead of an even 33.
Wider jump-cache entries (the lookup key and host code pointer copied into each entry, so a hit
reads no TB) showed no gain on the Pixel 10 with ADPF off, Doodle Jump menu, three interleaved rounds:
58.4/57.4/57.5 → 49.0/58.0/55.2 fps. That scene now runs near its 60 fps cap even without ADPF, so a
throughput change needs the Poco's Flight Control to judge; the patch is parked
(`perf/jump-cache-wide-entries.patch` in the development workspace) rather than shipped untested.
Keeping the jump cache when a CF_PCREL TB is invalidated (upstream empties every jump cache;
`tb_lookup` already rejects a stale entry by its `CF_INVALID` cflags, so only the indirect-branch
caches would be renewed) cut whole-cache flushes ~5 500 → ~1 200 over a desktop Flight Control run,
nearly all during loading, but lost on the Poco: 52.3/51.2/52.9/51.9 vs 53.6/53.2/54.6/54.8 fps with
the flush (52.1 vs 54.1, −3.7%, 4 of 4 pairs), with fewer windows at ≥ 55 fps (36–40% vs 48–63%).
Keeping the jump cache across a TLBIASID of the running process (patch kept as
`perf/rejected-jc-asid-revalidate.patch`): iOS 3 issues one ~60 times a second from a single
kernel routine, and on the desktop it accounted for 95% of jump-cache misses on empty slots in
Flight Control play. Each entry carried a generation in the unused upper half of its 32-bit PC and
was checked against the PC's current physical page(s) on its next hit instead of being dropped.
Desktop jump-cache misses in play fell 40% (8.3M → 5.0M per 50 s; an unsafe no-flush upper bound
gave the same). The Poco did not gain: interleaved, flush 57.7/57.0/53.6/56.5/53.7/56.3 vs
revalidate 54.3/54.1/57.4/51.9/55.6/55.0 fps (55.8 vs 54.7, behind in 4 of 6). Like the 2-way
cache, fewer misses do not show up in frame rate; a miss now costs about what the page check does.
Doing user-mode LDM/STM to unaligned addresses directly (patch `perf/rejected-user-unaligned-ldm.patch`):
Jetpack Joyride takes 3 300–5 100 alignment faults a second, all from one `STMIA r1!, {r8, r9}` in
the shared cache writing to a 2-byte-aligned heap address. An ARM1176 faults on that too (U=1 only
covers single loads and stores) and iOS's `sleh_alignment` performs the store, so the program
cannot tell; dropping `MO_ALIGN` for EL0 LDM/STM removed every fault (0/s). Frame rate did not
follow: Poco, interleaved, faults 37.5/31.6/37.4/38.2 vs direct 38.4/32.8/34.7/36.2 fps (36.2 vs
35.5). Each fault costs only a few µs here, so not worth departing from the hardware.
The upstream qemu-ios merge brings no speed by itself (its two TCG wins were already here).
Turning off the GL bridge's `glGetError` checks (`IT_GLES_STRICT=0`: up to nine per draw, a sync point
in some drivers): Poco, Jetpack title with guest package serial 27, three interleaved pairs, on
47.4/52.1/50.6 vs off 49.2/49.7/47.9 fps (50.0 vs 48.9). No gain on the Adreno driver, so the checks stay.
A 2-way set-associative jump cache halved jump-cache misses in Flight Control (10.6% → 5.4% of
lookups) but did not move frame rate: Poco 23.3/22.3/22.5 → 25.1/21.0/19.6 fps, Pixel JellyCar
14.0/13.8/14.9 → 13.7/14.3/13.0. Misses are no longer where lookup time goes: at ~2M lookups/s
the *hit* path (`tb_lookup` 12%, `helper_lookup_tb_ptr` 6%) costs more than the remaining qht
lookups (5.6%), mostly cache misses on the jump-cache entry and the TB it points to.

### Where game time goes

Debug builds can log the vCPU's time split: `adb shell run-as org.lighttouch.android touch
files/debug-vcpu-accounting`, start the guest, and read the `[acct]` lines in the record's
`state/engine-PID.log`. The `run`/`halt`/`io (bql …)` split on each line understates lock time: its "bql" covers only
the halt path. The `vcpu-bql`, `bql-by-mr`, `bql-wait-hist`, `bql-holder` and `bql-hold-hist` lines
(engine `256d81f9fb`) time every vCPU acquisition by MMIO region or call site, and the other threads'
holds by taker; `tools/perf/bqlsites.py LOG T0 T1` turns two dumps into per-site rates. They showed
Flight Control's vCPU waiting for the BQL ~26% of wall time before the BQL work above, ~9% after. The host GL calls are
~2% of the vCPU. With 4K jump caches, TB lookup was ~26% of vCPU time (`qht_lookup_custom`
12.6%, `helper_lookup_tb_ptr` 9.2%, `tb_lookup_cmp` 4.1%).
The `[acct]` lines also count jump-cache lookups and misses, split into empty slots, another PC in
the slot, and the same PC with different `cs_base`/flags (with the differing bits), plus whole-cache
flushes; `68d4df35e4` was found this way. Guest-side attribution: put `QEMU_PERFMAP_DIR=/data/data/org.lighttouch.android/files`
in `files/debug-env` (debug builds read `KEY=VALUE` lines into the engine's environment), profile
with `tools/perf/gameprof.sh`, and map JIT samples to guest PCs with the perf map. In Flight Control
the guest's time is ~50% kernel (IPC, scheduling, timers), ~41% shared cache, ~8.5% the game itself.
That split was taken while the GL shim still wrote to syslog on every frame (fixed in `7f744e80cc`,
above): a share of that kernel IPC and shared-cache time was syslogd and launchd handling the shim's
log. **Check the guest syslog when a profile shows system daemons** (`tools/perf/androidsyslog.py` reads it through
the app's USB bridge and counts lines by sender); look for per-frame lines.

**Jetpack Joyride, instruction by instruction (2026-10-23, desktop, gameplay).** Counting every guest instruction (`LT_ICOUNT`) and every translated block's runs (`LT_TBPROF`; TEMP tools, `perf/n81/temp-tbprof-icount-flush.patch`): a frame is ~2M guest instructions, 64% of them in the kernel. A third of all instructions are IOKit walking the scaler's and the framebuffer's pages for DMA (`IODMACommand::genIOVMSegments`, ~1800 pages a frame), but those tight loops take only 4% of the vCPU's cycles. Cost per instruction depends on how often its block runs: 0.45 ns in blocks that run over 100 times a frame, 1.6 ns at 10–100, 3.8 ns at 1–10, 6.5 ns below once a frame. The generated code is ~62 bytes per guest instruction (~10 host instructions, standard for TCG), the code cache holds ~430 MB by the time the game starts, and instruction-fetch misses outnumber data misses about 2.4 to 1. Making blocks bigger costs speed on the desktop (256 jumped-over bytes per block: −16% frames; 88 executed bytes: −5%), but not on the Poco (32 executed bytes: 29.1/29.3 → 28.5/30.9 fps), and making them smaller did not buy any (stubs above), and flushing the cache so the game's code is packed together helped only a few percent once retranslation settled (cycles per instruction 1.13 → 1.08). The remaining host-side time on the Poco (30%) is spread thin: TLB refills 5%, float vector helpers 5%, block lookup 3%.

**Ceiling versus dips.** A mean fps hides the shape: Flight Control's 37 fps on the Poco (engine
`ad729e45fe`) was not a steady 37 but frames at a full 60 Hz (610 of 1119 at 16 ms) broken by stalls of
3–8 refreshes (121 frames over 50 ms cost 8.9 of 30 s). The per-frame trace (`IT_FRAME_TRACE`, engine
`983fb3e6e1`) split them: an on-time frame was 10.1 ms of guest code, 2.7 ms GL and 4.0 ms BQL wait; a
hitch 51 ms guest code, 6.3 GL, 9.1 BQL wait. The vCPU was never idle (≤0.3 ms a frame) and never
off-CPU, so slow frames were frames in which the guest did ~5× the work (~5.7× the instructions), not
host preemption or the guest waiting on a device. Faster guest code and less BQL waiting lift both.
`[acct] main-cb` lines (same commit) time every timer and bottom-half callback that holds the BQL;
that is how the LCD refresh (5.7% of wall) and I2S pacing (4.7%) were found as the holders the vCPU
waited behind.

**Hitches in Jetpack's title (Poco, 2026-10-19).** With the BQL spin on, the title runs at 57–59 fps
with a median of 60, but the 1% low stays near 30 fps and a few frames a minute take 50–90 ms. What
the diagnostics in [PROFILING.md](PROFILING.md) found so far:

- Inside the >50 ms gaps the vCPU is running 83–84% of the time, runnable 9–11% and sleeping 5–8%
  (Perfetto scheduler traces). The long frames are mostly execution, not a host wait. CPU samples in
  the hitches spread like those in ordinary frames, with no single hot function.
- Audio conversion. The guest's AAC decoder converts with `STMIA r1!, {r8, r9}` into 2-byte-aligned
  buffers, which traps on an ARM1176, and iOS performs each store in its alignment handler. A
  converter call with output 2 mod 4 takes a median 420 µs, against 25 µs aligned. The decoder runs in
  the frontend interval before 48 of 87 gaps over 30 ms. The faults are architectural, so they stay.
- Logging. The two longest gaps in one capture follow ~26 ms of launchd and syslogd work right
  after a guest GL shim tally line. Making those lines quiet or a single write did not give a
  repeatable gain (rejected above), so other log sources may be involved.
- Bluetooth. One 166 ms gap contained 84 ms of BTServer execution, and one boot hit a 10 s BTServer
  timeout and relaunch even though every HCI reply was injected. Neither has reproduced.
- Many held refreshes come 88–92 ticks (~1.5 s) apart. This is a cadence observation, not yet a cause.
- Pixel: a 214 ms gameplay gap had 206 ms of execution wall time but only 83 ms of vCPU CPU time, so
  the Pixel's long frames need a scheduler trace of their own. Poco conclusions don't carry over.

**Battle Bears: Zombies! (2026-10-20).** Story, Easy, Stage 1 gameplay: the Pixel's capture averaged 22.2 fps and the desktop ceiling is ~24 (21–25 a second; release v0.0.8 payload, Ryzen 7 3700X VM). It is guest-CPU-bound on both: GL is ~1.6 ms a frame (18 draws from buffer objects, so not the client-array fetch fixed in `f4eeeab129`), slow Pixel frames are vCPU time (74 of 75 ms), and on the desktop, pinning the vCPU to a core shared with a busy loop cut it to 24% CPU and the game to 4.9 fps. A gameplay profile (`QEMU_PERFMAP_DIR=/tmp`, `perf record -t` on the TCG thread, addresses named with `guestsymbols.py`) put ~45% of guest time in run-loop polling (`objc_msgSend`, autorelease pool push/pop, `-[NSRunLoop runMode:beforeDate:]`, `__CFRunLoopFindMode`'s string hashing, `__CFRunLoopModeIsEmpty`) and ~10% in the game's own code; host TB lookup was ~20% of the thread. LT Bench's `objc-send` and `runloop` tests time that path. `LT_IBTC_L2=on` with `LT_TB_LOOKUP_INLINE=on` cut `runloop` 9.4 → 8.0 µs and `icall` 29.7 → 13.4 ns but not the game: 24.3/23.2/24.9/22.7 → 22.7/23.1/24.3/22.9/22.8 fps, interleaved. **Cause: a bug in the game's bundled OpenFeint 2.9.1.** `+[OpenFeint initializeWithProductKey:…]` starts a thread running `_mainDefault:`, which loops `-[NSRunLoop runMode:beforeDate:distantFuture]` until cancelled; with nothing scheduled on that run loop (OpenFeint declined, its servers long gone) every call returns at once, so the thread spins and takes its full share of the single guest CPU. Only that loop of the game's five `runMode:beforeDate:` callers shows in the profile. Stubbing `_mainDefault:` out of the game binary, as a test only, took the game to its own 30 fps cap: 29.7 and 29.7 fps (from ~24). Faster emulation of the polling can't recover this, because a spinning thread uses its whole time slice however quickly each poll runs (the `runloop` gain above did nothing for the game). Fixed in the guest tools instead (serial 31, engine `7021b5a362`): our OpenGLES replacement, which QuartzCore loads into every app, replaces `-[NSRunLoop runMode:beforeDate:]` so that off the main thread a NO with the limit date still ahead sleeps toward it (10 ms at most) instead of returning at once. The unmodified game: 29.6/29.7/29.7 fps. Any game whose OpenFeint 2.x thread spins this way gets the same relief.

The manual Start/Stop profile in the Android app (engine `50a08eb2d2`, `ProfileCapture.kt`) records
these gaps from real play, but it costs 6–7% of the frame rate while recording. Don't use a recorded
rate as a baseline.

### Storage (2026-10-25)

LT Bench's `fs-*` tests ([GUEST-BENCHMARKS.md](GUEST-BENCHMARKS.md)) time uncached file I/O inside iOS. Two fixes
(rows above): the iPod touch 2G's flash writes no longer spin the vCPU (`3b4f4a67c3`, desktop `fs-write`
381 → 148–198 µs/KiB), and the iPod touch 4G's reads are ~15% faster (`56505fbfdb`). Where a 4G read goes now
(desktop, `LT_NAND_PROF`'s `iop_us`/`ap_us` and per-thread schedstat): the AP and the IOP hand each request
back and forth, never both busy, ~2.8 s and ~2.0 s of every 5 s of reading. Host file I/O is ~3 µs a page.
The IOP's remaining cost is spread over MMIO dispatch and the BQL; the AP's is iOS's kernel itself, flat,
with its whole-cache clean-by-set/way loops (~8.7K cache ops a call, no-ops under TCG) the largest piece at
~5%. Further gains need faster translated code in general, not storage changes. Tried and dropped: serving
the IOP's cache-control registers without the BQL (no change).

### Frame pacing on the iPod touch 4G (2026-10-25)

Epic Citadel's guided tour (`fcbench.py APK 60 n81-ec-tour`) on the Poco: ~44 fps cool, 1% low ~20; ~30 / 15
when the phone is hot. What was checked:

- **The 1% low is real.** The game's own present intervals (frame path, its GL context) give the same 1% low
  as the frames LightTouch publishes, so reading the display does not invent it. Published intervals are
  multiples of the 60 Hz vsync, as on the device: the game presents at 16–36 ms and backboardd composites on
  the vsync.
- **Slow frames are more guest work, not waiting.** The vCPU is ~97% busy in fast and slow frames alike. A
  guest timeline (TEMP tracer) of the slowest 1%: the game itself +8 ms over a median frame, plus bursts
  from iOS's own threads (interrupt work loops, mediaserverd about every 0.83 s, backboardd, locationd).
  The I2C traffic behind one of them is real sensor polling (the gyro at 200 Hz, two transfers a sample,
  the accelerometer at 60 Hz: ~490 transfers and interrupts a second, ~5% of the guest).
- **The CPU and the GPU already run on their own threads** (GL thread `57fb8a115f`, queued present
  `429e0a3551`). What still stopped the guest was main-loop work holding the BQL: the two rows above
  removed ~60% of the vCPU's lock waits. What is left (~0.65% of the time) is spread over device timers.
- The tour reads only tens of flash pages per 5 s once running; storage is not what slows it.
- **The slowest frames are iOS's audio cycle, not the game.** In them the game takes ~20 ms, as in a median
  frame. The other 30+ ms are iOS's own work. Every ~0.83 s the audio decoder (AMC) finishes a block, the music
  file's next pages are read and decrypted (a burst of DMA interrupts), and mediaserverd, kernel threads and
  backboardd run for 20–30 ms. Almost every frame over 45 ms in a 20 s Poco trace falls on one of those cycles.
  The AMC's register reads are only ~0.2% of the time.
- **The tail follows guest throughput.** Making the vCPU 26% slower per frame (the rejected TLB cap below) gave
  4.5× as many ≥60 ms frames, so cycles per frame is what moves the 1% low. Tool: `cyc.py` in the development
  folder's `perf/ec/scripts` (with `abn.sh`, interleaved arms; `slowframes.py` splits each slow frame by process).
- **Jetpack Joyride's title holds ~57 fps with a 1% low of 28–29 on both phones.** Its 1% low is a whole
  refresh missed: p99 is 35 ms, two refreshes. Poco, four valid interleaved 60 s runs with the two stall fixes
  above on and off: 57.5/57.5 vs 57.7/57.3 fps, 1% low 29 in all four, so they do not move it. The Pixel 10's two
  hitch bundles (engine `7ba03a85fd`): 50.2 and 53.3 fps, 1% low 27, with ~13% of frames holding 20–22 ms of guest
  work against 11 ms and shown at 30–36 ms, on the same big cores at the same clock as the fast frames.
- **What makes a Jetpack frame miss its refresh is the scaler, not the game.** A Poco trace (414 frames: 301 shown
  after one refresh, 106 after two): the game's own work (~7 ms) comes before the vblank and is the same in both.
  The late ones wait after it, ~10 ms of it idle. Jetpack is landscape, so CoreAnimation has the scaler turn every
  640×960 frame and backboardd swaps only once that is done. The host took 2.8 ms for a turn at the median
  (p90 4.3, p99 6.6) against the 0.6 ms the model allows, and 50 of 414 turns ended after the next vblank (45 of
  them would have been in time at the model's speed). A faster turn (`d58ecd7e9c`, table above) halves the late
  frames, and turning in place (`e15a9a95e2`) takes them from 7.7% to under 5%. After both, a trace
  (tracing inflates late frames to 12%) has the turn past the vblank on 20 of 629 frames; the other late frames are
  the game presenting a refresh late, behind idle time and background daemons (pasteboardd, sandboxd), not the scaler. Tool: `presentsplit.py` (work before and after the vblank per frame).

### Guest benchmarks

[LT Bench](GUEST-BENCHMARKS.md) times one guest path at a time inside iOS (best of 5 rounds, ns per
operation). Baselines, engine `16c09e7fe3` (Poco: ic8 APK with `LT_TLB_BANKS=off`; desktop: Ryzen 7
3700X VM, headless):

| Test | Poco F3 | Desktop | Note |
| --- | --- | --- | --- |
| `loop` | 4.1 | 3.5 | translated code baseline: about as fast on the phone as on the desktop |
| `fib` (call + return) | 9.6 | 7.8 | |
| `icall` (indirect call + return) | 30.7–62.7 | 28.2 | 3–6× a direct call: a site with more targets than the 2-entry branch cache misses to `helper_lookup_tb_ptr` |
| `objc-send` / `runloop` | — | 29.5 / 9 400 | added 2026-10-20 (engine `f4eeeab129`): a message send costs an indirect call; one run-loop poll ~320 of them |
| `memcpy` (per KiB) | 612 | 441 | 1.6 / 2.3 GB/s |
| `syscall` (`getppid`) | 972 | 937 | |
| `time` (`mach_absolute_time`) | 975 | 715 | a trap on this OS |
| `thread-switch` (pipe round trip) | 65 600 | 26 600 | 4× a Mach RPC to another process; the vCPU is busy (~95%, <1% halted) and the profile flat, so guest kernel work (TODO) |
| `rpc` (Mach RPC to launchd) | 13 200–16 600 | 5 700 | two address-space switches |
| `rpc-64p` minus `rpc` | ~600–800 | ~1 350 | refilling 64 pages after the switches (desktop with the rejected TLB banks: ~120) |
| `touch-64p` (per 64 pages) | 345 | 218 | the same pages with a warm TLB |
| `fault` (anonymous page) | 17 500 | 11 100 | |
| `mmap` + `munmap` | 11 800 | 4 700 | |
| `tlb-32p` / `tlb-1024p` | 5.8 / 11.3 | 5.7 / 12.7 | |
| `sleep` (`usleep(1000)`) | 1.56 ms | 1.15 ms | |
| `fs-write` / `fs-read` / `fs-read-16k` (2G, per KiB / per KiB / per read) | — | 148 000 / 4 800 / 230 000 | added 2026-10-25 (engine `3b4f4a67c3`, desktop overlay record): writes go page by page through the overlay's file + fsync |

The same Poco and APK measured 13.2 µs and 16.6 µs `rpc` on two boots, and `icall` 31 and 63 ns:
absolute numbers move between boots, so compare interleaved runs as with games.

## Current state and next targets

Game frame rates, from the measurements above (full speed from the desktop runs; "—" was not
measured at that point):

| Game (scene) | Phone | Full speed | v0.0.2 | Before cross-page chaining | Before the BQL work | After it (`974c213352`) | LCD blit unlocked | `a62f05bae3` | Now (`16c09e7fe3`) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Flight Control 1.9.2 (first 30 s of play) | Poco F3 | 60 | ~18 | 19.8 | 25.3 | 35.4 | 40.7–43.2 | 44.7 (ceiling 52–58) | **55.5** (55.0–56.5; ceiling 60, median 56–58, ≥55 fps 61–76% of windows, 13–22 hitches per 30 s) |
| Doodle Jump (animated main menu) | Pixel 10 | 60 | — | 15.3 | 18.1 | 32.2 | 37.9–38.5 | 55.5 (ceiling 58–62, no hitches) | — |
| JellyCar (live-physics main menu) | Pixel 10 | 30 | — (5.7 before ADPF) | 13.7 | 14.7 | 23.3 | — | — | — |

With the quiet GL shim (`7f744e80cc`, the real guest package serial 27, engine `64d111110b`, Poco):
**Flight Control 59.0** (median 60, 1 hitch per 30 s, ≥55 fps 98–100% of the time) and **Jetpack
Joyride's title 50.5 of 60** (was 39.4; ceiling 56, 1% low 21–28, 6–7 hitches). Flight Control is
now at its ceiling, so Jetpack is the Poco's game benchmark from here. With the BQL spin (on by default on Android since
`b5708085da`) the Jetpack title runs at **57.5** (50 µs) to **58.4** (150 µs), median 58–60, 1% low ~29–30;
without it 53.4 in the same sessions. Doodle Jump holds 58–60 on both
phones and is dropped too (a game that holds a steady 60 on both phones leaves the benchmark set). Doodle Jump
and JellyCar on the Poco were already 57.2 of 60 and 29.8 of 30 before it.

Absolute rates drift a few fps between sessions (the LCD-blit column was 43.2 in one A/B and 40.7 in the
next), which is why every change is judged by interleaved pairs.

| Workload | Pixel 10 | Poco F3 |
| --- | --- | --- |
| Boot to lock screen (in app) | ~21 s | ~37–45 s |
| Springboard Home ↔ Spotlight | ~50 fps | ~47–51 fps |

Beside the frame counter the app draws the guest's fps on a fixed 0–60 scale for the last 60 s, a point
per 500 ms so dips stay visible, with the current rate at the trace head and the 10 s average and low in
the corner (`71624bd7`); `fcbench.py` with `FC_FRAMES=chart.png` gives the same view, with percentiles,
from a measurement run.

Games are the main gap; see [TODO.md](TODO.md).

# Recording hitching during play

With a guest running on Android, **Start profile** appears above the FPS graph. Open the iOS app
and scene you want, press Start profile, play through the hitching, then press **Stop profile**.
The label shows the iOS app's localized name. Start again for another scene or game; each recording
gets its own directory. The app name and bundle ID are queried at the start of each recording.
If the guest agent cannot identify the app, Start reports an error and does not begin recording.
Switch apps between captures so each capture's label describes its contents.

Recording continues in fullscreen. Return to the normal view to stop. It also stops when the guest
worker stops. Captures are limited to 15 minutes or about 128 MiB each. Older captures are retained;
there is no automatic deletion. A killed worker may leave a partial capture with its final buffered
events missing; the analysis tool flags a missing stop footer. Pausing/backgrounding the guest while
recording leaves the pause in the elapsed timeline.

## Retrieve and analyze

For a debug APK, with wireless ADB connected and `ANDROID_SERIAL` set:

```sh
tools/perf/profile.py --pull build/profiles
# Choose one of the timestamped directories under build/profiles/profiles/:
/usr/bin/python3 tools/perf/profile.py build/profiles/profiles/CAPTURE \
    --chart build/profile.png --json build/profile.json
```

Pull after Stop so all buffered files have been flushed. This copies only the profiles, not guest
state or firmware. Captures stay in the app's private `files/profiles/` directory on the phone.
The optional chart uses matplotlib; the text and JSON report need only Python's standard library
(Python 3.12+ for safe tar extraction when pulling).

The report provides ceiling, median, low, 1% low and hitches, a frame-time histogram, a timeline,
GL calls ranked by total wall time, and the longest individual engine and Android presentation
intervals. The JSON retains the full frame samples and details of overlapping slow GL calls and
BQL waits/holds for the longest frames. `--top 30` increases the number of detailed frames/calls.

## What is captured

- `metadata.json`: frontmost iOS app response, localized name, bundle ID, start/end timestamps,
  Android model/build, LightTouch version, worker PID and performance environment switches.
  New captures also include audio underrun, PCM queue/drop and output counters at start/end.
- `engine-*-manifest.json`: the APK's build and engine notice/identity manifests.
- `engine-start.log`: the first 64 KiB of engine setup diagnostics, including graphics initialization.
- `engine.log`: up to 8 MiB of engine diagnostics produced during the capture.
- `events.tsv`: engine frame publications, cumulative vCPU wall/CPU time and BQL waits, GL call
  count/total/max duration per publication interval, and individual GL calls/BQL waits/holds of
  at least 0.5 ms. GL timing covers the host bridge, context selection and driver call. Each frame
  record also carries the vCPU's time waiting for a host core, the core it last ran on and that
  core's clock, so a slow or throttled core can be told apart from preemption. `engine-start.log`
  says whether hardware memory tagging is on for the vCPU (`[vcpu] memory tagging:`).
- `presentation.tsv`: Android post completion timestamps and separate durations for control mutex
  waits, frame copy, native-window buffer acquisition, scaling, frame pacing, posting and commit.

No guest package update is needed. Profiling is activated dynamically by the button; it does not
require `IT_VCPU_ACCT` or `IT_GLES_PROF`, and does not enable per-instruction instrumentation.
`LT_PROFILE=off` (or `0`) disables the engine recording API for comparison. The additional timers and
buffered writes run during a capture, so captures are diagnostic runs, not uninstrumented performance
baselines. Use this recorder for user-selected obvious hitches; ordinary performance
measurements keep Start profile off.
Use narrow engine counters only for a specific investigation; confirm improvements in
interleaved A/B pairs with all diagnostics off.

Captures pass through bounded memory queues to background writers (8 MiB for engine events, 1 MiB
for Android presentation), so storage writes never hold the BQL or the JNI lock that audio reads use.
A writer that falls behind fails the capture with an error instead of blocking playback.
`LT_PROFILE_SYNC=on` writes synchronously instead, for comparisons. Recording costs 6–7% of the frame
rate on the Poco (PERFORMANCE.md), so a recorded rate is not a baseline.

## Reading the timings

Engine publication and Android post completion are different events. A late engine publication
points to guest or engine work, or a main-loop delay. A timely engine frame followed by a long Android
buffer wait points to presentation backpressure. Post completion is not physical scanout.

Counters overlap. GL runs inside vCPU execution, and GL wall time includes host descheduling and
synchronous driver waits. A GL call is charged to the frame in which it finishes, so a call that crosses
a boundary can exceed its frame. The vCPU snapshots are sampled from another thread and can lag by one
execution slice. vCPU wall time minus CPU time can be descheduling or blocking; it does not name a cause.
Static screens, loading and game pauses also make long gaps, so read a capture against the scene you
recorded.

## Engine diagnostics for hitch hunting

The engine has opt-in diagnostics for following one long frame through the guest, the engine and the
host. All are off by default and cost nothing when off. On Android, a debug APK reads them from
`files/debug-env` (`KEY=VALUE` lines, through `run-as org.lighttouch.android`); on the desktop, set
them in the environment. Each perturbs timing, so **never compare frame rates with a diagnostic on**:
A/B pairs run with all of them off.

Most of them add records to the **frame path**, `LT_FRAME_PATH=/absolute/path.tsv`. It is a bounded
in-memory trace (131 072 records, 8× or 16× with the heavier diagnostics). Producers only timestamp
and store a record, with no I/O, allocation or locks. The file is written after a clean engine stop,
so pull it after Stop. Each line is `monotonic_ns event a b c`. A missing `# end` footer or nonzero
`dropped` means the capture is incomplete, and the analyzers refuse it. Keep the runtime's
`LT_FRAME_LOG` publication log from the same run: the analyzers join the two by serial.

| Switch | Records | Read with |
| --- | --- | --- |
| `LT_FRAME_PATH=FILE` | Guest surface presents, LCD flips, vsync and scan, frame capture and publication | `framepath.py` |
| `LT_FRAME_PATH_CONTEXT=on` | Also CONTEXTIDR and TTBR0 changes (guest address-space switches) | `frameresidency.py` |
| `LT_FRAME_PATH_DELAY_MS=N` | Nothing until N ms after start, so the buffer goes to a game's steady state rather than boot | |
| `LT_GUEST_TRACE_PC=PC,PC,…` (up to 32; bit 0 set for Thumb), `LT_GUEST_TRACE_DELAY_MS` | Register snapshots (PC, thread, R0–R3, LR, SP, R12, mode) each time a selected guest instruction is reached. Point sets for `n72ap-7E18` are in `tools/perf/n72-7e18-*-points.json` | `guestpoints.py`, `swapcalls.py`, `queuepath.py`, `audiostages.py` |
| `LT_NATIVE_WAIT_PROF=on` / `conditions` | vCPU 0 mutex and condition waits of 10 µs or more, with their source site (`conditions`: condition waits only, so it works with the BQL spin; `on` is refused while the spin is installed, so on Android add `LT_BQL_SPIN=off`) | `nativewaits.py` |
| `LT_BQL_FALLBACK_PROF=on` | With `LT_BQL_SPIN=on` and `LT_NATIVE_WAIT_PROF=conditions`: the blocking BQL takes left after the spin | `nativewaits.py` |
| `LT_AUDIO_PATH_PROF=on` | Sampled VFP add, sub and mul operands, timing and guest PC (1 in 1021), plus alignment-fault PCs | `audiopath.py` |
| `LT_ALIGNMENT_PATH_PROF=on` / `returns` | Alignment faults; `returns` also pairs a sample of them with the exception return | `alignmentlatency.py` |
| `LT_GLES_SURFACE_PATH=on` | Physical backing of each surface present | `surfacemap.py` |
| `LT_GLES_AHB_PATH=on` | Android AHardwareBuffer present stages: GPU finish, describe, lock, unlock | `ahbpath.py` |
| `LT_FMSS_PATH=on` | NAND loads, stores, erases and their fsyncs | `fmsspath.py` |
| `LT_BT_PATH=on` | Bluetooth HCI commands, reply queueing and UART injection (reply prefixes only, never firmware payloads) | `btpath.py` |

These print five-second blocks to the engine log instead (`files/logs/engine*.log` on Android, `logs/engine*.log` on the desktop; see [LOGS.md](LOGS.md)):

| Switch | Prints | Read with |
| --- | --- | --- |
| `LT_BQL_PROF=on` | The `[acct]` BQL tables (waits by region and call site, hold histograms, main-loop callbacks) without the instruction accounting of `IT_VCPU_ACCT`. It still costs ~6% of the frame rate | `bqlsites.py` |
| `LT_EXCEPTION_PROF=on` | Exceptions by type, SVC numbers, Mach message send and receive attempts, data-abort status | `exceptions.py` |
| `LT_GLES_PRESENT_PROF=on` | Surface present stages | `presentstages.py` |
| `LT_GLES_CONTEXT_PROF=on` | Context selection: init, make-current and dispatch for viewport, framebuffer bind and present | `contextstages.py` |
| `LT_LCD_SNAPSHOT_PROF=on`, `LT_LCD_SNAPSHOT_STAGES=on` | The LCD's dirty snapshot, then split into listener sync, bitmap copy, TLB dirty reset, clear and the TCG ordering barrier | the log |
| `LT_IBTC_L2_PROF=on` | The indirect miss helper: calls, side-table hits, sampled time | the log |

### Scheduler state inside a gap

To tell execution from runnable delay or sleep in a long frame, run a Perfetto trace with
`tools/perf/hitch-sched.pbtxt` (65 s: scheduler, wakeups, blocking reasons, CPU frequency;
`hitch-wake.pbtxt` adds IRQ and idle-state events, to tell idle CPUs from ones running interrupts). Run a 60 s `framestats.py --device 60 --save frames.log`
inside it:

```sh
adb shell perfetto --txt -c - -o /data/misc/perfetto-traces/hitch.pftrace < tools/perf/hitch-sched.pbtxt
adb pull /data/misc/perfetto-traces/hitch.pftrace build/
python3 tools/perf/hitchsched.py build/hitch.pftrace frames.log --engine-pid PID \
    --trace-processor /path/to/trace_processor_shell --frequency --json out.json --chart out.png
```

`hitchsched.py` refuses traces with data loss or incomplete vCPU states, and picks the busiest
`CPU 0/TCG` thread (check the reported TID, or pass `--vcpu-tid`). To see what the vCPU ran inside the
gaps, attach simpleperf at the same time (`-p PID -m 8 --cpu-percent 25 --clockid monotonic -g`), and
compare ordinary and hitch frames with `hitchcpu.py`. `guestpc.py` names translated-code samples by
guest PC through the worker's `QEMU_PERFMAP_DIR` perf map, and `guestsymbols.py` names guest PCs from
the matching kernel and dyld shared cache.

### Which guest process

`LT_GUEST_GDB_PORT=PORT` (1024–65535) starts the engine's gdb stub on `127.0.0.1` only. Attach
**after** the timed capture, because attaching stops the guest. Forward the port with `adb forward`,
then, in LLDB with the matching unslid ARMv6 kernel:

```text
command script import tools/perf/guestprocs.py
lt-guest-procs --kernel kernel.macho --xnu /path/to/qemu-ios/imgtools/lldb/xnu.py \
    --connect connect://127.0.0.1:PORT --json processes.json
```

It lists each process's task and pmap (`--thread 0xADDRESS` resolves a thread's owner), so the TTBR0
and thread values in a frame path can be named. Address-space values change between boots: take the
snapshot from the same worker.

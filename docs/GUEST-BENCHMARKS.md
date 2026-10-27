# Guest benchmarks (LT Bench)

A game's frame rate mixes everything the emulator does. LT Bench is a small iOS 3.1.3 app that
times one guest path at a time, so a change to the engine can be measured against the path it
targets: the same test, run before and after, in nanoseconds per operation.

Why it exists: Geekbench 2 in the guest on a Pixel 10 scored **1201** where a real iPod touch
2G scores about **193**, so straight-line guest code already runs ~6× faster than the
hardware, yet games miss 60 fps on the same phone. What costs time is everything around the
translated code: exceptions and system calls, address-space switches (QEMU's TLB is not
ASID-tagged), TLB refills, indirect branches that leave a translation block, the GL bridge.
LT Bench measures those paths; the `loop` test is the compute baseline they are compared with.

## The tests

Each runs 5 rounds; the app reports the best and the median ns per operation (the best is the
repeatable figure, a median well above it means interference).

| Test | One operation | Path it exercises |
| --- | --- | --- |
| `loop` | an integer LCG step | translated straight-line code (baseline) |
| `vfp-mla` | one scalar VMLA/VMLS/VNMLA/VNMLS | eight independent VFP2 accumulators, separately rounded multiply/add and generated helper calls; long loop timing, no per-operation clocks |
| `icall` | an indirect call through a 4-entry table, and its return | indirect branches: TB exits, the per-site branch cache, `helper_lookup_tb_ptr` |
| `fib` | a recursive call and return | returns to many sites |
| `objc-send` | `objc_msgSend` to `+[NSObject class]` | Objective-C dispatch: the guest method-cache lookup, then an indirect branch to the method |
| `runloop` | an autorelease pool around `-[NSRunLoop runMode:beforeDate:]` (`distantPast`) on a thread whose run loop has no sources, then the pool's release | a game loop that polls its run loop: message sends, CoreFoundation's mode lookup by name and its empty-mode check (Battle Bears: Zombies!' OpenFeint thread spun here; a faster poll does not help a spinning thread). Uses `distantPast`, so serial 31's empty-run-loop sleep does not apply |
| `memcpy` | 1 KiB copied (libSystem `memcpy`, 256 KiB blocks) | guest loads/stores through the softmmu TLB fast path |
| `syscall` | `getppid()` | SWI, exception entry and return, kernel entry/exit |
| `time` | `mach_absolute_time()` | the guest time base (a Mach trap, plus the timer read) |
| `thread-switch` | a pipe round trip between two threads | two kernel context switches in one address space |
| `thread-switch-64p` | the same, each side writing 64 pages per turn | TLB refills without an address-space switch |
| `rpc` | a Mach RPC to launchd and back (`bootstrap_look_up`) | two address-space (ASID) switches: TLB and jump-cache switching, Mach IPC |
| `rpc-64p` | the same, writing 64 of this process's pages between calls | what an address-space switch costs this process in TLB refills (QEMU's TLB is not ASID-tagged): compare with `touch-64p` |
| `touch-64p` | writing 64 pages, no switch | the warm-TLB cost of the same writes |
| `fault` | an anonymous page fault (map 256 pages, write each, unmap) | data abort, page-table update, TLB invalidation |
| `stm-aligned` / `stm-unaligned` | `STMIA r1!, {r8,r9}` into a pre-touched 2 KiB buffer, 4-byte aligned / 2 mod4 | the audio converter's store instruction; the misaligned case preserves ARM1176 alignment faults and iOS's handler/return |
| `mmap` | `mmap` + `munmap` of one untouched page | VM system calls and their TLB maintenance |
| `tlb-32p` / `tlb-1024p` | a dependent load chasing a pointer through 32 / 1024 pages in scrambled order | softmmu TLB hits vs. a working set beyond its reach |
| `fork` | `fork` + `_exit` + `waitpid` | address-space creation; reports `unavailable` in an app, whose sandbox denies `fork` |
| `sleep` | `usleep(1000)`, actual time | timer interrupt and wakeup latency |
| `fs-write` | 1 KiB of an 8 MiB file in the app's `Documents/`, written 64 KiB at a time, then `fsync` | the emulated flash controller's write path; the file is opened with `F_NOCACHE`, so every byte reaches it |
| `fs-read` | 1 KiB of that file, read 64 KiB at a time in order, first byte of each 4 KiB checked | the flash read path, as a game loads one big asset file; `unavailable` if the bytes are not what `fs-write` wrote |
| `fs-check` | the same read of the file a previous launch wrote, without writing it first | whether written data survives a restart (run it alone, after a pass and a reboot) |
| `fs-read-16k` | `pread` of 16 KiB at a scattered offset in the file | an engine pulling textures and sounds out of a pack file |
| `gl-get` | `glGetError()` | a GL call that returns a value: the guest waits for the host each time (QEMU_CALL round trip plus the host driver) |
| `gl-state` | `glEnable` / `glDisable(GL_BLEND)` in turn | a GL call with no reply, which the guest shim may queue |
| `gl-draw` | `glDrawArrays` of the same 32×32 textured quad | the draw call alone: fetching the client arrays from guest memory, the host draw |
| `gl-sprite` | bind a texture, set the vertex and texture-coordinate pointers, draw | one sprite as a 2D game of the era draws it (Jetpack Joyride's title: ~230 a frame) |
| `gl-mesh` | `glDrawElements` of 144 indices reaching 50 vertices of a 16 384-vertex interleaved mesh (float positions, `GL_FIXED` texture coordinates), a different piece each time | a 3D game's level geometry (Asphalt 5's races: ~125 such draws a frame): the index scan and the vertex fetch |
| `gl-upload` | `glTexSubImage2D` of 64×64 RGBA (16 KiB) | texture streaming from guest memory |
| `gl-present` | full-screen clear + `-presentRenderbuffer:` | the frame's way out; paced by the display, so ~16.7 ms means it kept up with 60 Hz, not what a present costs |

The GL tests draw as a game does: an `EAGLContext` (ES 1.1) on a full-screen `CAEAGLLayer` under the
results label, client-side vertex arrays, the entry points from `dlsym`. Each run ends with `glFinish`,
so queued calls are counted when the host has done them. If the setup fails, the GL tests report
`unavailable` and the log has a `[ltbench] gl: <reason>` line.

Not covered yet: audio, touch input.

A pass takes ~6 s on a Poco F3 and ~3 s on the desktop, plus the storage tests (~10 s on the desktop's iPod touch 2G). To profile one test, build a variant that
runs only it, for many rounds: `LTB_CFLAGS='-DONLY="rpc" -DROUNDS=60' build.sh OUT_DIR` (same bundle
id, so it replaces the full one when installed).
`ONLY` also accepts comma-separated exact names, for example
`LTB_CFLAGS='-DONLY="syscall,fault,stm-aligned,stm-unaligned"'`.
The STM benchmarks time the complete loop, with no per-fault clocks, then check
all stored bytes, surrounding guards and base-register writeback outside timing.
`unavailable` means one of those checks failed.

Naming guest addresses from a profile: a build with `LTB_CFLAGS='-DSYMS=0x33989748,0x3398719d
-DONLY="loop" -DROUNDS=1'` prints one `[ltbench] sym 0x33989748 libSystem.B.dylib regexec+0x…`
line per address at launch (dladdr in the guest, so shared-cache addresses resolve to the exported
symbol at or below them; a static function shows as the export before it). This is how the
desktop's guest stack samples of Flight Control were put down to syslogd's regex time parsing.

Writing the app: there is no iOS SDK; `ltbench.c` declares the libSystem and Objective-C runtime
calls it makes with their ARM32 ABI, and the link stub lists exactly its imports. iPhone OS 3's
libSystem does not export the compiler's division helpers (`__udivdi3` and friends live in its
libgcc_s), so `ltbench.c` defines them; a new import that fails to bind shows up as the app
vanishing at the first call.

## Running it

Build (needs a Mac with Xcode and `ldid` for the armv6 link; no iOS SDK needed, the link stub
is generated from the imports):

```sh
LT_MAC=user@mac tools/guestbench/build.sh OUT_DIR     # from Linux; on a Mac just build.sh OUT_DIR
```

Install and read on Android (guest running, APK with the USB bridge):

```sh
export ANDROID_SERIAL=...
tools/guestbench/android.sh install OUT_DIR/LTBench.ipa
# tap the "LT Bench" icon in the guest: it runs at launch and again on each tap (~6 s on a Poco F3)
tools/guestbench/android.sh results          # the newest pass's lines from the engine log
```

Poco F3 layout preset: `python3 tools/perf/fcbench.py APK 0 ltbench` boots, unlocks and taps the icon
without sampling fps on the static results screen.

On the desktop, install the IPA through the app's Install IPA path (or headless: `lighttouch run ID --install
LTBench.ipa`), tap the icon, and read the `[ltbench]` lines from the engine log (`lighttouch run` prints it to the
console).

Results go to the engine log through the QEMU_CALL log channel (the same one the GLES shim
uses), one line per test:

```
[ltbench] syscall best 1234.5 median 1301.2 ns/op (getppid(), n=50000)
```

and the app's screen shows the best figure of each test when the pass is done.

Compare runs of the same engine build on the same phone, interleaved, as with games
(`docs/PERFORMANCE.md`); absolute numbers drift a little between boots.

## Results

Baselines for the Poco F3 and the desktop are in
[PERFORMANCE.md](PERFORMANCE.md#guest-benchmarks), and each engine change that targets one of these
paths records its before → after there.

What it has shown so far:
- Translated straight-line code is as fast on a Poco F3 as on a desktop (`loop` ~4 ns); what separates
  them is everything around it (system calls, switches, faults: 1.5–3× slower on the phone).
- An indirect call with more targets than the per-site branch cache holds costs 3–6× a direct one.
- An Objective-C message send costs as much as such an indirect call (desktop: `objc-send` 29.5 ns, `icall` 29.7 ns, `fib` 7.3 ns), and one run-loop poll is ~320 of them (`runloop` 9.4 µs).
- A thread round trip over pipes costs 4× a Mach RPC to another process. The vCPU is busy, not
  waiting, through it, and no emulator path stands out, so it looks like the guest kernel's own cost
  (four BSD syscalls and two scheduler switches against two Mach traps with a direct handoff).
- Keeping the user TLB per address space (rejected, PERFORMANCE.md) cut `rpc-64p`'s refill cost
  ~10× on the desktop, but cost Flight Control 3% on the Poco.
- On iOS 6 (iPod touch 4G) every syscall and IRQ from user mode switches to the kernel's own address
  space and back, so `syscall` started at ~4.5× the 2G's on the same desktop; it is ~1.9× after the
  n81 work in PERFORMANCE.md. `rpc` is ~25× the 2G's because launchd sandbox-checks each lookup:
  compare it only between iOS 6 builds.
- On the iPod touch 2G, every flash write ran the controller's write program to its step limit
  (~4 ms on the vCPU, ~250 times a second while iOS writes), so the app stalled while it saved.
  Fixed in the engine: desktop `fs-write` 381 → 148–198 µs/KiB. Reads were not affected
  (`fs-read` ~5 µs/KiB, `fs-read-16k` ~230 µs per read).

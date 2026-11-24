# Guest storage

How an emulated device's flash (NAND) is stored on the host, where the time of a guest write or an app install goes,
what is durable when, and what has been tried. Read this before working on storage speed or safety: it records the
measurements, so they needn't be redone.

Numbers are labelled by where they come from: **in-guest** = LT Bench inside iOS (below), **host counters** = the
engine's own counts of NAND page programs (also the overlay's figure), **wall** = a stopwatch on the host (an install
from its start to the app being listed). The desktop is a shared VM whose speed varies up to ~2× between hours: its
rows are interleaved A/B pairs or ranges.

## Formats

A device's NAND lives in its record (`records/ID/base` read-only, `records/ID/state` the device's own writes).

| Format | Seen in | Page program on the host | In-guest `fs-write-big`, desktop |
| --- | --- | --- | --- |
| qcow2 container: `base/nand/nand.qcow2` + the device's overlay `state/nand/nand.qcow2` | every board, devices prepared with v0.0.12 and later | `pwrite` into the overlay (the first one in a 64 KiB cluster goes through qcow2) | 2G 3.1.3 150 MB/s, 4G 42–57, 3GS 48 |
| Packed base + one file per written page | iPod touch 2G devices prepared before v0.0.12 | new temporary file, `fsync`, rename over `N.page` | 2G 3.1.3 6–8 MB/s |
| Chip files written in place | an early desktop iPod touch 2G | block-layer write, no sync until stop | 2G 4.2.1 30–42 MB/s |
| Chip files + ownership map | A4 devices prepared before v0.0.12 | `pwrite`; the map is a shared mapping | 3GS 52–57 MB/s |

Nothing converts an old device (export + import keeps its format). Old 2G page-file devices write 8–10× slower on
both hosts; **preparing the device again** gives it a container. There is no migration: the owner decided preparing
again is the answer (2026-11-23).

## What the overlay's "write" figure measures

The bytes of NAND pages the engine programmed (page plus spare) over the last second, refreshed four times a second;
"busy" is the share of wall time the host spent on them. It shows what iOS asks for, not what the device can do. A
container 4G shows 0.3–1.9 MB/s in bursts while it boots and starts a game (host busy 1–5%), 20–54 MB/s while LT
Bench writes, and 1–3 MB/s through an install's small files, where iOS's own CPU work sets the pace (below).

## The write path, and where the time goes

- **2G (S5L8720, FMSS):** the vCPU runs iOS's flash driver; the engine's FMSS model stores a write program's pages
  synchronously in its MMIO handler (`hw/arm/ipod_touch_fmss.c` → `nand-image.c`). A sequential write program carries
  5–8 pages, a synchronous one 1–2.
- **A4 boards (4G, 3GS, iPhone 4, iPad; S5L8920/8930):** iOS on the main CPU (AP) hands each NAND request to the
  storage processor (IOP), which runs Apple's IOP firmware; the engine serves its flash commands
  (`s5l8930_iop.c` → `nand-image.c`). Per request on the desktop (`F_FULLFSYNC` loop, `LT_NAND_PROF=1`): ~400 µs of
  AP time, ~235 µs of IOP time of which the IOP thread is busy ~127 µs (~250 cache-control register writes and the
  controller's MMIO), host `pread`+`pwrite` ~26 µs. The AP's part is iOS's kernel (a `perf` profile with
  `QEMU_PERFMAP_DIR`: 72% translated guest code, 99% of it kernel, flat; 6% `tb_jmp_cache_clear_page`).
- **Reads (2026-10-25):** the same hand-off; host file I/O ~3 µs a page; the AP's kernel work dominates.
- **iPhone 5 (A6):** PPN NAND through the same IOP path and container.

So long writes run at 30–150 MB/s in the guest, and small synchronous writes (a database commit, a new file) cost
a fixed ~0.5–5 ms each, nearly all guest CPU. Nothing host-side dominates; faster translated code in general is what
would move them.

### App installs

An install uploads the IPA over the emulated USB cable with AFC into `PublicStaging`, then iOS's installd unzips it.
Asphalt 5 (281 MB IPA, 4510 entries of which 4485 under 64 KiB plus one 308 MB file; 410 MB unpacked) on the
desktop, before the upload change (2026-11-23): upload ~28 s at ~10 MB/s, unpack 30 s (2G) to 52–57 s (4G, 3GS).

- **Unpack:** guest-CPU bound (AP 70–90%, IOP 25–30% on the A4 boards). installd makes no per-file sync: across the
  4G's whole unpack, 0 `F_FULLFSYNC`, 5 `fsync`, 17 `sync` (counted by `LT_EXCEPTION_PROF`), against ~3900 renames,
  ~4800 opens, ~50 800 small reads; the 2G (3.1.3) likewise (0, 4, 8). The NAND programs ~494 MB for the 410 MB app,
  plus the ~290 MB the staged IPA took.
- **Upload: the USB link was the cap.** The engine's USB transport is strictly request/response: the host bridge
  (`usbmuxd-qemu`) sends one transaction, the emulated OTG controller delivers it into the transfer the guest armed
  and answers; until iOS re-arms the endpoint every transaction is NAKed. iOS arms 16–32 KiB OUT transfers while
  the host sends 16 KiB packets (`LT_USB_STATS=1` on 3.1.3, 4.2.1, 5.1.1, 6.1.x), 4–32 KiB once it sends 32760-byte
  ones, always 32 KiB on iOS 10.3.4, and every armed size checked was a 512 multiple (4.2.1, iPad 5.1.1, 10.3.4); it
  takes ~300–600 µs per turnaround. The bridge sent at most 16 KiB per transaction, plus a zero-length packet after every full one, so
  ~8 KiB of data per guest turnaround; and each AFC write of 1 MiB waited for the guest to store it before the next
  left. Neither CPU was busy (AP ~40%, bridge ~5%): pure latency.
- **Now:** the bridge sends 32760-byte transactions (time inside the bridge's sends during an upload 22.5 → 11.9 s) (`build-support/patches/usbmuxd-qemu-large-transactions.patch`,
  applied by the Android, Linux and Windows bridge builds; macOS ships a different usbmuxd and keeps 16 KiB
  transactions), and the apps upload an
  IPA over 8 AFC sessions at once, each writing its own contiguous part (`lighttouch-services` `apps.rs`,
  `Android/native-services/lt_appservice.c`). A packet longer than the transfer iOS armed fills it and continues in
  the next, as on a real link (qemu-ios `docs/tcp-usb-protocol.md`); every install verifies the zip's CRCs. With
  sessions writing ahead of the file's end, iOS 3's HFS+ appears to zero-fill the gap: the 2G programs ~58 MB more NAND per
  Asphalt 5 upload (344 vs 286 MB); the A4 boards don't (~294 MB either way). Checked: the app installed this way
  launches and keeps running (2G 3.1.3, 4G 6.1.6, 3GS 5.1.1); the new bridge also works with v0.0.13's engine (it
  takes the engine's advertised maximum); preparation boots run on chip files, not the container, so they are
  unaffected.

## Durability

A guest NAND program reaches the host's page cache before the guest is told it is done; nothing the guest does
reaches the host as a flush (NAND has none: iOS's `F_FULLFSYNC` becomes ordinary programs).

| Event | Container devices | Old 2G page files |
| --- | --- | --- |
| Engine killed (Android stopping the app, a crash) | nothing lost: the page cache keeps every program; iOS sees a power loss (yaFTL "CXT is not valid … R/O restore", fsck clean) | nothing lost |
| Host crash or power loss | at most the last ~2 s of programs: a thread fdatasyncs the overlay every 2 s while the guest writes | pages are fsynced one by one; their renames are not, so recent pages can be lost, never torn |
| iOS stops | the overlay is synced | — |

**The periodic sync** (engine `nand-image.c`, thread `nand-sync`): every page program and erase marks the overlay
pending; every 2 s, if it is, the thread `fdatasync`s the overlay file with no lock held (not `syncfs`, which on
Android's f2fs is a whole-filesystem checkpoint, ~8.6 ms on the Poco). `fdatasync` covers every dirty page of the
file, so a program is on disk within ~2 s plus the sync's own time (≤0.3 s measured, while flushing ~100 MB). Without
it, the host's own writeback decides when (Linux's default expiry is 30 s; in the simulation below it had already
flushed within ~18 s). `LT_NAND_SYNC=off` turns it off,
`LT_NAND_SYNC_MS` sets the interval, `LT_NAND_SYNC_LOG=1` logs each sync. 2 s: the "few seconds" asked for, ≤0.5
syncs a second, and no measurable cost at that rate (Results). It bounds the loss; it does not make the overlay
crash-consistent (the overlay is opened without flushes, so qcow2's metadata ordering isn't enforced on disk between
syncs).

Inside iOS, a plain `fsync` on HFS+ does not make a new file's metadata durable at once; `F_FULLFSYNC` or iOS's own
periodic sync does. Kill tests on the desktop 4G (8 MiB `fs-write`, engine `SIGKILL`ed, restarted with Recover, then
`fs-check`): `F_FULLFSYNC` then a kill 0.4 s later: intact; plain `fsync` then a kill 2 s later: not intact; plain
`fsync` then 35 s idle: intact; clean stop: intact.

## Measuring

- **LT Bench** (`tools/guestbench`, [GUEST-BENCHMARKS.md](GUEST-BENCHMARKS.md)): `fs-write`, `fs-write-big` (uncached,
  then `fsync`: the flash path, not iOS's cache), `fs-write-cached`, `fs-rand-4k`, `fs-fsync-4k`, `fs-fullfsync-4k`,
  `fs-create`, `fs-read*`, `fs-check`. The `LT Bench` GitHub workflow (run by hand) builds the IPA plus storage-only,
  single-test and durability (`IO_FULLSYNC`) variants.
- **`tools/bench/ltbio.py RECORD`**: boots a device headless (desktop `lighttouch run --control`, or `--host android`
  over ADB with a debuggable build), unlocks, installs (`--install`), launches LT Bench and prints its results, per-thread
  CPU and the storage counters (`TAG.storage.tsv`); `--linger` for kill tests.
- **Engine switches:** `LT_NAND_PROF=1` (per 5 s: pages, requests, IOP/AP time, longest page write),
  `LT_EXCEPTION_PROF=1` (syscalls by number, `F_FULLFSYNC`s), `LT_USB_STATS=1` (armed OUT sizes, splits),
  `LT_NAND_SYNC=off` / `LT_NAND_SYNC_MS` / `LT_NAND_SYNC_LOG=1`, `QEMU_PERFMAP_DIR` (guest addresses in `perf`).
  The bridge: `USBMUXD_QEMU_MTU` (transaction size), `USBMUXD_QEMU_POLL_MS`. The app: `LT_UPLOAD_AFC` (sessions).

## Results

**App installs, wall time from the start of the upload to the app being listed** (Asphalt 5, see above; desktop
headless, `ltbio.py --install`; "before" = develop's bridge and one upload session, "after" = this branch; both on
the same engine, with the periodic sync):

| Board | Before: total (upload) | After: total (upload) | Median |
| --- | --- | --- | --- |
| iPod touch 2G 3.1.3, desktop | 57.1 (25.9), 56.5 (27.3), 65.3 (27.2) s | 45.7 (18.3), 54.2 (20.7), 51.1 (21.6) s | 57.1 → 51.1 s (−11%) |
| iPod touch 4G 6.1.6, desktop | 84.8 (27.9), 106.7 (34.1), 79.9 (27.2) s | 84.5 (24.1), 73.1 (16.4), 69.0 (16.4) s | 84.8 → 73.1 s (−14%) |
| iPhone 3GS 5.1.1, desktop | 84.7 (28.5), 93.2 (28.3), 81.9 (29.3) s | 78.7 (20.8), 76.5 (20.7), 73.6 (20.6) s | 84.7 → 76.5 s (−10%) |
| iPod touch 4G 6.1.6, Poco F3 (one pair: the phone was needed elsewhere) | 137.0 (28.7) s | 128.9 (16.3) s | |

The upload goes from ~10 MB/s to 13–17 MB/s on the desktop (17 MB/s on the Poco); the unpack is unchanged (guest
CPU). Of the before/after pairs, the first two ran while the host was building, the third alone; an earlier
interleaved 4G run with a development build of the bridge gave 83.4 / 87.9 / 82.9 → 73.2 / 67.7 / 70.2 s. The 2G
and 3GS were not measured on the Poco.

**In-guest write speed per board** (LT Bench; desktop best of 3, ranges across runs; Poco F3 one pass):

| Test | 2G 3.1.3 container | 2G 3.1.3 page files | 4G 6.1.6 container | 3GS 5.1.1 container |
| --- | --- | --- | --- | --- |
| `fs-write-big`, desktop / Poco | 150 / 93 MB/s | 6–8 / 9 MB/s | 42–57 / 34 MB/s | 48 / 31 MB/s |
| `fs-write` (64 KiB writes), desktop / Poco | 57 / 44 MB/s | 5–6 / 7 MB/s | 26–41 / 16 MB/s | 28 / 18 MB/s |
| `fs-rand-4k`, desktop / Poco | 136 / 250 µs | 568–822 / 732 µs | 305–398 / 568 µs | 305 / 615 µs |
| `fs-fsync-4k`, desktop / Poco | 0.45 / 0.66 ms | 1.5–2.0 / 2.1 ms | 0.49–0.58 / 0.74 ms | 0.42 / 0.80 ms |
| `fs-fullfsync-4k`, desktop / Poco | 0.53 / 0.77 ms | 1.7–2.0 / 2.1 ms | 2.7–4.8 / 5.4 ms | 2.8 / 4.9 ms |
| `fs-create` (16 KiB file), desktop / Poco | 0.79 / 1.3 ms | 3.2–5.6 / 4.1 ms | 1.9–4.0 / 4.7 ms | 3.2 / 4.4 ms |

## Experiments

| Experiment | Measured | Verdict |
| --- | --- | --- |
| Host bridge: 32760-byte transactions instead of 16384 (`USBMUXD_QEMU_MTU`) | desktop 4G, alone: upload 26.0 s vs ~28 (half the time inside the bridge's sends, 11.9 vs 22.5 s) | **kept**, with the next row |
| Apps: 8 AFC sessions upload the IPA at once (`LT_UPLOAD_AFC`) | desktop 4G, alone (16 KiB transactions): upload 24.9 s vs 27.6; with 32760-byte transactions: 4 sessions 20.8 s, 8 sessions 15.5–19.8 s | **kept**: see Results |
| Bridge polls the guest every 1 ms instead of 3 (`USBMUXD_QEMU_POLL_MS=1`) | desktop 4G upload 26.9 s vs 27.7 | no gain: the guest's replies were not what waited |
| Bridge retries a NAKed transaction after 0 / 100 µs instead of 1 ms | upload 27.9 / 27.6 / 28.2 s; time inside sends ~23 s either way | no gain: the guest's re-arm, not the backoff, sets the pace |
| No ZLP: 16380-byte packets (not a 512 multiple) | 4G upload 30.8 s; transactions 57 K → 40 K | no gain alone |
| Store-only IPA (entries uncompressed, so installd skips inflate) | desktop 4G, two pairs: unpack 49–60 s vs 52–53, upload +11–16 s; 2G: unpack 27.4 → 16.9 s, upload +11.8 s (old link); with the fast link 2G 45.0 → 41.5 s total, 3GS 75.2 → 81.0 s | **deferred**: a gain on armv6 boards only; needs a zip rewriter in Rust and Kotlin and a free-space check on the unpacked size |
| Unpacked app over N parallel AFC sessions + a Developer install (`lighttouch run --install-dir`) | old link, total s, N = 1 / 2 / 4 / 8 / 16: 4G 118.6 / 84.2 / 70.4 / 71.8 / 72.0 vs IPA 79–82; 2G 85.8 / – / 68.5 / 67.8 vs 56.9; 3GS 116.8 / – / 92.2 / 86.7 vs 78.8. Fast link, N = 4 / 8: 2G 69.0 / 62.3–75.6, 4G 76.6 / 67.0, 3GS 106.9 / 80.3 | **rejected**: ~3 AFC round trips per file (3904 files) cap the upload at ~6 MB/s; slower than the improved IPA path on every board. The installed app runs (2G 3.1.3, 4G 6.1.6); the IPAs are already decrypted, so no FairPlay data is involved. iOS 3.1.3 and 6 move the staged folder (install 2.3–4 s, ~30 MB NAND); iOS 5 rewrites ~245 MB of it (17–24 s). Its upload programs 500–600 MB for the 410 MB app (small-file overhead), against ~290 + ~494 MB for an IPA install |
| A guest tool that extracts a streamed archive itself, one sync at the end | installd syncs no file (counts above), so there is no per-file sync to batch | **not built**. What it could still save: the staged IPA's ~290 MB NAND write and overlapping the upload with the unpack. It needs a guest-package tool and a bulk channel into the guest (a usbmux-reachable socket), then a Developer install (iOS 5 rewrites the folder): a multi-day project |
| Several AFC sessions for the old 16 KiB link (`LT_UPLOAD_AFC=4/8`) | 4G upload 27.7 / 24.9 s vs 27.6 | little alone: the link was the cap |
| Periodic overlay sync, cost (`LT_NAND_SYNC`) | desktop 4G `fs-write-big` best of 8 (µs/KiB), 5 pairs: off 17.2 / 17.6 / 20.3 / 17.4 / 19.5, on 15.3 / 23.1 / 21.2 / 15.5 / 16.5; longest page write per 5 s window off 0.7–3.9 ms, on 1.4–7.4 ms; 4G install pairs on 81.9 / 89.4 vs off 82.6 / 112.0 s; 2G installs, 3 pairs: on 60.5 / 58.3 / 55.6 s, off 57.0 / 58.3 / 58.2 s | **kept** (2 s) |
| Power-cut simulation: freeze the engine 10 s after writes, copy the overlay's on-disk blocks (`filefrag` extents read through the block device), boot that copy | sync on: boots, fsck clean, `fs-check` intact; sync off: the same | inconclusive on this host: its own writeback had flushed the data too. The bound is argued from the sync timing |
| Migrate old 2G page-file devices to containers | – | **not done** (owner, 2026-11-23): preparing the device again gives it a container |
| Old 2G page files: drop the per-page `fsync` | host test program: desktop 62–95 µs/page vs 470–560 | rejected: a host crash can leave an empty renamed page, which reads back as zeros |
| Old 2G page files: one `syncfs` per write program | desktop 8 pages 335 µs/page; Poco 1454 µs/page vs 464 now | rejected: an f2fs checkpoint per program |
| Old 2G page files: `fsync` a program's pages from 8 threads | desktop 257–452 vs 605–660 µs/page; Poco 513–694 vs 676–755 | rejected: no clear gain on the phone |
| Old 2G page files: an append-only page log, one `fdatasync` per program | host test program: desktop 66, Poco 60 µs/page | rejected: changes how existing devices' writes are stored and recovered |
| 2G flash: a transmit's busy bit clears at once (`LT_FMSS_TX_DONE`, `3b4f4a67c3`, 2026-10-25) | desktop `fs-write` 381 → 148–198 µs/KiB (page-file device) | kept |
| NAND pages through `pread`/`pwrite` instead of the block layer's thread pool (`LT_NAND_DIRECT`, `b4db28d554`) | reads 31–42 → 3–4.5 µs, programs 41–45 → 7–17 µs; Infinity Blade II unpack 170 → 101–108 s | kept |
| A4: CDMA moves FIFO runs in one call (`LT_CDMA_BULK`, `bec13dcdbf`); PL192 ignores unchanged levels (`56505fbfdb`) | IOP MMIO 1.5 M → 0.25 M/s; 4G `fs-read` ~15% faster | kept |
| A4: the IOP's cache-control registers without the BQL; a spinning IOP; NAND I/O without the BQL | no measurable change (2026-10-22/25) | rejected |

Unpacked app over N AFC sessions (row above), upload phase, desktop: AFC requests (12 674 for the 3904 files and
602 folders) a second and CPU (AP / IOP / USB bridge):

| Board, link | N = 1 | N = 4 | N = 8 |
| --- | --- | --- | --- |
| 2G, old link | 153/s; 42% / – / 4% | 193/s; 57% / – / 4% | 195/s; 55% / – / 4% |
| 4G, old link | – | 186/s; 56% / 12% / 5% | – |
| 3GS, old link | 130/s; 59% / 8% / 4% | 170/s; 71% / 11% / 4% | 182/s; 78% / 13% / 5% |
| 2G, fast link | – | 195/s; 65% / – / 5% | 177/s; 78% / – / 6% |
| 4G, fast link | – | 172/s; 63% / 13% / 4% | 197/s; 67% / 16% / 5% |
| 3GS, fast link | – | 152/s; 79% / 11% / 4% | 202/s; 84% / 14% / 4% |

AFC requests top out near 200 a second with the guest's CPU taking over as sessions are added: afcd's per-file work
in iOS, not the link.

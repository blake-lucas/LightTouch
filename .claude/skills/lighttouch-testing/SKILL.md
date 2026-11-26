---
name: lighttouch-testing
description: How to get and run emulated test devices for LightTouch. Covers finding IPSWs and IPAs, preparing devices with games installed (firmwarekit / `lighttouch prepare`), keeping and moving device archives (export/import), running guests headless on the desktop or on Android phones over ADB, the benchmark harnesses and A/B method, and recording PGO profiles. Use it whenever a task needs a prepared iPod/iPhone/iPad device, a game or app installed in the guest, a headless or phone run, a performance measurement, a reproduction of a user's bug report, or anything about where firmware, IPAs, records, phones or test hosts live, even if the request doesn't say "test".
---

# LightTouch test environment

How to get from "I need to try this on an iPhone 4 running Epic Citadel" to a measured result, without
re-downloading what's already here or fighting the same rig problems again.

The repo docs stay the source of truth for what the tools are. This skill is the operating manual for using them.
Read `CLAUDE.md` first if you haven't. `docs/DESKTOP.md` (CLI), `tools/bench/README.md` (game matrix),
`tools/perf/README.md` (phone scripts), `docs/PERFORMANCE.md` (method and results) and `docs/BUILDING.md` (builds,
PGO) are linked from the references below rather than repeated.

## Two kinds of notes: shared and local

- `references/*.md` hold what's true for anyone working in this repo: commands, file formats, procedures, pitfalls.
- `references/local/` is **git-ignored** and holds what's true only on one machine or network: archive mount points,
  test hosts and their ssh ports, phone ADB addresses and locks, where device archives sit, quirks of this box.

**Before you start:** read `references/local/*.md` (all of them, except the template). If the folder holds only
`_TEMPLATE.md`, you're on a new machine: copy the template to `references/local/<hostname>.md` and fill in what
you find as you go.

**While you work:** whenever you find a path, address, credential location (never the secret itself), host, device
or quirk that's specific to this environment, write it into the local file, not into the shared references or the
repo docs. That keeps one developer's mounts and phones from being taken as everyone's. Shared references change
only for things that hold on any machine, and those changes get committed like code.

## Which reference to read

| Task | Read |
| --- | --- |
| Find an IPSW or an IPA (game, LT Bench) for a board and iOS version | `references/firmware-and-apps.md` |
| Make a device: prepare from an IPSW, games preinstalled, or install into a running guest | `references/preparing-devices.md` |
| Keep, copy or restore devices: device files (export/import), the standard test set, phones | `references/device-archives.md` |
| Run a guest headless on the desktop, or drive one in the Android app over ADB | `references/running-guests.md` |
| Measure something: harnesses, A/B method, profilers, where results go | `references/benchmarks.md` |
| Record or refresh a PGO profile (Android clang, desktop GCC) | `references/pgo.md` |

`scripts/ipainfo.py IPA...` prints each IPA's bundle ID, version, minimum iOS, device family and CPU slices, to
tell whether it will install and run on a given board.

## Ground rules that bite

- **Reuse before downloading.** Check the local archives (local notes) and the standard device set before fetching
  IPSWs, store copies or preparing devices from scratch. Preparation is fast (20 s to 2 min, longer for the iPhone 5),
  but downloads and first-run prompts aren't.
- **Never modify someone's working device.** Copy a record (or import its device file) into your own data folder
  and work on the copy. Prepared base images are read-only by design. A guest's writable state is its record's
  `state/`.
- **Shared machines and phones are shared.** Check the local notes for locks and for who else uses a host before
  long runs. Never `pkill -f` (kill by PID). Stop a guest cleanly before installing over its app.
- **Measurements:** interleave A/B arms in one session, keep phones cool between arms, judge capped games by CPU
  per frame instead of fps, and record every kept or rejected change in `docs/PERFORMANCE.md` in the same commit.
- **No iOS binary patches.** If a game misbehaves, the fix goes in the engine, the GL shim or the app (`CLAUDE.md`).

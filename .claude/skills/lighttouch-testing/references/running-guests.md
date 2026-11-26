# Running guests: headless desktop and Android over ADB

## Desktop CLI (headless)

`crates/lighttouch/src/cli.rs` documents every flag in its module comment; `docs/DESKTOP.md` covers the
environment. The commands you'll use most:

```sh
lighttouch run REC --seconds 120 --shot end.ppm             # boot, run, screenshot, clean iOS shutdown
lighttouch run REC --script steps.txt --frames frames.log   # timed commands; one line per guest frame
lighttouch run REC --control --no-audio                     # JSON commands on stdin, events on stdout
lighttouch run REC --recover                                # after an unclean stop ("needs recovery")
```

- **Script lines** are `<seconds> <command json>`, e.g. `40 {"cmd":"button","button":0,"down":true}` (Home).
  Touches are `{"cmd":"touch","contacts":[{"id":1,"phase":0|1|2,"x":..,"y":..}]}` in normalized guest
  coordinates (0..1 on the guest's own portrait screen).
- **Guest agent operations,** which need no screen coordinates: `{"cmd":"guest","operation":N,"argument":"..."}`.
  - 1 = frontmost app;
  - 11 = launch a bundle ID;
  - 12 = lock status (`locked=0|1`).

  These are what `tools/bench/bench.py` uses to unlock (Home, then the slider) and launch games on every board.
  The iPhone 5 unlocks with Home alone (no passcode).
- **Network:** `--network direct|archive[:yyyyMMdd]` (the Wayback proxy; mind its connection limit). Audio:
  `--no-audio`. Headphones are on by default; `--no-headphones` routes to the speaker.
- **Engine A/B switches** are environment variables (`LT_X=off`, listed in `docs/PERFORMANCE.md` per row). Set them
  on the `lighttouch` process; the worker inherits them. Pick an engine with `LIGHTTOUCH_ENGINE=…/libqemu-arm.so`.
  Engines from different checkouts can sit side by side, one folder per arm.
- **GL:** a VM without a GPU renders with llvmpipe, so GL-heavy games are host-GPU-bound there. Use it for CPU
  counts, BQL and correctness, not game frame rates. A real GPU host is in the local notes, if this network has one.
- **Logs:** the worker's stderr (engine log), the record's `logs/` and `guest-serial.log` (kernel panics land there).

For long sessions driven from another program, use `bench.py`'s `Desktop` class (`tools/bench/bench.py`) or write
JSON lines to `run --control` yourself.

## Android over ADB

- **Builds:** a debuggable side-by-side APK (`LT_APP_ID_SUFFIX=.name LT_APP_LABEL="LT Name" scripts/build.sh android`).
  Set `QEMU_IOS` to the engine checkout you mean. The engine build tree remembers the first one it was configured
  with: check `grep -o 'qemu-ios[a-z0-9-]*/configure' build/obj/android-engine-arm64/config.status` and use
  `scripts/build.sh --clean android-engine` after switching. Install with `adb install -r`, only while the app's
  guest is stopped.
- **Engine switches on a phone:** `files/debug-env` holds `KEY=VALUE` lines the engine reads at start (debug builds):
  ```sh
  adb shell "run-as PKG sh -c 'printf \"LT_X=off\n\" > files/debug-env'"
  ```
  Empty it when you're done, so the next person doesn't measure your switch.
- **Driving the app (debug builds):** `am start --activity-single-top -n PKG/org.lighttouch.android.MainActivity -a
  org.lighttouch.android.BENCH --es cmd CMD [--es arg ARG]`, answered on logcat tag `LTBench` (`CMD ARG -> answer`).
  - Device control: `select ID`, `start`, `stop`, `status`.
  - Input: `button "N press"`, `tap "x y"`, `swipe "x0 y0 x1 y1 ms"`.
  - Guest agent: `launch BUNDLE`, `frontmost`, `lockstatus`.
  - Frame timeline: `frames start NAME` / `frames stop`, which writes `files/bench/NAME.frames`.
  - Display: `scaling`, `view`.

  `tools/bench/bench.py`'s `Android` class wraps these. The app must be open for `select` to take effect.
- **Phone scripts in `tools/perf/`:**
  - `fcbench.py APK SECONDS PRESET` (with `LT_PKG` for a side-by-side install) boots, unlocks, opens Home page 2
    and measures with `gamefps.py`. Its presets follow one record's icon layout, so confirm what it opened with
    `FC_SHOT`.
  - `inappfps.py --no-start --boot-only --stop` stops a running guest cleanly.
- **Known flakes:**
  - The first start right after installing an APK has missed SpringBoard several times. Retry once before
    debugging.
  - The phone throttles when warm, so check `/sys/class/thermal/thermal_zone0/temp` or battery temperature between
    arms.
  - Wireless ADB ports change after a reboot (`adb mdns services`).
- **Etiquette:** phones are shared between agents. Take the lock the local notes describe, check `ps -A | grep
  lighttouch` for other packages' engines, stop your guests and empty your `debug-env` when you're done.

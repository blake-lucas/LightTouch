# Game benchmark matrix

Results: [docs/GAME-BENCHMARKS.md](../../docs/GAME-BENCHMARKS.md).

`bench.py` measures games across guest configs (a device and iOS version) on any host: headless on the desktop
(Linux, macOS, Windows: anything that runs the `lighttouch` command) and on an Android phone over ADB. Each run
reports, per config and game, whether the game launched and stayed up, its frame rate, 1% low and frame-time
percentiles, and how long it took to come to the front.

```sh
tools/bench/bench.py fetch                      # each game's Legacy Store copy for each config (ipa/<firmware>/)
tools/bench/bench.py prepare                    # one device per config, its games installed while it is prepared
tools/bench/bench.py warm                       # each game's first-run prompts answered once; runs start from that
tools/bench/bench.py run --host desktop         # headless, this computer
ANDROID_SERIAL=192.168.1.55:40049 LT_PKG=org.lighttouch.android.bench tools/bench/bench.py run --host android
tools/bench/bench.py report results/*/results.json
```

`--config` and `--game` (repeatable) pick a subset; `--jailbreak` prepares or runs the jailbroken devices; `--rounds N`
repeats the matrix (Android: `--cool S` seconds of rest between configs, battery temperature logged). `run --survey`
saves screenshots while each game loads, to write its steps from.

## The matrix

[matrix.json](matrix.json): `configs` (catalog firmware ids) and `games` (name, store `query`, `bundle` ID, `settle`
seconds from the launch to the measurement, `measure` seconds, `steps`). Steps run as soon as the game is in front:
`[x, y]` taps, `[x0, y0, x1, y1, ms]` swipes and plain numbers as pauses, in normalized guest coordinates (0..1 on
the guest's own screen, as screenshots show it, so a landscape game's taps are in portrait coordinates). A game whose
first-run prompts differ by version takes `"steps": {"<firmware id>": [...], "*": [...]}`.

## How it works

- **Installing games.** `lighttouch prepare --ipa GAME.ipa ...` puts each app's bundle into its own container on the
  data volume while the device is built, before its first start, which registers them the way a restore with apps
  would. A dozen games add about half a minute to a preparation, against minutes each through the USB installer.
  `lighttouch fetch FIRMWARE QUERY --bundle ID --out DIR` downloads the store copy that runs on that firmware.
- **First-run prompts.** Benchmark devices are prepared without Game Center (`prepare --no-game-center`): its
  sign-in came up over the games at every launch, and there is no network to sign in with. `warm` then launches
  each game twice on the desktop, runs its steps and taps iOS's push notice away, and keeps the state after a clean
  shutdown; both hosts start every run from it. Measured runs only tap an alert's OK when a game draws nothing.
- **iOS 6** registers only some of a dozen apps installed at once, so its configs split their games over devices
  (`apps_per_device`).
- **Desktop.** `lighttouch run RECORD --control` takes the session's commands as JSON lines on stdin as they come and
  prints every event as a JSON line on stdout (frames with their publish time, guest-agent answers, status).
- **Android.** Debuggable builds take the same verbs as `BENCH` intents (`BenchControl.kt`) and answer on logcat (tag
  `LTBench`); `frames start|stop` records the engine's frame timeline. Devices are prepared on the desktop and their
  records copied into the app with `run-as` the first time. Build a side-by-side debuggable app to keep the phone's
  own install and devices: `LT_APP_ID_SUFFIX=.bench LT_APP_LABEL="LightTouch Bench" scripts/build.sh android`.
- **Driving the guest.** The guest agent unlocks-checks (`lockstatus`), launches games by bundle ID (`SBSLaunch`),
  and says which app is in front, so nothing depends on where icons sit. Frame times are the engine's publish times
  on both hosts, so desktop and phone numbers mean the same thing.

Environment: `LT_BENCH_WORK` (default `~/Documents/LightTouch-Development/bench`: ipa/, ipsw/, data/, devices.json,
results/), `LT_BENCH_LIGHTTOUCH` (the desktop command, default `WORK/lt-dev.sh`), `LT_BENCH_IPSW` (folders searched
for IPSWs, else `prepare` downloads them), `ANDROID_SERIAL`, `LT_PKG`.

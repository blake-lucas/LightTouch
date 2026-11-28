# Preparing devices

Preparation turns a stock IPSW into a prepared device: a read-only base image plus a record with its own
writable state. `firmwarekit` (`crates/lighttouch-firmware`, shipped on every platform) does it. The desktop CLI's
`prepare` runs it and adds the result to the library. The Android app runs the same `firmwarekit` in its
`:firmware` process.

## Desktop CLI

```sh
cargo build --release -p lighttouch -p lighttouch-firmware     # target/release/{lighttouch,firmwarekit}
export LIGHTTOUCH_DATA=DIR                          # a library of your own: never someone else's
export LIGHTTOUCH_FIRMWAREKIT=target/release/firmwarekit
export LIGHTTOUCH_ENGINE=build/linux/libqemu-arm.so  # scripts/build.sh linux-engine (QEMU_IOS = the engine checkout)
export LIGHTTOUCH_GUEST_TOOLS=DIR                   # armv6.itpack/armv7.itpack + helpers (local notes say where)
target/release/lighttouch prepare n81ap-10B500 --ipsw FILE --name "iPod touch 4G test" \
    --no-game-center --ipa A.ipa --ipa B.ipa
target/release/lighttouch list                       # records: id, name, board, build
```

- **`--ipa` (repeatable):** puts each app into its own container on the data volume before the first start, which
  registers it as a restore with apps would. About 2 s per app, against minutes through the USB installer.
  Not on the iPhone 5 recipe; see below.
- **`--no-game-center`:** turns `gamed` off. Its sign-in came up over every game launch with no network to sign in
  with. Use it for any benchmark device.
- **Timings:** on a Ryzen 3700X preparation takes 20 s (2G) to about 2 min (iPhone 4). The iPhone 5 takes about
  10–14 min on Linux, longer on a phone. Ignore boot times that print without a load figure.
- **Lean by design (`CLAUDE.md`):** no backups, retries or verification boots. If a preparation fails, prepare
  again. "keybag boot: halted without the keybag" after `[effaceable:ERR ] unable to write any units` (issue 36:
  the iPhone 4 on iOS 6/7, mostly on Windows) was the SPI NOR reading busy while the host flushed its file; since
  engine bd85199519 only the write counts. If it comes back, save `work/keybag-1.log` from the job folder and report
  it. To reproduce a slow flush, sleep in the engine's `nor_flush_co` (200 ms broke the old engine).
- **Jailbroken devices:** `--jailbreak`.

`tools/bench/bench.py prepare [--config FW] [--game BUNDLE]` wraps this. It reads `$LT_BENCH_WORK/ipa/<FW>/*.ipa`
(only games listed in `tools/bench/matrix.json`), splits iOS 6 games over devices (`apps_per_device`), records each
device in `devices.json` and snapshots its fresh state into `pristine/`. Point `LT_BENCH_LIGHTTOUCH` at a wrapper
script that sets the four variables above and runs your `lighttouch`, so the bench data stays in its own folder.

## Installing into a running guest

When the recipe can't preinstall (the iPhone 5), or for an app added later, install over the emulated USB cable:

```sh
LIGHTTOUCH_USBMUXD=build/linux/usb/usbmuxd-qemu target/release/lighttouch run REC --install App.ipa
LIGHTTOUCH_USBMUXD=... target/release/lighttouch run REC --store "doodle jump"     # the Legacy Store's copy
```

`scripts/build.sh linux-usb` builds `usbmuxd-qemu`. The iPhone 5 (iOS 10) installs through system edits made at
preparation, and pairing is seeded then, so no Trust alert appears. Let the guest reach SpringBoard and stay
unlocked; the install takes about a minute per game.

## After preparing

- **First-run prompts:** push-notification notices, OpenFeint, age gates and Game Center pause games. Run
  `bench.py warm --config FW` once (desktop) to answer them with each game's `steps`. It keeps the state after a
  clean shutdown as `pristine/<record>.warm.tar`.
- **Export each finished device to a device file** (`references/device-archives.md`). Note it in the local notes'
  device table.
- **A record that says "(needs recovery)"** was stopped uncleanly; `run --recover` replays its journal. Never copy
  a running guest's record: the NAND copy is torn.

## Android

The app prepares from Add a device: IPSW download or a local file, with a progress bar per phase. Records
prepared on the desktop can be copied to a phone instead (`references/device-archives.md`), which is faster than
preparing on the phone.

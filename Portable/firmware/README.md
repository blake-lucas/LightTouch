# Firmware preparation

Every LightTouch app prepares devices with **firmwarekit** ([crates/lighttouch-firmware](../../crates/lighttouch-firmware), Rust, ported from upstream Light Touch for Mac's Swift preparer). It turns a stock IPSW into a prepared device for any board in [firmware-catalog.json](firmware-catalog.json), using only its own code: no mounts, no root, no macOS tools. The desktop app and the Android APK both run it as a separate program.

```sh
scripts/build.sh firmwarekit          # build/firmwarekit/linux/firmwarekit
scripts/build.sh firmwarekit-android  # build/firmwarekit/android (the Rust Android target, NDK clang)
scripts/build.sh firmware-tools       # on a Mac: build/firmware-tools (the guest tools preparation installs)

build/firmwarekit/linux/firmwarekit create --catalog Portable/firmware/firmware-catalog.json \
  --id n81ap-10B500 --ipsw iPod4,1_6.1.6_10B500_Restore.ipsw --out n81ap-10B500 --guest-tools build/firmware-tools
```

`create` prints one JSON event per line (`begin` with each step's expected seconds, `step`, `progress`, `warning`, `error`, `done`). iOS 4 and later boot the new system with the LightTouch engine (`firmwarekit oneshot`, which loads `LTM_QEMU_DYLIB`, else the `libqemu-arm` library beside it): once to create the data-protection keybag, and on the A4 boards once more so iOS finishes its first-start setup (`it_seal` halts it). The iPod touch 2G's keybag boot runs its boot ROM (iBoot-240.4), which no IPSW carries: pass it with `--bootrom`. On Android the boot asks the system for fast cores and performance hints.

firmwarekit reads and writes disk images itself: UDIF to raw, then HFS+ extracted to a folder and rebuilt, case-insensitive volumes included. Hard links are kept as they were when unchanged, since Android cannot make them.

Measured on Linux: n72ap-7E18 takes 39 s (no boots), n72ap-8C148 53 s, and n81ap-10B500 about 4½ minutes, against 7 minutes before it installed the preinstalled assets itself and turned off the dead services (interleaved on the same host). On a Poco F3, n72ap-7E18 takes 54 s.

The other files here are historical audit notes from the earlier n72-only C++ preparer, and the read-only shutdown evidence tools ([SHUTDOWN-EVIDENCE.md](SHUTDOWN-EVIDENCE.md)).

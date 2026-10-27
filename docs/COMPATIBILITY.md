# Compatibility runs

Which devices and iOS versions start on LightTouch, and how far they get. [`tools/compat/compat.py`](../tools/compat/compat.py)
boots a fresh device for one catalog firmware (`n81ap-10B500` and so on), presses Home once iOS has had
time to start, asks the guest tools which app is in front, shuts down, and records how far it got. It runs
the same way on headless Linux and on an Android phone; Linux is the fast loop, the phone confirms.

## Levels

Each level needs the ones before it.

| Level | Means |
| --- | --- |
| `admitted` | the engine accepted the prepared base and started |
| `lit` | the guest drew its first lit frame (iBoot or the kernel got as far as the display) |
| `tools` | LightTouch's guest tools loaded (`guest package … judged good` in the engine log) |
| `ui` | an app is in front: SpringBoard, or Setup on a base that was not set up (Android: the guest screen is lit and not just the Apple logo) |
| `clean` | iOS shut down cleanly when asked |

A firmware is **Tested** in the Android app (`testedFirmwares` in `FirmwareCatalog.kt`) once it reaches
`clean` on a phone or has been checked by hand on one (2026-10-21: iPod touch 3G 4.2.1, iPhone 3GS 5.1.1 and iPhone 4
4.2.1/4.3.5); one that runs noticeably slower than the iPod touch 2G on iOS 3.1.3 is also in
`slowFirmwares` and shows **Tested, slow**, so the 2G stays the obvious first pick.

## Running

The runner predates the Rust desktop app: it still drives the removed Linux frontend and adds devices by importing
prepared `.itbase` bases, which neither app does any more. It needs porting to `lighttouch prepare` and `lighttouch
run` (and Android's own preparation) before it runs again; the results below were recorded with it.

```sh
python3 tools/compat/compat.py prepare n81ap-10B500   # on the Mac (firmwarekit), copied back to the bases folder
python3 tools/compat/compat.py linux n81ap-10B500 n72ap-7E18
ANDROID_SERIAL=... python3 tools/compat/compat.py android n81ap-10B500
python3 tools/compat/compat.py table                   # newest result per firmware and host
```

- Bases: `~/Documents/LightTouch-Development/compat/bases/<id>.itbase` (Light Touch for Mac's packed
  folder, or an `.itbase` archive); the iPod touch 2G also needs `bootrom.bin` there. `prepare` runs
  [`mac-prepare.sh`](../tools/compat/mac-prepare.sh) on the Mac: it downloads Light Touch for Mac's
  release and the IPSW (the catalog's link, then the Internet Archive), then `firmwarekit create` and
  `pack-base`.
- Linux needs `scripts/build.sh linux` and the guest tools in `build/guest-package/`. The base is imported
  once into `compat/work/<id>/base`; every run starts from an empty state.
- Android needs a debuggable APK (it reads the engine log with `run-as`) and no device running. A
  firmware with no device yet is added through **Add device › Choose a folder**, from the base pushed to
  the phone's Download folder.
- Results: one JSON per run in `compat/results/`, with milestone times, the screens it saw, and the log.

## Results

| Config | iOS | Host | Level | Lit (CPU s) | Tools up (s) | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| n72ap-7E18 (iPod touch 2G) | 3.1.3 | Linux (Ryzen 7 3700X) | clean | 21.6 | 10.9 | SpringBoard |
| n72ap-7E18 (iPod touch 2G) | 3.1.3 | Poco F3 | clean | 24.8 | | lock screen; Start 4.5 s |
| n72ap-8C148 (iPod touch 2G) | 4.2.1 | Poco F3 | clean | | | prepared on the phone in 53 s; works fully (user check) |
| n72ap-5F138 (iPod touch 2G) | 2.1.1 | Linux (Ryzen 7 3700X) | ui | 101.0 | 38.8 | SpringBoard; prepared in 45 s, boots through the SecureROM |
| n18ap-8C148 (iPod touch 3G) | 4.2.1 | Linux (Ryzen 7 3700X) | clean | 98.4 | 33.5 | SpringBoard; prepared in 252 s |
| n90ap-8C148 (iPhone 4) | 4.2.1 | Linux (Ryzen 7 3700X) | clean | 23.9 | 24.1 | SpringBoard; prepared in 122 s |
| n90ap-8C148 (iPhone 4) | 4.2.1 | Poco F3 | | | | lock screen with carrier signal; prepared on the phone in 186 s |
| n81ap-10B500 (iPod touch 4G) | 6.1.6 | Linux (Ryzen 7 3700X) | clean | 21.1 | 76.9 | SpringBoard; Setup on a fresh base |
| n81ap-10B500 (iPod touch 4G) | 6.1.6 | Poco F3 | clean | 6.1 | | lock screen after Setup; Start 4.6 s |

Known: the iPod touch 4G logs its first lit frame at 0.0 s wall time (CPU time is right).

## Guest services

[`services.py`](../tools/compat/services.py) checks what the app services need from a running guest. It boots a fresh
state of the same base on headless Linux, with a private usbmuxd-qemu bridge as the desktop and Android apps use, and
grades how far it got:

| Level | Meaning |
| --- | --- |
| usb | usbmuxd found the guest and lockdown answered |
| install | the IPA (LT Bench by default) installed and is in the installed-app list |
| launch | the guest tools unlocked the device, launched Settings and reported it in front |
| clean | iOS shut down cleanly |

```sh
python3 tools/compat/services.py n90ap-8L1 n72ap-7E18   # LT_SERVICES_ROOT: an extracted Linux AppDir
python3 tools/compat/services.py table
```

Results go to `compat/services-results/`; the USB bridge log, screenshots and engine log go to
`compat/work/<id>/services/`.

| Config | iOS | Level | USB up (s) | Install (s) | Notes |
| --- | --- | --- | --- | --- | --- |
| n72ap-7E18 (iPod touch 2G) | 3.1.3 | clean | 48.8 | 1.2 | |
| n72ap-8C148 (iPod touch 2G) | 4.2.1 | clean | 45.9 | 4.2 | |
| n72ap-5F138 (iPod touch 2G) | 2.1.1 | usb | 27.7 | | LT Bench needs iOS 3 |
| n18ap-8C148 (iPod touch 3G) | 4.2.1 | clean | 47.9 | 3.2 | |
| n88ap-9B206 (iPhone 3GS) | 5.1.1 | clean | 53.5 | 15.5 | prepared on Linux in 135 s; first start 95 s Linux, 168 s Poco |
| n90ap-8C148 (iPhone 4) | 4.2.1 | clean | 54.6 | 3.3 | |
| n90ap-8L1 (iPhone 4) | 4.3.5 | clean | 74.0 | 4.2 | |
| n81ap-10B500 (iPod touch 4G) | 6.1.6 | install | 78.1 | 45.3 | Mac base without `--skip-setup`: Setup Assistant in front |

Linux (Ryzen 7 3700X), 2026-10-21. A4 bases prepared between 2201b407 and the fix after it never start USB (usbptpd was
disabled; every USB configuration needs it), so lockdown is never found: prepare them again.

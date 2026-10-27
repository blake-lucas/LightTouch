# Where the iOS files come from

LightTouch sets up each device from Apple's stock firmware (an IPSW). You don't need any files of your own: choose
**Get started** (or **Add device**), pick a device and iOS version, and LightTouch downloads what it needs and
prepares it. This page says what it downloads, from where, and how to use files you already have.

## The IPSW

**Download** tries, in order: the link [ipsw.me](https://ipsw.me) lists for that build, the firmware catalog's own
links, and the Internet Archive's [iOS Firmware Exhibit Collection](https://archive.org/download/iOS_Firmware_Exhibit_Collection).
Every download is checked against the catalog's checksum, and an interrupted one picks up where it left off.

**Choose a file** uses an IPSW you already have instead. It must be the exact build you picked (the file name says
it, e.g. `iPod2,1_3.1.3_7E18_Restore.ipsw`); LightTouch checks it before preparing. Apple never hosted the iPod
touch 2G's 3.1.3 update publicly and ipsw.me doesn't list it; the Internet Archive has it, and nearly every other
IPSW in the catalog.

## The boot file

The iPod touch 1G and 2G and the original iPhone start from their chip's boot ROM, which no IPSW contains. LightTouch
downloads it when you prepare one of these devices, or reuses the one another device already has:

| Device | Boot file | From |
| --- | --- | --- |
| iPod touch 2G | S5L8720 SecureROM, iBoot-240.4 | [securerom.fun](https://securerom.fun), its GitHub mirror, or devos50's qemu-ios release |
| iPod touch 1G, original iPhone | S5L8900 boot ROM | devos50's qemu-ios release |

The file is checked against its SHA-256 (the iPod touch 2G's is `55f4d8ea…ca054`). Every other device needs no boot
file.

## Guest tools

LightTouch adds its own tools inside iOS (`armv6.itpack` for the iPod touch 1G and 2G and the original iPhone,
`armv7.itpack` for every other device) when it prepares a device. They are what make app installs, the Legacy Store,
networking and fast graphics work. When an update of LightTouch carries newer ones, an existing device gets them the
next time it starts, keeping its apps and data. Each release also attaches both files.

## Preparing from the command line

The apps prepare with `firmwarekit`, which ships beside them (in the desktop app's resources folder, and inside the
Android APK). It can also run on its own:

```sh
firmwarekit create --catalog Portable/firmware/firmware-catalog.json --id n72ap-7E18 \
  --ipsw iPod2,1_3.1.3_7E18_Restore.ipsw --bootrom bootrom.bin --guest-tools GUEST-TOOLS --out n72ap-7E18
```

`GUEST-TOOLS` is the folder of guest tools preparation installs (`armv6.itpack`, `armv7.itpack`, `it_seal`, …): the
desktop app's `resources/guest-tools`, or `assets/guest-tools` in the APK. Preparation boots iOS once or twice with
the engine (`LTM_QEMU_DYLIB`, else the `libqemu-arm` library beside `firmwarekit`). [Portable/firmware](../Portable/firmware/README.md)
has the details.

## Which versions work

Every device and iOS version in the [firmware catalog](../Portable/firmware/firmware-catalog.json) can be chosen. Each
is marked **Tested** (checked on LightTouch), **Tested, slow** (works, but slower than the iPod touch 2G on iOS 3.1.3),
**Experimental** or **Untested**. On a recent phone the iPod touch 2G on iOS 3.1.3 prepares in about a minute; the
iPod touch 4G on iOS 6.1.6 takes several minutes, because preparation starts iOS once so its first-start setup is done.

Use firmware you are entitled to use, and keep firmware, boot ROMs and personal guest data out of public bug reports
and repositories.

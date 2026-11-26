# Firmware (IPSWs) and apps (IPAs)

## Which firmware a device needs

Devices are named by catalog firmware id: `<board>-<build>`, e.g. `n72ap-7E18` (iPod touch 2G, iOS 3.1.3).
`Portable/firmware/firmware-catalog.json` lists every supported one with its IPSW URL, keys and recipe
(`Portable/firmware/README.md`). The ones the test set uses, one major iOS version per board:

| Device | Firmware id | IPSW file name |
| --- | --- | --- |
| iPod touch 2G | `n72ap-7E18` (3.1.3) | `iPod2,1_3.1.3_7E18_Restore.ipsw` |
| iPod touch 3G | `n18ap-8C148` (4.2.1) | `iPod3,1_4.2.1_8C148_Restore.ipsw` |
| iPhone 3GS | `n88ap-9B206` (5.1.1) | `iPhone2,1_5.1.1_9B206_Restore.ipsw` |
| iPod touch 4G | `n81ap-10B500` (6.1.6) | `iPod4,1_6.1.6_10B500_Restore.ipsw` |
| iPhone 4 | `n90ap-10B329` (6.1.3) | `iPhone3,1_6.1.3_10B329_Restore.ipsw` |
| iPad | `k48ap-9B206` (5.1.1) | `iPad1,1_5.1.1_9B206_Restore.ipsw` |
| iPhone 5 | `n42ap-14G61` (10.3.4) | `iPhone_4.0_32bit_10.3.4_14G61_Restore.ipsw` (Apple's name) |

The original iPhone and iPod touch (S5L8900) don't boot yet.

## Getting an IPSW

1. **A local archive or mirror.** See `local/*.md` for where they are on this machine and network. File names
   follow archive.org's iOS Firmware Exhibit Collection, which is also what the catalog's URLs end with.
2. **Pass it explicitly:** `lighttouch prepare FW --ipsw FILE`. With `tools/bench/bench.py prepare`, set
   `LT_BENCH_IPSW` to a `:`-separated list of folders. It looks for the catalog URL's file name in each.
3. **Otherwise preparation downloads it** from the catalog URL (Apple or archive.org) into the data folder's
   `downloads/`. Copy anything you had to download into your local archive and note it in `local/`, so nobody
   downloads it again.

An IPSW that doesn't match the catalog's hash is refused unless the user picks "Use anyway"
(`firmwarekit identify`). For tests, use the catalog's exact build.

## Getting an IPA

**Which version runs where:**
- The 2G-era boards (2G, and 3G for many games) need an `armv6` slice.
- A4 and newer run `armv7`.
- The iPhone 5 on iOS 10 runs `armv7`/`armv7s` only, never armv6-only binaries (LT Bench is currently armv6-only).
- The app's `MinimumOSVersion` must not exceed the device's iOS. `UIDeviceFamily` must include the device: 1 is
  iPhone/iPod, 2 is iPad. iPad-only "HD" editions are separate apps with their own bundle ID, e.g.
  `com.firemint.RealRacingHD`.
- Check an IPA before using it: `python3 -I .claude/skills/lighttouch-testing/scripts/ipainfo.py FILE.ipa`.

**Sources, in order:**
1. **A local IPA archive**, if this machine has one (`local/`). Its usual layout is
   `apps/<Name>--<appid>/<Name>--<appid>--v<version>--<archs>--installable|encrypted--<sha1>.ipa`. Use only
   `installable` ones, which are decrypted; `encrypted` ones are FairPlay-encrypted and won't run.
2. **The bench workspace's per-firmware picks:** `$LT_BENCH_WORK/ipa/<firmware>/<bundle id>.ipa`. These are what
   `bench.py fetch` downloaded, already the version that runs on that firmware.
3. **The Legacy Store:** `lighttouch fetch FIRMWARE "query" --bundle ID --out DIR`, which picks the newest copy that
   runs on FIRMWARE. In the app it's the Store page. It downloads through the Wayback Machine, so stay within its
   limits (`CLAUDE.md`: at most 6 kept-alive connections).
4. **LT Bench**, our guest benchmark app: `scripts/build.sh ltbench` on a Mac (`LT_MAC`), or the `ltbench.yml` CI
   artifact (`gh workflow run ltbench.yml`). Its bundle ID is `org.lighttouch.ltbench`.

## The standard game set

What benchmarks and profiles use, per board, so results stay comparable. iOS 6 SpringBoard shows at most six
preinstalled apps per device.

| Game (bundle ID) | Boards | Why it's in the set |
| --- | --- | --- |
| LT Bench (`org.lighttouch.ltbench`) | all but the iPhone 5 (armv6 only) | CPU, Objective-C, file system and timing microbenchmarks |
| Epic Citadel (`com.epicgames.EpicCitadel`) | 3G, 3GS, 4G, iPhone 4, iPad, iPhone 5 | Unreal Engine 3: heavy GL, the frame-rate benchmark |
| Real Racing (`com.firemint.realracing`; iPad: Real Racing HD) | all | 3D racing with physics; its attract demo is a steady workload |
| Jetpack Joyride (`com.halfbrick.jetpack`) | all | 2D sprites, the Poco's long-standing benchmark |
| Tiny Wings (`com.andreasilliger.tinywings`; iPad: Tiny Wings HD) | all | procedural terrain and physics at 60 fps, a frame-pacing check |

Infinity Blade adds little over Epic Citadel (same engine). Doodle Jump, Flight Control and Cut the Rope hold 60 fps
everywhere, so they show pacing, not speed.

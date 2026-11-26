# Device archives: device files, the standard set, phones

## Device files (export / import)

A device file is one zip holding a record: its base, state, assets (boot ROM, guest package) and `record.json`.
It is the same format both apps use for Export and Import, and moves devices between machines and platforms.

```sh
lighttouch export RECORD_ID FILE.lighttouch.zip      # prints MB progress, then "exported ... in N s"
lighttouch import FILE.lighttouch.zip               # prints "imported <id> "<name>" (<board> <build>) in N s"
```

- Stop the guest first; a running record exports torn.
- Import assigns a **new record ID**. Note it before scripting against it.
- Prefer importing into a fresh `LIGHTTOUCH_DATA` per experiment and throwing the folder away afterwards, so the
  archive stays the pristine copy.
- Size: about 0.5 GB (2G) to 4 GB (iPhone 5).

## The standard test set

One device per board at one major iOS version, prepared with Game Center off and the standard games
(`references/firmware-and-apps.md`), exported as device files.

**File names:** `<firmware id>-<YYYYMMDD>.lighttouch.zip`, e.g. `n81ap-10B500-20261125.lighttouch.zip`, with a `MANIFEST.md` beside them.
The manifest records, per file:
- the firmware;
- the apps and their versions;
- the app/engine commits and guest-tools serial it was prepared with;
- whether it was warmed (first-run prompts answered).

Where they live on this machine goes in the local notes.

**Refresh** when a preparation recipe or the guest tools change, since old devices keep their old guest package.
Prepare anew and export; don't upgrade old archives in place. An old guest package is refused or updated at start
by `Portable/src/GuestPackageJudge.hpp`, so a stale archive can hide a guest-side bug.

## Phones

Both apps keep their library in `files/library/` (Android: `/data/user/0/<package>/files/library/records/<id>/`).

- **Side-by-side debug builds** (`LT_APP_ID_SUFFIX=.name LT_APP_LABEL="LT Name" scripts/build.sh android`) have their
  own data. Leave the release install's devices alone; it's not debuggable anyway, so `run-as` can't reach it.
- **Copy a record between two debuggable packages, or from the desktop to a phone,** through `run-as` and tar:
  ```sh
  adb exec-out "run-as SRC.pkg tar -cSf - -C files/library/records/ID ." |
      adb exec-in "run-as DST.pkg tar -xf - -C files/library/records/ID"     # mkdir -p the target first
  ```
  `-S` keeps sparse files sparse. About 3.6 GB took 9.5 min over Wi-Fi ADB.
- **Select a record** by writing its ID to `files/library/selected` with **no trailing newline** (`printf`, not
  `echo`). With a newline the app shows "invalid record ID" and its first-run screen. With the app open, the
  `BENCH` intent's `select ID` does the same.
- **Free space:** check `df -h /data` before copying. A state is a few GB, and holding three copies at once has filled
  a phone before.

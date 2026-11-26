# Local environment: <hostname>

<!-- Copy this file to references/local/<hostname>.md and fill it in. Everything in references/local/ except this
template is git-ignored. Record only what's true for this machine and network; anything that holds everywhere
belongs in the shared references. Never write secrets here: say where they come from (a key file, a password
manager entry, "ask the owner"). Date the facts that change (ports, free space, what's installed). -->

## This machine

- **Host name, OS, CPU/GPU:** whether GL is hardware or llvmpipe.
- **Checkouts:**
  - the app repo and its worktrees;
  - the engine repo (`QEMU_IOS`);
  - where release-layout worktrees go.
- **Build inputs:**
  - `LT_INPUTS`: never delete it;
  - guest-tools sets (serial numbers);
  - the Android SDK/NDK.
- **Disk:** free space and any rule about it.

## Firmware and apps

- **IPSW archive(s):** path or mount, naming, what's missing.
- **IPA archive(s):** path, layout, what to prefer.
- **The LT Bench IPA:** where the current build is.

## Devices

- **Standard test-set device files:** folder, manifest, date prepared.
- **Libraries in use:** `LIGHTTOUCH_DATA` folders and which records they hold. Mark which are someone else's: don't
  touch those.

## Other hosts

- **Name/address, how to reach it** (ssh port, user, key): what it's for, its rig folders, who else uses it,
  limits (no root, shared load).

## Phones

- **Name, model, GPU:** ADB address and how to find the port. Installed side-by-side packages and their records.
- **Sharing rules:** lock files, who to ask, what not to touch (e.g. the owner's release install).

## Quirks

- **Things that wasted time once:** stale build trees, shells that don't word-split, scripts that need a flag,
  flaky first boots.

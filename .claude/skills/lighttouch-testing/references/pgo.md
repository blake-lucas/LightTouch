# PGO profiles (profile-guided optimization)

A profile is execution counts from an instrumented engine, fed back to the compiler so hot code is laid out and
optimized for what games run. It is compile-time: one engine binary gets one merged profile, so merge every
board's counts with weights instead of choosing one board. Noise barely matters while recording (counts, not
timings); it matters for the A/B that decides whether a new profile ships.

## Android (clang)

`docs/BUILDING.md` ("Engine profile") has the build switches. What isn't obvious:

- **Paths must match the release layout.** Clang names static functions by their source path relative to the build
  directory. Release and CI builds compile `release-VER/app` beside `release-VER/engine`, so the profile must be
  recorded from that layout (`../../../../engine/...`), or about half its functions match nothing in releases.
  - Set up the layout with `git worktree add` of both repos into `X/app` and `X/engine`, and copy the engine's
    `subprojects/{berkeley-softfloat-3,berkeley-testfloat-3,keycodemapdb,dtc,slirp}` from a configured checkout.
  - `llvm-profdata show --all-functions` shows which prefix a profile has. A text round trip
    (`merge --text`, sed, `merge`) renames one.
- **Instrumented build:** `LT_ENGINE_VARIANT=pgogen LT_ENGINE_CFLAGS=-fprofile-generate LT_ENGINE_LDFLAGS=-fprofile-generate
  scripts/build.sh android`, with `QEMU_IOS=X/engine`, from `X/app`. Use a side-by-side suffix whose app already holds
  the records you'll play.
- **The window:**
  - `LT_PGO_TRIGGER=/data/user/0/PKG/files/pgo.trigger` and `LT_PGO_FILE=.../engine.profraw` in `files/debug-env`.
  - The driver writes `reset` into the trigger file once the guest is unlocked, so boot is left out, and `dump`
    after the last game.
  - The timed alternative is `LT_PGO_RESET` / `LT_PGO_DUMP` seconds after start, but boot length varies by board.
  - Pull the result with `adb exec-out run-as PKG cat files/engine.profraw`.
- **What to play:** each board's record with the standard games (`references/firmware-and-apps.md`), each for about
  75 s after its first-run steps. Use `bench.py`'s `run_game` for the launch and prompts.
- **Merge** with `$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-profdata merge -o OUT -weighted-input=W,FILE ...`.
  Weights should even out boards whose windows ran longer (compare `llvm-profdata show` maximum counts) and favour
  the boards most people use.
- **Ship:** build both arms in the release layout (old and new profile via `LT_ENGINE_PGO=FILE`) and A/B on a phone.
  Profile changes reach CI as a `ci-inputs-N` pre-release plus `ci/release-inputs.json`; that upload is the owner's
  call. The local default is `$LT_INPUTS/backend/pgo/engine-arm64.profdata`; keep the previous one beside it when
  replacing it.

## Desktop Linux (GCC 10, the release container)

The Linux release engine builds in the Debian 11 image (`LT_LINUX_IMAGE`, GCC 10.2) under fixed `/build` paths, so
profiles must be recorded and used from builds in that image too.

- **Instrumented:** `build-engine.py ... --cflags='-fprofile-generate=/pgo -fprofile-update=atomic'
  --ldflags='-fprofile-generate=/pgo'`. Use atomic counters, because the iPhone 5's cores run on threads. The
  `.gcda` names are the object paths mangled under `/pgo/`.
- **Collecting:** run the session with `LT_PGO_TRIGGER=FILE GCOV_PREFIX=DIR GCOV_PREFIX_STRIP=1`; the files land in
  DIR. On the desktop, `bench.py`'s `Desktop` class drives the games. Keep each board's DIR separate so they can be
  re-weighted.
- **Merge** inside the same image (`gcov-tool merge [-w a,b] -o OUT DIR1 DIR2`, pairwise).
- **Use:** `--cflags='-fprofile-use=/pgo -fprofile-correction -fprofile-partial-training -Wno-missing-profile -Wno-coverage-mismatch'` with
  the merged folder mounted at `/pgo`. Partial training keeps code no game reached (preparation, installs, video)
  optimized normally instead of for size.
- **Measure:** host A/B against a plain container build, not against a local build with another compiler.

## Windows

The Windows engine is MinGW GCC 13 (cross-built). GCC profiles depend on compiler version and object paths, so a
Linux GCC 10 profile can't serve it. It needs its own instrumented `.exe` run on Windows.

# Logs

LightTouch keeps a small set of logs on every platform so a problem can be reported after it happens, without turning anything on first. **Settings › Diagnostics › Export logs** collects them into one zip, and the device preparation pages offer **Save logs** after a preparation finishes or fails. The zip is meant to be attached to a GitHub issue, so it leaves out personal data (see [What's left out](#whats-left-out)).

## Where they live

| Platform | Folder |
| --- | --- |
| Android | the app's private `files/logs/` (not readable from outside; use Export logs) |
| Linux | `~/.local/share/LightTouch/logs/` |
| Windows | `%LOCALAPPDATA%\LightTouch\App\logs\` |
| macOS | `~/Library/Application Support/LightTouch/logs/` |

## The logs

Every log is written as it happens (nothing is buffered in memory), so the lines before a crash are kept. Each has a size ceiling; ordinary runs stay far below it.

| Log | What writes it | Starts over | Ceiling | Typical |
| --- | --- | --- | --- | --- |
| `app` | LightTouch itself: version, phone or computer, settings that matter, devices started and stopped, preparation results, errors | each time LightTouch opens | 768 KiB | 1–20 KiB |
| `engine` | the emulator engine for the latest device start: GL driver and renderer, boot, guest tools, GL refusals, shutdown | each device start | 3 MiB | 6–100 KiB |
| `guest-serial` | iOS's own console (the kernel and early boot) for the latest device start | each device start | 1 MiB exported (tail) | 2–30 KiB |
| `prepare` | the latest device preparation: each step, the one-shot boots, their serial logs | each preparation | 3 MiB | 7–40 KiB |

Measured on a Poco F3 (an iPad booted and two apps opened) and on Linux (an iPod touch 2G booted and left for a minute; a preparation, and one whose test boot was killed); the zip was 23–26 KiB.

`guest-serial` is written by the engine itself, which empties it at each start: on Android it is `logs/guest-serial.log` (the start before: `guest-serial.prev.log`); on the desktop it stays in the device's own folder as `state/serial.log` (`state/serial.prev.log`). In the zip both are `guest-serial.log`.

When a log starts over, the one before it is kept as its **previous** generation (`app.prev`, `engine.prev`, …), so reopening LightTouch or restarting a device after a crash doesn't erase the crash. Older generations are deleted. With every log full, its previous generation full, and the export's extra files, the folder stays under about 25 MiB; in practice it is a few hundred KiB.

### Head and tail

The first lines of a run say the most (versions, the GL driver, the device), and so do the last (what happened before it stopped). A capped log keeps both:

- `NAME.log` is the **head**: the first part of the run, up to its head size.
- After that, output goes to `NAME.tail.log`. When the tail passes its segment size, it becomes `NAME.tail.1.log` (replacing the older one) and a new `NAME.tail.log` starts, so the newest output is always kept.
- Each tail segment begins with a line `----- [log] tail segment N, HH:MM:SS -----`, so a reader knows output was left out between the head and the tail when N > 1.

| Log | Head | Tail segment | Segments kept | Most per run |
| --- | --- | --- | --- | --- |
| `app` | 256 KiB | 256 KiB | 2 | 768 KiB |
| `engine` | 1 MiB | 1 MiB | 2 | 3 MiB |
| `prepare` | 1 MiB | 1 MiB | 2 | 3 MiB |

The export joins a log's head and tail segments into one file in the zip.

Starting a generation: delete `NAME.prev*.log`, rename each `NAME*.log` to the same name with `.prev` after `NAME` (`engine.log` → `engine.prev.log`, `engine.tail.1.log` → `engine.prev.tail.1.log`), then open a fresh `NAME.log`.

### How each is written

- **Android engine**: the engine runs inside the `:engine` process, whose stdout and stderr point at the log file itself. A watcher thread checks the file's size every second and, past the limit, points stdout and stderr at the next segment (`dup2`). Writes never pass through a pipe, so nothing is lost if the engine crashes.
  Perf runs on a debuggable build that print all along (`LT_FRAME_LOG`, vCPU accounting, `IT_GLES_PROF`) can add `LT_LOG_UNCAPPED=1` to `files/debug-env` to keep the whole engine log in `engine.log`.
- **Child processes** (firmwarekit on both platforms; the desktop device worker): LightTouch reads the child's stderr from a pipe and writes it to the capped log. The pipe outlives a crashing child, so its last lines arrive.
- **App log**: one line at a time, appended (`O_APPEND`), from any of the app's processes.
- **Preparation serial logs**: firmwarekit removes the one-shot boots' serial logs (`work/keybag.log`, `seal.log`, …) after a success; when a preparation fails, LightTouch appends them to `prepare` (their last 256 KiB each) before the job folder goes. The lines that matter (`it_keybag: …`) are in firmwarekit's own output either way.
- **Engine crashes during a preparation**: when the engine dies of a signal inside a one-shot boot (`one-shot: halted after N s (exit -11)`), `prepare` holds a report just before that line, from `*** firmwarekit oneshot: the engine crashed` to `*** end of crash report`: the signal and fault address, the registers, the GL bridge's newest guest call (`GL bridge: inside glTexImage2D (id …)`), and the backtrace, its frames in tombstone form (`#00 pc <offset>  <library> (BuildId: …)`). Symbolize with `llvm-symbolizer --obj=<unstripped library> <offset>` or `ndk-stack -sym <dir>`; the unstripped Android engine is `build/android/engine/arm64-v8a/libqemu-arm.so` of the same build (match the BuildId). Debuggable builds can test it: `LT_ONESHOT_CRASH=30` (or `30:write`, `30:abort`) in `files/debug-env` crashes the first one-shot boot 30 s in.

## Export

The zip is named `LightTouch-logs-YYYYMMDD-HHMMSS.zip` and holds:

| In the zip | What |
| --- | --- |
| `summary.txt` | LightTouch version and commit (with `-modified` when built from uncommitted changes), platform, OS version, phone or computer model, CPU, RAM, free storage, GPU (vendor, renderer, version, extensions), the settings that change behaviour, every device (model, iOS version and build, jailbroken (read from the lock file's Cydia bootstrap, as the lock doesn't record the choice), guest tools version), which device is selected and whether it is running, and a list of the files below with their sizes |
| `app.log`, `app-previous.log` | the app log, this run and the one before |
| `engine.log`, `engine-previous.log` | the engine, latest device start and the one before |
| `guest-serial.log`, `guest-serial-previous.log` | iOS's console, the last 1 MiB of each |
| `prepare.log`, `prepare-previous.log` | the latest preparation and the one before |
| `guest/last-crash.txt` | the newest crash report inside iOS, when the device is running and its guest tools answer within a few seconds |
| `device/record.json`, `device/device.lock.json` | the selected device's description and what it was prepared from |
| `usbmuxd.log` | the newest USB bridge log, if any |
| `controllers.log` | desktop only: controllers seen and their mappings |
| `android/logcat.txt` | Android only: the last 1 MiB of LightTouch's own Android log, which includes the GPU driver's messages and crashes |
| `android/exits/` | Android only: why the engine process ended, for its last 3 exits (each trace cut to 1 MiB; a crash's trace is Android's tombstone protobuf, kept byte for byte with the app's paths blanked to the same length) |

A file that doesn't exist is left out, and `summary.txt` says so.

## What's left out

- Paths with your user name: the home folder becomes `~` and the app's data folder becomes `<data>`.
- Network settings, proxy identities and USB pairing records.
- IPSWs, firmware, NAND images, apps and anything else from inside iOS other than its console and the newest crash report.

## Housekeeping

Diagnostics that used to pile up are trimmed when LightTouch opens:

- Android: `worker-exits/` keeps the newest 10 files, traces cut to 1 MiB; `worker-observations-*`, `worker-shutdown-*` and `worker-stop-error-*` keep the newest 5 each; `hitches/` keeps the newest 5.
- Old per-start `engine-PID.log` files in each device's `state/` folder (Android) and `session-*.log` files (desktop) are deleted; the engine log now lives in `logs/`.

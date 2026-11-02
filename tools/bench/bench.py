#!/usr/bin/env python3
"""Game benchmarks across a matrix of guest configs (device + iOS version), on any host.

  bench.py fetch   [MATRIX]                 download each game's Legacy Store copy for each config (ipa/<firmware>/)
  bench.py prepare [MATRIX] [--jailbreak]   prepare one device per config with its games installed (prepare --ipa)
  bench.py push    [MATRIX]                 copy the prepared devices to the phone (run --host android does it too)
  bench.py run     [MATRIX] --host desktop|android [--config ID ...] [--game BUNDLE ...] [--rounds N]
  bench.py report  RESULTS.json [...]       Markdown tables of one or more runs

MATRIX defaults to matrix.json beside this script. Devices are prepared on the desktop (`lighttouch prepare --ipa`,
games installed in seconds while the device is built) and kept in WORK/data; `run --host android` copies a device's
record to the phone the first time (a debuggable LightTouch build: run-as, and the bench intents of BenchControl.kt).

One run boots each config once, unlocks it, then for each game: Home, launch by bundle ID (the guest agent),
wait `settle` seconds, run the game's `steps` (taps/swipes in normalized guest coordinates), record the engine's
frame timeline for `measure` seconds, and check the game is still in front. Results: WORK/results/<stamp>-<host>.json
plus a Markdown table. Frame times are the engine's publish times on both hosts (the desktop CLI's frame events,
the phone's frame timeline), so desktop and phone numbers mean the same thing.

Environment: LT_BENCH_WORK (default ~/Documents/LightTouch-Development/bench), LT_BENCH_LIGHTTOUCH (the desktop
`lighttouch` command, default WORK/lt-dev.sh), ANDROID_SERIAL, LT_PKG (default org.lighttouch.android),
LT_BENCH_IPSW (folders searched for IPSWs, ':'-separated).
"""
import argparse, datetime, json, os, re, shlex, socket, statistics, subprocess, sys, threading, time, queue
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
WORK = Path(os.environ.get("LT_BENCH_WORK", Path.home() / "Documents/LightTouch-Development/bench"))
LIGHTTOUCH = os.environ.get("LT_BENCH_LIGHTTOUCH", str(WORK / "lt-dev.sh"))
PKG = os.environ.get("LT_PKG", "org.lighttouch.android")
CATALOG = REPO / "Portable/firmware/firmware-catalog.json"
DEVICES = WORK / "devices.json"  # firmware id (+ "-jb") -> {record, apps: [bundle], name}
GUEST_FRONTMOST, GUEST_LAUNCH_APP, GUEST_LOCK_STATUS = 1, 11, 12
HOME, POWER = 0, 1


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def catalog_entry(firmware_id):
    for e in json.loads(CATALOG.read_text())["entries"]:
        if e["id"] == firmware_id:
            return e
    sys.exit(f"{firmware_id} is not in {CATALOG}")


def load_matrix(path):
    m = json.loads(Path(path).read_text())
    defaults = m.get("defaults", {})
    m["games"] = [{**defaults, **g} for g in m["games"]]
    return m


def devices():
    return json.loads(DEVICES.read_text()) if DEVICES.exists() else {}


def lt(*args, capture=True):
    cmd = shlex.split(LIGHTTOUCH) + [str(a) for a in args]
    return subprocess.run(cmd, capture_output=capture, text=True)


# ---- fetch / prepare ---------------------------------------------------------------------------------------------

def fetch(args):
    m = load_matrix(args.matrix)
    for c in selected(m["configs"], args.config, "firmware"):
        fw = c["firmware"]
        out = WORK / "ipa" / fw
        for g in selected(m["games"], args.game, "bundle"):
            if (out / f"{g['bundle']}.ipa").exists() or (out / f"{g['bundle']}.none").exists():
                continue
            r = lt("fetch", fw, g.get("query", g["name"]), "--bundle", g["bundle"], "--out", out)
            if r.returncode == 0:
                log(fw, "fetched", r.stdout.strip().split("\t")[:2])
            else:
                why = (r.stderr.strip().splitlines() or ["?"])[-1]
                log(fw, g["bundle"], "not available:", why)
                if "nothing for" in why:  # remember: no copy runs on this firmware
                    out.mkdir(parents=True, exist_ok=True)
                    (out / f"{g['bundle']}.none").write_text(why + "\n")


def find_ipsw(entry):
    name = entry["source"]["url"].rsplit("/", 1)[-1]
    dirs = os.environ.get("LT_BENCH_IPSW", f"{WORK}/ipsw:{Path.home()}/Documents/LightTouch-Development/ipsw:"
                          f"{Path.home()}/Documents/LightTouch-Development/desktop/ipsw").split(":")
    return next((Path(d) / name for d in dirs if (Path(d) / name).is_file()), None)


def prepare(args):
    m = load_matrix(args.matrix)
    known = devices()
    for c in selected(m["configs"], args.config, "firmware"):
        fw = c["firmware"]
        key = fw + ("-jb" if args.jailbreak else "")
        if key in known and not args.again:
            log(key, "already prepared:", known[key]["record"]); continue
        entry = catalog_entry(fw)
        ipas = sorted((WORK / "ipa" / fw).glob("*.ipa"))
        wanted = {g["bundle"] for g in selected(m["games"], args.game, "bundle")}
        ipas = [p for p in ipas if p.stem in wanted]
        name = f"bench {c.get('name', fw)}" + (" JB" if args.jailbreak else "")
        cmd = ["prepare", fw, "--name", name] + (["--jailbreak"] if args.jailbreak else [])
        ipsw = find_ipsw(entry)
        if ipsw:
            cmd += ["--ipsw", ipsw]
        for p in ipas:
            cmd += ["--ipa", p]
        log(key, f"preparing with {len(ipas)} apps", f"({ipsw or 'downloading the IPSW'})")
        started = time.monotonic()
        r = lt(*cmd)
        found = re.search(r"added .* \(([0-9a-f]{32})\)", r.stdout)
        if not found:
            log(key, "prepare failed:", (r.stderr.strip().splitlines() or ["?"])[-1]); continue
        known[key] = {"record": found[1], "name": name, "apps": [p.stem for p in ipas],
                      "prepared_s": round(time.monotonic() - started, 1)}
        DEVICES.write_text(json.dumps(known, indent=1) + "\n")
        snapshot(found[1])
        log(key, "prepared", found[1], f"in {known[key]['prepared_s']} s")


def selected(items, wanted, key):
    return [i for i in items if not wanted or i[key] in wanted or i.get("name") in wanted]


# ---- sessions ----------------------------------------------------------------------------------------------------

class Desktop:
    """`lighttouch run RECORD --control`: commands as JSON lines on stdin, events as JSON lines on stdout."""
    host = "desktop"

    def __init__(self, record, out):
        self.out = out
        cmd = shlex.split(LIGHTTOUCH) + ["run", record, "--control", "--no-audio"]
        self.log = open(out / f"{record}.log", "w")
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, text=True, bufsize=1)
        self.frames, self.results, self.status, self.exited = [], queue.Queue(), {}, None
        self.recording = False
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            try:
                e = json.loads(line)
            except ValueError:
                continue
            kind = e.get("event")
            if kind == "frame":
                if self.recording:
                    self.frames.append(e["published_ns"])
            elif kind == "guest_result":
                self.results.put(e)
            elif kind == "status":
                self.status = e
            elif kind == "exit":
                self.exited = e

    def send(self, cmd):
        self.p.stdin.write(json.dumps(cmd) + "\n"); self.p.stdin.flush()

    def guest(self, op, arg="", timeout=15):
        while not self.results.empty():
            self.results.get()
        self.send({"cmd": "guest", "operation": op, "argument": arg, "timeout_ms": int(timeout * 1000)})
        try:
            r = self.results.get(timeout=timeout + 5)
        except queue.Empty:
            return None
        return r["output"] if r.get("state") == 2 and r.get("status") == 0 else None

    def button(self, b):
        self.send({"cmd": "button", "button": b, "down": True}); time.sleep(0.1)
        self.send({"cmd": "button", "button": b, "down": False})

    def touch(self, phase, x, y):
        self.send({"cmd": "touch", "contacts": [{"id": 1, "phase": phase, "x": x, "y": y}]})

    def tap(self, x, y):
        self.touch(0, x, y); time.sleep(0.15); self.touch(2, x, y)

    def swipe(self, x0, y0, x1, y1, ms=300):
        self.touch(0, x0, y0)
        for i in range(1, 16):
            time.sleep(ms / 15000); f = i / 15
            self.touch(1, x0 + (x1 - x0) * f, y0 + (y1 - y0) * f)
        self.touch(2, x1, y1)

    def running(self):
        return self.exited is None and self.p.poll() is None

    def frames_start(self, name):
        self.frames = []; self.recording = True

    def frames_stop(self, name):
        self.recording = False
        return list(self.frames)

    def screenshot(self, path):
        self.send({"cmd": "screenshot", "path": str(path.parent / (path.name + ".bmp"))})

    def stop(self):
        if self.running():
            self.send({"cmd": "stop"})
        try:
            self.p.wait(timeout=120)
        except subprocess.TimeoutExpired:
            self.p.kill()
        self.log.close()
        return bool(self.exited and self.exited.get("clean"))


class Android:
    """The LightTouch app on a phone over ADB (debuggable build): BenchControl intents, answers on logcat."""
    host = "android"

    def __init__(self, record, out):
        self.out, self.record = out, record
        # the phone stays awake on the charger, its keyguard down, while the guest runs
        self.adb("shell", "svc power stayon true; input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard")
        self.adb("logcat", "-c")
        self.lines = queue.Queue()
        self.cat = subprocess.Popen(["adb", "logcat", "-v", "raw", "-s", "LTBench:I"], stdout=subprocess.PIPE, text=True)
        threading.Thread(target=lambda: [self.lines.put(l.strip()) for l in self.cat.stdout], daemon=True).start()
        self.adb("shell", "am", "start", "-n", f"{PKG}/org.lighttouch.android.MainActivity")
        time.sleep(3)
        if self.cmd("status").startswith("state=running"):
            sys.exit("a guest is running in the app; stop it first")
        self.cmd("select", record); time.sleep(1)
        self.cmd("start")
        self.started = time.monotonic()

    @staticmethod
    def adb(*a, binary=False):
        r = subprocess.run(["adb", *a], capture_output=True)
        return r.stdout if binary else r.stdout.decode(errors="replace")

    def cmd(self, cmd, arg="", timeout=30):
        while not self.lines.empty():
            self.lines.get()
        extra = f" --es arg {shlex.quote(arg)}" if arg else ""
        self.adb("shell", f"am start --activity-single-top -n {PKG}/org.lighttouch.android.MainActivity "
                          f"-a org.lighttouch.android.BENCH --es cmd {cmd}{extra} >/dev/null")
        prefix = f"{cmd} {arg}".strip() + " -> "
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            try:
                line = self.lines.get(timeout=1)
            except queue.Empty:
                continue
            if line.startswith(prefix):
                return line[len(prefix):]
        return "error: no answer"

    def guest(self, op, arg="", timeout=15):
        name = {GUEST_FRONTMOST: "frontmost", GUEST_LAUNCH_APP: "launch", GUEST_LOCK_STATUS: "lockstatus"}[op]
        r = self.cmd(name, arg, timeout + 5)
        m = re.match(r"state=2 status=0 output=(.*)", r, re.S)
        return m[1] if m else None

    def button(self, b):
        self.cmd("button", f"{b} press")

    def tap(self, x, y):
        self.cmd("tap", f"{x} {y}")

    def swipe(self, x0, y0, x1, y1, ms=300):
        self.cmd("swipe", f"{x0} {y0} {x1} {y1} {ms}")

    def running(self):
        state = self.cmd("status").split(" ")[0]
        # A first start checks a freshly copied base and can read "stopped" meanwhile: only "failed", or "stopped"
        # once that has had time, means the guest is gone.
        if state == "state=stopped" and time.monotonic() - self.started < 240:
            return True
        return state in ("state=starting", "state=running")

    def frames_start(self, name):
        self.cmd("frames", f"start {name}")

    def frames_stop(self, name):
        self.cmd("frames", "stop")
        text = self.adb("exec-out", "run-as", PKG, "cat", f"files/bench/{name}.frames")
        self.adb("shell", "run-as", PKG, "rm", "-f", f"files/bench/{name}.frames")
        return [int(l.split()[1]) for l in text.splitlines() if len(l.split()) >= 2]

    def screenshot(self, path):
        (path.parent / (path.name + ".png")).write_bytes(self.adb("exec-out", "screencap", "-p", binary=True))

    def stop(self):
        self.cmd("stop")
        s, end = "", time.monotonic() + 120
        while time.monotonic() < end:
            s = self.cmd("status")
            if s.startswith("state=stopped") or s.startswith("state=failed"):
                break
            time.sleep(2)
        self.cat.kill()
        self.adb("shell", "svc power stayon false")
        return s.startswith("state=stopped")


def pristine(record):
    return WORK / "pristine" / f"{record}.state.tar"


def snapshot(record):
    """The device's state as prepared (sparse tar): every run starts from it, on either host."""
    path = pristine(record)
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["tar", "-cSf", str(path), "-C", str(WORK / "data/library/records" / record), "--exclude",
                        "state/work", "--exclude", "state/.lease", "--exclude", "state/serial.log", "--exclude",
                        "state/engine-*.log", "state"], check=True)
    return path


def restore_desktop(record):
    rec = WORK / "data/library/records" / record
    tar = snapshot(record)
    subprocess.run(["rm", "-rf", str(rec / "state")], check=True)
    subprocess.run(["tar", "-xSf", str(tar), "-C", str(rec)], check=True)


def push_record(record):
    """The device on the phone: its base and assets copied once, its state replaced by the prepared one each time."""
    tar = snapshot(record)
    remote = f"files/library/records/{record}"
    have = Android.adb("shell", "run-as", PKG, "ls", f"{remote}/record.json").strip()
    if not have.endswith("record.json"):
        Android.adb("shell", "run-as", PKG, "mkdir", "-p", "files/library/records")
        src = WORK / "data/library/records" / record
        log("copying", record, "to the phone")
        out = subprocess.Popen(["tar", "-cSf", "-", "-C", str(src.parent), "--exclude", f"{record}/state", record],
                               stdout=subprocess.PIPE)
        subprocess.run(["adb", "exec-in", "run-as", PKG, "tar", "-xf", "-", "-C", "files/library/records"], stdin=out.stdout, check=True)
        out.wait()
    Android.adb("shell", "run-as", PKG, "rm", "-rf", f"{remote}/state")
    with open(tar, "rb") as f:
        subprocess.run(["adb", "exec-in", "run-as", PKG, "tar", "-xf", "-", "-C", remote], stdin=f, check=True)


# ---- the run -----------------------------------------------------------------------------------------------------

# A one-button alert's button (iOS's push-notification notice, an app's first-run notice) in a portrait app, then
# under a landscape app turned left and right: tried only while the game draws nothing.
ALERT_BUTTONS = [(0.5, 0.63), (0.26, 0.5), (0.74, 0.5)]
# iPhone and iPod portrait; the iPad's panel scans landscape, its slider runs up the right edge in the scan's axes
UNLOCK_SLIDES = [(0.2, 0.89, 0.95, 0.89), (0.94, 0.62, 0.94, 0.28), (0.22, 0.87, 0.95, 0.87), (0.93, 0.64, 0.93, 0.30)]


def wait_for_home(s, seconds=300):
    """The guest agent answers once SpringBoard is up; then unlock (Home wakes the screen, never sleeps it)."""
    end = time.monotonic() + seconds
    while time.monotonic() < end and s.running():
        if s.guest(GUEST_LOCK_STATUS, timeout=5) is not None:
            break
        time.sleep(3)
    else:
        return False
    time.sleep(8)  # let SpringBoard finish its launch work before the first gesture
    return unlock(s)


def unlock(s, attempts=8):
    """Unlock if the lock screen is up (it comes back with Auto-Lock between games)."""
    for attempt in range(attempts):
        status = s.guest(GUEST_LOCK_STATUS, timeout=5) or ""
        if "locked=0" in status:
            return True
        s.button(HOME); time.sleep(2.5)
        s.tap(0.5, 0.3); time.sleep(0.5)  # a throwaway touch after the wake
        s.swipe(*UNLOCK_SLIDES[attempt % len(UNLOCK_SLIDES)], ms=500); time.sleep(2)
    return "locked=0" in (s.guest(GUEST_LOCK_STATUS, timeout=5) or "")


def home(s):
    """Back to the Home screen. Only from an app: Home on the Home screen opens Spotlight, whose keyboard the alert
    taps would then type into (and its web search opens Safari)."""
    if frontmost(s) not in (None, "com.apple.springboard"):
        s.button(HOME); time.sleep(3)


def frontmost(s):
    out = s.guest(GUEST_FRONTMOST, timeout=5)
    return out.split("\n")[0].strip() if out else None


def frame_stats(published_ns, seconds):
    t = sorted(published_ns)
    if len(t) < 3:
        return {"frames": len(t), "fps": round(len(t) / seconds, 1)}
    gaps = [(b - a) / 1e6 for a, b in zip(t, t[1:])]
    gaps_sorted = sorted(gaps)
    worst = gaps_sorted[-max(1, len(gaps) // 100):]
    pct = lambda p: round(gaps_sorted[min(len(gaps_sorted) - 1, int(p * len(gaps_sorted)))], 1)
    return {"frames": len(t), "fps": round(len(t) / seconds, 1),
            "frame_ms_p50": pct(0.5), "frame_ms_p95": pct(0.95), "frame_ms_p99": pct(0.99),
            "low1_fps": round(1000 / statistics.mean(worst), 1), "hitches_100ms": sum(g > 100 for g in gaps),
            "max_gap_ms": round(gaps_sorted[-1], 1)}


def steps_for(g, firmware):
    """A game's steps: a list, or {firmware id: list, "*": list} for steps that differ by game version."""
    steps = g.get("steps", [])
    return steps.get(firmware, steps.get("*", [])) if isinstance(steps, dict) else steps


def run_game(s, g, out, tag, firmware, survey=False):
    r = {"bundle": g["bundle"], "name": g["name"]}
    home(s)
    # A notice the last game raised (iOS's "Connect to iTunes to Use Push Notifications") stays up over the next one
    # until its OK: tap where it is, which on an iPhone or iPod Home screen is the gap between two icons.
    def clear():
        if not firmware.startswith("k48"):
            s.tap(*ALERT_BUTTONS[0]); time.sleep(1.5)
        unlock(s, 4)
    clear()
    started = time.monotonic()
    tries = 1
    while s.guest(GUEST_LAUNCH_APP, g["bundle"], timeout=20) is None:
        # SpringBoard refuses for a while at times (iOS 6: error 7, the app registered): Home, clear, again
        if tries == 4:
            r["status"] = "launch refused"; s.screenshot(out / f"{tag}-refused"); return r
        tries += 1; home(s); time.sleep(5); clear()
        started = time.monotonic()
    r["launch_tries"] = tries
    while time.monotonic() - started < 60 and frontmost(s) != g["bundle"]:
        time.sleep(1)
    r["front_s"] = round(time.monotonic() - started, 1)
    if frontmost(s) != g["bundle"]:
        r["status"] = "never in front"; s.screenshot(out / f"{tag}-launch"); return r
    # first-run prompts and menus: taps, swipes and waits from the moment the game is in front
    for step in steps_for(g, firmware):
        if isinstance(step, (int, float)):
            time.sleep(step)
        elif len(step) == 2:
            s.tap(*step)
        elif len(step) == 5:
            s.swipe(*step)
    if survey:  # screens while it loads, to write steps from
        for at in (5, 10, 20, 30, 45, 60, 90, 120, 150):
            if at < g["settle"]:
                time.sleep(max(0, at - (time.monotonic() - started)))
                s.screenshot(out / f"{tag}-at{at:03d}")
    time.sleep(max(0, g["settle"] - (time.monotonic() - started)))
    s.screenshot(out / f"{tag}-settled")
    name = re.sub(r"[^A-Za-z0-9._-]", "_", tag)
    if g.get("sweep", True):  # nothing drawing: an alert may be up over the game
        def drawing():
            s.frames_start(name + "-probe"); time.sleep(2)
            return len(s.frames_stop(name + "-probe")) >= 3
        if not drawing():
            # one alert can lead to another (Game Center's sign-in, then "Game Center Selected"): a few rounds
            r["alert_taps"] = 0
            for x, y in ALERT_BUTTONS * 3:
                s.tap(x, y); r["alert_taps"] += 1; time.sleep(2)
                if drawing():
                    break
            s.screenshot(out / f"{tag}-swept")
    s.frames_start(name)
    time.sleep(g["measure"])
    frames = s.frames_stop(name)
    r.update(frame_stats(frames, g["measure"]))
    s.screenshot(out / f"{tag}-end")
    front = frontmost(s)
    r["status"] = "ok" if front == g["bundle"] else f"quit (front: {front})"
    return r


def run(args):
    m = load_matrix(args.matrix)
    known = devices()
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    out = WORK / "results" / f"{stamp}-{args.host}"
    out.mkdir(parents=True, exist_ok=True)
    host = (Android.adb("shell", "getprop", "ro.product.model").strip() if args.host == "android"
            else socket.gethostname())
    result = {"host": args.host, "machine": host, "date": stamp, "matrix": str(args.matrix), "configs": []}
    path = out / "results.json"
    for rnd in range(args.rounds):
        for c in selected(m["configs"], args.config, "firmware"):
            key = c["firmware"] + ("-jb" if args.jailbreak else "")
            dev = known.get(key)
            if not dev:
                log(key, "not prepared: bench.py prepare"); continue
            games = [g for g in selected(m["games"], args.game, "bundle") if g["bundle"] in dev["apps"]]
            cr = {"firmware": c["firmware"], "name": c.get("name", c["firmware"]), "jailbreak": args.jailbreak,
                  "record": dev["record"], "round": rnd + 1, "games": []}
            result["configs"].append(cr)
            if args.host == "android":
                push_record(dev["record"])
            else:
                restore_desktop(dev["record"])
                if args.thermal:
                    cr["battery_c_start"] = battery_c()
            log(key, f"round {rnd + 1}: starting {dev['record']}")
            s = (Android if args.host == "android" else Desktop)(dev["record"], out)
            boot = time.monotonic()
            if not wait_for_home(s):
                cr["error"] = "never unlocked"; log(key, "never unlocked"); s.screenshot(out / f"{key}-unlock")
            else:
                cr["home_s"] = round(time.monotonic() - boot, 1)
                for g in games:
                    log(key, "game", g["name"])
                    gr = run_game(s, g, out, f"{key}-r{rnd + 1}-{g['bundle']}", c["firmware"], args.survey)
                    cr["games"].append(gr)
                    log(key, g["name"], json.dumps({k: v for k, v in gr.items() if k not in ("bundle", "name")}))
                    path.write_text(json.dumps(result, indent=1) + "\n")
                    if not s.running():
                        cr["error"] = f"guest stopped during {g['name']}"; break
            cr["clean_stop"] = s.stop()
            path.write_text(json.dumps(result, indent=1) + "\n")
            if args.host == "android" and args.cool:
                time.sleep(args.cool)
    print(table(result))
    (out / "results.md").write_text(table(result) + "\n")
    log("results:", path)


def battery_c():
    m = re.search(r"temperature: (\d+)", Android.adb("shell", "dumpsys", "battery"))
    return int(m[1]) / 10 if m else None


def table(result):
    rows = [f"### {result['host']} ({result['machine']}), {result['date']}", "",
            "| Config | Game | Status | fps | 1% low | p50 / p95 / p99 ms | Hitches >100 ms | In front after |",
            "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for c in result["configs"]:
        label = c["name"] + (" JB" if c.get("jailbreak") else "")
        if c.get("error") and not c["games"]:
            rows.append(f"| {label} | | {c['error']} | | | | | |")
        for g in c["games"]:
            pcts = " / ".join(str(g.get(k, "")) for k in ("frame_ms_p50", "frame_ms_p95", "frame_ms_p99")) if "frame_ms_p50" in g else ""
            rows.append(f"| {label} | {g['name']} | {g['status']} | {g.get('fps', '')} | {g.get('low1_fps', '')} | {pcts} | "
                        f"{g.get('hitches_100ms', '')} | {g.get('front_s', '')} s |")
    return "\n".join(rows)


def push(args):
    m = load_matrix(args.matrix)
    known = devices()
    for c in selected(m["configs"], args.config, "firmware"):
        dev = known.get(c["firmware"] + ("-jb" if args.jailbreak else ""))
        if dev:
            push_record(dev["record"])


def report(args):
    for p in args.results:
        print(table(json.loads(Path(p).read_text())) + "\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="command", required=True)
    for name, fn in (("fetch", fetch), ("prepare", prepare), ("push", push), ("run", run)):
        p = sub.add_parser(name)
        p.add_argument("matrix", nargs="?", default=str(HERE / "matrix.json"))
        p.add_argument("--config", action="append", help="firmware id or config name (repeatable; default all)")
        p.add_argument("--game", action="append", help="bundle ID or game name (repeatable; default all)")
        p.add_argument("--jailbreak", action="store_true", help="the jailbroken devices (prepare: make them)")
        p.set_defaults(fn=fn)
        if name == "prepare":
            p.add_argument("--again", action="store_true", help="prepare even if devices.json has the config")
        if name == "run":
            p.add_argument("--host", choices=["desktop", "android"], required=True)
            p.add_argument("--rounds", type=int, default=1)
            p.add_argument("--cool", type=float, default=60, help="android: seconds of rest between configs")
            p.add_argument("--thermal", action="store_true", default=True)
            p.add_argument("--survey", action="store_true", help="screenshots while each game loads (for writing steps)")
    p = sub.add_parser("report"); p.add_argument("results", nargs="+"); p.set_defaults(fn=report)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()

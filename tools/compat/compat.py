#!/usr/bin/env python3
"""Compatibility runs: does a device and iOS version start on LightTouch, and how far does it get?

  compat.py prepare ID [--mac USER@HOST]   make BASES/ID.itbase with Light Touch for Mac's firmwarekit
  compat.py linux ID [ID ...]              headless Linux run per ID (base imported once, fresh state)
  compat.py android ID [ID ...]            run on the phone in ANDROID_SERIAL (adds the device if needed)
  compat.py table                          Markdown table of the newest result per ID and host

Each run boots a fresh device, presses Home once iOS has had time to start, asks the guest tools
which app is in front, shuts down, and grades how far it got (docs/COMPATIBILITY.md):
  admitted  the engine accepted the base and started
  lit       the guest drew its first lit frame
  tools     LightTouch's guest tools loaded (guest package judged good)
  ui        an app is in front (SpringBoard, or Setup on a base that was not set up)
  clean     iOS shut down cleanly when asked
"""
import argparse, datetime, hashlib, json, os, re, shutil, socket, struct, subprocess, sys, time, zlib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEV = Path.home() / "Documents/LightTouch-Development/compat"
BASES = Path(os.environ.get("LT_COMPAT_BASES", DEV / "bases"))
WORK = Path(os.environ.get("LT_COMPAT_WORK", DEV / "work"))
RESULTS = Path(os.environ.get("LT_COMPAT_RESULTS", DEV / "results"))
CATALOG = REPO / "Portable/firmware/firmware-catalog.json"
LEVELS = ["admitted", "lit", "tools", "ui", "clean"]
PACKAGE = "org.lighttouch.android"


def catalog_entry(firmware_id):
    for entry in json.loads(CATALOG.read_text())["entries"]:
        if entry["id"] == firmware_id:
            return entry
    sys.exit(f"{firmware_id} is not in {CATALOG}")


def arch(board):
    return "armv6" if board in ("n72ap", "n45ap", "m68ap") else "armv7"


def base_source(firmware_id):
    """The prepared base for an ID: a packed .itbase folder or file, or an extracted folder."""
    for candidate in (BASES / f"{firmware_id}.itbase", BASES / firmware_id):
        if candidate.exists():
            return candidate
    sys.exit(f"No base for {firmware_id} in {BASES}; run: compat.py prepare {firmware_id}")


# Milestones the engine logs on every port.
LIT = re.compile(r"\[boot\] first lit frame after ([\d.]+) s wall, ([\d.]+) s cpu")
TOOLS = re.compile(r"guest package serial (\d+) \(loader result (-?\d+)\) judged (\w+)")


def engine_milestones(text, result):
    if m := LIT.search(text):
        result["lit_wall_s"], result["lit_cpu_s"] = float(m[1]), float(m[2])
    if m := TOOLS.search(text):
        result["guest_package"] = {"serial": int(m[1]), "loader": int(m[2]), "verdict": m[3]}


def grade(result):
    reached = {
        "admitted": result.get("admitted", False),
        "lit": "lit_wall_s" in result,
        "tools": result.get("guest_package", {}).get("verdict") == "good",
        "ui": bool(result.get("frontmost")) or result.get("screen") == "ui",
        "clean": result.get("clean", False),
    }
    result["reached"] = [level for level in LEVELS if reached[level]]
    result["level"] = next((level for level in reversed(LEVELS) if reached[level]
                            and all(reached[l] for l in LEVELS[:LEVELS.index(level)])), "none")
    return result


def screen_class(pixels, width, height, channels, bottom_up=False):
    """off (dark), logo (a small bright mark on black: the Apple logo) or ui (anything lit)."""
    bright = colour = total = 0
    for y in range(0, height, 4):
        row = (height - 1 - y) if bottom_up else y
        for x in range(0, width, 4):
            i = (row * width + x) * channels
            r, g, b = pixels[i], pixels[i + 1], pixels[i + 2]
            total += 1
            if max(r, g, b) > 60:
                bright += 1
            if max(r, g, b) - min(r, g, b) > 40:
                colour += 1
    if bright < total * 0.005:
        return "off"
    if bright < total * 0.08 and colour < total * 0.01:
        return "logo"
    return "ui"


def bmp_class(path):
    data = Path(path).read_bytes()
    offset, = struct.unpack_from("<I", data, 10)
    width, height = struct.unpack_from("<ii", data, 18)
    bpp, = struct.unpack_from("<H", data, 28)
    channels = bpp // 8
    stride = (width * channels + 3) & ~3
    pixels = bytearray()
    for y in range(abs(height)):
        pixels += data[offset + y * stride: offset + y * stride + width * channels]
    # BMP rows are BGR(A); the classifier only compares channels, so order does not matter.
    return screen_class(pixels, width, abs(height), channels, bottom_up=height > 0)


def save(result):
    RESULTS.mkdir(parents=True, exist_ok=True)
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    path = RESULTS / f"{stamp}-{result['host']}-{result['id']}.json"
    path.write_text(json.dumps(result, indent=1) + "\n")
    print(f"{result['id']} on {result['host']}: {result['level']} ({', '.join(result['reached']) or 'nothing'}) -> {path}")


# --- prepare -----------------------------------------------------------------------------------

def prepare(args):
    for firmware_id in args.ids:
        entry = catalog_entry(firmware_id)
        url = entry["source"]["url"]
        mac, remote = args.mac, "~/Documents/LightTouch-Compat"
        subprocess.run(["ssh", mac, f"mkdir -p {remote}"], check=True)
        subprocess.run(["scp", "-q", str(Path(__file__).with_name("mac-prepare.sh")), f"{mac}:{remote}/prep.sh"], check=True)
        subprocess.run(["ssh", mac, f"zsh -l {remote}/prep.sh {firmware_id} '{url}'"], check=True)
        BASES.mkdir(parents=True, exist_ok=True)
        subprocess.run(["rsync", "-a", "--delete", f"{mac}:{remote}/out/{firmware_id}.itbase/", str(BASES / f"{firmware_id}.itbase")],
                       check=True)
        print(f"{firmware_id}: {BASES / (firmware_id + '.itbase')}")


# --- headless Linux -----------------------------------------------------------------------------

def import_base(exe, firmware_id, base, result):
    """Import BASES' base for an ID into `base` once; False (with result["error"]) when that fails."""
    if base.exists():
        return True
    base.parent.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    source = base_source(firmware_id).resolve()
    # An archive must match its SHA-256; a folder is checked against its own lock.
    sha256 = hashlib.sha256(source.read_bytes()).hexdigest() if source.is_file() else "0" * 64
    run = subprocess.run([str(exe), "--import", str(source), "--sha256", sha256, "--base", str(base)],
                         capture_output=True, text=True)
    result["import_s"] = round(time.monotonic() - started, 1)
    if not (base / "device.lock.json").exists():
        result["error"] = (run.stderr or run.stdout).strip().splitlines()[-1:] or ["import failed"]
        return False
    return True


# Boards that start from a boot ROM, and its file in BASES.
ROMS = {"n72ap": "bootrom.bin", "n45ap": "bootrom_s5l8900", "m68ap": "bootrom_s5l8900"}


def linux(args):
    build = REPO / "build"
    exe, engine = build / "linux/lighttouch-linux", build / "linux/libqemu-arm.so"
    for path in (exe, engine):
        if not path.exists():
            sys.exit(f"{path} missing: scripts/build.sh linux")
    for firmware_id in args.ids:
        entry = catalog_entry(firmware_id)
        work = WORK / firmware_id
        base, state, shots = work / "base", work / "state", work / "shots"
        result = {"id": firmware_id, "board": entry["board"], "build": entry["build"], "version": entry["version"],
                  "catalog_status": entry.get("status", ""), "host": f"linux-{socket.gethostname()}",
                  "date": datetime.datetime.now().isoformat(timespec="seconds"), "seconds": args.seconds}
        if not import_base(exe, firmware_id, base, result):
            save(grade(result)); continue
        for path in (state, shots):
            if path.exists():
                shutil.rmtree(path)
        shots.mkdir(parents=True)
        package = Path(args.package or build / f"guest-package/{arch(entry['board'])}.itpack")
        # Screens every 15 s; Home near the end wakes a sleeping screen, then the guest tools name the front app.
        wake = args.seconds - 40
        lines = [f"{t} screenshot {shots}/{t:03d}.bmp" for t in range(15, wake, 15)]
        lines += [f"{wake} button 0 1", f"{wake + 0.15} button 0 0", f"{wake + 4} screenshot {shots}/wake.bmp",
                  f"{wake + 6} guest frontmost", f"{wake + 20} guest frontmost", f"{args.seconds - 15} stop"]
        script = work / "script.txt"
        script.write_text("\n".join(lines) + "\n")
        command = [str(exe), "--base", str(base), "--state", str(state), "--engine", str(engine), "--package", str(package),
                   "--headless", "--seconds", str(args.seconds + 60), "--script", str(script), "--no-audio"]
        # The iPod touch 2G, the iPod touch 1G and the original iPhone start from their boot ROM.
        rom = ROMS.get(entry["board"])
        if rom:
            command += ["--rom", str(BASES / rom)]
        log = work / "run.log"
        with log.open("w") as out:
            subprocess.run(command, stdout=out, stderr=subprocess.STDOUT)
        text = log.read_text(errors="replace")
        parent = re.search(r"parent_started_monotonic_ns=(\d+)", text)
        digest = re.search(r"worker_monotonic_ns=(\d+) checkpoint_sequence=\d+ done=\d+ total=\d+ phase=post-digest", text)
        if parent and digest:
            result["admission_ms"] = (int(digest[1]) - int(parent[1])) // 1_000_000
        result["admitted"] = "admission failed" not in text and bool(re.search(r"status observed", text))
        engine_milestones(text, result)
        for m in re.finditer(r"status observed_monotonic_ns=(\d+) state=\d+ confirmed=\d+ dirty=\d+ frames=\d+ \S+ \S+ agent=1", text):
            if parent:
                result["agent_s"] = round((int(m[1]) - int(parent[1])) / 1e9, 1)
            break
        front = [m[1].strip() for m in re.finditer(r"guest request=\d+ state=\d+ status=0 bytes=\d+ output=(\S+)", text)]
        if front:
            result["frontmost"] = front[-1]
        final = re.search(r"FINAL .* confirmed=(-?\d+) dirty=(-?\d+)", text)
        result["clean"] = bool(final and final[1] == "1" and final[2] == "0")
        if (shots / "wake.bmp").exists():
            result["screen"] = bmp_class(shots / "wake.bmp")
        result["screens"] = {p.stem: bmp_class(p) for p in sorted(shots.glob("*.bmp"))}
        result["log"] = str(log)
        save(grade(result))


# --- Android ------------------------------------------------------------------------------------

class Phone:
    def __init__(self):
        if not os.environ.get("ANDROID_SERIAL"):
            sys.exit("Set ANDROID_SERIAL")
        self.serial = os.environ["ANDROID_SERIAL"]

    def adb(self, *command, binary=False, check=True):
        out = subprocess.run(["adb", *command], capture_output=True, check=check)
        return out.stdout if binary else out.stdout.decode(errors="replace")

    def nodes(self):
        self.adb("shell", "uiautomator", "dump", "/sdcard/lt-ui.xml")
        xml = self.adb("shell", "cat", "/sdcard/lt-ui.xml")
        for node in re.findall(r"<node [^>]*>", xml):
            text = re.search(r' text="([^"]*)"', node)[1]
            desc = re.search(r'content-desc="([^"]*)"', node)[1]
            box = list(map(int, re.findall(r"\d+", re.search(r'bounds="([^"]*)"', node)[1])))
            yield text, desc, box

    def find(self, label):
        return next((box for text, desc, box in self.nodes() if label in (text, desc)), None)

    def tap(self, label, wait=1.5):
        box = self.find(label)
        if not box:
            return False
        self.adb("shell", "input", "tap", str((box[0] + box[2]) // 2), str((box[1] + box[3]) // 2))
        time.sleep(wait)
        return True

    def wait_for(self, label, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if self.find(label):
                return True
            time.sleep(1)
        return False

    def run_as(self, command):
        return self.adb("shell", "run-as", PACKAGE, "sh", "-c", f"'{command}'", check=False)

    def records(self):
        """(record id, name, board, build) for every device in the app's library."""
        found = []
        for rid in self.run_as("ls files/library/records").split():
            if rid.startswith("."):
                continue
            try:
                meta = json.loads(self.run_as(f"cat files/library/records/{rid}/record.json"))
                found.append((rid, meta.get("name", ""), meta.get("board", ""), meta.get("build", "")))
            except ValueError:
                pass
        return found

    def screen(self):
        """Class of the guest screen in a raw screencap (12- or 16-byte header, RGBA)."""
        raw = self.adb("exec-out", "screencap", binary=True)
        width, height = struct.unpack_from("<II", raw, 0)
        header = len(raw) - width * height * 4
        box = self.find("Guest display")
        if not box:
            return "unknown"
        x0, y0, x1, y1 = box
        crop = bytearray()
        for y in range(y0, y1):
            start = header + (y * width + x0) * 4
            crop += raw[start:start + (x1 - x0) * 4]
        return screen_class(crop, x1 - x0, y1 - y0, 4)


def android(args):
    phone = Phone()
    for firmware_id in args.ids:
        entry = catalog_entry(firmware_id)
        result = {"id": firmware_id, "board": entry["board"], "build": entry["build"], "version": entry["version"],
                  "catalog_status": entry.get("status", ""), "date": datetime.datetime.now().isoformat(timespec="seconds"),
                  "host": "android-" + phone.adb("shell", "getprop", "ro.product.model").strip().replace(" ", "-"),
                  "seconds": args.seconds}
        phone.adb("shell", "am", "start", "-n", f"{PACKAGE}/.MainActivity")
        time.sleep(3)
        if phone.find("Stop"):
            sys.exit("A device is running in the app; stop it first")
        records = [r for r in phone.records() if (r[2], r[3]) == (entry["board"], entry["build"])]
        if not records:
            records = add_on_phone(phone, firmware_id, result)
            if not records:
                save(grade(result)); continue
        rid, name = records[0][0], records[0][1]
        result["record"] = rid
        phone.tap("Devices", 1)
        if not phone.tap(name, 2):
            result["error"] = f"device {name} not in the list"; save(grade(result)); continue
        phone.adb("logcat", "-c")
        started = time.monotonic()
        phone.tap("Start", 0)
        result["admitted"] = phone.wait_for("Stop", 120)
        result["start_s"] = round(time.monotonic() - started, 1)
        time.sleep(max(0, args.seconds - 40 - (time.monotonic() - started)))
        phone.tap("Home", 4)
        result["screen"] = phone.screen()
        shot = WORK / firmware_id / "android-wake.png"
        shot.parent.mkdir(parents=True, exist_ok=True)
        shot.write_bytes(phone.adb("exec-out", "screencap", "-p", binary=True))
        log = phone.run_as("cat files/logs/engine.log files/logs/engine.tail.1.log files/logs/engine.tail.log 2>/dev/null")
        engine_milestones(log, result)
        phone.tap("Stop", 1.5)
        phone.tap("Shut down", 0)
        result["clean"] = phone.wait_for("Start", 90) and not phone.find("Recover")
        result["screenshot"] = str(shot)
        save(grade(result))


def add_on_phone(phone, firmware_id, result):
    """Adds the base through Add device › Choose a folder, from the phone's Download folder."""
    source = base_source(firmware_id)
    folder = f"{firmware_id}.itbase"
    if phone.adb("shell", "ls", f"/sdcard/Download/{folder}", check=False).strip() == "":
        phone.adb("push", str(source), f"/sdcard/Download/{folder}")
    phone.tap("Devices", 1)
    phone.tap("Add device")
    phone.tap("Choose a folder", 2.5)
    # The picker opens where it was last; the folder's name in its title bar means it is open already.
    def shown():
        return next((box for text, _, box in phone.nodes() if text == folder), None)
    box = shown()
    if not box:
        phone.tap("Download")
        box = shown()
    if box and box[1] > 300:
        phone.tap(folder)
    phone.tap("Use this folder")
    phone.tap("Allow")
    if not phone.wait_for("Create device", 30):
        result["error"] = "the folder was not recognised"; return []
    started = time.monotonic()
    phone.tap("Create device", 0)
    end = time.monotonic() + 600
    while time.monotonic() < end and not phone.find("Not now"):
        time.sleep(2)
    result["import_s"] = round(time.monotonic() - started, 1)
    if not phone.tap("Not now"):
        result["error"] = "import did not finish"; return []
    entry = catalog_entry(firmware_id)
    return [r for r in phone.records() if (r[2], r[3]) == (entry["board"], entry["build"])]


# --- table --------------------------------------------------------------------------------------

def table(args):
    newest = {}
    for path in sorted(RESULTS.glob("*.json")):
        r = json.loads(path.read_text())
        newest[(r["id"], r["host"])] = r
    print("| Config | iOS | Host | Level | Lit (CPU s) | Tools up (s) | Front app | Notes |")
    print("| --- | --- | --- | --- | --- | --- | --- | --- |")
    for (firmware_id, host), r in sorted(newest.items()):
        notes = r.get("error", "") or (f"start {r['start_s']} s" if "start_s" in r else
                                       f"admission {r['admission_ms']} ms" if "admission_ms" in r else "")
        print(f"| {firmware_id} | {r['version']} | {host} | {r['level']} | {r.get('lit_cpu_s', '')} | {r.get('agent_s', '')} | "
              f"{r.get('frontmost', r.get('screen', ''))} | {notes} |")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("prepare"); p.add_argument("ids", nargs="+"); p.add_argument("--mac", default="blake@192.168.1.54")
    p.set_defaults(run=prepare)
    for name, run in (("linux", linux), ("android", android)):
        p = sub.add_parser(name); p.add_argument("ids", nargs="+")
        p.add_argument("--seconds", type=int, default=150, help="how long the guest runs before it is shut down")
        p.add_argument("--package", help="guest tools (default build/guest-package/<arch>.itpack)")
        p.set_defaults(run=run)
    sub.add_parser("table").set_defaults(run=table)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()

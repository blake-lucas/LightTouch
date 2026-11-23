#!/usr/bin/env python3
"""LT Bench storage runs on the desktop, headless: boot a record, unlock, launch LT Bench, collect its lines, stop.

  ltbio.py RECORD [--host desktop|android] [--install LTBench.ipa] [--recover] [--passes N] [--env K=V ...] [--out DIR] [--tag NAME]

The app runs one pass at launch and another on each tap. Uses bench.py's desktop session (`lighttouch run --control`)
with LT_BENCH_LIGHTTOUCH (default WORK/lt-dev.sh, one word); --env values reach the worker, so engine A/B switches go there.
Prints one line per test (`name best median`) and writes OUT/TAG.log (the session's stderr, engine log included).
--host android drives the LT_PKG app over ADB instead (debuggable build; tools/guestbench/android.sh installs).
"""
import argparse, os, re, sys, time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench  # noqa: E402

BUNDLE = "org.lighttouch.ltbench"

p = argparse.ArgumentParser()
p.add_argument("record")
p.add_argument("--host", choices=["desktop", "android"], default="desktop",
               help="android: the LT_PKG app over ADB (debuggable build; ANDROID_SERIAL)")
p.add_argument("--install")
p.add_argument("--passes", type=int, default=1)
p.add_argument("--recover", action="store_true", help="start a device that was not shut down cleanly")
p.add_argument("--env", action="append", default=[])
p.add_argument("--out", default=".")
p.add_argument("--tag", default="ltbio")
p.add_argument("--timeout", type=float, default=900, help="seconds for one pass")
p.add_argument("--launch", help="instead of LT Bench: launch this bundle id and watch for --watch seconds")
p.add_argument("--watch", type=float, default=60)
a = p.parse_args()
out = Path(a.out); out.mkdir(parents=True, exist_ok=True)
for kv in a.env:
    k, v = kv.split("=", 1)
    os.environ[k] = v

# Desktop runs LIGHTTOUCH run REC --control --no-audio; these ride on the end, through a wrapper script
extra = (["--install", str(Path(a.install).resolve())] if a.install else []) + (["--recover"] if a.recover else [])
if extra:
    wrapper = out / f"{a.tag}.wrap.sh"
    wrapper.write_text("#!/bin/sh\nexec " + bench.LIGHTTOUCH + ' "$@" ' + " ".join(bench.shlex.quote(x) for x in extra) + "\n")
    wrapper.chmod(0o755)
    bench.LIGHTTOUCH = str(wrapper)

storage_log = open(out / f"{a.tag}.storage.tsv", "w")


class Recorder(bench.Desktop):
    """Also writes each status event's storage counters (the overlay's source) to TAG.storage.tsv."""

    def _read(self):
        import json
        for line in self.p.stdout:
            try:
                e = json.loads(line)
            except ValueError:
                continue
            kind = e.get("event")
            if kind == "guest_result":
                self.results.put(e)
            elif kind == "status":
                self.status = e
                st = e.get("storage")
                if st:
                    storage_log.write(f"{time.monotonic() - t0:.3f}\t" + "\t".join(
                        str(st.get(k, 0)) for k in ("read_ops", "read_bytes", "read_ns", "write_ops", "write_bytes",
                                                    "write_ns", "erase_ops", "erase_ns")) + "\n")
                    storage_log.flush()
            elif kind == "exit":
                self.exited = e


t0 = time.monotonic()
if a.host == "android":
    s = bench.Android(a.record, out)
    logpath = out / f"{a.tag}.android.log"
    logpath.write_text("")
else:
    s = Recorder(a.record, out)
    logpath = out / f"{a.record}.log"


def run_as(script):
    return bench.Android.adb("exec-out", "run-as", bench.PKG, "sh", "-c", script)


def worker_pid():
    if a.host == "android":
        out = (bench.Android.adb("shell", "pidof", bench.PKG + ":engine") or bench.Android.adb("shell", "pidof", bench.PKG)).split()
        return int(out[0]) if out else None
    for d in Path("/proc").iterdir():
        try:
            if d.name.isdigit() and b"worker" in (d / "cmdline").read_bytes() and \
                    int((d / "stat").read_text().rsplit(")", 1)[1].split()[1]) == s.p.pid:
                return int(d.name)
        except (OSError, ValueError, IndexError):
            pass
    return None


def thread_cpu(pid):
    """Thread name -> CPU seconds (user + system), threads of one name summed."""
    out = {}
    if a.host == "android":
        stats = run_as(f"for t in /proc/{pid}/task/*; do cat $t/stat; done").splitlines()
        tick = 100
    else:
        stats = []
        for t in Path(f"/proc/{pid}/task").iterdir():
            try:
                stats.append((t / "stat").read_text())
            except OSError:
                pass
        tick = os.sysconf("SC_CLK_TCK")
    for st in stats:
        try:
            name = st[st.index("(") + 1:st.rindex(")")]
            f = st.rsplit(")", 1)[1].split()
            out[name] = out.get(name, 0) + (int(f[11]) + int(f[12])) / tick
        except (ValueError, IndexError):
            pass
    return out


def log_text():
    if a.host == "android":   # the engine log's LT Bench lines, kept in TAG.android.log
        text = run_as("cat files/logs/engine.log 2>/dev/null | grep -a ltbench")
        logpath.write_text(text)
        return text
    return logpath.read_text(errors="replace")


seen_lines = set()


def wait_for(pattern, seconds, start=0):
    end = time.monotonic() + seconds
    while time.monotonic() < end and s.running():
        text = log_text()
        for line in text[start:].splitlines():   # when each LT Bench line appeared, for the storage log
            if "[ltbench]" in line and line not in seen_lines:
                seen_lines.add(line)
                storage_log.write(f"# {time.monotonic() - t0:.3f} {line}\n")
        m = re.search(pattern, text[start:])
        if m:
            return m
        time.sleep(0.25 if a.host == "desktop" else 2)
    return None


try:
    if not bench.wait_for_home(s):
        sys.exit("no home screen")
    print(f"home at {time.monotonic() - t0:.0f} s", flush=True)
    if a.install and a.host == "android":
        r = bench.subprocess.run([str(bench.REPO / "tools/guestbench/android.sh"), "install", a.install],
                                 env={**os.environ, "LT_PKG": bench.PKG}, capture_output=True, text=True)
        print("install:", (r.stdout + r.stderr).strip()[-300:], flush=True)
    elif a.install and not wait_for(r"installed |install failed", 600):
        sys.exit("install did not finish")
    pid = worker_pid()
    if a.launch:
        bench.home(s)
        print(f"launch {a.launch} at {time.monotonic() - t0:.1f} s: "
              f"{s.guest(bench.GUEST_LAUNCH_APP, a.launch, timeout=20) is not None}", flush=True)
        c0, w0 = thread_cpu(pid) if pid else {}, time.monotonic()
        time.sleep(a.watch)
        if pid:
            c1, wall = thread_cpu(pid), time.monotonic() - w0
            busy = sorted(((c1.get(k, 0) - c0.get(k, 0), k) for k in c1), reverse=True)[:4]
            print(f"{a.tag} watch threads over {wall:.1f} s: " +
                  ", ".join(f"{k} {v / wall * 100:.0f}%" for v, k in busy), flush=True)
    for n in range(1, a.passes + 1 if not a.launch else 0):
        mark = len(log_text())
        cpu0, w0 = thread_cpu(pid) if pid else {}, time.monotonic()
        if n == 1:
            bench.home(s)
            if s.guest(bench.GUEST_LAUNCH_APP, BUNDLE, timeout=20) is None:
                sys.exit("launch failed")
        else:
            s.tap(0.5, 0.5)
        if not wait_for(r"\[ltbench\] done pass", a.timeout, mark):
            sys.exit(f"pass {n} did not finish")
        if pid:
            cpu1, wall = thread_cpu(pid), time.monotonic() - w0
            busy = sorted(((cpu1.get(k, 0) - cpu0.get(k, 0), k) for k in cpu1), reverse=True)[:6]
            print(f"{a.tag} pass{n} threads over {wall:.1f} s: " +
                  ", ".join(f"{k} {v / wall * 100:.0f}%" for v, k in busy), flush=True)
        for line in log_text()[mark:].splitlines():
            m = re.search(r"\[ltbench\] (\S+) best (\S+) median (\S+)", line)
            if m:
                print(f"{a.tag} pass{n} {m[1]} {m[2]} {m[3]}", flush=True)
            elif "[ltbench]" in line and "unavailable" in line:
                print(f"{a.tag} pass{n} {line.split('[ltbench] ')[1]}", flush=True)
finally:
    clean = s.stop()
    print(f"stopped clean={clean} after {time.monotonic() - t0:.0f} s", flush=True)
    if a.host == "desktop":
        os.replace(logpath, out / f"{a.tag}.log")

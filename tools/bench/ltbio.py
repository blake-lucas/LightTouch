#!/usr/bin/env python3
"""LT Bench storage runs on the desktop, headless: boot a record, unlock, launch LT Bench, collect its lines, stop.

  ltbio.py RECORD [--install LTBench.ipa] [--recover] [--passes N] [--env K=V ...] [--out DIR] [--tag NAME]

The app runs one pass at launch and another on each tap. Uses bench.py's desktop session (`lighttouch run --control`)
with LT_BENCH_LIGHTTOUCH (default WORK/lt-dev.sh, one word); --env values reach the worker, so engine A/B switches go there.
Prints one line per test (`name best median`) and writes OUT/TAG.log (the session's stderr, engine log included).
"""
import argparse, os, re, sys, time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bench  # noqa: E402

BUNDLE = "org.lighttouch.ltbench"

p = argparse.ArgumentParser()
p.add_argument("record")
p.add_argument("--install")
p.add_argument("--passes", type=int, default=1)
p.add_argument("--recover", action="store_true", help="start a device that was not shut down cleanly")
p.add_argument("--env", action="append", default=[])
p.add_argument("--out", default=".")
p.add_argument("--tag", default="ltbio")
p.add_argument("--timeout", type=float, default=900, help="seconds for one pass")
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

t0 = time.monotonic()
s = bench.Desktop(a.record, out)
logpath = out / f"{a.record}.log"


def log_text():
    return logpath.read_text(errors="replace")


def wait_for(pattern, seconds, start=0):
    end = time.monotonic() + seconds
    while time.monotonic() < end and s.running():
        m = re.search(pattern, log_text()[start:])
        if m:
            return m
        time.sleep(1)
    return None


try:
    if not bench.wait_for_home(s):
        sys.exit("no home screen")
    print(f"home at {time.monotonic() - t0:.0f} s", flush=True)
    if a.install and not wait_for(r"installed |install failed", 600):
        sys.exit("install did not finish")
    for n in range(1, a.passes + 1):
        mark = len(log_text())
        if n == 1:
            bench.home(s)
            if s.guest(bench.GUEST_LAUNCH_APP, BUNDLE, timeout=20) is None:
                sys.exit("launch failed")
        else:
            s.tap(0.5, 0.5)
        if not wait_for(r"\[ltbench\] done pass", a.timeout, mark):
            sys.exit(f"pass {n} did not finish")
        for line in log_text()[mark:].splitlines():
            m = re.search(r"\[ltbench\] (\S+) best (\S+) median (\S+)", line)
            if m:
                print(f"{a.tag} pass{n} {m[1]} {m[2]} {m[3]}", flush=True)
            elif "[ltbench]" in line and "unavailable" in line:
                print(f"{a.tag} pass{n} {line.split('[ltbench] ')[1]}", flush=True)
finally:
    clean = s.stop()
    print(f"stopped clean={clean} after {time.monotonic() - t0:.0f} s", flush=True)
    os.replace(logpath, out / f"{a.tag}.log")

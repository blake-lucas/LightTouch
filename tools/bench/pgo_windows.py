#!/usr/bin/env python3
"""pgo_desktop.py for a Windows PC over ssh: each board boots with the instrumented engine, unlocks, resets the
counters, plays its games SECONDS each through bench.py, and writes its .gcda set to ROOT\\pgo\\TAG on the PC.

The PC needs OpenSSH Server with key login. Under ROOT (default C:\\lt-pgo): lighttouch.exe (a release or test exe),
runtime-gen\\ (that exe's unpacked runtime with the instrumented libqemu-arm.dll from
`LT_ENGINE_PGO=gen Windows/tools/build-engine.sh` + make-shared-windows.py) and data\\ (a library holding the
standard test devices, `lighttouch import`). A session started over ssh draws with the PC's GPU (WGL), as one on its
desktop does. Bring the folders back with scp and merge them with x86_64-w64-mingw32-gcov-tool (docs/BUILDING.md).

usage: LT_WIN_HOST=user@pc LT_BENCH_WORK=DIR pgo_windows.py OUT FIRMWARE:BUNDLE,BUNDLE[:SECONDS] ...
DIR/devices.json maps each firmware to its record ID on the PC (bench.py's format)."""
import json, os, shlex, subprocess, sys, time
from pathlib import Path
HOST = os.environ["LT_WIN_HOST"]
ROOT = os.environ.get("LT_WIN_ROOT", "C:\\lt-pgo")
HERE = Path(__file__).resolve().parent
# bench.Desktop runs LT_BENCH_LIGHTTOUCH: the exe on the PC, stdin and stdout piped through ssh
env = f"set LIGHTTOUCH_DATA={ROOT}\\data&& set LIGHTTOUCH_RESOURCES={ROOT}\\runtime-gen&&"
os.environ["LT_BENCH_LIGHTTOUCH"] = shlex.join(["ssh", "-T", "-o", "ServerAliveInterval=30", HOST]) + " " + \
    shlex.quote(env + " %LTD_SET% " + f"{ROOT}\\lighttouch.exe")
import bench as B  # noqa: E402 (reads LT_BENCH_LIGHTTOUCH)


def ssh(cmd):
    return subprocess.run(["ssh", "-T", HOST, cmd], capture_output=True, text=True).stdout


out = Path(sys.argv[1]).resolve(); out.mkdir(parents=True, exist_ok=True)
games = {g["bundle"]: g for g in B.load_matrix(os.environ.get("LT_BENCH_MATRIX", HERE / "matrix.json"))["games"]}
known = B.devices()
for spec in sys.argv[2:]:
    key, bundles, *rest = spec.split(":")
    secs = int(rest[0]) if rest else 75
    rec = known[key]["record"]; tag = key.replace("#", "-")
    trig, gcda = f"{ROOT}\\pgo\\{tag}.trigger", f"{ROOT}\\pgo\\{tag}"
    ssh(f"mkdir {ROOT}\\pgo 2>nul & del /q {trig} 2>nul & rmdir /s /q {gcda} 2>nul & mkdir {gcda}")
    sets = f"set LT_PGO_TRIGGER={trig}&& set GCOV_PREFIX={gcda}&& set GCOV_PREFIX_STRIP=1&&"
    B.LIGHTTOUCH = os.environ["LT_BENCH_LIGHTTOUCH"].replace("%LTD_SET%", sets)
    B.log(tag, rec, bundles)
    s = B.Desktop(rec, out)
    if not B.wait_for_home(s, 600):
        B.log(tag, "never unlocked"); s.stop(); continue
    ssh(f"echo reset>{trig}"); t0 = time.monotonic()
    for b in bundles.split(","):
        g = dict(games.get(b, {"name": b, "bundle": b, "settle": 40}), measure=secs)
        r = B.run_game(s, g, out, f"{tag}-{b}", key.split("#")[0])
        B.log(tag, g["name"], json.dumps({k: v for k, v in r.items() if k in ("status", "front_s")}))
    ssh(f"echo dump>{trig}"); time.sleep(8)
    files = ssh(f"dir /b {gcda} | find /c \".gcda\"").strip()
    B.log(tag, "window %.0f s, %s gcda files" % (time.monotonic() - t0, files))
    s.stop(); time.sleep(10)

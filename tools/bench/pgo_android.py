#!/usr/bin/env python3
"""Android engine PGO counters per board, through the bench harness (tools/bench/bench.py) and the Poco's bench app's
records: boot, unlock, then reset the counters (LT_PGO_TRIGGER), play each game MEASURE s with bench.py's launch and
first-run steps, write the profile and pull it.
usage: ANDROID_SERIAL=... LT_PKG=org.lighttouch.android.bench pgo_android.py OUTDIR FIRMWARE:RECORD:BUNDLE,BUNDLE[:SECONDS] ..."""
import json, os, sys, time
from pathlib import Path
import bench as B
out = Path(sys.argv[1]).resolve(); out.mkdir(parents=True, exist_ok=True)
games = {g["bundle"]: g for g in json.load(open(B.HERE / "matrix.json"))["games"]}
files = f"/data/user/0/{B.PKG}/files"
for spec in sys.argv[2:]:
    fw, rec, bundles, *rest = spec.split(":")
    secs = int(rest[0]) if rest else 75
    tag = fw.split("-")[0] + "-" + rec[:4]
    B.log(tag, "record", rec, bundles)
    B.Android.adb("shell", "run-as", B.PKG, "rm", "-f", "files/pgo.trigger", "files/engine.profraw")
    B.Android.adb("shell", f"run-as {B.PKG} sh -c 'printf \"LT_PGO_TRIGGER={files}/pgo.trigger\\nLT_PGO_FILE={files}/engine.profraw\\n\" > files/debug-env'")
    s = B.Android(rec, out)
    if not B.wait_for_home(s, 600):  # a first start after an install has missed SpringBoard: once more
        B.log(tag, "never unlocked, retrying"); s.screenshot(out / f"{tag}-boot"); s.stop(); time.sleep(20)
        s = B.Android(rec, out)
        if not B.wait_for_home(s, 600):
            B.log(tag, "never unlocked"); s.screenshot(out / f"{tag}-boot2"); s.stop(); continue
    B.Android.adb("shell", f"run-as {B.PKG} sh -c 'echo reset > files/pgo.trigger'")
    t0 = time.monotonic()
    for b in bundles.split(","):
        g = dict(games.get(b, {"name": b, "bundle": b, "settle": 40}), measure=secs)
        r = B.run_game(s, g, out, f"{tag}-{b}", fw)
        B.log(tag, g["name"], json.dumps({k: v for k, v in r.items() if k in ("status", "fps", "front_s")}))
    B.Android.adb("shell", f"run-as {B.PKG} sh -c 'echo dump > files/pgo.trigger'")
    time.sleep(8)
    data = B.Android.adb("exec-out", "run-as", B.PKG, "cat", "files/engine.profraw", binary=True)
    B.log(tag, "window %.0f s, profraw %d bytes" % (time.monotonic() - t0, len(data)))
    if data:
        (out / f"{tag}.profraw").write_bytes(data)
    s.stop()
    B.Android.adb("shell", "run-as", B.PKG, "rm", "-f", "files/debug-env")
    time.sleep(30)

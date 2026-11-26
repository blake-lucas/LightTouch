#!/usr/bin/env python3
"""Android engine A/B per board through the bench harness (Poco, the .bench app's records): arms are APKs, run in the
given order; each arm installs its APK once and plays every config: cool to TEMP, boot, unlock, launch, settle,
measure fps over MEASURE s from the engine's frame timeline. usage:
ANDROID_SERIAL=... LT_PKG=org.lighttouch.android.bench ab_android.py OUT MEASURE name=APK,... ORDER FW:RECORD:BUNDLE ..."""
import json, os, subprocess, sys, time
from pathlib import Path
import bench as B
out = Path(sys.argv[1]).resolve(); out.mkdir(parents=True, exist_ok=True)
measure = int(sys.argv[2]); arms = dict(a.split("=", 1) for a in sys.argv[3].split(",")); order = sys.argv[4].split(",")
games = {g["bundle"]: g for g in json.load(open(B.HERE / "matrix.json"))["games"]}
res = open(out / "results.tsv", "a")
def temp():
    return int(B.Android.adb("shell", "cat /sys/class/thermal/thermal_zone0/temp").strip() or 0)
for n, arm in enumerate(order):
    B.log("install", arm, subprocess.run(["adb", "install", "-r", arms[arm]], capture_output=True, text=True).stdout.strip()[-20:])
    B.Android.adb("shell", "run-as", B.PKG, "rm", "-f", "files/debug-env")
    for spec in sys.argv[5:]:
        fw, rec, bundle = spec.split(":")
        while temp() > 35000:
            time.sleep(20)
        t_start = temp(); tag = f"{fw.split('-')[0]}-{rec[:4]}-{n}-{arm}"
        for attempt in range(2):  # a first start after an install has missed SpringBoard
            s = B.Android(rec, out)
            if B.wait_for_home(s, 600):
                break
            B.log(tag, "never unlocked"); s.stop(); time.sleep(20); s = None
        if not s:
            continue
        g = dict(games[bundle], measure=measure)
        r = B.run_game(s, g, out, tag, fw)
        line = f"{fw}\t{bundle}\t{n}\t{arm}\t{r.get('fps')}\t{r.get('low1_fps')}\t{r.get('status')}\t{t_start}"
        res.write(line + "\n"); res.flush(); B.log(line)
        s.stop(); time.sleep(20)

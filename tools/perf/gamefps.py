#!/usr/bin/env python3
"""Delivered guest fps over a window of live play: SurfaceFlinger timestats for the guest
SurfaceView. usage: gamefps.py SECONDS [--series] (ANDROID_SERIAL set; the game must be on screen).
--series prints one fps figure per second instead, to see a rate that swings."""
import os, re, subprocess, sys, time
PKG = os.environ.get("LT_PKG", "org.lighttouch.android")  # LT_PKG: a side-by-side install
def sh(*a): return subprocess.run(["adb", "shell", *a], capture_output=True, text=True).stdout
secs = float(sys.argv[1]) if len(sys.argv) > 1 else 20
def frames(out):
    i = out.find("SurfaceView[" + PKG + "/")
    return int(re.search(r"totalFrames = (\d+)", out[i:]).group(1)) if i >= 0 else 0
if "--series" in sys.argv:
    sh("dumpsys", "SurfaceFlinger", "--timestats", "-enable", "-clear")
    series, t0, last = [], time.monotonic(), 0
    for k in range(int(secs)):
        time.sleep(max(0, t0 + k + 1 - time.monotonic()))
        n = frames(sh("dumpsys", "SurfaceFlinger", "--timestats", "-dump"))
        series.append(n - last); last = n
    sh("dumpsys", "SurfaceFlinger", "--timestats", "-disable")
    print(f"fps per second: {' '.join(map(str, series))}; mean {sum(series)/len(series):.1f}, min {min(series)}, max {max(series)}")
    sys.exit()
sh("dumpsys", "SurfaceFlinger", "--timestats", "-enable", "-clear")
time.sleep(secs)
out = sh("dumpsys", "SurfaceFlinger", "--timestats", "-dump")
sh("dumpsys", "SurfaceFlinger", "--timestats", "-disable")
i = out.find("SurfaceView[" + PKG + "/")
blk = out[i:out.find("layerName", i + 10)]
m = re.search(r"(?:present2present|presentToPresent) histogram is as below:\n([^\n]*)", blk) if i >= 0 else None
if not m:
    sys.exit("no guest frames in the window (a static screen, or the guest layer not found)")
h = {int(k): int(v) for k, v in re.findall(r"(\d+)ms=(\d+)", m.group(1)) if int(v)}
n = int(re.search(r"totalFrames = (\d+)", blk).group(1))
gaps = sum(h.values()); mean = sum(k * v for k, v in h.items()) / max(gaps, 1)
print(f"frames={n} over {secs:.0f}s => {n/secs:.1f} fps delivered; mean gap {mean:.1f}ms; hist {dict(sorted(h.items()))}")

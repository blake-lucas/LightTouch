#!/usr/bin/env python3
"""Desktop (Linux GCC) engine PGO counters per board through the bench harness on this host: each config boots with
the instrumented engine (LTD_ENGINE), unlocks, resets the counters (LT_PGO_TRIGGER), plays each game SECONDS with
bench.py's launch and first-run steps, and writes its .gcda set to OUT/TAG (GCOV_PREFIX).
usage: pgo_desktop.py OUT ENGINE FIRMWARE[#N]:BUNDLE,BUNDLE[:SECONDS] ..."""
import json, os, sys, time
from pathlib import Path
import bench as B
out = Path(sys.argv[1]).resolve(); out.mkdir(parents=True, exist_ok=True)
os.environ["LTD_ENGINE"] = sys.argv[2]
games = {g["bundle"]: g for g in json.load(open(B.HERE / "matrix.json"))["games"]}
known = B.devices()
for spec in sys.argv[3:]:
    key, bundles, *rest = spec.split(":")
    secs = int(rest[0]) if rest else 75
    rec = known[key]["record"]; tag = key.replace("#", "-")
    trig = out / f"{tag}.trigger"; gcda = out / tag
    trig.unlink(missing_ok=True); gcda.mkdir(exist_ok=True)
    os.environ["LTD_ENV"] = f"LT_PGO_TRIGGER={trig} GCOV_PREFIX={gcda} GCOV_PREFIX_STRIP=1"
    B.log(tag, rec, bundles)
    s = B.Desktop(rec, out)
    if not B.wait_for_home(s, 600):
        B.log(tag, "never unlocked"); s.stop(); continue
    trig.write_text("reset\n"); t0 = time.monotonic()
    for b in bundles.split(","):
        g = dict(games.get(b, {"name": b, "bundle": b, "settle": 40}), measure=secs)
        r = B.run_game(s, g, out, f"{tag}-{b}", key.split("#")[0])
        B.log(tag, g["name"], json.dumps({k: v for k, v in r.items() if k in ("status", "fps", "front_s")}))
    trig.write_text("dump\n"); time.sleep(5)
    B.log(tag, "window %.0f s, %d gcda files" % (time.monotonic() - t0, len(list(gcda.iterdir()))))
    s.stop(); time.sleep(10)

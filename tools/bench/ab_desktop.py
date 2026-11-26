#!/usr/bin/env python3
"""Desktop engine A/B per board through the bench harness: for each config, arms in the given order (e.g. A B B A);
each boots, unlocks, launches one game, settles, then measures fps and the session's host CPU per frame over MEASURE s.
usage: ab_desktop.py OUT MEASURE ARMS(name=engine.so,...) ORDER(A,B,B,A) FIRMWARE:BUNDLE ...  -> OUT/results.tsv"""
import json, os, sys, time
from pathlib import Path
import bench as B
out = Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=True)
measure = int(sys.argv[2])
arms = dict(a.split("=", 1) for a in sys.argv[3].split(","))
order = sys.argv[4].split(",")
games = {g["bundle"]: g for g in json.load(open(B.HERE / "matrix.json"))["games"]}
known = B.devices()
def tree_cpu(pid):
    """utime+stime (ticks) of pid and all its descendants."""
    kids, total = {}, 0
    for p in Path("/proc").iterdir():
        if p.name.isdigit():
            try:
                f = (p / "stat").read_text().rsplit(")", 1)[1].split()
                kids.setdefault(int(f[1]), []).append(int(p.name))
            except OSError:
                pass
    todo = [pid]
    while todo:
        q = todo.pop()
        try:
            f = Path(f"/proc/{q}/stat").read_text().rsplit(")", 1)[1].split()
            total += int(f[11]) + int(f[12])
        except OSError:
            pass
        todo += kids.get(q, [])
    return total
res = open(out / "results.tsv", "a")
for spec in sys.argv[5:]:
    key, bundle = spec.split(":")
    rec = known[key]["record"]
    for n, arm in enumerate(order):
        os.environ["LTD_ENGINE"] = arms[arm]
        os.environ["LTD_ENV"] = ""
        tag = f"{key}-{n}-{arm}"
        s = B.Desktop(rec, out)
        if not B.wait_for_home(s, 600):
            B.log(tag, "never unlocked"); s.stop(); continue
        g = dict(games[bundle], measure=1)
        B.run_game(s, g, out, tag, key.split("#")[0])  # launch, steps, settle, then a 1 s probe
        load0 = os.getloadavg()[0]
        c0 = tree_cpu(s.p.pid); s.frames_start(tag); t0 = time.monotonic()
        time.sleep(measure)
        frames = s.frames_stop(tag); dt = time.monotonic() - t0; c1 = tree_cpu(s.p.pid)
        st = B.frame_stats(frames, dt)
        cpu_s = (c1 - c0) / os.sysconf("SC_CLK_TCK")
        line = f"{key}\t{bundle}\t{n}\t{arm}\t{st.get('fps')}\t{st.get('low1_fps')}\t{cpu_s / max(len(frames), 1) * 1000:.2f}\t{cpu_s / dt * 100:.0f}\t{load0:.1f}"
        res.write(line + "\n"); res.flush(); B.log(line)
        s.screenshot(out / f"{tag}-measured")
        s.stop(); time.sleep(20)

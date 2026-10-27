#!/usr/bin/env python3
"""A/B game fps between installed LightTouch versions on one phone (ANDROID_SERIAL set).
usage: versionab.py OUTDIR GAME SECONDS ROUNDS PKG_A PKG_B [PKG...]
Each PKG is an installed package (version-apk.sh builds org.lighttouch.android.vXYZ) whose device has the game.
Round r runs every package once, the order reversed every other round (A B, B A, ...), so a phone warming up
over the session doesn't favour one side. Each run starts the guest, runs fcbench.py's GAME preset with a per-second
fps series, saves the screen (OUTDIR/rN-PKG.png: check it measured the game), then shuts the guest down, so only one
guest runs at a time. VERSIONAB_COOL (default 60) seconds of rest go between runs. The battery temperature is
logged before each run. Results go to OUTDIR/results.tsv and a summary at the end.
Leave other guests stopped (or paused) during a session: one running beside the measured guest takes its CPU."""
import os, re, statistics, subprocess, sys, time
D = os.path.dirname(os.path.abspath(__file__))
out, game, secs, rounds, pkgs = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), sys.argv[5:]
if len(pkgs) < 2: sys.exit(__doc__)
cool = float(os.environ.get("VERSIONAB_COOL", "60"))
os.makedirs(out, exist_ok=True)
def sh(*a): return subprocess.run(["adb", "shell", *a], capture_output=True, text=True).stdout
def battery_c():
    m = re.search(r"temperature: (\d+)", sh("dumpsys", "battery")); return int(m.group(1)) / 10 if m else float("nan")
def stop(pkg):  # Home, then shut the guest down through the app, as a user would, and wait for it to be off
    env = dict(os.environ, LT_PKG=pkg)
    code = (f"import sys, time\nsys.argv = ['x']\n__file__ = {os.path.join(D, 'inappfps.py')!r}\n" +
            open(os.path.join(D, "inappfps.py")).read().split("ap = argparse")[0] +
            "\nsh('am', 'start', '-n', f'{PKG}/org.lighttouch.android.MainActivity'); time.sleep(2)\n"
            # iOS Home first: the game goes to the background and saves, so the next run opens to the same screens
            "home(); time.sleep(5)\nstop()\n"
            "for _ in range(60):\n    time.sleep(2)\n    if state() == 'stopped': break\nprint('stopped:', state())\n")
    subprocess.run([sys.executable, "-c", code], env=env)
rows = []
with open(os.path.join(out, "results.tsv"), "a") as log:
    log.write("# round\tpackage\tbattery-C\tmean-fps\tmin\tmax\tseconds-at-55+\tseries\n")
    for r in range(rounds):
        order = pkgs if r % 2 == 0 else pkgs[::-1]
        for pkg in order:
            temp = battery_c()
            print(f"== round {r + 1}/{rounds}: {pkg} (battery {temp:.1f} C)", flush=True)
            env = dict(os.environ, LT_PKG=pkg, FC_SERIES="1", FC_SHOT=os.path.join(out, f"r{r + 1}-{pkg}.png"))
            text = subprocess.run([sys.executable, os.path.join(D, "fcbench.py"), "-", secs, game], env=env,
                                  capture_output=True, text=True).stdout
            print(text.strip().splitlines()[-1] if text.strip() else "(no output)", flush=True)
            m = re.search(r"fps per second: ([\d ]+);", text)
            series = [int(x) for x in m.group(1).split()] if m else []
            if series:
                row = (r + 1, pkg, temp, statistics.mean(series), min(series), max(series),
                       sum(x >= 55 for x in series) / len(series))
                rows.append(row)
                log.write("\t".join(map(str, row)) + "\t" + " ".join(map(str, series)) + "\n"); log.flush()
            else:
                log.write(f"{r + 1}\t{pkg}\t{temp}\tfailed\n"); log.flush()
                with open(os.path.join(out, f"r{r + 1}-{pkg}.log"), "w") as f: f.write(text)
            stop(pkg)
            time.sleep(cool)
print("\npackage                          runs  mean fps (per run)            share of seconds at 55+")
for pkg in pkgs:
    mine = [row for row in rows if row[1] == pkg]
    if mine:
        runs = f"{statistics.mean(r[3] for r in mine):5.1f} ({', '.join(f'{r[3]:.1f}' for r in mine)})"
        print(f"{pkg:32s} {len(mine):4d}  {runs:30s}{statistics.mean(r[6] for r in mine):.0%}")

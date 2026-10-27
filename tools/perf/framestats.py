#!/usr/bin/env python3
"""Guest frame pacing from the runtime's per-frame log (LT_FRAME_LOG): separates the rate a game
reaches (its ceiling) from the dips under it, which one mean fps figure hides.

usage: framestats.py --device SECONDS [--chart out.png] [--label NAME] [--save LOG]
         measure live play on the phone (ANDROID_SERIAL set; the engine started with
         LT_FRAME_LOG=<files>/frames.log in files/debug-env, which fcbench.py FC_FRAMES=1 arranges)
       framestats.py LOG [LOG...] [--skip S | --last S] [--secs N] [--chart out.png]
         analyse logs already pulled (or written by a desktop run); several logs share one chart

Each log line is "monotonic_ns sequence" for one published frame. Reported per run:
  mean      frames / seconds
  ceiling   95th percentile of fps over sliding 500 ms windows: what the game sustains when nothing
            gets in the way. A change that lifts this lifts the limit even if the mean barely moves.
  median    50th percentile of the same windows
  low       5th percentile of the same windows: how deep the regular dips go
  1% low    the frame time 99% of frames beat, as fps
  hitches   frames that took over 50 ms (3 refreshes at 60 Hz), and the time they cost
  >=55/<30  share of windows at full speed and below half speed
  skipped   engine frames the publisher never copied (sequence gaps): frames the guest made that
            the host never showed
The chart (matplotlib) plots each frame's instantaneous rate and the 500 ms windows on 0-60.

A LOG may instead be the engine's per-frame trace (IT_FRAME_TRACE, with IT_VCPU_ACCT and
IT_GLES_PROF): then each class of frame (on time, late, hitch) is also split by where its interval
went -- vCPU running guest code, in GL calls, waiting for the BQL, halted (the guest had nothing to
do), and off-CPU (neither halted nor on a host core: the host scheduler took the thread away). A
hitch with the vCPU halted is the guest waiting on something; one with the vCPU running is guest
work that did not fit in a refresh; one spent off-CPU is the host."""
import os, subprocess, sys, time

PKG = os.environ.get("LT_PKG", "org.lighttouch.android")  # LT_PKG: a side-by-side install
DEVICE_LOG = "files/frames.log"

def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, max(0, int(round(p / 100 * (len(v) - 1)))))] if v else 0.0

def parse(text):
    """(ns, sequence) rows, or from a trace (ns, sequence, run, outside, bql-outside, bql-inside, cpu,
    insns, gl, present, main); see ios_frame_trace in the engine's qemu-ios-ui.c."""
    rows = []
    for line in text.splitlines():
        parts = line.split()
        if len(parts) in (2, 11) and all(p.lstrip("-").isdigit() for p in parts):
            rows.append(tuple(map(int, parts)))
    return rows

CLASSES = (("on time", 0, 20), ("late", 20, 50), ("hitch", 50, 1e9))

def causes(rows):
    """Per frame class: mean ms per frame in run (less GL), GL, BQL wait, halt, off-CPU; main-loop CPU."""
    out = []
    for name, lo, hi in CLASSES:
        acc, n = [0.0] * 7, 0
        for a, b in zip(rows, rows[1:]):
            dt = (b[0] - a[0]) / 1e6
            if not lo <= dt < hi: continue
            run, outside, bql_out, bql_in, cpu, gl, main = ((b[i] - a[i]) / 1e6 for i in (2, 3, 4, 5, 6, 8, 10))
            # Guest code is run less its GL calls and MMIO BQL waits; idle is time outside
            # cpu_exec less the relock wait. Off-CPU: not idle, not on a core, not blocked on the
            # BQL. GL time is booked when a call returns, so a call that spans frames is not split.
            bql, halt = bql_out + bql_in, outside - bql_out
            off = max(0.0, dt - halt - cpu - bql)
            for k, v in enumerate((dt, run - gl - bql_in, gl, bql, halt, off, main)): acc[k] += v
            n += 1
        out.append((name, n, [v / n for v in acc] if n else acc))
    return out

def windows(ts, start, end, width=0.5, step=0.25):
    """fps over sliding windows [t, t+width) stepped by `step` seconds; ts in seconds."""
    out, j0, j1, t = [], 0, 0, start
    while t + width <= end + 1e-9:
        while j0 < len(ts) and ts[j0] < t: j0 += 1
        while j1 < len(ts) and ts[j1] < t + width: j1 += 1
        out.append((t + width, (j1 - j0) / width))
        t += step
    return out

def analyse(rows, skip=0.0, secs=None):
    if len(rows) < 2:
        return None
    t0 = rows[0][0] + skip * 1e9
    rows = [r for r in rows if r[0] >= t0 and (secs is None or r[0] < t0 + secs * 1e9)]
    if len(rows) < 2:
        return None
    ts = [(r[0] - t0) / 1e9 for r in rows]
    end = secs if secs is not None else ts[-1]
    gaps = [(b - a) * 1e3 for a, b in zip(ts, ts[1:])]
    win = windows(ts, 0.0, end)
    fps = [f for _, f in win]
    hitch = [g for g in gaps if g > 50]
    return {
        "causes": causes(rows) if len(rows[0]) == 11 else None,
        "ts": ts, "gaps": gaps, "win": win, "secs": end, "frames": len(ts),
        "mean": len(ts) / end if end else 0, "ceiling": pct(fps, 95), "median": pct(fps, 50),
        "low": pct(fps, 5), "low1": 1000 / pct(gaps, 99) if gaps else 0,
        "hitches": len(hitch), "hitch_ms": sum(hitch),
        "full": sum(f >= 55 for f in fps) / len(fps) if fps else 0,
        "half": sum(f < 30 for f in fps) / len(fps) if fps else 0,
        "skipped": sum(max(0, b[1] - a[1] - 1) for a, b in zip(rows, rows[1:])),
    }

def spark(s, cols=60):
    """1 s fps as one text row per run, to see the shape without the chart."""
    bars = " ▁▂▃▄▅▆▇█"
    per = [sum(1 for t in s["ts"] if k <= t < k + 1) for k in range(int(s["secs"]))]
    per = per[-cols:]
    return "".join(bars[min(8, int(f / 60 * 8 + 0.5))] for f in per)

def report(name, s):
    print(f"{name}: {s['frames']} frames / {s['secs']:.0f} s = mean {s['mean']:.1f} fps | ceiling {s['ceiling']:.0f} "
          f"median {s['median']:.0f} low {s['low']:.0f} 1%low {s['low1']:.0f} | hitches {s['hitches']} "
          f"({s['hitch_ms']:.0f} ms) | >=55 {s['full']:.0%} <30 {s['half']:.0%} | skipped {s['skipped']}")
    print(f"{'':{len(name)}}  1 s fps |{spark(s)}|")
    if s["causes"]:
        print(f"{'':{len(name)}}  ms per frame     n   frame  guest-code  gl  bql-wait  idle  off-cpu | main-cpu")
        for cls, n, (dt, run, gl, bql, halt, off, main) in s["causes"]:
            print(f"{'':{len(name)}}  {cls:<12} {n:5d} {dt:7.1f} {run:10.1f} {gl:5.1f} {bql:7.1f} {halt:5.1f} {off:7.1f} | {main:6.1f}")

def chart(runs, path):
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
    except ImportError:
        print("chart skipped: no matplotlib for this python (try /usr/bin/python3)", file=sys.stderr)
        return
    fig, axes = plt.subplots(len(runs), 1, figsize=(11, 2.4 * len(runs)), squeeze=False, sharex=True)
    for ax, (name, s) in zip(axes[:, 0], runs):
        inst = [min(70, 1000 / g) if g > 0 else 70 for g in s["gaps"]]
        ax.scatter(s["ts"][1:], inst, s=2, alpha=0.25, color="tab:blue", label="per frame")
        ax.plot([t for t, _ in s["win"]], [f for _, f in s["win"]], color="tab:blue", lw=1.4, label="500 ms")
        for t, g in zip(s["ts"][1:], s["gaps"]):
            if g > 50: ax.axvline(t, color="tab:red", alpha=0.35, lw=0.8)
        for y in (30, 60): ax.axhline(y, color="0.6", lw=0.8, ls="--")
        ax.set_ylim(0, 66); ax.set_ylabel("fps")
        ax.set_title(f"{name}: mean {s['mean']:.1f}, ceiling {s['ceiling']:.0f}, median {s['median']:.0f}, "
                     f"low {s['low']:.0f}, hitches {s['hitches']} (red)", fontsize=9, loc="left")
    axes[-1, 0].set_xlabel("seconds")
    fig.tight_layout(); fig.savefig(path, dpi=110)
    print(f"chart: {path}")

def adb(*a, **kw):
    return subprocess.run(["adb", "shell", "run-as", PKG, *a], capture_output=True, **kw)

def main():
    args = sys.argv[1:]
    def opt(name, default=None):
        if name in args:
            i = args.index(name); v = args[i + 1]; del args[i:i + 2]; return v
        return default
    out = opt("--chart"); label = opt("--label"); save = opt("--save")
    skip = float(opt("--skip", 0)); secs = opt("--secs")
    last = opt("--last")
    secs = float(secs) if secs else None
    dev = opt("--device")
    if save and not dev:
        sys.exit("--save requires --device")
    runs = []
    if dev:
        size = adb("stat", "-c", "%s", DEVICE_LOG, text=True).stdout.strip()
        if not size.isdigit():
            sys.exit(f"no {DEVICE_LOG} on the device: add LT_FRAME_LOG=/data/user/0/{PKG}/{DEVICE_LOG} "
                     "to files/debug-env and restart the guest")
        time.sleep(float(dev))
        text = adb("tail", "-c", f"+{int(size) + 1}", DEVICE_LOG).stdout.decode(errors="replace")
        if save:
            with open(save, "w") as f:
                f.write(text)
        rows = parse(text)
        s = analyse(rows, secs=float(dev))
        if not s: sys.exit("fewer than 2 frames in the window (static screen?)")
        runs.append((label or os.environ.get("ANDROID_SERIAL", "device"), s))
    for path in args:
        rows = parse(open(path).read())
        if last and rows:  # the final S seconds, e.g. the window a benchmark just measured
            skip = max(0.0, (rows[-1][0] - rows[0][0]) / 1e9 - float(last))
        s = analyse(rows, skip, secs)
        if not s: print(f"{path}: fewer than 2 frames"); continue
        runs.append((label if label and len(args) == 1 else os.path.basename(path), s))
    for name, s in runs: report(name, s)
    if out and runs: chart(runs, out)

if __name__ == "__main__":
    main()

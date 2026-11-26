# Measuring: harnesses, method, profilers

`docs/PERFORMANCE.md` opens with "How to measure" and holds every result. This page says which tool answers which
question and what keeps an A/B honest.

## Pick the tool

| Question | Tool |
| --- | --- |
| Does game X run, at what fps / 1% low, on every board, desktop or phone? | `tools/bench/bench.py run --host desktop\|android` (`tools/bench/README.md`) |
| fps of one game on a phone, A vs B | `tools/perf/fcbench.py` (+ `FC_ENV` for engine switches, `FC_FRAMES` for frame stats) |
| Delivered fps / present gaps of whatever is on the phone's screen | `tools/perf/gamefps.py SECS [--series]` (SurfaceFlinger timestats; the histogram is the pacing) |
| CPU microbenchmarks inside iOS | LT Bench in the guest (`tools/perf/ltbench.py`, `docs/GUEST-BENCHMARKS.md`) |
| Where host CPU goes (helpers, JIT, TB lookups) with the guest's process and thread | `LT_SAMPLE=1000` (engine sampler; capture between profile start/stop), then a symbolizer against the unstripped engine. On phones, `tools/perf/gameprof.sh` (simpleperf) |
| BQL waits and holds, frame path stages, hitches | the opt-in diagnostics in `docs/PROFILING.md` |
| UI smoothness of the app itself | `tools/perf/uirate.py` |

## A/B method

- **One build, one switch.** New behaviour goes behind `LT_X=off`, so both arms are the same binary. When the arms
  must be different binaries (PGO profiles, compiler flags), build them from the same commit in the same directory
  layout.
- **Interleave** arms in one session: A B B A, or A B A B with three or more pairs. Phones drift 1–2 fps between
  sessions and throttle on the charger. The first boot after an install can misbehave, so don't let it be one arm's
  only run.
- **Heat:**
  - Cool the phone to the same starting temperature before each arm (`thermal_zone0` in m°C, or battery
    temperature).
  - Log it, and report results by starting temperature when the arms differ.
  - A 34 °C start can cost an iPhone 5 game 10–15%.
- **Capped games:** many sit at 30 or 60 fps. Compare CPU per frame or vCPU busy, not fps.
- **Same scene:** screenshot every arm (`FC_SHOT`, or bench.py's `-settled`/`-end` shots) and check it shows the
  same screen. Dialogs, age gates and attract-mode differences masquerade as speedups.
- **Shared hosts:** check `uptime` and other agents' processes first, and log load per run. A desktop arm under
  load 8 isn't comparable to one at 4.

## Recording results

Every kept or rejected change gets one row in `docs/PERFORMANCE.md` in the same commit as the change:
- what changed and its switch;
- the commit;
- before → after numbers, labelled host-side or in-guest, and which device and phone.

Write rejected experiments as rows too, so nobody retries them blind. Long investigations go in the commit
message or git history, not the doc (`CLAUDE.md`: keep tracked docs short).

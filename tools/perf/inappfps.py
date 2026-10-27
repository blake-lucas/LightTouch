#!/usr/bin/env python3
"""In-app delivered frame rate. Starts (or reuses) the guest in the installed app,
unlocks, flings Home pages, and reads SurfaceFlinger's present timestamps for the
guest SurfaceView. usage: inappfps.py [--no-start] [--swipes N] [--stop]"""
import argparse, os, re, subprocess, sys, time, struct, statistics
sys.path.insert(0, __file__.rsplit("/", 1)[0])
PKG = os.environ.get("LT_PKG", "org.lighttouch.android")  # LT_PKG: a side-by-side install
def sh(*a): return subprocess.run(["adb", "shell", *a], capture_output=True, text=True).stdout
def ui():
    sh("uiautomator", "dump", "/sdcard/ui.xml")
    return subprocess.run(["adb", "exec-out", "cat", "/sdcard/ui.xml"], capture_output=True, text=True).stdout
def find(xml, text):
    for m in re.finditer(r'<node [^>]*?text="([^"]*)"[^>]*?enabled="(true|false)"[^>]*?bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', xml):
        if m.group(1) == text:
            x0, y0, x1, y1 = map(int, m.groups()[2:]); return m.group(2) == "true", ((x0+x1)//2, (y0+y1)//2)
    return None, None
def tap_text(text):
    en, pos = find(ui(), text)
    if en: sh("input", "tap", str(pos[0]), str(pos[1]))
    return bool(en)
def tap_id(*tags):
    """Taps the first enabled control with one of these resource ids (the current UI's tags)."""
    xml = ui()
    for tag in tags:
        m = re.search(r'resource-id="' + tag + r'"[^>]*?enabled="(true|false)"[^>]*?bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', xml)
        if m and m.group(1) == "true":
            x0, y0, x1, y1 = map(int, m.groups()[1:])
            sh("input", "tap", str((x0+x1)//2), str((y0+y1)//2)); return True
    return False
def start(): return tap_id("recover_guest", "start_guest") or tap_text("Recover retained state") or tap_text("Start")
def wake(): return tap_id("guest_screen_asleep", "power_button") or tap_text("Power")
def home(): return tap_id("device_home", "home_button") or tap_text("Home")
def stop():
    if tap_id("stop_guest"): time.sleep(1); return tap_id("confirm_stop")
    if tap_id("fullscreen_menu"):  # full screen keeps Stop in its menu
        time.sleep(1)
        if tap_text("Stop"): time.sleep(1); return tap_id("confirm_stop")
    return tap_text("Stop")
def state():
    xml = ui()
    m = re.search(r'text="([a-z]+)" resource-id="session_status"', xml)
    return m.group(1) if m else "?"
def guest_rect():
    for _ in range(2):
        xml = ui()
        m = re.search(r'class="android.view.SurfaceView"[^>]*?bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', xml)
        if m: break
        time.sleep(1)
    if m: x0, y0, x1, y1 = map(int, m.groups())
    else: x0, y0, x1, y1 = box_from_layout(xml)
    w, h = x1-x0, y1-y0
    s = min(w/320, h/480); gw, gh = 320*s, 480*s
    return x0+(w-gw)/2, y0+(h-gh)/2, gw, gh
def box_from_layout(xml):
    """Guest view bounds as x0, y0, x1, y1 when the SurfaceView is not in the accessibility tree."""
    # Guest view sits between the controls above it and the Home/Power row.
    ids = dict((m.group(1), tuple(map(int, m.groups()[1:]))) for m in
               re.finditer(r'resource-id="([a-z_]+)"[^>]*?bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', xml))
    if "guest_screen" in ids:  # current UI: the guest view's own bounds
        return ids["guest_screen"]
    if "stop_guest" in ids and "home_button" in ids:
        # Tagged controls (current app): Start/Stop row above, Home row below; the view spans
        # the content width, from the Start button's left edge to the same margin on the right.
        x0 = ids.get("start_guest", ids["stop_guest"])[0]
        w = int(sh("wm", "size").split()[-1].split("x")[0])
        return x0, ids["stop_guest"][3], w - x0, ids["home_button"][1]
    nodes = [tuple(map(int, b)) for b in re.findall(r'bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', xml)]
    nodes = [n for n in nodes if n[3]-n[1] < 1500]
    _, home = find(xml, "Home")
    hb = [n for n in nodes if n[0] <= home[0] <= n[2] and n[1] <= home[1] <= n[3]]
    bottom = min(n[1] for n in hb) - 11
    top = max(n[3] for n in nodes if n[3] <= bottom - 300) + 11
    x0 = min(n[0] for n in hb)
    w = max(n[2] for n in nodes)  # widest content edge
    return x0, top, max(w, 1080 - x0), bottom
def black_box():
    # The guest view is the tallest run of identical pixels down the centre column
    # (black while stopped/asleep). Used when the accessibility tree hides it.
    w, h, px = shot(); cx = w//2; best = (0, 0, 0); start = 0
    col = [px[(y*w+cx)*4:(y*w+cx)*4+3] for y in range(h)]
    for y in range(1, h+1):
        if y == h or col[y] != col[start]:
            if y-start > best[0]: best = (y-start, start, y)
            start = y
    _, y0, y1 = best; row = y0 + (y1-y0)//2; ref = px[(row*w+cx)*4:(row*w+cx)*4+3]
    x0 = cx; x1 = cx
    while x0 > 0 and px[(row*w+x0-1)*4:(row*w+x0-1)*4+3] == ref: x0 -= 1
    while x1 < w-1 and px[(row*w+x1+1)*4:(row*w+x1+1)*4+3] == ref: x1 += 1
    return x0, y0, x1+1, y1
def shot():
    raw = subprocess.run(["adb", "exec-out", "screencap"], capture_output=True).stdout
    w, h = struct.unpack("<II", raw[:8]); return w, h, raw[len(raw)-w*h*4:]
def avg(rect, fx0, fy0, fx1, fy1, frame=None):
    w, h, px = frame or shot(); gx, gy, gw, gh = rect; r = g = b = n = 0
    for y in range(int(gy+gh*fy0), int(gy+gh*fy1), 8):
        for x in range(int(gx+gw*fx0), int(gx+gw*fx1), 8):
            i = (y*w+x)*4; r += px[i]; g += px[i+1]; b += px[i+2]; n += 1
    return r/n, g/n, b/n
def is_lock(rect):
    f = shot()  # one capture: the guest re-sleeps ~5 s after a wake
    r, g, b = avg(rect, .12, .27, .88, .69, f)
    k = avg(rect, .10, .87, .27, .93, f); t = avg(rect, .40, .88, .85, .92, f)
    # The default blue wallpaper, over the lock screen's dark slider bar (Home can use the same wallpaper,
    # but has its dock there).
    if b > 60 and b > r+25 and max(t) < 110: return True
    # Any wallpaper: the slide-to-unlock knob is light grey on a dark track.
    return min(k) > 140 and max(k) - min(k) < 30 and max(t) < 100
def gpt(rect, fx, fy):
    gx, gy, gw, gh = rect; return str(int(gx+gw*fx)), str(int(gy+gh*fy))
def layer():
    names = re.findall(r'(SurfaceView\[org\.lighttouch[^\]]*\]\(BLAST\)#\d+)', sh("dumpsys", "SurfaceFlinger", "--list"))
    return names[-1] if names else None
def latency(name):
    out = sh("dumpsys", "SurfaceFlinger", "--latency", f"'{name}'").split("\n")
    rows = []
    for l in out[1:]:
        p = l.split()
        if len(p) == 3 and p[1] != "0" and int(p[1]) < 2**62: rows.append(int(p[1]))
    return sorted(set(rows))
ap = argparse.ArgumentParser(); ap.add_argument("--no-start", action="store_true"); ap.add_argument("--swipes", type=int, default=6)
ap.add_argument("--stop", action="store_true"); ap.add_argument("--threads", action="store_true"); ap.add_argument("--boot-only", action="store_true"); ap.add_argument("--idle", type=int, default=0); a = ap.parse_args()
sh("input", "keyevent", "KEYCODE_WAKEUP"); sh("am", "start", "-n", f"{PKG}/org.lighttouch.android.MainActivity"); time.sleep(2)
rect = guest_rect()
if not a.no_start:
    st = state()
    if st == "paused": tap_text("Resume")
    elif st in ("stopped", "?"):
        start()
        t0 = time.monotonic(); time.sleep(4); rect = guest_rect()  # layout changes once running
        samples = []
        while time.monotonic()-t0 < 300 and not is_lock(rect):
            # The guest can sleep before the lock screen is seen; the app then covers it with a wake card.
            if time.monotonic()-t0 > 8 and state() == "running" and wake(): time.sleep(2)
            if a.threads and len(samples) < 6 and time.monotonic()-t0 > 8:
                samples.append(sh("sh", "-c", f"ps -T -o TID,PSR,PCPU,CMD -p $(pidof {PKG}:engine) | sort -k3 -rn | head -3 | tr '\\n' ';'"))
            time.sleep(1)
        for x in samples: print("  threads:", x.strip(), flush=True)
        print("lock screen after", round(time.monotonic()-t0, 1), flush=True)
if a.boot_only:
    if a.stop: stop()
    sys.exit(0)
rect = guest_rect()
for _ in range(4):
    if sum(avg(rect, .1, .1, .9, .9)) < 15 or wake(): time.sleep(3)
    if is_lock(rect):
        sh("input", "swipe", *gpt(rect, .2, .89), *gpt(rect, .95, .89), "400"); time.sleep(3)
    elif sum(avg(rect, .1, .1, .9, .9)) >= 15: break
print("guest rect", [round(v) for v in rect], "lock" if is_lock(rect) else "home?", flush=True)
time.sleep(1)
def timestats():
    out = sh("dumpsys", "SurfaceFlinger", "--timestats", "-dump")
    i = out.find("SurfaceView[" + PKG + "/"); 
    if i < 0: return None
    blk = out[i:out.find("layerName", i+10)]
    hist = re.search(r"present2present histogram is as below:\n([^\n]*)", blk).group(1)
    h = {int(k): int(v) for k, v in re.findall(r"(\d+)ms=(\d+)", hist) if int(v)}
    return int(re.search(r"totalFrames = (\d+)", blk).group(1)), h
home(); time.sleep(2); home(); time.sleep(2)
sh("dumpsys", "SurfaceFlinger", "--timestats", "-enable", "-clear")
time.sleep(0.3)
# Home has one page on 3.1.3: alternate Home -> Spotlight -> Home.
for i in range(a.swipes):
    if i % 2 == 0: sh("input", "swipe", *gpt(rect, .15, .35), *gpt(rect, .85, .35), "150")
    else:          sh("input", "swipe", *gpt(rect, .85, .35), *gpt(rect, .15, .35), "150")
    time.sleep(0.9)
r = timestats(); sh("dumpsys", "SurfaceFlinger", "--timestats", "-disable")
if r:
    total, h = r; anim = {k: v for k, v in h.items() if k < 100}
    n = sum(anim.values()); mean = sum((k+0.5)*v for k, v in anim.items())/max(n, 1)
    print(f"posted={total} in-animation n={n} mean gap {mean:.1f}ms => {1000/mean:.1f} fps; min gap {min(anim) if anim else '-'}ms; hist {anim}")
else: print("no layer stats")
def proc_ticks():
    out = sh(f"cat /proc/$(pidof {PKG}:engine)/stat /proc/uptime")
    st, up = out.strip().split("\n")[-2:]
    f = st[st.rindex(")")+2:].split()
    return int(f[11]) + int(f[12]), float(up.split()[0])
if a.idle:
    home(); time.sleep(3)
    t0, u0 = proc_ticks(); time.sleep(a.idle); t1, u1 = proc_ticks()
    print(f"idle engine process CPU {100*(t1-t0)/100/(u1-u0):.1f}% of one core over {u1-u0:.0f}s", flush=True)
if a.stop:
    stop()

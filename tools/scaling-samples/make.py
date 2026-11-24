#!/usr/bin/env python3
"""Draw the Screen scaling preview's sample pictures (art/preview/scaling/*.png) at each guest screen's own resolution.

Everything is drawn here from shapes, gradients and a hand-made pixel font, plus labels in Inter (the desktop app's
font, SIL Open Font License), so the pictures carry no game, Apple or other third-party art. Each one shows what the
scaling methods change: hard pixel edges and 1-pixel lines, diagonals, small text and smooth gradients.

    python3 -m venv /tmp/v && /tmp/v/bin/pip install pillow numpy
    /tmp/v/bin/python tools/scaling-samples/make.py [OUT_DIR]
    oxipng -o max --strip all -Z art/preview/scaling/*.png    # about 700 KB in all
"""
import math
import os
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
FONTS = os.path.join(REPO, "crates", "lighttouch", "assets", "fonts")
# The screens as shown upright: iPhone and iPod touch, Retina, iPhone 5, iPad.
SIZES = [(320, 480), (640, 960), (640, 1136), (768, 1024)]


def font(weight, size):
    return ImageFont.truetype(os.path.join(FONTS, f"Inter-{weight}.ttf"), size)


# A 3x5 pixel font for the game's score line.
GLYPHS = {
    "0": "111101101101111", "1": "010110010010111", "2": "111001111100111", "3": "111001111001111",
    "4": "101101111001001", "5": "111100111001111", "6": "111100111101111", "7": "111001010010010",
    "8": "111101111101111", "9": "111101111001111", "S": "111100111001111", "C": "111100100100111",
    "O": "111101101101111", "R": "110101110101101", "E": "111100111100111", "X": "101101010101101",
    "L": "100100100100111", "V": "101101101101010", " ": "000000000000000", "x": "000101010101000",
}


def pixel_text(img, x, y, text, color, shadow=None):
    for i, ch in enumerate(text):
        bits = GLYPHS[ch]
        for r in range(5):
            for c in range(3):
                if bits[r * 3 + c] == "1":
                    if shadow:
                        img[y + r + 1, x + i * 4 + c + 1] = shadow
                    img[y + r, x + i * 4 + c] = color


SPRITE = [  # 12 x 16: a round explorer with a backpack. . clear, k outline, others fill.
    "....kkkk....",
    "...kyyyyk...",
    "..kyyyyyyk..",
    "..kyykkyyk..",
    ".kkyywkwyk..",
    ".kyyyyyyykk.",
    ".kryyyyyykbk",
    "..krrrrrrkbk",
    "..kyrrrrrkbk",
    ".kyykrrrkkbk",
    ".kyyk...kkk.",
    "..kkrrrrrk..",
    "..krrkkrrk..",
    "..krk..krk..",
    ".kddk..kddk.",
    ".kkkk..kkkk.",
]
SPRITE_COLORS = {"k": (24, 20, 37), "y": (255, 205, 117), "w": (255, 255, 255), "r": (177, 62, 83),
                 "b": (59, 93, 201), "d": (87, 60, 46)}


def pixel_scene(gw, gh):
    """The pixel-art game, gw x gh art pixels."""
    a = np.zeros((gh, gw, 3), np.uint8)
    bands = [(41, 54, 111), (59, 93, 201), (65, 166, 246), (115, 239, 247), (190, 240, 232)]
    horizon = int(gh * 0.62)
    for i, color in enumerate(bands):
        a[int(horizon * i / len(bands)):int(horizon * (i + 1) / len(bands))] = color
    # The sun, a pixel circle with a dithered rim.
    cx, cy, r = int(gw * 0.74), int(gh * 0.16), 11
    for y in range(cy - r - 2, cy + r + 3):
        for x in range(cx - r - 2, cx + r + 3):
            d = math.hypot(x - cx, y - cy)
            if d <= r:
                a[y, x] = (255, 241, 164)
            elif d <= r + 2 and (x + y) % 2 == 0:
                a[y, x] = (255, 205, 117)
    # Clouds.
    for (ox, oy, n) in [(10, int(gh * 0.12), 5), (int(gw * 0.4), int(gh * 0.27), 4), (int(gw * 0.08), int(gh * 0.38), 3)]:
        for k in range(n):
            ccx, ccy, cr = ox + k * 7, oy - (3 if k % 2 else 0), 5 + (k % 2)
            for y in range(ccy - cr, ccy + cr + 1):
                for x in range(ccx - cr, ccx + cr + 1):
                    if 0 <= x < gw and math.hypot(x - ccx, y - ccy) <= cr:
                        a[y, x] = (244, 244, 244) if y < ccy + 2 else (148, 176, 194)
    # Hills in two tones, the far ones in a 1-pixel checker dither.
    for x in range(gw):
        far = int(horizon - 18 - 10 * math.sin(x / 13.0) - 5 * math.sin(x / 5.3))
        near = int(horizon - 6 - 8 * math.sin(x / 21.0 + 1.3))
        for y in range(far, horizon):
            a[y, x] = (56, 183, 100) if (x + y) % 2 else (37, 113, 121)
        a[near:horizon, x] = (37, 113, 121)
    # Ground: bricks with 1-pixel mortar lines.
    a[horizon:] = (139, 80, 54)
    for y in range(horizon, gh):
        row = (y - horizon) // 6
        if (y - horizon) % 6 == 0:
            a[y] = (87, 60, 46)
        else:
            for x in range(gw):
                if (x + (row % 2) * 6) % 12 == 0:
                    a[y, x] = (87, 60, 46)
    a[horizon:horizon + 2] = (56, 183, 100)
    a[horizon + 2] = (37, 113, 121)
    # A ramp of 45-degree and 2:1 steps, and a rope across the sky at a shallow angle.
    base = horizon
    for i in range(24):
        x0 = 8 + i
        a[base - i:base, x0] = (167, 240, 112) if i % 4 else (56, 183, 100)
    for i in range(48):
        x0 = gw - 60 + i
        h = i // 2
        if 0 <= x0 < gw:
            a[base - h:base, x0] = (238, 135, 64) if (i // 4) % 2 else (177, 62, 83)
    for x in range(gw):
        y = int(gh * 0.08 + x * 0.21)
        if y < horizon - 20:
            a[y, x] = (24, 20, 37)
    # Coins.
    for k in range(5):
        ccx, ccy = int(gw * 0.3) + k * 12, horizon - 26 - int(6 * math.sin(k))
        for y in range(ccy - 3, ccy + 4):
            for x in range(ccx - 3, ccx + 4):
                d = math.hypot(x - ccx, y - ccy)
                if d <= 3.2:
                    a[y, x] = (24, 20, 37) if d > 2.4 else (255, 205, 117)
        a[ccy - 1:ccy + 2, ccx] = (255, 241, 164)
    # The explorer, twice: once on the ground and once mid-jump.
    for (sx, sy) in [(int(gw * 0.18), horizon - 16), (int(gw * 0.52), horizon - 44)]:
        for r, line in enumerate(SPRITE):
            for c, ch in enumerate(line):
                if ch != ".":
                    a[sy + r, sx + c] = SPRITE_COLORS[ch]
    # Score line on a dark strip.
    a[0:9] = (24, 20, 37)
    pixel_text(a, 2, 2, "SCORE 004210", (255, 255, 255))
    pixel_text(a, gw - 4 * 6 - 1, 2, "LVL 3", (255, 205, 117))
    return a


def pixel_art(w, h):
    s = 2 if w == 320 else 4
    art = pixel_scene(w // s, h // s)
    big = np.repeat(np.repeat(art, s, axis=0), s, axis=1)
    out = np.zeros((h, w, 3), np.uint8)
    out[:big.shape[0], :big.shape[1]] = big[:h, :w]
    return Image.fromarray(out)


def scene_3d(w, h):
    """A racing game's view: a sunset sky, fogged hills, a checkered road to the horizon and a shaded orb."""
    ss = 3
    W, H = w * ss, h * ss
    y, x = np.mgrid[0:H, 0:W].astype(np.float32)
    u, v = x / W, y / H
    horizon = 0.45
    sky_top, sky_low = np.array([22, 34, 84], np.float32), np.array([250, 150, 92], np.float32)
    t = np.clip(v / horizon, 0, 1)[..., None]
    img = sky_top * (1 - t) ** 1.4 + sky_low * (1 - (1 - t) ** 1.4)
    # Sun glow.
    d = np.hypot((u - 0.68) * W / H, v - 0.36)
    img += (np.array([255, 210, 140]) * np.clip(1 - d / 0.25, 0, 1)[..., None] ** 2.2) * 0.8
    img[d < 0.045] = (255, 244, 214)
    # Two ridges of hills, each fogged toward the sky's colour.
    for k, (amp, freq, off, col, fog) in enumerate([(0.06, 9.0, 0.0, (92, 74, 130), 0.55),
                                                   (0.09, 5.0, 2.0, (48, 52, 96), 0.25)]):
        ridge = horizon - 0.01 - amp * (0.6 + 0.4 * np.sin(u * freq + off) * np.cos(u * freq * 0.37 + off))
        mask = v >= ridge
        shade = np.clip((v - ridge) / 0.12, 0, 1)[..., None]
        c = np.array(col, np.float32) * (1 - 0.35 * shade)
        c = c * (1 - fog) + sky_low * fog
        img = np.where(mask[..., None], c, img)
    # The ground: a checkered plane in perspective, fogged with distance.
    below = v > horizon
    depth = 1.0 / np.maximum(v - horizon, 1e-4)
    gx = (u - 0.5) * depth * 0.9
    gz = depth * 0.35
    check = ((np.floor(gx * 2) + np.floor(gz * 2)) % 2).astype(np.float32)[..., None]
    road = np.abs(gx) < 0.9
    ground = np.where(road[..., None], np.array([70, 70, 78]) * (1 - check) + np.array([210, 210, 216]) * check,
                      np.array([44, 120, 70]) * (1 - check) + np.array([60, 150, 86]) * check)
    stripe = (np.abs(np.abs(gx) - 0.9) < 0.05) & road
    ground = np.where(stripe[..., None], np.array([240, 200, 60], np.float32), ground)
    fog = np.clip((depth - 2.2) / 30.0, 0, 1)[..., None] ** 0.7
    ground = ground * (1 - fog) + sky_low * fog
    img = np.where(below[..., None], ground, img)
    # A shaded orb hovering over the road, and its shadow.
    ox, oy, orad = 0.5, 0.66, 0.11
    dx, dy = (u - ox) * W / H, v - oy
    sh = ((dx / (orad * 1.2)) ** 2 + ((v - (oy + orad * 1.25)) / (orad * 0.22)) ** 2) < 1
    img = np.where(sh[..., None], img * 0.45, img)
    rr = np.hypot(dx, dy) / orad
    inside = rr < 1
    nz = np.sqrt(np.clip(1 - rr ** 2, 0, 1))
    nx, ny = dx / orad, dy / orad
    light = np.array([-0.45, -0.55, 0.7])
    light /= np.linalg.norm(light)
    diff = np.clip(nx * light[0] + ny * light[1] + nz * light[2], 0, 1)
    shine = np.exp(-((nx + 0.33) ** 2 + (ny + 0.4) ** 2) / 0.012)
    rim = np.clip(rr - 0.75, 0, 1) * 4 * np.clip(ny, 0, 1)
    orb = np.array([235, 60, 80]) * (0.22 + 0.9 * diff[..., None]) + 255 * shine[..., None] * 0.85
    orb += np.array([250, 150, 92]) * rim[..., None] * 0.35
    img = np.where(inside[..., None], orb, img)
    img = np.clip(img, 0, 255).astype(np.uint8)
    im = Image.fromarray(img).resize((w, h), Image.BOX)
    # The HUD: speed, lap, a thin bar and small labels.
    d = ImageDraw.Draw(im)
    s = w / 320
    d.rounded_rectangle([8 * s, 8 * s, 108 * s, 46 * s], radius=6 * s, fill=(10, 12, 28))
    d.text((14 * s, 10 * s), "128", font=font("SemiBold", int(24 * s)), fill=(255, 255, 255))
    d.text((62 * s, 22 * s), "km/h", font=font("Regular", int(10 * s)), fill=(190, 200, 230))
    d.text((14 * s, 35 * s), "LAP 2/3   0:41.27", font=font("Regular", max(8, int(8 * s))), fill=(255, 210, 140))
    bar_y = h - 22 * s
    d.rectangle([0, bar_y - 18 * s, w, h], fill=(10, 12, 28))
    d.rectangle([12 * s, bar_y, w - 12 * s, bar_y + 3 * s], fill=(10, 12, 28))
    d.rectangle([12 * s, bar_y, 12 * s + (w - 24 * s) * 0.62, bar_y + 3 * s], fill=(255, 210, 140))
    d.text((12 * s, bar_y - 13 * s), "Coastline Circuit · sector 2", font=font("Regular", max(8, int(9 * s))),
           fill=(255, 255, 255))
    return im


ICONS = [
    ("Garden", (88, 196, 104), (30, 120, 60), "leaf"), ("Tides", (64, 160, 255), (20, 70, 170), "wave"),
    ("Chess", (210, 210, 220), (110, 110, 125), "grid"), ("Radio", (255, 140, 70), (190, 60, 30), "bars"),
    ("Sketch", (255, 220, 90), (220, 150, 20), "pen"), ("Atlas", (120, 220, 210), (30, 130, 140), "globe"),
    ("Recipes", (255, 120, 140), (190, 40, 80), "dot"), ("Budget", (170, 130, 255), (90, 50, 190), "coin"),
    ("Trails", (150, 200, 90), (70, 120, 40), "peak"), ("Words", (250, 250, 250), (180, 180, 190), "lines"),
    ("Stars", (60, 60, 110), (15, 15, 40), "star"), ("Pianola", (40, 40, 44), (10, 10, 12), "keys"),
    ("Kites", (110, 210, 255), (30, 120, 210), "kite"), ("Lantern", (255, 190, 80), (200, 100, 20), "dot"),
    ("Orbit", (90, 90, 140), (30, 30, 70), "globe"), ("Pebbles", (200, 190, 170), (130, 120, 100), "coin"),
]
DOCK = [("Call", (100, 220, 100), (30, 140, 50), "bars"), ("Letters", (90, 170, 255), (30, 90, 210), "lines"),
        ("Browse", (120, 200, 255), (40, 110, 220), "globe"), ("Tunes", (255, 110, 120), (200, 40, 80), "wave")]


def icon(size, top, bottom, glyph):
    """A rounded-square app icon with a vertical gradient, a gloss band and a simple glyph, drawn 4x and reduced."""
    k = 4
    S = size * k
    grad = np.linspace(0, 1, S, dtype=np.float32)[:, None, None]
    body = np.array(top, np.float32) * (1 - grad) + np.array(bottom, np.float32) * grad
    body = np.broadcast_to(body, (S, S, 3)).copy()
    body[: int(S * 0.48)] = body[: int(S * 0.48)] * 0.78 + 255 * 0.22  # gloss
    im = Image.fromarray(body.astype(np.uint8)).convert("RGBA")
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, S - 1, S - 1], radius=int(S * 0.18), fill=255)
    im.putalpha(mask)
    d = ImageDraw.Draw(im)
    c = S / 2
    ink = (255, 255, 255, 235) if sum(bottom) < 500 else (60, 60, 70, 235)
    lw = max(k, int(S * 0.05))
    if glyph == "leaf":
        d.ellipse([c - S * 0.22, c - S * 0.3, c + S * 0.22, c + S * 0.3], fill=ink)
        d.line([c - S * 0.18, c + S * 0.28, c + S * 0.14, c - S * 0.22], fill=bottom + (255,), width=lw)
    elif glyph == "wave":
        for j in range(3):
            pts = [(S * 0.15 + i * S * 0.7 / 40, c - S * 0.15 + j * S * 0.15 + math.sin(i / 40 * 6.28 * 1.5) * S * 0.05)
                   for i in range(41)]
            d.line(pts, fill=ink, width=lw)
    elif glyph == "grid":
        n = 4
        for i in range(n):
            for j in range(n):
                if (i + j) % 2 == 0:
                    x0, y0 = S * 0.2 + i * S * 0.6 / n, S * 0.2 + j * S * 0.6 / n
                    d.rectangle([x0, y0, x0 + S * 0.6 / n, y0 + S * 0.6 / n], fill=(40, 40, 50, 255))
    elif glyph == "bars":
        for i, hh in enumerate([0.2, 0.35, 0.5, 0.3]):
            x0 = S * 0.22 + i * S * 0.15
            d.rectangle([x0, S * 0.75 - S * hh, x0 + S * 0.09, S * 0.75], fill=ink)
    elif glyph == "pen":
        d.polygon([(S * 0.25, S * 0.75), (S * 0.32, S * 0.55), (S * 0.68, S * 0.2), (S * 0.8, S * 0.32),
                   (S * 0.45, S * 0.68)], fill=(70, 50, 20, 240))
    elif glyph == "globe":
        d.ellipse([c - S * 0.28, c - S * 0.28, c + S * 0.28, c + S * 0.28], outline=ink, width=lw)
        d.ellipse([c - S * 0.12, c - S * 0.28, c + S * 0.12, c + S * 0.28], outline=ink, width=lw)
        d.line([c - S * 0.28, c, c + S * 0.28, c], fill=ink, width=lw)
    elif glyph == "dot":
        d.ellipse([c - S * 0.2, c - S * 0.2, c + S * 0.2, c + S * 0.2], fill=ink)
    elif glyph == "coin":
        d.ellipse([c - S * 0.25, c - S * 0.25, c + S * 0.25, c + S * 0.25], outline=ink, width=lw * 2)
        d.line([c, c - S * 0.13, c, c + S * 0.13], fill=ink, width=lw)
    elif glyph == "peak":
        d.polygon([(S * 0.15, S * 0.75), (S * 0.42, S * 0.3), (S * 0.58, S * 0.52), (S * 0.68, S * 0.4),
                   (S * 0.85, S * 0.75)], fill=ink)
    elif glyph == "lines":
        for i in range(5):
            d.line([S * 0.22, S * (0.25 + i * 0.12), S * (0.78 - (i % 2) * 0.15), S * (0.25 + i * 0.12)],
                   fill=(70, 70, 90, 255) if sum(bottom) > 500 else ink, width=lw)
    elif glyph == "star":
        pts = []
        for i in range(10):
            ang = -math.pi / 2 + i * math.pi / 5
            rad = S * (0.3 if i % 2 == 0 else 0.13)
            pts.append((c + rad * math.cos(ang), c + rad * math.sin(ang)))
        d.polygon(pts, fill=(255, 230, 120, 255))
    elif glyph == "keys":
        d.rectangle([S * 0.15, S * 0.3, S * 0.85, S * 0.72], fill=(250, 250, 250, 255))
        for i in range(1, 7):
            x0 = S * 0.15 + i * S * 0.1
            d.line([x0, S * 0.3, x0, S * 0.72], fill=(30, 30, 30, 255), width=max(2, k))
            if i not in (3,):
                d.rectangle([x0 - S * 0.03, S * 0.3, x0 + S * 0.03, S * 0.52], fill=(20, 20, 20, 255))
    elif glyph == "kite":
        d.polygon([(c, S * 0.15), (S * 0.72, c - S * 0.05), (c, S * 0.8), (S * 0.28, c - S * 0.05)], fill=ink)
        d.line([c, S * 0.15, c, S * 0.8], fill=bottom + (255,), width=lw)
    return im.resize((size, size), Image.LANCZOS)


def home_screen(w, h):
    s = w / 320 if w != 768 else 1.6
    t = np.linspace(0, 1, h, dtype=np.float32)[:, None, None]
    wall = np.array([58, 28, 96], np.float32) * (1 - t) + np.array([12, 120, 140], np.float32) * t
    wall = np.broadcast_to(wall, (h, w, 3)).copy()
    im = Image.fromarray(np.clip(wall, 0, 255).astype(np.uint8)).convert("RGBA")
    # Two translucent diagonal bands with hard, anti-aliased edges.
    k = 4
    bands = Image.new("RGBA", (w * k, h * k), (0, 0, 0, 0))
    bd = ImageDraw.Draw(bands)
    bd.polygon([(0, h * k * 0.55), (w * k, h * k * 0.18), (w * k, h * k * 0.34), (0, h * k * 0.75)],
               fill=(255, 140, 190, 60))
    bd.polygon([(0, h * k * 0.82), (w * k, h * k * 0.5), (w * k, h * k * 0.56), (0, h * k * 0.9)],
               fill=(120, 230, 255, 55))
    im.alpha_composite(bands.resize((w, h), Image.BOX))
    d = ImageDraw.Draw(im)
    small = font("Regular", round(10 * s) if w != 768 else 13)
    bold = font("SemiBold", round(12 * s) if w != 768 else 15)
    # Status bar: signal bars, the time and a battery.
    bar = round(20 * s) if w != 768 else 20
    d.rectangle([0, 0, w, bar], fill=(0, 0, 0, 150))
    for i in range(5):
        hh = (i + 1) * bar * 0.12
        d.rectangle([4 * s + i * 4 * s, bar * 0.75 - hh, 4 * s + i * 4 * s + 2.5 * s, bar * 0.75], fill=(255, 255, 255))
    d.text((w / 2, bar / 2), "10:24", font=bold, fill=(255, 255, 255), anchor="mm")
    bx = w - 30 * s
    d.rectangle([bx, bar * 0.3, bx + 20 * s, bar * 0.7], outline=(255, 255, 255), width=max(1, round(s)))
    d.rectangle([bx + 2 * s, bar * 0.3 + 2 * s, bx + 14 * s, bar * 0.7 - 2 * s], fill=(255, 255, 255))
    d.rectangle([bx + 20 * s, bar * 0.42, bx + 22 * s, bar * 0.58], fill=(255, 255, 255))
    # A notification banner with two lines of small text.
    top = bar + 8 * s
    d.rounded_rectangle([8 * s, top, w - 8 * s, top + 40 * s], radius=8 * s, fill=(20, 20, 28, 220))
    im.alpha_composite(icon(round(24 * s), (88, 196, 104), (30, 120, 60), "leaf"), (round(16 * s), round(top + 8 * s)))
    d.text((48 * s, top + 7 * s), "Garden", font=bold, fill=(255, 255, 255))
    d.text((48 * s, top + 22 * s), "Water the tomatoes before 6 pm, then pick the beans.", font=small,
           fill=(220, 220, 230))
    # The icon grid, its labels, page dots and the dock.
    size = round(57 * s) if w != 768 else 72
    cols = 4
    gap = (w - cols * size) / (cols + 1)
    grid_top = top + 52 * s
    rows = 4 if h / w < 1.6 else 5
    dock_h = size + round(30 * s)
    pitch = (h - dock_h - grid_top - 20 * s) / rows
    for i, (name, a, b, g) in enumerate((ICONS * 2)[: rows * cols]):
        r, c = divmod(i, cols)
        x0 = round(gap + c * (size + gap))
        y0 = round(grid_top + r * pitch)
        im.alpha_composite(icon(size, a, b, g), (x0, y0))
        d.text((x0 + size / 2 + 1, y0 + size + 4 * s + 1), name, font=small, fill=(0, 0, 0, 160), anchor="ma")
        d.text((x0 + size / 2, y0 + size + 4 * s), name, font=small, fill=(255, 255, 255), anchor="ma")
    dots_y = h - dock_h - 10 * s
    for i in range(3):
        cx = w / 2 + (i - 1) * 10 * s
        d.ellipse([cx - 2.5 * s, dots_y - 2.5 * s, cx + 2.5 * s, dots_y + 2.5 * s],
                  fill=(255, 255, 255, 255 if i == 0 else 110))
    shelf = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    sd = ImageDraw.Draw(shelf)
    sd.rectangle([0, h - dock_h, w, h], fill=(255, 255, 255, 46))
    sd.line([0, h - dock_h, w, h - dock_h], fill=(255, 255, 255, 120), width=max(1, round(s)))
    im.alpha_composite(shelf)
    for i, (name, a, b, g) in enumerate(DOCK):
        x0 = round(gap + i * (size + gap))
        y0 = round(h - dock_h + 6 * s)
        im.alpha_composite(icon(size, a, b, g), (x0, y0))
        d.text((x0 + size / 2, y0 + size + 3 * s), name, font=small, fill=(255, 255, 255), anchor="ma")
    return im.convert("RGB")


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(REPO, "art", "preview", "scaling")
    os.makedirs(out, exist_ok=True)
    total = 0
    for (w, h) in SIZES:
        for name, make in [("pixel", pixel_art), ("game", scene_3d), ("home", home_screen)]:
            path = os.path.join(out, f"{name}-{w}x{h}.png")
            make(w, h).save(path, optimize=True)
            total += os.path.getsize(path)
            print(f"{path}: {os.path.getsize(path) // 1024} KB")
    print(f"total {total // 1024} KB")


if __name__ == "__main__":
    main()

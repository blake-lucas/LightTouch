#!/usr/bin/env python3
"""Render the LightTouch icon from background.svg, foreground.svg and monochrome.svg into every place that ships it.

  Android/app/src/main/res/          adaptive launcher icon (API 26+; minSdk is 29): background, foreground and
                                     monochrome layers at 432 px (108 dp at xxxhdpi), and mipmap-anydpi-v26
  crates/lighttouch/assets/icon/     lighttouch.png (256, window icon and Linux AppImage), lighttouch.ico
                                     (Windows), lighttouch.icns (macOS)

The desktop icon is the two layers composited and cut to a rounded square, with the phone drawn larger than in
the adaptive icon, whose foreground must stay inside the launcher's 66 dp safe zone.

Needs cairosvg and Pillow (a venv is enough: python3 -m venv v && v/bin/pip install cairosvg pillow); the outputs
are committed, so only a change to the artwork needs this.
"""
import io
from pathlib import Path

import cairosvg
from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
RES = ROOT / 'Android/app/src/main/res'
DESKTOP = ROOT / 'crates/lighttouch/assets/icon'
DESKTOP_ZOOM = 1.3


def layer(name, size):
    png = cairosvg.svg2png(url=str(HERE / f'{name}.svg'), output_width=size, output_height=size)
    return Image.open(io.BytesIO(png)).convert('RGBA')


def desktop(size):
    background = layer('background', size)
    big = layer('foreground', round(size * DESKTOP_ZOOM))
    offset = (big.width - size) // 2
    icon = Image.alpha_composite(background, big.crop((offset, offset, offset + size, offset + size)))
    mask = Image.new('L', (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, size - 1, size - 1), radius=round(size * 0.225), fill=255)
    icon.putalpha(mask)
    return icon


def main():
    drawable = RES / 'drawable-nodpi'
    drawable.mkdir(parents=True, exist_ok=True)
    layer('background', 432).convert('RGB').save(drawable / 'ic_launcher_background.png', optimize=True)
    foreground = layer('foreground', 432)
    foreground.save(drawable / 'ic_launcher_foreground.png', optimize=True)
    # Themed icons (Android 13+) tint this one-colour line drawing.
    layer('monochrome', 432).save(drawable / 'ic_launcher_monochrome.png', optimize=True)
    anydpi = RES / 'mipmap-anydpi-v26'
    anydpi.mkdir(parents=True, exist_ok=True)
    (anydpi / 'ic_launcher.xml').write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
        '    <background android:drawable="@drawable/ic_launcher_background"/>\n'
        '    <foreground android:drawable="@drawable/ic_launcher_foreground"/>\n'
        '    <monochrome android:drawable="@drawable/ic_launcher_monochrome"/>\n'
        '</adaptive-icon>\n')

    DESKTOP.mkdir(parents=True, exist_ok=True)
    big = desktop(1024)
    desktop(256).save(DESKTOP / 'lighttouch.png', optimize=True)
    big.save(DESKTOP / 'lighttouch.ico', sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    big.save(DESKTOP / 'lighttouch.icns')


if __name__ == '__main__':
    main()

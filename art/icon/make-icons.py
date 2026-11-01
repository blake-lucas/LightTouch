#!/usr/bin/env python3
"""Render the LightTouch icon from background.svg, foreground.svg and monochrome.svg into every place that ships it.

The icon is a teal disc with the foreground on it. The SVGs are drawn in the disc's coordinates (the disc fills
their 1024 x 1024 view box), so every platform shows the same picture at the same proportions:

  Android/app/src/main/res/          adaptive launcher icon (API 26+; minSdk is 29): background, foreground and
                                     monochrome layers at 432 px (108 dp at xxxhdpi), and mipmap-anydpi-v26. The
                                     launcher shows the middle 72 dp in its own shape (a circle on Pixels), so the
                                     layers are drawn with the disc filling that 72 dp.
  crates/lighttouch/assets/icon/     lighttouch.png (256, the window icon and the Linux AppImage's), lighttouch.ico
                                     (Windows; each size rendered from the SVGs, so the small ones stay sharp) and
                                     lighttouch.icns (macOS; the disc inside Apple's 824-of-1024 grid, with a shadow)

Needs cairosvg and Pillow (a venv is enough: python3 -m venv v && v/bin/pip install cairosvg pillow); the outputs
are committed, so only a change to the artwork needs this.
"""
import io
from pathlib import Path

import cairosvg
from PIL import Image, ImageChops, ImageFilter

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
RES = ROOT / 'Android/app/src/main/res'
DESKTOP = ROOT / 'crates/lighttouch/assets/icon'
# An adaptive icon's layers are 108 dp; the launcher's mask shows the middle 72 dp.
ANDROID_ZOOM = 108 / 72
ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)


def svg(source, size, zoom=1.0):
    """SOURCE (SVG text) rendered SIZE px square, its view box widened ZOOM times around the middle."""
    if zoom != 1.0:
        span = 1024 * zoom
        start = (1024 - span) / 2
        source = source.replace('viewBox="0 0 1024 1024"', f'viewBox="{start:g} {start:g} {span:g} {span:g}"', 1)
    png = cairosvg.svg2png(bytestring=source.encode(), output_width=size, output_height=size)
    return Image.open(io.BytesIO(png)).convert('RGBA')


def layer(name, size, zoom=1.0):
    return svg((HERE / f'{name}.svg').read_text(), size, zoom)


def disc(size):
    """The icon: the background cut to a disc, the foreground on top."""
    mask = svg('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">'
               '<circle cx="512" cy="512" r="512" fill="#FFFFFF"/></svg>', size).getchannel('A')
    icon = layer('background', size)
    icon.putalpha(ImageChops.multiply(icon.getchannel('A'), mask))
    return Image.alpha_composite(icon, layer('foreground', size))


def macos(size=1024):
    """The disc on Apple's grid: 824 px of the 1024 canvas, over a soft shadow like the Dock's other icons."""
    inner = round(size * 824 / 1024)
    at = (size - inner) // 2
    icon = disc(inner)
    shadow = Image.new('RGBA', (size, size), (0, 0, 0, 0))
    shadow.paste((0, 0, 0, 90), (at, at + round(size * 0.012)), icon.getchannel('A'))
    canvas = shadow.filter(ImageFilter.GaussianBlur(size * 0.012))
    canvas.alpha_composite(icon, (at, at))
    return canvas


def main():
    drawable = RES / 'drawable-nodpi'
    drawable.mkdir(parents=True, exist_ok=True)
    layer('background', 432, ANDROID_ZOOM).convert('RGB').save(drawable / 'ic_launcher_background.png', optimize=True)
    layer('foreground', 432, ANDROID_ZOOM).save(drawable / 'ic_launcher_foreground.png', optimize=True)
    # Themed icons (Android 13+) tint this one-colour layer.
    layer('monochrome', 432, ANDROID_ZOOM).save(drawable / 'ic_launcher_monochrome.png', optimize=True)
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
    disc(256).save(DESKTOP / 'lighttouch.png', optimize=True)
    sizes = [disc(n) for n in ICO_SIZES]
    sizes[-1].save(DESKTOP / 'lighttouch.ico', sizes=[(n, n) for n in ICO_SIZES], append_images=sizes[:-1])
    macos().save(DESKTOP / 'lighttouch.icns')


if __name__ == '__main__':
    main()

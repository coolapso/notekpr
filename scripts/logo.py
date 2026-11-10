#!/usr/bin/env python3
"""The notekpr logo and icon in docs/assets/, for the README and anywhere
else they're needed (avatars, thumbnails, slides).

    python3 scripts/logo.py path/to/FiraCode-SemiBold.ttf

writes, all with transparent backgrounds:

    docs/assets/logo.svg, logo.png            the name, for light backgrounds
    docs/assets/logo-dark.svg, logo-dark.png  the same for dark backgrounds
    docs/assets/icon.svg, icon.png            the icon alone

and the web app's icons for home screens, in crates/web/static/ (see
APP_ICONS).

The logo is "notekpr" in Fira Code SemiBold with the icon's diamond ring as
the "o". The letters are stored as outlines, so it looks the same where the
font isn't installed. Fira Code is free (SIL Open Font License 1.1):
https://github.com/tonsky/FiraCode/releases — ttf/FiraCode-SemiBold.ttf in
the zip (the logo was made with 6.2).

The icon is the favicon, crates/web/static/favicon.svg: change it there (and
its copies, see docs/development.md "Logo and icon"), then run this again.

Needs fontTools (python-fonttools, or `pip install fonttools`) and
rsvg-convert (librsvg) for the PNGs. Not part of the build, `task setup` or
CI: run it by hand when the logo or the icon changes, and commit what it
writes.
"""

import argparse
import os
import shutil
import subprocess
import sys

try:
    from fontTools.pens.boundsPen import BoundsPen
    from fontTools.pens.svgPathPen import SVGPathPen
    from fontTools.pens.transformPen import TransformPen
    from fontTools.ttLib import TTFont
except ImportError:
    sys.exit("logo.py needs fontTools: install python-fonttools, or `pip install fonttools`")

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "docs", "assets")
STATIC = os.path.join(ROOT, "crates", "web", "static")
FAVICON = os.path.join(STATIC, "favicon.svg")

# Sizes in the icon's units (its viewBox is 64 x 64).
X_HEIGHT = 24.0  # the lower-case letters
TRACKING = -1.5  # after each letter: Fira Code is monospaced, a little tighter reads as one word
RING = 27.0  # the diamond ring: pointed shapes look smaller, so it's taller than the letters

# Text and accent colours of the app and the website (crates/web/static/style.css),
# light theme and dark theme.
LOGOS = (
    ("logo", "#2a2a28", "#5b5bd6"),
    ("logo-dark", "#e6e6e3", "#8b8cf4"),
)
LOGO_PNG_WIDTH = 1600
ICON_PNG_WIDTH = 512

# The web app's icons for home screens (its manifest.webmanifest and
# index.html): file, size, and whether the colour fills the whole square.
# iPhones (apple-touch-icon) and Android ("maskable") cut their own shape out
# of a full square, and an iPhone turns transparent corners black; the others
# keep the favicon's rounded corners. The diamond stays inside the middle 80%
# that a maskable icon may lose nothing of.
APP_ICONS = (
    ("apple-touch-icon.png", 180, True),
    ("icon-192.png", 192, False),
    ("icon-512.png", 512, False),
    ("icon-maskable.png", 512, True),
)


def num(v):
    """A coordinate as short as it gets: 12.50 → 12.5, 40.00 → 40."""
    return ("%.2f" % v).rstrip("0").rstrip(".")


def diamond_ring(cx, cy):
    """The icon's diamond ring (outer 36 x 40, inner 20 x 20 around (32, 32)),
    RING high and centred on (cx, cy): its path and bounds."""
    k = RING / 40

    def at(points):
        return " ".join("%s %s" % (num(cx + (x - 32) * k), num(cy + (y - 32) * k)) for x, y in points)

    outer = at([(32, 12), (50, 32), (32, 52), (14, 32)])
    inner = at([(32, 22), (22, 32), (32, 42), (42, 32)])
    return "M%sZM%sZ" % (outer, inner), (cx - 18 * k, cy - 20 * k, cx + 18 * k, cy + 20 * k)


def wordmark(font_path):
    """The letters of "notekpr" but the "o" as one path, the ring in the o's
    place, and the viewBox around both."""
    font = TTFont(font_path)
    if font["OS/2"].usWeightClass != 600 or "Fira" not in font["name"].getBestFamilyName():
        print("warning: %s isn't Fira Code SemiBold" % font_path, file=sys.stderr)
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    scale = X_HEIGHT / font["OS/2"].sxHeight
    baseline = 40.0
    letters = SVGPathPen(glyphs, ntos=num)
    bounds = BoundsPen(glyphs)
    x = 0.0
    for ch in "notekpr":
        glyph = glyphs[cmap[ord(ch)]]
        # font units to SVG units: scale, flip (fonts count up, SVG down), move to x on the baseline
        to_svg = (scale, 0, 0, -scale, x, baseline)
        if ch == "o":
            o = BoundsPen(glyphs)
            glyph.draw(TransformPen(o, to_svg))
            ring, ring_bounds = diamond_ring((o.bounds[0] + o.bounds[2]) / 2, baseline - X_HEIGHT / 2)
        else:
            glyph.draw(TransformPen(letters, to_svg))
            glyph.draw(TransformPen(bounds, to_svg))
        x += glyph.width * scale + TRACKING
    x0, y0 = min(bounds.bounds[0], ring_bounds[0]), min(bounds.bounds[1], ring_bounds[1])
    x1, y1 = max(bounds.bounds[2], ring_bounds[2]), max(bounds.bounds[3], ring_bounds[3])
    view_box = " ".join(num(v) for v in (x0, y0, x1 - x0, y1 - y0))
    return letters.getCommands(), ring, view_box


def png(svg, width):
    out = svg[: -len(".svg")] + ".png"
    subprocess.run(["rsvg-convert", "-w", str(width), svg, "-o", out], check=True)
    return out


def app_icons():
    """The web app's PNG icons (APP_ICONS), from the favicon."""
    with open(FAVICON) as f:
        rounded = f.read()
    square = rounded.replace(' rx="14"', "", 1)
    if square == rounded:
        sys.exit('logo.py: the favicon has no rx="14" to square its corners: change APP_ICONS\' drawing')
    written = []
    for name, size, full in APP_ICONS:
        out = os.path.join(STATIC, name)
        svg = (square if full else rounded).encode()
        subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), "-o", out], input=svg, check=True)
        written.append(out)
    return written


def main():
    parser = argparse.ArgumentParser(description="Draw the notekpr logo and icon into docs/assets/.")
    parser.add_argument("font", help="Fira Code SemiBold (FiraCode-SemiBold.ttf)")
    args = parser.parse_args()
    if not shutil.which("rsvg-convert"):
        sys.exit("logo.py needs rsvg-convert (librsvg) for the PNGs")

    letters, ring, view_box = wordmark(args.font)
    os.makedirs(ASSETS, exist_ok=True)
    written = []
    for name, text, accent in LOGOS:
        svg = os.path.join(ASSETS, name + ".svg")
        with open(svg, "w") as f:
            f.write(
                '<svg xmlns="http://www.w3.org/2000/svg" viewBox="%s" role="img" aria-label="notekpr">'
                '<!-- Made by scripts/logo.py: Fira Code SemiBold (SIL Open Font License 1.1) '
                "as outlines; the \"o\" is the icon's diamond. -->"
                '<path d="%s" fill="%s"/><path d="%s" fill="%s" fill-rule="evenodd"/></svg>\n'
                % (view_box, letters, text, ring, accent)
            )
        written += [svg, png(svg, LOGO_PNG_WIDTH)]
    icon = os.path.join(ASSETS, "icon.svg")
    shutil.copyfile(FAVICON, icon)
    written += [icon, png(icon, ICON_PNG_WIDTH)]
    written += app_icons()
    for path in written:
        print(os.path.relpath(path, ROOT))


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Shadow Glyph — Shadow's own display typeface (9.0), generated from stroke definitions.

An original design for the Vitruvian / kinetic-unfolding look:
  * every letter is drawn on a construction square (the Vitruvian square) with its corners cut
    at 45°, so curves become facets — a tessellated, alien geometry instead of round bowls;
  * stroke ends carry small diamond nodes, like compass pivots on a construction drawing;
  * each letter's longest stroke is split by a narrow seam — the fold line it "unfolds" along —
    and shadowed by a hairline construction stroke, the ghost of where it folded from.
Unicase (lower case uses the capitals), monospaced (600 units), cap height 700.

    python3 tools/font/build_font.py [OUT.ttf]

Needs fontTools. The output is committed (assets/fonts/ShadowGlyph-Regular.ttf), so building
Shadow does not need Python.
"""
import math
import sys

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

UPM = 1000
ADV = 600
X0, X1 = 92, 508          # the construction square's sides
CAP = 700
STROKE = 46
NODE = 60                 # half-diagonal of a stroke-end diamond
SEAM = 0.075              # the unfolding seam, fraction of the segment

def oct_(x0, y0, x1, y1, c):
    """closed chamfered rectangle (a tessellated bowl)"""
    return [(x0 + c, y0), (x1 - c, y0), (x1, y0 + c), (x1, y1 - c), (x1 - c, y1), (x0 + c, y1), (x0, y1 - c), (x0, y0 + c), (x0 + c, y0)]

O = oct_(0, 0, 1, 1, 0.2)

# strokes per character: lists of polylines in the unit box (x 0..1, y 0..1 = baseline..cap)
G = {
    "A": [[(0, 0), (0.5, 1), (1, 0)], [(0.24, 0.36), (0.76, 0.36)]],
    "B": [[(0, 0), (0, 1), (0.7, 1), (0.92, 0.84), (0.92, 0.68), (0.72, 0.53), (0, 0.53)], [(0.72, 0.53), (1, 0.36), (1, 0.16), (0.8, 0), (0, 0)]],
    "C": [[(1, 0.8), (0.8, 1), (0.2, 1), (0, 0.8), (0, 0.2), (0.2, 0), (0.8, 0), (1, 0.2)]],
    "D": [[(0, 0), (0, 1), (0.66, 1), (1, 0.7), (1, 0.3), (0.66, 0), (0, 0)]],
    "E": [[(1, 1), (0, 1), (0, 0), (1, 0)], [(0, 0.53), (0.72, 0.53)]],
    "F": [[(1, 1), (0, 1), (0, 0)], [(0, 0.53), (0.72, 0.53)]],
    "G": [[(1, 0.8), (0.8, 1), (0.2, 1), (0, 0.8), (0, 0.2), (0.2, 0), (0.8, 0), (1, 0.2), (1, 0.48), (0.55, 0.48)]],
    "H": [[(0, 0), (0, 1)], [(1, 0), (1, 1)], [(0, 0.53), (1, 0.53)]],
    "I": [[(0.5, 0), (0.5, 1)], [(0.18, 1), (0.82, 1)], [(0.18, 0), (0.82, 0)]],
    "J": [[(0.3, 1), (1, 1)], [(0.82, 1), (0.82, 0.2), (0.62, 0), (0.2, 0), (0, 0.2), (0, 0.32)]],
    "K": [[(0, 0), (0, 1)], [(1, 1), (0, 0.4)], [(0.32, 0.6), (1, 0)]],
    "L": [[(0, 1), (0, 0), (1, 0)]],
    "M": [[(0, 0), (0, 1), (0.5, 0.42), (1, 1), (1, 0)]],
    "N": [[(0, 0), (0, 1), (1, 0), (1, 1)]],
    "O": [O],
    "P": [[(0, 0), (0, 1), (0.76, 1), (1, 0.8), (1, 0.62), (0.76, 0.42), (0, 0.42)]],
    "Q": [O, [(0.6, 0.3), (1.06, -0.1)]],
    "R": [[(0, 0), (0, 1), (0.76, 1), (1, 0.8), (1, 0.62), (0.76, 0.42), (0, 0.42)], [(0.5, 0.42), (1, 0)]],
    "S": [[(1, 0.8), (0.8, 1), (0.2, 1), (0, 0.8), (0, 0.68), (0.18, 0.53), (0.82, 0.47), (1, 0.32), (1, 0.2), (0.8, 0), (0.2, 0), (0, 0.2)]],
    "T": [[(0, 1), (1, 1)], [(0.5, 1), (0.5, 0)]],
    "U": [[(0, 1), (0, 0.2), (0.2, 0), (0.8, 0), (1, 0.2), (1, 1)]],
    "V": [[(0, 1), (0.5, 0), (1, 1)]],
    "W": [[(0, 1), (0.22, 0), (0.5, 0.6), (0.78, 0), (1, 1)]],
    "X": [[(0, 1), (1, 0)], [(1, 1), (0, 0)]],
    "Y": [[(0, 1), (0.5, 0.48), (1, 1)], [(0.5, 0.48), (0.5, 0)]],
    "Z": [[(0, 1), (1, 1), (0, 0), (1, 0)]],
    "0": [oct_(0.06, 0, 0.94, 1, 0.24), [(0.28, 0.26), (0.72, 0.74)]],
    "1": [[(0.2, 0.78), (0.5, 1), (0.5, 0)], [(0.18, 0), (0.82, 0)]],
    "2": [[(0, 0.8), (0.2, 1), (0.8, 1), (1, 0.8), (1, 0.62), (0, 0), (1, 0)]],
    "3": [[(0, 1), (1, 1), (0.42, 0.56), (0.8, 0.56), (1, 0.38), (1, 0.2), (0.8, 0), (0.2, 0), (0, 0.2)]],
    "4": [[(0.76, 0), (0.76, 1), (0, 0.3), (1, 0.3)]],
    "5": [[(1, 1), (0.04, 1), (0, 0.56), (0.8, 0.56), (1, 0.38), (1, 0.2), (0.8, 0), (0.2, 0), (0, 0.2)]],
    "6": [[(0.92, 1), (0.32, 1), (0, 0.7), (0, 0.2), (0.2, 0), (0.8, 0), (1, 0.2), (1, 0.36), (0.8, 0.56), (0, 0.56)]],
    "7": [[(0, 1), (1, 1), (0.34, 0)]],
    "8": [[(0.2, 0.54), (0.04, 0.68), (0.04, 0.84), (0.2, 1), (0.8, 1), (0.96, 0.84), (0.96, 0.68), (0.8, 0.54), (0.2, 0.54), (0, 0.38), (0, 0.18), (0.2, 0), (0.8, 0), (1, 0.18), (1, 0.38), (0.8, 0.54)]],
    "9": [[(0.08, 0), (0.68, 0), (1, 0.3), (1, 0.8), (0.8, 1), (0.2, 1), (0, 0.8), (0, 0.64), (0.2, 0.44), (1, 0.44)]],
    "-": [[(0.18, 0.46), (0.82, 0.46)]],
    "_": [[(0, -0.12), (1, -0.12)]],
    "/": [[(0.12, -0.06), (0.88, 1.06)]],
    "\\": [[(0.12, 1.06), (0.88, -0.06)]],
    "(": [[(0.7, 1.08), (0.4, 0.78), (0.4, 0.22), (0.7, -0.08)]],
    ")": [[(0.3, 1.08), (0.6, 0.78), (0.6, 0.22), (0.3, -0.08)]],
    "[": [[(0.72, 1.08), (0.36, 1.08), (0.36, -0.08), (0.72, -0.08)]],
    "]": [[(0.28, 1.08), (0.64, 1.08), (0.64, -0.08), (0.28, -0.08)]],
    "{": [[(0.72, 1.08), (0.5, 1.0), (0.5, 0.6), (0.3, 0.5), (0.5, 0.4), (0.5, 0.0), (0.72, -0.08)]],
    "}": [[(0.28, 1.08), (0.5, 1.0), (0.5, 0.6), (0.7, 0.5), (0.5, 0.4), (0.5, 0.0), (0.28, -0.08)]],
    "!": [[(0.5, 1), (0.5, 0.3)]],
    "?": [[(0.04, 0.8), (0.24, 1), (0.8, 1), (1, 0.8), (1, 0.64), (0.5, 0.42), (0.5, 0.28)]],
    "'": [[(0.5, 1.02), (0.5, 0.74)]],
    '"': [[(0.34, 1.02), (0.34, 0.74)], [(0.66, 1.02), (0.66, 0.74)]],
    "`": [[(0.38, 1.04), (0.6, 0.82)]],
    "+": [[(0.14, 0.46), (0.86, 0.46)], [(0.5, 0.1), (0.5, 0.82)]],
    "=": [[(0.14, 0.6), (0.86, 0.6)], [(0.14, 0.32), (0.86, 0.32)]],
    "*": [[(0.5, 0.86), (0.5, 0.3)], [(0.26, 0.72), (0.74, 0.44)], [(0.74, 0.72), (0.26, 0.44)]],
    "#": [[(0.34, 0.06), (0.42, 0.94)], [(0.62, 0.06), (0.7, 0.94)], [(0.08, 0.66), (0.94, 0.66)], [(0.06, 0.34), (0.92, 0.34)]],
    "%": [[(0.06, 0.02), (0.94, 0.98)], oct_(0.02, 0.66, 0.34, 0.98, 0.09), oct_(0.66, 0.02, 0.98, 0.34, 0.09)],
    "&": [[(1, 0), (0.18, 0.72), (0.18, 0.88), (0.32, 1), (0.6, 1), (0.72, 0.88), (0.72, 0.74), (0, 0.32), (0, 0.16), (0.18, 0), (0.6, 0), (1, 0.36)]],
    "@": [[(0.74, 0.3), (0.74, 0.7), (0.3, 0.7), (0.3, 0.3), (0.74, 0.3), (0.92, 0.3), (1, 0.42), (1, 0.82), (0.8, 1), (0.2, 1), (0, 0.8), (0, 0.2), (0.2, 0), (0.86, 0)]],
    "<": [[(0.86, 0.86), (0.14, 0.46), (0.86, 0.06)]],
    ">": [[(0.14, 0.86), (0.86, 0.46), (0.14, 0.06)]],
    "|": [[(0.5, -0.1), (0.5, 1.1)]],
    "~": [[(0.06, 0.38), (0.26, 0.56), (0.48, 0.46), (0.7, 0.38), (0.94, 0.56)]],
    "^": [[(0.16, 0.66), (0.5, 1.02), (0.84, 0.66)]],
    "$": [[(1, 0.78), (0.8, 0.94), (0.2, 0.94), (0, 0.78), (0, 0.66), (0.18, 0.52), (0.82, 0.48), (1, 0.34), (1, 0.22), (0.8, 0.06), (0.2, 0.06), (0, 0.22)], [(0.5, 1.1), (0.5, -0.1)]],
    "→": [[(0.04, 0.46), (0.94, 0.46)], [(0.6, 0.76), (0.94, 0.46), (0.6, 0.16)]],
    "←": [[(0.96, 0.46), (0.06, 0.46)], [(0.4, 0.76), (0.06, 0.46), (0.4, 0.16)]],
    "↑": [[(0.5, 0), (0.5, 0.96)], [(0.18, 0.64), (0.5, 0.96), (0.82, 0.64)]],
    "↓": [[(0.5, 1), (0.5, 0.04)], [(0.18, 0.36), (0.5, 0.04), (0.82, 0.36)]],
    "°": [oct_(0.32, 0.72, 0.68, 1.06, 0.1)],
}
# dots are diamonds (the construction pivots), not strokes
DOTS = {
    ".": [(0.5, 0.05)], ",": [(0.5, 0.05)], ":": [(0.5, 0.05), (0.5, 0.58)], ";": [(0.5, 0.05), (0.5, 0.58)],
    "!": [(0.5, 0.05)], "?": [(0.5, 0.05)], "·": [(0.5, 0.46)], "…": [(0.12, 0.05), (0.5, 0.05), (0.88, 0.05)],
}
TAILS = {",": [[(0.5, 0.05), (0.36, -0.16)]], ";": [[(0.5, 0.05), (0.36, -0.16)]]}


def to_units(p):
    return (X0 + p[0] * (X1 - X0), p[1] * CAP)


def quad(a, b, w):
    """the stroke of a segment: a rectangle around it (clockwise)"""
    (x0, y0), (x1, y1) = a, b
    dx, dy = x1 - x0, y1 - y0
    n = math.hypot(dx, dy) or 1.0
    nx, ny = -dy / n * w / 2, dx / n * w / 2
    pts = [(x0 + nx, y0 + ny), (x1 + nx, y1 + ny), (x1 - nx, y1 - ny), (x0 - nx, y0 - ny)]
    return clockwise(pts)


def joint(p, w):
    """an octagon at a corner: joins strokes without gaps (faceted, not round)"""
    r = w / 2 / math.cos(math.pi / 8)
    return clockwise([(p[0] + r * math.cos(math.pi / 8 + i * math.pi / 4), p[1] + r * math.sin(math.pi / 8 + i * math.pi / 4)) for i in range(8)])


def diamond(p, r):
    return clockwise([(p[0], p[1] + r), (p[0] + r, p[1]), (p[0], p[1] - r), (p[0] - r, p[1])])


def area(pts):
    return sum(pts[i][0] * pts[(i + 1) % len(pts)][1] - pts[(i + 1) % len(pts)][0] * pts[i][1] for i in range(len(pts))) / 2


def clockwise(pts):
    # TrueType outer contours run clockwise (negative signed area with y up)
    return pts if area(pts) < 0 else list(reversed(pts))


def contours(ch):
    lines = [[to_units(p) for p in pl] for pl in G.get(ch, [])] + [[to_units(p) for p in pl] for pl in TAILS.get(ch, [])]
    out = []
    # the unfolding seam: split the longest segment of the glyph (letters and digits)
    seam = None
    if ch.isalnum() and lines:
        best = max(((li, si, math.dist(pl[si], pl[si + 1])) for li, pl in enumerate(lines) for si in range(len(pl) - 1)), key=lambda t: t[2])
        if best[2] > 300:
            seam = best[:2]
    for li, pl in enumerate(lines):
        closed = len(pl) > 2 and math.dist(pl[0], pl[-1]) < 1
        for si in range(len(pl) - 1):
            a, b = pl[si], pl[si + 1]
            if seam == (li, si):
                m = ((a[0] + b[0]) / 2, (a[1] + b[1]) / 2)
                t = SEAM / 2
                a2 = (m[0] - (b[0] - a[0]) * t, m[1] - (b[1] - a[1]) * t)
                b2 = (m[0] + (b[0] - a[0]) * t, m[1] + (b[1] - a[1]) * t)
                out.append(quad(a, a2, STROKE))
                out.append(quad(b2, b, STROKE))
                # the construction hairline beside the seam
                dx, dy = b[0] - a[0], b[1] - a[1]
                n = math.hypot(dx, dy)
                ox, oy = -dy / n * 40, dx / n * 40
                k = 0.22
                h0 = (a[0] + dx * k + ox, a[1] + dy * k + oy)
                h1 = (b[0] - dx * k + ox, b[1] - dy * k + oy)
                if 40 < (h0[0] + h1[0]) / 2 < ADV - 40:
                    out.append(quad(h0, h1, 13))
            else:
                out.append(quad(a, b, STROKE))
        for p in pl[1:-1]:
            out.append(joint(p, STROKE))
        if closed:
            out.append(joint(pl[0], STROKE))
        elif ch.isalnum():
            # compass-pivot nodes at the stroke ends
            out.append(diamond(pl[0], NODE))
            out.append(diamond(pl[-1], NODE))
        else:
            out.append(joint(pl[0], STROKE))
            out.append(joint(pl[-1], STROKE))
    for d in DOTS.get(ch, []):
        out.append(diamond(to_units(d), NODE * 1.25))
    return out


def glyph_name(ch):
    if ch == " ":
        return "space"
    return "uni%04X" % ord(ch)


def build(path):
    chars = sorted(set(G) | set(DOTS))
    names = [".notdef", "space"] + [glyph_name(c) for c in chars]
    glyphs = {}
    pen = TTGlyphPen(None)
    # .notdef: an empty construction square
    for c in [[(X0, 0), (X0, CAP), (X1, CAP), (X1, 0)], [(X0 + 50, 50), (X1 - 50, 50), (X1 - 50, CAP - 50), (X0 + 50, CAP - 50)]]:
        pen.moveTo(c[0])
        for p in c[1:]:
            pen.lineTo(p)
        pen.closePath()
    glyphs[".notdef"] = pen.glyph()
    glyphs["space"] = TTGlyphPen(None).glyph()
    for ch in chars:
        pen = TTGlyphPen(None)
        for c in contours(ch):
            pts = [(round(x), round(y)) for x, y in c]
            pen.moveTo(pts[0])
            for p in pts[1:]:
                pen.lineTo(p)
            pen.closePath()
        glyphs[glyph_name(ch)] = pen.glyph()
    cmap = {ord(" "): "space"}
    for ch in chars:
        cmap[ord(ch)] = glyph_name(ch)
        if ch.isalpha() and ch.isascii():
            cmap[ord(ch.lower())] = glyph_name(ch)  # unicase
    fb = FontBuilder(UPM, isTTF=True)
    fb.setupGlyphOrder(names)
    fb.setupCharacterMap(cmap)
    fb.setupGlyf(glyphs)
    metrics = {n: (ADV, glyphs[n].xMin if hasattr(glyphs[n], "xMin") and glyphs[n].numberOfContours else 0) for n in names}
    fb.setupHorizontalMetrics(metrics)
    fb.setupHorizontalHeader(ascent=920, descent=-230)
    fb.setupNameTable({
        "familyName": "Shadow Glyph", "styleName": "Regular", "uniqueFontIdentifier": "ShadowGlyph-Regular-9.0",
        "fullName": "Shadow Glyph Regular", "psName": "ShadowGlyph-Regular", "version": "Version 9.000",
        "copyright": "Shadow Glyph — an original typeface made for Shadow TUI Restricted",
        "licenseDescription": "Licensed under the SIL Open Font License, Version 1.1",
    })
    fb.setupOS2(sTypoAscender=920, sTypoDescender=-230, sTypoLineGap=80, usWinAscent=1000, usWinDescent=260, sxHeight=700, sCapHeight=700,
                achVendID="SHDW", fsType=0)
    fb.setupPost(isFixedPitch=1)
    fb.save(path)
    return len(chars)


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "assets/fonts/ShadowGlyph-Regular.ttf"
    n = build(out)
    print(f"{out}: {n} characters")

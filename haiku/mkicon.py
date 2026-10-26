"""Writes Temino's Haiku vector icon (HVIF) and the resource file around it.

usage: python3 -I haiku/mkicon.py [--preview out.ppm]
       (rewrites haiku/Temino.rdef and haiku/temino.hvif)

The icon (John's design, 2026-10-25): a chunky silver terminal screen seen
slightly from the left, with a blue side and a yellow tab on top, and a
cream ">_" prompt on the dark screen. It was traced from a 1254 px drawing,
so shapes are written in that drawing's pixels and scaled onto the 64x64
HVIF grid. Rounded corners are sampled curves; every shape is a polygon.
"""
import math
import os
import sys

SOURCE = 1254.0


def rounded(points, radius, steps=5):
    """Rounds every corner of a polygon (radius in source pixels)."""
    out = []
    n = len(points)
    for i in range(n):
        ax, ay = points[i - 1]
        px, py = points[i]
        bx, by = points[(i + 1) % n]
        r = radius[i] if isinstance(radius, (list, tuple)) else radius
        la = math.hypot(ax - px, ay - py)
        lb = math.hypot(bx - px, by - py)
        r = min(r, la / 2, lb / 2)
        if r <= 0:
            out.append((px, py))
            continue
        p1 = (px + (ax - px) * r / la, py + (ay - py) * r / la)
        p2 = (px + (bx - px) * r / lb, py + (by - py) * r / lb)
        for s in range(steps + 1):
            t = s / steps
            # Quadratic Bezier with the corner as control point.
            x = (1 - t) ** 2 * p1[0] + 2 * (1 - t) * t * px + t * t * p2[0]
            y = (1 - t) ** 2 * p1[1] + 2 * (1 - t) * t * py + t * t * p2[1]
            out.append((x, y))
    return out


def inset(points, amount):
    """Moves each vertex toward the polygon's centroid (cheap inset)."""
    cx = sum(p[0] for p in points) / len(points)
    cy = sum(p[1] for p in points) / len(points)
    out = []
    for x, y in points:
        d = math.hypot(x - cx, y - cy)
        out.append((x + (cx - x) * amount / d, y + (cy - y) * amount / d))
    return out


def offset(points, dx, dy):
    return [(x + dx, y + dy) for x, y in points]


colors = {
    "shadow": (0x00, 0x00, 0x00, 0x48),
    "blue_dark": (0x0f, 0x3d, 0xb8, 0xff),
    "blue": (0x1e, 0x5c, 0xe6, 0xff),
    "blue_light": (0x4a, 0x86, 0xff, 0xff),
    "tab_front": (0xe0, 0x9c, 0x00, 0xff),
    "tab": (0xff, 0xc8, 0x1e, 0xff),
    "tab_light": (0xff, 0xe2, 0x6a, 0xff),
    "depth": (0x5f, 0x64, 0x6d, 0xff),
    "frame_dark": (0x8e, 0x93, 0x9c, 0xff),
    "frame": (0xc4, 0xc8, 0xce, 0xff),
    "frame_light": (0xec, 0xee, 0xf1, 0xff),
    "bezel": (0x15, 0x18, 0x1d, 0xff),
    "screen": (0x28, 0x2c, 0x33, 0xff),
    "prompt": (0xf8, 0xf3, 0xe4, 0xff),
}

# The frame's outer outline, then the screen inside it.
FRAME = [(272, 420), (1092, 238), (1104, 888), (282, 1104)]
SCREEN = [(356, 527), (1040, 352), (1042, 858), (362, 1028)]

shapes = [
    ("shadow", rounded(offset(FRAME, 18, 34), 70)),
    # Blue side slab, darker toward the back.
    ("blue_dark", rounded([(150, 382), (282, 430), (290, 1104), (160, 1030)], [30, 10, 40, 40])),
    ("blue", rounded([(178, 392), (282, 430), (290, 1104), (190, 1050)], [20, 10, 40, 30])),
    ("blue_light", [(250, 418), (280, 430), (286, 1060), (256, 1045)]),
    # Yellow tab: front face, top face, highlight.
    ("tab_front", rounded([(166, 362), (575, 300), (578, 352), (272, 440), (176, 398)], [20, 12, 20, 10, 30])),
    ("tab", rounded([(166, 362), (198, 290), (300, 220), (470, 180), (560, 186), (576, 214),
                     (576, 316), (282, 398), (180, 370)], [10, 50, 80, 60, 40, 30, 12, 20, 20])),
    ("tab_light", rounded([(214, 300), (300, 240), (468, 200), (540, 202), (520, 226),
                           (300, 268), (226, 318)], 18)),
    # Silver frame: depth behind, then face, top highlight, lower shade.
    ("depth", rounded(offset(FRAME, 14, 20), 70)),
    ("frame_dark", rounded(FRAME, 70)),
    ("frame", rounded(inset(FRAME, 14), 60)),
    ("frame_light", rounded([(300, 432), (1078, 258), (1082, 300), (306, 478)], 30)),
    ("frame_dark", rounded([(310, 1056), (1080, 852), (1082, 872), (300, 1086)], 16)),
    # Screen with a dark bezel line.
    ("bezel", rounded(SCREEN, 40)),
    ("screen", rounded(inset(SCREEN, 10), 32)),
    # The ">_" prompt.
    ("prompt", rounded([(526, 608), (576, 596), (694, 704), (582, 838), (530, 850), (642, 716)], 10)),
    ("prompt", rounded([(698, 772), (822, 740), (824, 786), (700, 820)], 6)),
]


def to_grid(points):
    return [(x * 64.0 / SOURCE, y * 64.0 / SOURCE) for x, y in points]


def coord(v):
    n = round((v + 128.0) * 102.0)
    return bytes([0x80 | (n >> 8), n & 0xff])


def hvif():
    names = list(colors)
    out = bytearray(b"ncif")
    out.append(len(names))
    for name in names:
        out += bytes([0x01, *colors[name]])          # solid color style
    out.append(len(shapes))
    for _, points in shapes:
        points = to_grid(points)
        assert len(points) < 256
        out += bytes([0x0a, len(points)])           # closed path, no curves
        for x, y in points:
            out += coord(x) + coord(y)
    out.append(len(shapes))
    for i, (style, _) in enumerate(shapes):
        out += bytes([0x0a, names.index(style), 1, i, 0x00])
    return bytes(out)


def preview(path, size=256, samples=4):
    """Rasterizes the shapes (even-odd, supersampled) to a PPM on gray."""
    scale = size * samples / SOURCE
    big = size * samples
    pixels = [[(0x6f, 0x8f, 0xb8)] * size for _ in range(size)]
    acc = [[[0.0, 0.0, 0.0] for _ in range(size)] for _ in range(size)]
    base = (0x6f, 0x8f, 0xb8)
    for y in range(size):
        for x in range(size):
            acc[y][x] = [base[0] * samples * samples, base[1] * samples * samples, base[2] * samples * samples]
    # Paint shape by shape into a supersampled color buffer, row by row.
    buffer = [[base] * big for _ in range(big)]
    for style, points in shapes:
        r, g, b, a = colors[style]
        alpha = a / 255.0
        pts = [(x * scale, y * scale) for x, y in points]
        for sy in range(big):
            fy = sy + 0.5
            xs = []
            for i in range(len(pts)):
                x1, y1 = pts[i - 1]
                x2, y2 = pts[i]
                if (y1 <= fy < y2) or (y2 <= fy < y1):
                    xs.append(x1 + (fy - y1) * (x2 - x1) / (y2 - y1))
            xs.sort()
            row = buffer[sy]
            for j in range(0, len(xs) - 1, 2):
                for sx in range(max(0, int(xs[j] + 0.5)), min(big, int(xs[j + 1] + 0.5))):
                    pr, pg, pb = row[sx]
                    row[sx] = (pr + (r - pr) * alpha, pg + (g - pg) * alpha, pb + (b - pb) * alpha)
    with open(path, "wb") as f:
        f.write(b"P6 %d %d 255\n" % (size, size))
        for y in range(size):
            for x in range(size):
                tr = tg = tb = 0.0
                for dy in range(samples):
                    for dx in range(samples):
                        pr, pg, pb = buffer[y * samples + dy][x * samples + dx]
                        tr += pr; tg += pg; tb += pb
                n = samples * samples
                f.write(bytes([int(tr / n), int(tg / n), int(tb / n)]))


data = hvif()
here = os.path.dirname(os.path.abspath(__file__))
hexdata = data.hex()
lines = "\n".join(f'\t$"{hexdata[i:i + 64]}"' for i in range(0, len(hexdata), 64))
rdef = f'''// Generated by haiku/mkicon.py; edit that, not this.

resource app_signature "application/x-vnd.bluevisor-Temino";

resource app_flags B_MULTIPLE_LAUNCH;

resource app_version {{
	major  = 0,
	middle = 1,
	minor  = 0,
	variety = B_APPV_DEVELOPMENT,
	internal = 0,
	short_info = "Temino",
	long_info = "A modern GPU-accelerated terminal for Haiku"
}};

resource vector_icon array {{
{lines}
}};
'''
open(os.path.join(here, "Temino.rdef"), "w").write(rdef)
open(os.path.join(here, "temino.hvif"), "wb").write(data)
if "--preview" in sys.argv:
    preview(sys.argv[sys.argv.index("--preview") + 1])
print(len(data), "bytes")

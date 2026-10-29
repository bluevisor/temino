"""Converts Temino's icon (docs/icon.svg) to a Haiku vector icon (HVIF).

usage: uv run --with skia-python --with svgelements python -I haiku/mkicon.py
           [--preview out.png] [--png docs/icon.png]
       (rewrites haiku/Temino.rdef and haiku/temino.hvif; --preview renders
       the HVIF shapes back on gray, --png renders the SVG itself as the
       transparent 1024 px icon the macOS bundle is built from)

The icon (John's design, 2026-10-29) is a chunky silver terminal screen seen
from the right, a blue side and a yellow tab, with a cream ">_" prompt. HVIF
has fills and linear/radial gradients but no filters, masks or clipping, so:
blurred and masked elements (soft glows, invisible at icon sizes) are left
out, clip paths are applied by intersecting geometry, and strokes become
filled outlines. Every shape ends up as cubic curves on the 64x64 HVIF grid.
"""
import math
import os
import re
import struct
import sys
import xml.etree.ElementTree as ET

import skia
import svgelements

SVG = "{http://www.w3.org/2000/svg}"
SCALE = 64.0 / 1254.0
INHERITED = ("fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin",
             "stroke-miterlimit", "fill-opacity", "stroke-opacity", "fill-rule")

here = os.path.dirname(os.path.abspath(__file__))
root = ET.parse(os.path.join(here, "..", "docs", "icon.svg")).getroot()
ids = {el.get("id"): el for el in root.iter() if el.get("id")}


def parse_color(text):
    text = text.strip()
    if text.startswith("#"):
        h = text[1:]
        if len(h) == 3:
            h = "".join(c * 2 for c in h)
        return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))
    c = svgelements.Color(text)
    return (c.red, c.green, c.blue)


def ref(text):
    m = re.match(r"url\(#(.+)\)", text or "")
    return ids[m.group(1)] if m else None


def outline(el):
    """The element's geometry as a skia path (no transforms in this SVG)."""
    tag = el.tag[len(SVG):]
    if tag == "rect":
        x, y, w, h = (float(el.get(k)) for k in ("x", "y", "width", "height"))
        return skia.Path().addRect(skia.Rect.MakeXYWH(x, y, w, h))
    if tag == "ellipse":
        cx, cy, rx, ry = (float(el.get(k)) for k in ("cx", "cy", "rx", "ry"))
        return skia.Path().addOval(skia.Rect.MakeLTRB(cx - rx, cy - ry, cx + rx, cy + ry))
    path = skia.Path()
    for seg in svgelements.Path(el.get("d")):
        if isinstance(seg, svgelements.Move):
            path.moveTo(seg.end.x, seg.end.y)
        elif isinstance(seg, svgelements.Close):
            path.close()
        elif isinstance(seg, svgelements.Line):
            path.lineTo(seg.end.x, seg.end.y)
        elif isinstance(seg, svgelements.QuadraticBezier):
            path.quadTo(seg.control.x, seg.control.y, seg.end.x, seg.end.y)
        elif isinstance(seg, svgelements.CubicBezier):
            path.cubicTo(seg.control1.x, seg.control1.y, seg.control2.x, seg.control2.y,
                         seg.end.x, seg.end.y)
        elif isinstance(seg, svgelements.Arc):
            for c in seg.as_cubic_curves():
                path.cubicTo(c.control1.x, c.control1.y, c.control2.x, c.control2.y,
                             c.end.x, c.end.y)
    return path


def stroked(path, attrs):
    paint = skia.Paint(Style=skia.Paint.kStroke_Style,
                       StrokeWidth=float(attrs.get("stroke-width", "1")))
    paint.setStrokeCap({"round": skia.Paint.kRound_Cap, "square": skia.Paint.kSquare_Cap}
                       .get(attrs.get("stroke-linecap"), skia.Paint.kButt_Cap))
    paint.setStrokeJoin({"round": skia.Paint.kRound_Join, "bevel": skia.Paint.kBevel_Join}
                        .get(attrs.get("stroke-linejoin"), skia.Paint.kMiter_Join))
    paint.setStrokeMiter(float(attrs.get("stroke-miterlimit", "4")))
    out = skia.Path()
    paint.getFillPath(path, out)
    return out


def paint_style(paint, alpha):
    """An HVIF style for a fill: ("solid", rgba) or ("gradient", ...)."""
    grad = ref(paint)
    if grad is None:
        return ("solid", parse_color(paint) + (round(255 * alpha),))
    get = grad.get
    assert get("gradientUnits") == "userSpaceOnUse" and not get("gradientTransform")
    out = []
    for stop in grad.iter(SVG + "stop"):
        style = dict(kv.split(":") for kv in (stop.get("style") or "").split(";") if ":" in kv)
        off = stop.get("offset", "0")
        off = float(off[:-1]) / 100 if off.endswith("%") else float(off)
        color = parse_color(style.get("stop-color", stop.get("stop-color", "#000")))
        a = float(style.get("stop-opacity", stop.get("stop-opacity", "1")))
        out.append((round(255 * min(max(off, 0), 1)), color + (round(255 * a * alpha),)))
    if grad.tag == SVG + "linearGradient":
        x1, y1, x2, y2 = (float(get(k)) * SCALE for k in ("x1", "y1", "x2", "y2"))
        # HVIF's linear gradient runs along x from -64 to 64.
        dx, dy = (x2 - x1) / 128, (y2 - y1) / 128
        matrix = (dx, dy, -dy, dx, (x1 + x2) / 2, (y1 + y2) / 2)
        return ("gradient", 0, matrix, tuple(out))
    cx, cy, r = (float(get(k)) * SCALE for k in ("cx", "cy", "r"))
    # HVIF's circular gradient has radius 64 around the origin.
    return ("gradient", 1, (r / 64, 0, 0, r / 64, cx, cy), tuple(out))


def walk(el, attrs, alpha, clip, out):
    attrs = dict(attrs)
    for k in INHERITED:
        if el.get(k) is not None:
            attrs[k] = el.get(k)
    alpha *= float(el.get("opacity", "1"))
    if el.get("filter") or el.get("mask"):
        return
    if el.get("clip-path"):
        c = skia.Path()
        for child in ref(el.get("clip-path")):
            c = skia.Op(c, outline(child), skia.PathOp.kUnion_PathOp)
        clip = c if clip is None else skia.Op(clip, c, skia.PathOp.kIntersect_PathOp)
    tag = el.tag[len(SVG):]
    if tag in ("g", "svg"):
        for child in el:
            walk(child, attrs, alpha, clip, out)
        return
    if tag not in ("path", "rect", "ellipse"):
        return
    path = outline(el)
    if attrs.get("fill-rule") == "evenodd":
        path.setFillType(skia.PathFillType.kEvenOdd)
    layers = []
    if attrs.get("fill", "#000") != "none":
        layers.append((path, attrs.get("fill", "#000"), float(attrs.get("fill-opacity", "1"))))
    if attrs.get("stroke", "none") != "none":
        layers.append((stroked(path, attrs), attrs["stroke"],
                       float(attrs.get("stroke-opacity", "1"))))
    for geometry, paint, opacity in layers:
        if clip is not None:
            geometry = skia.Op(geometry, clip, skia.PathOp.kIntersect_PathOp)
        # Non-overlapping contours fill the same under even-odd and non-zero.
        geometry = skia.Simplify(geometry)
        if geometry.isEmpty():
            continue
        out.append((paint_style(paint, alpha * opacity), geometry))


def contours(path):
    """Splits a path into closed contours of HVIF vertices (point, in, out)."""
    result, verts = [], []
    path = skia.Path(path)
    path.transform(skia.Matrix.Scale(SCALE, SCALE))
    it = skia.Path.Iter(path, False)

    def cubic(c1, c2, end):
        verts[-1][2] = c1
        verts.append([end, c2, end])

    def quad(a, b, c):
        cubic((a[0] + 2 / 3 * (b[0] - a[0]), a[1] + 2 / 3 * (b[1] - a[1])),
              (c[0] + 2 / 3 * (b[0] - c[0]), c[1] + 2 / 3 * (b[1] - c[1])), c)

    while True:
        verb, pts = it.next()
        p = [(q.x(), q.y()) for q in pts]
        if verb == skia.Path.Verb.kDone_Verb:
            break
        if verb == skia.Path.Verb.kMove_Verb:
            verts = [[p[0], p[0], p[0]]]
            result.append(verts)
        elif verb == skia.Path.Verb.kLine_Verb:
            verts.append([p[1], p[1], p[1]])
        elif verb == skia.Path.Verb.kQuad_Verb:
            quad(*p)
        elif verb == skia.Path.Verb.kConic_Verb:
            q = [(v.x(), v.y()) for v in
                 skia.Path.ConvertConicToQuads(*pts, it.conicWeight(), 2)]
            for i in range(0, len(q) - 2, 2):
                quad(q[i], q[i + 1], q[i + 2])
        elif verb == skia.Path.Verb.kCubic_Verb:
            cubic(p[1], p[2], p[3])
    out = []
    for verts in result:
        # The closing vertex repeats the first: fold its "in" into the first.
        if len(verts) > 1 and math.dist(verts[-1][0], verts[0][0]) < 1e-6:
            verts[0][1] = verts.pop()[1]
        if len(verts) >= 2:
            out.append(verts)
    return out


def coord(v):
    n = round((v + 128.0) * 102.0)
    assert 0 <= n < 0x8000, v
    return bytes([0x80 | (n >> 8), n & 0xff])


def float24(v):
    if v == 0:
        return bytes(3)
    bits = struct.unpack(">I", struct.pack(">f", v))[0]
    exponent = ((bits >> 23) & 0xff) - 127 + 32
    assert 0 < exponent < 64, v
    value = (bits >> 31) << 23 | exponent << 17 | (bits & 0x7fffff) >> 6
    return value.to_bytes(3, "big")


def style_bytes(style):
    if style[0] == "solid":
        return bytes([0x01, *style[1]])                 # solid color
    _, kind, matrix, stops = style
    out = bytearray([0x02, kind, 0x02, len(stops)])     # gradient, has transform
    for v in matrix:
        out += float24(v)
    for offset, rgba in stops:
        out += bytes([offset, *rgba])
    return bytes(out)


def hvif(items):
    styles, paths, shapes = [], [], []
    for style, geometry in items:
        if style not in styles:
            styles.append(style)
        first = len(paths)
        for verts in contours(geometry):
            assert len(verts) < 256
            paths.append(verts)
        shapes.append((styles.index(style), list(range(first, len(paths)))))
    assert len(styles) < 256 and len(paths) < 256 and len(shapes) < 256
    out = bytearray(b"ncif")
    out.append(len(styles))
    for style in styles:
        out += style_bytes(style)
    out.append(len(paths))
    for verts in paths:
        out += bytes([0x02, len(verts)])                # closed, with curves
        for point in verts:
            for x, y in point:
                out += coord(x) + coord(y)
    out.append(len(shapes))
    for style, members in shapes:
        out += bytes([0x0a, style, len(members), *members, 0x00])
    print(f"{len(styles)} styles, {len(paths)} paths, {len(shapes)} shapes")
    return bytes(out)


def preview(items, path, size=512):
    """Draws the converted shapes the way HVIF will, as a check."""
    surface = skia.Surface(size, size)
    canvas = surface.getCanvas()
    canvas.clear(skia.Color(0x6f, 0x8f, 0xb8))
    s = size / 64.0
    for style, geometry in items:
        paint = skia.Paint(AntiAlias=True)
        if style[0] == "solid":
            r, g, b, a = style[1]
            paint.setColor(skia.Color(r, g, b, a))
        else:
            _, kind, (a, b, c, d, tx, ty), stops = style
            colors = [skia.Color(r, g, bl, al) for _, (r, g, bl, al) in stops]
            positions = [o / 255 for o, _ in stops]
            m = skia.Matrix.MakeAll(a, c, tx, b, d, ty, 0, 0, 1)
            if kind == 0:
                shader = skia.GradientShader.MakeLinear([(-64, 0), (64, 0)], colors, positions,
                                                        skia.TileMode.kClamp, 0, m)
            else:
                shader = skia.GradientShader.MakeRadial((0, 0), 64, colors, positions,
                                                        skia.TileMode.kClamp, 0, m)
            paint.setShader(shader)
        p = skia.Path()
        for verts in contours(geometry):
            p.moveTo(*verts[0][0])
            for i in range(1, len(verts) + 1):
                a, b = verts[i - 1], verts[i % len(verts)]
                p.cubicTo(*a[2], *b[1], *b[0])
            p.close()
        canvas.save()
        canvas.scale(s, s)
        canvas.drawPath(p, paint)
        canvas.restore()
    surface.makeImageSnapshot().save(path, skia.kPNG)


def png(path, size=1024):
    with open(os.path.join(here, "..", "docs", "icon.svg"), "rb") as f:
        dom = skia.SVGDOM.MakeFromStream(skia.MemoryStream(f.read()))
    surface = skia.Surface(size, size)
    canvas = surface.getCanvas()
    canvas.clear(skia.ColorTRANSPARENT)
    canvas.scale(size / 1254.0, size / 1254.0)
    dom.render(canvas)
    surface.makeImageSnapshot().save(path, skia.kPNG)


items = []
walk(root, {}, 1.0, None, items)
data = hvif(items)
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
	long_info = "An ADHD-friendly, GPU-accelerated modern terminal, designed for the AI age"
}};

resource vector_icon array {{
{lines}
}};
'''
open(os.path.join(here, "Temino.rdef"), "w").write(rdef)
open(os.path.join(here, "temino.hvif"), "wb").write(data)
if "--preview" in sys.argv:
    preview(items, sys.argv[sys.argv.index("--preview") + 1])
if "--png" in sys.argv:
    png(sys.argv[sys.argv.index("--png") + 1])
print(len(data), "bytes")

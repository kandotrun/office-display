"""The Earendil emblem as polygons for /emblem.json.

The emblem is not stored in the repository: the SVG is fetched at runtime from
EARENDIL_EMBLEM_URL (or read from a local path), its bytes cached on disk, and
every drawn <path> flattened into rings the frame fills with the even-odd rule:

    {"aspect": width / height, "rings": [[[x, y], ...], ...]}

x and y are fractions of the view box (y down). Path data supports every
command (M L H V C S Q T A Z, absolute and relative). Quadratic curves are
raised to cubics exactly; elliptical arcs are converted to centre form (SVG
1.1 F.6.5, radii scaled up when too small) and approximated by one cubic per
quarter turn at most (k = 4/3 tan(delta/4), radial error below 0.03 %).
Cubics are then flattened in view-box units with Wang's bound, so the
polyline stays within MAX(width, height) / 2000 of the curve. translate,
scale, matrix, rotate, skewX and skewY transforms of the path and its
enclosing groups apply. Paths inside defs/clipPath/mask/... or with fill or
display none are skipped; CSS classes are not evaluated. Documents declaring
entities are refused before parsing (no entity expansion, nothing external).

Limits: 1..64 rings (the largest are kept) and 3..4096 points in total (rings
are simplified with Visvalingam-Whyatt until they fit, O(n log n)). The work
is bounded as well as the output: a document may hold at most 20000 path
commands and flatten to at most 100000 points before simplifying; more is
refused (ValueError) as soon as the budget runs out. A download is flattened
once, both to validate it and to serve it.
"""
import heapq
import math
import os
import re
import xml.etree.ElementTree as ET

MAX_SVG_BYTES = 1 << 20
MAX_RINGS = 64
MAX_POINTS = 4096
MAX_PATH_COMMANDS = 20000  # per document
MAX_RAW_POINTS = 100000  # per document, before simplifying
MIN_ASPECT, MAX_ASPECT = 0.1, 10.0
TOLERANCE = 1 / 2000  # of the larger view-box side
MAX_SEGMENTS = 128  # per cubic
CACHE_KEY = "emblem.svg"
MAX_AGE = 86400

NOT_DRAWN = {"defs", "clipPath", "mask", "symbol", "pattern", "marker", "title", "desc", "metadata", "style", "script"}
IDENTITY = (1.0, 0.0, 0.0, 1.0, 0.0, 0.0)

_NUMBER = re.compile(r"[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?")
_SEPARATORS = " \t\r\n,"
_ARGS = {"M": 2, "L": 2, "H": 1, "V": 1, "C": 6, "S": 4, "Q": 4, "T": 2, "A": 7, "Z": 0}
_TRANSFORM = re.compile(r"\s*(matrix|translate|scale|rotate|skewX|skewY)\s*\(([^)]*)\)\s*,?")


# -- transforms ----------------------------------------------------------------


def compose(m, n):
    """m after n: (m . n)(p) = m(n(p)); (a, b, c, d, e, f) as in SVG."""
    a, b, c, d, e, f = m
    a2, b2, c2, d2, e2, f2 = n
    return (
        a * a2 + c * b2,
        b * a2 + d * b2,
        a * c2 + c * d2,
        b * c2 + d * d2,
        a * e2 + c * f2 + e,
        b * e2 + d * f2 + f,
    )


def apply(m, point):
    a, b, c, d, e, f = m
    x, y = point
    return (a * x + c * y + e, b * x + d * y + f)


def parse_transform(text):
    m = IDENTITY
    text = (text or "").strip()
    pos = 0
    while pos < len(text):
        hit = _TRANSFORM.match(text, pos)
        if hit is None:
            raise ValueError("emblem: unsupported transform %r" % text[pos : pos + 32])
        pos = hit.end()
        name = hit.group(1)
        args = [float(v) for v in _NUMBER.findall(hit.group(2))]
        if name == "matrix" and len(args) == 6:
            step = tuple(args)
        elif name == "translate" and len(args) in (1, 2):
            step = (1.0, 0.0, 0.0, 1.0, args[0], args[1] if len(args) == 2 else 0.0)
        elif name == "scale" and len(args) in (1, 2):
            step = (args[0], 0.0, 0.0, args[-1], 0.0, 0.0)
        elif name == "rotate" and len(args) in (1, 3):
            r = math.radians(args[0])
            step = (math.cos(r), math.sin(r), -math.sin(r), math.cos(r), 0.0, 0.0)
            if len(args) == 3:
                cx, cy = args[1], args[2]
                step = compose((1.0, 0.0, 0.0, 1.0, cx, cy), compose(step, (1.0, 0.0, 0.0, 1.0, -cx, -cy)))
        elif name == "skewX" and len(args) == 1:
            step = (1.0, 0.0, math.tan(math.radians(args[0])), 1.0, 0.0, 0.0)
        elif name == "skewY" and len(args) == 1:
            step = (1.0, math.tan(math.radians(args[0])), 0.0, 1.0, 0.0, 0.0)
        else:
            raise ValueError("emblem: bad %s() arguments" % name)
        m = compose(m, step)  # a list applies right to left
    return m


# -- path data -------------------------------------------------------------------


class Budget:
    """The work one document may cause: path commands, then points before simplifying."""

    def __init__(self, commands=MAX_PATH_COMMANDS, points=MAX_RAW_POINTS):
        self.commands = commands
        self.points = points

    def command(self):
        self.commands -= 1
        if self.commands < 0:
            raise ValueError("emblem: more than %d path commands" % MAX_PATH_COMMANDS)

    def add_points(self, n):
        self.points -= n
        if self.points < 0:
            raise ValueError("emblem: more than %d points before simplifying" % MAX_RAW_POINTS)


class _Scanner:
    def __init__(self, d):
        self.d = d
        self.pos = 0

    def skip(self):
        while self.pos < len(self.d) and self.d[self.pos] in _SEPARATORS:
            self.pos += 1

    def done(self):
        self.skip()
        return self.pos >= len(self.d)

    def peek(self):
        self.skip()
        return self.d[self.pos] if self.pos < len(self.d) else ""

    def number(self):
        self.skip()
        hit = _NUMBER.match(self.d, self.pos)
        if hit is None:
            raise ValueError("emblem: number expected in path data at %d" % self.pos)
        self.pos = hit.end()
        return float(hit.group())

    def flag(self):
        self.skip()
        if self.pos >= len(self.d) or self.d[self.pos] not in "01":
            raise ValueError("emblem: arc flag expected at %d" % self.pos)
        self.pos += 1
        return self.d[self.pos - 1] == "1"


def _quad_to_cubic(p0, q, p):
    return (
        (p0[0] + 2 / 3 * (q[0] - p0[0]), p0[1] + 2 / 3 * (q[1] - p0[1])),
        (p[0] + 2 / 3 * (q[0] - p[0]), p[1] + 2 / 3 * (q[1] - p[1])),
    )


def _angle(ux, uy, vx, vy):
    return math.atan2(ux * vy - uy * vx, ux * vx + uy * vy)


def arc_to_cubics(p1, rx, ry, phi_deg, large, sweep, p2):
    """An SVG arc as cubic segments [(c1, c2, end), ...]; [(end,)] (a line) for zero radii."""
    (x1, y1), (x2, y2) = p1, p2
    if p1 == p2:
        return []
    rx, ry = abs(rx), abs(ry)
    if rx == 0 or ry == 0:
        return [(p2,)]
    phi = math.radians(phi_deg % 360)
    cos, sin = math.cos(phi), math.sin(phi)
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    x1p, y1p = cos * dx + sin * dy, -sin * dx + cos * dy
    lam = (x1p / rx) ** 2 + (y1p / ry) ** 2
    if lam > 1:
        rx, ry = rx * math.sqrt(lam), ry * math.sqrt(lam)
    num = (rx * ry) ** 2 - (rx * y1p) ** 2 - (ry * x1p) ** 2
    den = (rx * y1p) ** 2 + (ry * x1p) ** 2
    coef = math.sqrt(max(0.0, num / den)) if den else 0.0
    if large == sweep:
        coef = -coef
    cxp, cyp = coef * rx * y1p / ry, -coef * ry * x1p / rx
    cx = cos * cxp - sin * cyp + (x1 + x2) / 2
    cy = sin * cxp + cos * cyp + (y1 + y2) / 2
    ux, uy = (x1p - cxp) / rx, (y1p - cyp) / ry
    vx, vy = (-x1p - cxp) / rx, (-y1p - cyp) / ry
    theta = _angle(1.0, 0.0, ux, uy)
    delta = _angle(ux, uy, vx, vy)
    if not sweep and delta > 0:
        delta -= 2 * math.pi
    elif sweep and delta < 0:
        delta += 2 * math.pi
    n = max(1, math.ceil(abs(delta) / (math.pi / 2) - 1e-9))
    step = delta / n
    k = 4 / 3 * math.tan(step / 4)

    def at(t):
        return (cx + rx * math.cos(t) * cos - ry * math.sin(t) * sin, cy + rx * math.cos(t) * sin + ry * math.sin(t) * cos)

    def tangent(t):
        return (-rx * math.sin(t) * cos - ry * math.cos(t) * sin, -rx * math.sin(t) * sin + ry * math.cos(t) * cos)

    out = []
    for i in range(n):
        t1, t2 = theta + i * step, theta + (i + 1) * step
        a, b = at(t1), (p2 if i == n - 1 else at(t2))
        da, db = tangent(t1), tangent(t2)
        out.append(((a[0] + k * da[0], a[1] + k * da[1]), (b[0] - k * db[0], b[1] - k * db[1]), b))
    return out


def parse_path(d, budget=None):
    """Subpaths of path data: [(start, [(c1, c2, end) or (end,), ...]), ...] in user units."""
    budget = budget or Budget()
    scan = _Scanner(d or "")
    subpaths = []
    segments = None
    cur = start = (0.0, 0.0)
    cmd = None
    last_cubic = last_quad = None  # reflected control points for S and T
    while not scan.done():
        budget.command()
        ch = scan.peek()
        if ch.isalpha():
            if ch.upper() not in _ARGS:
                raise ValueError("emblem: unknown path command %r" % ch)
            cmd = ch
            scan.pos += 1
        elif cmd is None or cmd in "Zz":
            raise ValueError("emblem: path data must start with a moveto")
        if cmd is None or (segments is None and cmd not in "Mm"):
            raise ValueError("emblem: path data must start with a moveto")
        upper, rel = cmd.upper(), cmd.islower()
        ox, oy = cur if rel else (0.0, 0.0)
        cubic = quad = None
        if upper == "Z":
            if segments is not None:
                cur = start
            # the next command starts a new subpath at the same start
            segments = []
            subpaths.append((start, segments))
            last_cubic = last_quad = None
            continue
        if upper == "M":
            cur = (ox + scan.number(), oy + scan.number())
            start = cur
            segments = []
            subpaths.append((start, segments))
            cmd = "l" if rel else "L"  # further pairs are linetos
        elif upper == "L":
            cur = (ox + scan.number(), oy + scan.number())
            segments.append((cur,))
        elif upper == "H":
            cur = (ox + scan.number(), cur[1])
            segments.append((cur,))
        elif upper == "V":
            cur = (cur[0], oy + scan.number())
            segments.append((cur,))
        elif upper == "C":
            c1 = (ox + scan.number(), oy + scan.number())
            c2 = (ox + scan.number(), oy + scan.number())
            end = (ox + scan.number(), oy + scan.number())
            segments.append((c1, c2, end))
            cur, cubic = end, c2
        elif upper == "S":
            c1 = (2 * cur[0] - last_cubic[0], 2 * cur[1] - last_cubic[1]) if last_cubic else cur
            c2 = (ox + scan.number(), oy + scan.number())
            end = (ox + scan.number(), oy + scan.number())
            segments.append((c1, c2, end))
            cur, cubic = end, c2
        elif upper in "QT":
            if upper == "Q":
                q = (ox + scan.number(), oy + scan.number())
            else:
                q = (2 * cur[0] - last_quad[0], 2 * cur[1] - last_quad[1]) if last_quad else cur
            end = (ox + scan.number(), oy + scan.number())
            segments.append(_quad_to_cubic(cur, q, end) + (end,))
            cur, quad = end, q
        elif upper == "A":
            rx, ry, phi = scan.number(), scan.number(), scan.number()
            large, sweep = scan.flag(), scan.flag()
            end = (ox + scan.number(), oy + scan.number())
            segments.extend(arc_to_cubics(cur, rx, ry, phi, large, sweep, end))
            cur = end
        last_cubic, last_quad = cubic, quad
    return [(s, segs) for s, segs in subpaths if segs]


# -- flattening --------------------------------------------------------------------


def _cubic_points(p0, c1, c2, p3, tol):
    dd = max(
        math.hypot(p0[0] - 2 * c1[0] + c2[0], p0[1] - 2 * c1[1] + c2[1]),
        math.hypot(c1[0] - 2 * c2[0] + p3[0], c1[1] - 2 * c2[1] + p3[1]),
    )
    n = min(MAX_SEGMENTS, max(1, math.ceil(math.sqrt(0.75 * dd / tol))))
    out = []
    for i in range(1, n + 1):
        t = i / n
        u = 1 - t
        a, b, c, d = u * u * u, 3 * u * u * t, 3 * u * t * t, t * t * t
        out.append((a * p0[0] + b * c1[0] + c * c2[0] + d * p3[0], a * p0[1] + b * c1[1] + c * c2[1] + d * p3[1]))
    return out


def _polyline(start, segments, m, tol, budget):
    cur = apply(m, start)
    points = [cur]
    for seg in segments:
        if len(seg) == 1:
            cur = apply(m, seg[0])
            added = [cur]
        else:
            c1, c2, end = (apply(m, p) for p in seg)
            added = _cubic_points(cur, c1, c2, end, tol)
            cur = end
        budget.add_points(len(added))
        points.extend(added)
    return points


def ring_area(ring):
    return 0.5 * sum(x0 * y1 - x1 * y0 for (x0, y0), (x1, y1) in zip(ring, ring[1:] + ring[:1]))


def is_flat(ring, eps=1e-6):
    """No area to fill: fewer than 3 points, or all on one line.

    (Not "signed area == 0": a figure-eight has none and still shows two lobes.)
    """
    if len(ring) < 3:
        return True
    ax, ay = ring[0]
    bx, by = max(ring, key=lambda p: math.hypot(p[0] - ax, p[1] - ay))
    length = math.hypot(bx - ax, by - ay)
    if length == 0:
        return True
    return all(abs((bx - ax) * (y - ay) - (by - ay) * (x - ax)) / length < eps for x, y in ring)


def _normalise(points, box):
    minx, miny, w, h = box
    ring = []
    for x, y in points:
        p = [round(min(1.0, max(0.0, (x - minx) / w)), 4) + 0.0, round(min(1.0, max(0.0, (y - miny) / h)), 4) + 0.0]
        if not ring or ring[-1] != p:
            ring.append(p)
    while len(ring) > 1 and ring[-1] == ring[0]:
        ring.pop()
    return ring


def _triangle(a, b, c):
    """Twice the area of the triangle abc: what dropping b from a-b-c changes."""
    return abs((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]))


def simplify(rings, budget=MAX_POINTS):
    """Visvalingam-Whyatt over all rings at once, until at most budget points are left.

    The point making the smallest triangle with its neighbours goes first,
    whichever ring it is in; a ring keeps at least 3 points. One heap and
    lazy updates make it O(n log n), whatever the shape.
    """
    total = sum(len(r) for r in rings)
    if total <= budget:
        return rings
    prev = [[(i - 1) % len(r) for i in range(len(r))] for r in rings]
    nxt = [[(i + 1) % len(r) for i in range(len(r))] for r in rings]
    stamp = [[0] * len(r) for r in rings]  # entries with an older stamp are stale
    gone = [[False] * len(r) for r in rings]
    left = [len(r) for r in rings]
    heap = [(_triangle(r[prev[k][i]], r[i], r[nxt[k][i]]), k, i, 0) for k, r in enumerate(rings) for i in range(len(r))]
    heapq.heapify(heap)
    while total > budget and heap:
        area, k, i, s = heapq.heappop(heap)
        if gone[k][i] or s != stamp[k][i] or left[k] <= 3:
            continue
        r, p, n = rings[k], prev[k][i], nxt[k][i]
        nxt[k][p], prev[k][n] = n, p
        gone[k][i] = True
        left[k] -= 1
        total -= 1
        for j in (p, n):
            stamp[k][j] += 1
            # never below the area just removed: what follows is at least as significant
            heapq.heappush(heap, (max(area, _triangle(r[prev[k][j]], r[j], r[nxt[k][j]])), k, j, stamp[k][j]))
    return [[p for p, dropped in zip(r, gone[k]) if not dropped] for k, r in enumerate(rings)]


def _fit(rings):
    rings = sorted(rings, key=lambda r: abs(ring_area(r)), reverse=True)[:MAX_RINGS]
    return [r for r in simplify(rings) if not is_flat(r)]


# -- the document -------------------------------------------------------------------


def _local(tag):
    return tag.rsplit("}", 1)[-1] if isinstance(tag, str) else ""


def _style(el, key):
    for part in (el.get("style") or "").split(";"):
        name, _, value = part.partition(":")
        if name.strip() == key:
            return value.strip()
    return el.get(key)


def _length(value):
    hit = _NUMBER.match((value or "").strip())
    return float(hit.group()) if hit else None


def view_box(root):
    box = root.get("viewBox")
    if box:
        parts = [float(v) for v in _NUMBER.findall(box)]
        if len(parts) != 4:
            raise ValueError("emblem: bad viewBox")
        return tuple(parts)
    w, h = _length(root.get("width")), _length(root.get("height"))
    if w is None or h is None:
        raise ValueError("emblem: no viewBox and no width/height")
    return (0.0, 0.0, w, h)


def _paths(el, m, fill):
    """(path d, transform) of every drawn path below el."""
    for child in el:
        name = _local(child.tag)
        if name in NOT_DRAWN or _style(child, "display") == "none":
            continue
        child_m = compose(m, parse_transform(child.get("transform")))
        child_fill = _style(child, "fill") or fill
        if name == "path":
            if child_fill != "none" and child.get("d"):
                yield child.get("d"), child_m
        else:
            yield from _paths(child, child_m, child_fill)


def flatten(data):
    """The /emblem.json document for SVG bytes (ValueError when unusable)."""
    if len(data) > MAX_SVG_BYTES:
        raise ValueError("emblem: SVG larger than %d bytes" % MAX_SVG_BYTES)
    if b"<!ENTITY" in data or b"<!entity" in data:
        raise ValueError("emblem: entity declarations are not accepted")
    try:
        root = ET.fromstring(data)
    except ET.ParseError as exc:
        raise ValueError("emblem: not XML (%s)" % exc) from None
    if _local(root.tag) != "svg":
        raise ValueError("emblem: not an SVG document")
    box = view_box(root)
    if not (box[2] > 0 and box[3] > 0) or not MIN_ASPECT <= box[2] / box[3] <= MAX_ASPECT:
        raise ValueError("emblem: view box %r out of range" % (box,))
    tol = max(box[2], box[3]) * TOLERANCE
    root_m = parse_transform(root.get("transform"))
    budget = Budget()
    rings = []
    for d, m in _paths(root, root_m, _style(root, "fill")):
        for start, segments in parse_path(d, budget):
            ring = _normalise(_polyline(start, segments, m, tol, budget), box)
            if not is_flat(ring):
                rings.append(ring)
    rings = _fit(rings)
    if not rings:
        raise ValueError("emblem: nothing to draw")
    return {"aspect": round(box[2] / box[3], 4), "rings": rings}


class EmblemSource:
    """The emblem from a URL (SVG bytes cached on disk, daily) or a local file."""

    def __init__(self, location, disk, fetch):
        self.location = location
        self.cache = disk
        self.fetch = fetch
        self.last = None  # (SVG bytes, their document)

    def remote(self):
        return self.location.startswith(("http://", "https://"))

    def _download(self):
        return self.fetch(self.location, max_bytes=MAX_SVG_BYTES)

    def _flatten(self, data):
        """flatten() once per distinct SVG: the validated download is the one served."""
        if self.last is None or self.last[0] != data:
            self.last = (data, flatten(data))
        return self.last[1]

    def refresh(self):
        if not self.remote():
            path = os.path.expanduser(self.location)
            if os.path.getsize(path) > MAX_SVG_BYTES:
                raise ValueError("emblem: %s is larger than %d bytes" % (path, MAX_SVG_BYTES))
            with open(path, "rb") as f:
                return self._flatten(f.read())
        return self._flatten(self.cache.fetch(CACHE_KEY, MAX_AGE, self._download, validate=self._flatten))

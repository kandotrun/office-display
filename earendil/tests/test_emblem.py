import math
import os
import time

import pytest

import cache
import emblem
import net
from fakes import EARENDIL_SVG

def svg(body, box="0 0 40 40", extra=""):
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="%s"%s>%s</svg>' % (box, extra, body)).encode()


def path(d, **attrs):
    more = "".join(' %s="%s"' % (k.replace("_", "-"), v) for k, v in attrs.items())
    return '<path d="%s"%s/>' % (d, more)


def area(ring):
    return 0.5 * sum(x0 * y1 - x1 * y0 for (x0, y0), (x1, y1) in zip(ring, ring[1:] + ring[:1]))


def total_points(doc):
    return sum(len(r) for r in doc["rings"])


def test_the_earendil_emblem():
    doc = emblem.flatten(EARENDIL_SVG)
    assert doc["aspect"] == 0.549
    assert len(doc["rings"]) == 1
    ring = doc["rings"][0]
    assert ring[0] == [0.9868, 0.6725]  # M202.89,123.89
    assert 3 <= total_points(doc) <= emblem.MAX_POINTS
    assert all(0.0 <= c <= 1.0 for point in ring for c in point)
    xs, ys = [p[0] for p in ring], [p[1] for p in ring]
    # the long upper tip at (162.06, 22.62) and the lower one at (162.09, 172.0)
    top = ring[ys.index(min(ys))]
    bottom = ring[ys.index(max(ys))]
    assert top == pytest.approx([0.5007, 0.0106], abs=0.002)
    assert bottom == pytest.approx([0.5011, 0.9869], abs=0.002)
    # the side points (121.24, 123.89) and (202.88, 123.89), slightly bulged by the curves
    assert min(xs) == pytest.approx(0.0148, abs=0.004)
    assert max(xs) == pytest.approx(0.9868, abs=0.004)
    # the closing h0 adds no duplicate point
    assert all(a != b for a, b in zip(ring, ring[1:] + ring[:1]))
    assert abs(area(ring)) > 0.2


SQUARE = [[0.25, 0.25], [0.75, 0.25], [0.75, 0.75], [0.25, 0.75]]


@pytest.mark.parametrize(
    "d",
    [
        "M10 10 L30 10 L30 30 L10 30 Z",
        "m10 10 l20 0 l0 20 l-20 0 z",
        "M10 10 H30 V30 H10 Z",
        "m10,10h20v20h-20z",
        "M10 10 30 10 30 30 10 30z",  # implicit lineto after moveto
        "m10 10 20 0 0 20-20 0z",
        "M10 10 L30 10 L30 30 L10 30",  # not closed: filled as if it were
        "M10 10 L30 10 L30 30 L10 30 L10 10 Z",  # explicitly back at the start
    ],
)
def test_lines_absolute_relative_and_implicit(d):
    assert emblem.flatten(svg(path(d)))["rings"] == [SQUARE]


def test_every_subpath_is_a_ring_for_even_odd_filling():
    doc = emblem.flatten(svg(path("M0 0 H40 V40 H0 Z M10 10 H30 V30 H10 Z")))
    assert len(doc["rings"]) == 2
    assert doc["rings"][1] == SQUARE


def test_relative_moveto_after_close_starts_from_the_subpath_start():
    doc = emblem.flatten(svg(path("m0 0 h10 v10 h-10 z m20 0 h10 v10 h-10 z")))
    assert doc["rings"][1][0] == [0.5, 0.0]


def test_relative_command_after_close_starts_from_the_subpath_start():
    doc = emblem.flatten(svg(path("M10 10 h10 v10 z l10 0 v10 h-10 z")))
    assert doc["rings"][1][:2] == [[0.25, 0.25], [0.5, 0.25]]


def test_several_paths_are_all_used():
    doc = emblem.flatten(svg(path("M0 0 H10 V10 H0 Z") + path("M30 30 H40 V40 H30 Z")))
    assert len(doc["rings"]) == 2


def test_quadratic_and_smooth_quadratic():
    doc = emblem.flatten(svg(path("M0 0 Q 20 40 40 0 Z")))
    assert max(p[1] for p in doc["rings"][0]) == pytest.approx(0.5, abs=0.002)
    # T reflects (10,0) about (20,20) to (30,40): the second arch dips to y = 30
    doc = emblem.flatten(svg(path("M0 20 Q10 0 20 20 T40 20 Z")))
    ys = [p[1] for p in doc["rings"][0]]
    assert min(ys) == pytest.approx(0.25, abs=0.002) and max(ys) == pytest.approx(0.75, abs=0.002)


def test_smooth_cubic_reflects_the_previous_control_point():
    doc = emblem.flatten(svg(path("M0 20 C0 0 20 0 20 20 S40 40 40 20 Z")))
    ys = [p[1] for p in doc["rings"][0]]
    assert min(ys) == pytest.approx(0.125, abs=0.002) and max(ys) == pytest.approx(0.875, abs=0.002)


def test_smooth_cubic_without_a_previous_cubic_uses_the_current_point():
    a = emblem.flatten(svg(path("M0 20 S40 40 40 20 Z")))
    b = emblem.flatten(svg(path("M0 20 C0 20 40 40 40 20 Z")))
    assert a == b


def test_curves_are_flattened_onto_the_curve():
    doc = emblem.flatten(svg(path("M0 40 C0 0 40 0 40 40 Z")))
    ring = doc["rings"][0]
    assert len(ring) > 8
    for x, y in ring[1:-1]:
        # every point of B(t) satisfies x = 40 (3t^2 - 2t^3) ... check against a dense sampling
        best = min(math.hypot(x - bx, y - by) for bx, by in dense_cubic((0, 40), (0, 0), (40, 0), (40, 40)))
        assert best < 0.002


def dense_cubic(p0, p1, p2, p3, n=2000):
    out = []
    for i in range(n + 1):
        t = i / n
        u = 1 - t
        x = u**3 * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t**3 * p3[0]
        y = u**3 * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t**3 * p3[1]
        out.append((x / 40, y / 40))
    return out


def assert_circle(ring, cx, cy, r, tol=0.003):
    assert len(ring) >= 16
    for x, y in ring:
        assert math.hypot(x - cx, y - cy) == pytest.approx(r, abs=tol)


def test_arcs_draw_a_circle():
    doc = emblem.flatten(svg(path("M10 20 A10 10 0 1 0 30 20 A10 10 0 1 0 10 20 Z")))
    assert_circle(doc["rings"][0], 0.5, 0.5, 0.25)


def test_arc_flags_may_be_written_without_separators():
    doc = emblem.flatten(svg(path("M10 20a10 10 0 1020 0a10 10 0 10-20 0z")))
    assert_circle(doc["rings"][0], 0.5, 0.5, 0.25)


def test_arc_sweep_flag_picks_the_side():
    down = emblem.flatten(svg(path("M0 20 A20 20 0 0 0 40 20 Z")))["rings"][0]
    up = emblem.flatten(svg(path("M0 20 A20 20 0 0 1 40 20 Z")))["rings"][0]
    assert max(p[1] for p in down) == pytest.approx(1.0, abs=0.003)
    assert min(p[1] for p in up) == pytest.approx(0.0, abs=0.003)


def test_too_small_arc_radii_are_scaled_up():
    ring = emblem.flatten(svg(path("M0 20 A1 1 0 0 1 40 20 Z")))["rings"][0]
    assert_circle(ring, 0.5, 0.5, 0.5)


def test_rotated_elliptical_arcs():
    ring = emblem.flatten(svg(path("M20 0 A20 10 90 1 0 20 40 A20 10 90 1 0 20 0 Z")))["rings"][0]
    for x, y in ring:
        # a vertical ellipse: semi-axes 10 (x) and 20 (y) around (20, 20)
        assert ((x * 40 - 20) / 10) ** 2 + ((y * 40 - 20) / 20) ** 2 == pytest.approx(1.0, abs=0.01)


def test_a_zero_radius_arc_is_a_line():
    doc = emblem.flatten(svg(path("M10 10 H30 A0 5 0 0 1 30 30 H10 Z")))
    assert doc["rings"] == [SQUARE]


def test_path_transform():
    doc = emblem.flatten(svg(path("M0 0 H20 V20 H0 Z", transform="translate(10 10)")))
    assert doc["rings"] == [SQUARE]
    doc = emblem.flatten(svg(path("M5 5 H15 V15 H5 Z", transform="scale(2)")))
    assert doc["rings"] == [SQUARE]
    doc = emblem.flatten(svg(path("M0 0 H10 V10 H0 Z", transform="matrix(2,0,0,2,10,10)")))
    assert doc["rings"] == [SQUARE]


def test_parent_group_transforms_apply_after_the_paths_own():
    body = '<g transform="translate(10 10)">%s</g>' % path("M0 0 H10 V10 H0 Z", transform="scale(2)")
    assert emblem.flatten(svg(body))["rings"] == [SQUARE]
    # a transform list applies right to left: scale first, then translate
    doc = emblem.flatten(svg(path("M0 0 H10 V10 H0 Z", transform="translate(10,10) scale(2)")))
    assert doc["rings"] == [SQUARE]
    nested = '<g transform="translate(5 5)"><g transform="translate(5,5)">%s</g></g>' % path("M0 0 H20 V20 H0 Z")
    assert emblem.flatten(svg(nested))["rings"] == [SQUARE]


def test_rotate_and_scale_with_one_argument():
    # rotate(90) maps (x, y) to (-y, x)
    doc = emblem.flatten(svg(path("M10 -30 H30 V-10 H10 Z", transform="rotate(90)")))
    assert sorted(doc["rings"][0]) == sorted([[0.25, 0.25], [0.75, 0.25], [0.75, 0.75], [0.25, 0.75]])


def test_unknown_transforms_are_rejected():
    with pytest.raises(ValueError):
        emblem.flatten(svg(path("M0 0 H10 V10 H0 Z", transform="perspective(3)")))


def test_paths_that_are_not_drawn_are_skipped():
    body = (
        "<defs>%s</defs>" % path("M0 0 H40 V40 H0 Z")
        + "<clipPath>%s</clipPath>" % path("M0 0 H40 V40 H0 Z")
        + path("M0 0 H40 V40 H0 Z", fill="none")
        + path("M0 0 H40 V40 H0 Z", style="fill: none; stroke: #fff")
        + '<g fill="none">%s</g>' % path("M0 0 H40 V40 H0 Z")
        + path("M0 0 H40 V40 H0 Z", display="none")
        + path("M10 10 H30 V30 H10 Z")
    )
    assert emblem.flatten(svg(body))["rings"] == [SQUARE]


def test_width_and_height_when_there_is_no_viewbox():
    doc = emblem.flatten(
        b'<svg xmlns="http://www.w3.org/2000/svg" width="40px" height="80">' + path("M10 20 H30 V60 H10 Z").encode() + b"</svg>"
    )
    assert doc["aspect"] == 0.5
    assert doc["rings"] == [SQUARE]


def test_points_outside_the_view_box_are_clamped():
    doc = emblem.flatten(svg(path("M-10 -10 H50 V50 H-10 Z")))
    assert doc["rings"] == [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]]


def test_at_most_64_rings_keeping_the_largest():
    squares = "".join("M0 0 H%d V%d H0 Z " % (s, s) for s in range(1, 71))
    doc = emblem.flatten(svg(path(squares), box="0 0 100 100"))
    assert len(doc["rings"]) == emblem.MAX_RINGS
    sizes = sorted(max(p[0] for p in ring) for ring in doc["rings"])
    assert sizes[0] == pytest.approx(0.07) and sizes[-1] == pytest.approx(0.70)


def test_too_many_points_are_simplified():
    n = 6000
    pts = ["%.4f %.4f" % (20 + 15 * math.cos(2 * math.pi * i / n), 20 + 15 * math.sin(2 * math.pi * i / n)) for i in range(n)]
    doc = emblem.flatten(svg(path("M" + " L".join(pts) + " Z")))
    assert 3 <= total_points(doc) <= emblem.MAX_POINTS
    assert_circle(doc["rings"][0], 0.5, 0.5, 15 / 40)


def test_many_detailed_rings_share_the_point_budget():
    n = 300
    rings = []
    for k in range(64):
        cx, cy, r = 50 + (k % 8) * 100, 50 + (k // 8) * 100, 20 + k * 0.4
        pts = ["%.3f %.3f" % (cx + r * math.cos(2 * math.pi * i / n), cy + r * math.sin(2 * math.pi * i / n)) for i in range(n)]
        rings.append("M" + " L".join(pts) + " Z")
    doc = emblem.flatten(svg(path(" ".join(rings)), box="0 0 800 800"))
    assert len(doc["rings"]) == 64
    assert emblem.MAX_POINTS // 2 < total_points(doc) <= emblem.MAX_POINTS
    assert all(len(ring) >= 3 for ring in doc["rings"])


def test_overlapping_detail_is_simplified_quickly():
    # ~900 nearly full overlapping circles in one path (17 KiB): ~61k points, the
    # worst case of Ramer-Douglas-Peucker; this took ~20 s before
    d = "M500 500 " + "a400 400 0 1 1 1 0 " * 900
    started = time.monotonic()
    doc = emblem.flatten(svg(path(d), box="0 0 1000 1000"))
    assert time.monotonic() - started < 5.0
    assert 3 <= total_points(doc) <= emblem.MAX_POINTS


@pytest.mark.parametrize(
    "d,reason",
    [
        ("M500 500 " + "a400 400 0 1 1 1 0 " * 5000, "points"),  # ~340k points to simplify
        ("M0 0 " + "h1 v1 " * (emblem.MAX_PATH_COMMANDS // 2 + 1), "commands"),
    ],
    ids=["points", "commands"],
)
def test_too_much_detail_is_refused_quickly(d, reason):
    started = time.monotonic()
    with pytest.raises(ValueError, match=reason):
        emblem.flatten(svg(path(d), box="0 0 1000 1000"))
    assert time.monotonic() - started < 5.0


def test_the_command_budget_is_per_document():
    each = "M0 0 h1 v1 h-1 z"
    paths = "".join(path(each) for _ in range(emblem.MAX_PATH_COMMANDS // 5 + 1))
    with pytest.raises(ValueError, match="commands"):
        emblem.flatten(svg(paths))
    assert len(emblem.flatten(svg(path(each) * (emblem.MAX_PATH_COMMANDS // 5)))["rings"]) == emblem.MAX_RINGS


def test_degenerate_rings_are_dropped():
    doc = emblem.flatten(svg(path("M0 0 L10 10 Z M5 5 Z M10 10 H30 V30 H10 Z M0 0 L40 0 L0 0 Z M0 0 H10 H20 Z")))
    assert doc["rings"] == [SQUARE]


def test_a_figure_eight_has_no_net_area_but_is_kept():
    doc = emblem.flatten(svg(path("M0 0 L40 40 L40 0 L0 40 Z")))
    assert len(doc["rings"]) == 1 and emblem.ring_area(doc["rings"][0]) == 0


@pytest.mark.parametrize(
    "data",
    [
        svg(""),  # nothing to draw
        svg(path("M0 0 H10 V10 H0 Z"), box="0 0 1000 10"),  # aspect 100
        svg(path("M0 0 H10 V10 H0 Z"), box="0 0 0 10"),
        b'<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "aaaa">]><svg viewBox="0 0 1 1"><path d="M0 0 H1 V1 Z"/></svg>',
        b"<html><body>not an svg</body></html>",
        b"<svg",
        svg(path("M0 0 L")),
        svg(path("L10 10 20 20")),
        svg(path("M0 0 X10 10")),
        b"<svg>" + b" " * (emblem.MAX_SVG_BYTES + 1) + b"</svg>",
    ],
)
def test_unusable_svgs_are_rejected(data):
    with pytest.raises(ValueError):
        emblem.flatten(data)


class FakeFetch:
    def __init__(self, body=EARENDIL_SVG):
        self.body = body
        self.calls = []
        self.fail = False

    def __call__(self, url, **kw):
        self.calls.append((url, kw))
        if self.fail:
            raise net.FetchError(url, None, "offline")
        return self.body


class Clock:
    def __init__(self, t):
        self.t = t

    def __call__(self):
        return self.t


URL = "https://earendil.com/static/earendil-emblem.svg"


def test_source_fetches_caches_and_flattens(tmp_path):
    fetch = FakeFetch()
    src = emblem.EmblemSource(URL, cache.DiskCache(tmp_path), fetch)
    doc = src.refresh()
    assert doc == emblem.flatten(EARENDIL_SVG)
    assert fetch.calls[0][0] == URL and fetch.calls[0][1]["max_bytes"] == emblem.MAX_SVG_BYTES
    assert (tmp_path / emblem.CACHE_KEY).read_bytes() == EARENDIL_SVG


def test_a_download_is_flattened_once(tmp_path, monkeypatch):
    calls = []
    real = emblem.flatten

    def counting(data):
        calls.append(len(data))
        return real(data)

    monkeypatch.setattr(emblem, "flatten", counting)
    src = emblem.EmblemSource(URL, cache.DiskCache(tmp_path), FakeFetch())
    first = src.refresh()
    assert len(calls) == 1  # the validator's result is the one served
    assert src.refresh() == first and len(calls) == 1  # the same cached bytes again
    src.cache.clock = Clock(4_000_000_000.0)
    src.fetch = FakeFetch(svg(path("M10 10 H30 V30 H10 Z")))
    assert src.refresh()["rings"] == [SQUARE] and len(calls) == 2


def test_source_uses_the_cache_when_offline(tmp_path):
    fetch = FakeFetch()
    disk = cache.DiskCache(tmp_path)
    emblem.EmblemSource(URL, disk, fetch).refresh()
    disk.clock = Clock(4_000_000_000.0)
    fetch.fail = True
    assert emblem.EmblemSource(URL, disk, fetch).refresh()["aspect"] == 0.549


def test_source_without_cache_or_network_raises(tmp_path):
    fetch = FakeFetch()
    fetch.fail = True
    with pytest.raises(net.FetchError):
        emblem.EmblemSource(URL, cache.DiskCache(tmp_path), fetch).refresh()


def test_an_invalid_download_is_not_cached(tmp_path):
    fetch = FakeFetch(b"<html>gone</html>")
    with pytest.raises(ValueError):
        emblem.EmblemSource(URL, cache.DiskCache(tmp_path), fetch).refresh()
    assert not (tmp_path / emblem.CACHE_KEY).exists()


def test_source_from_a_local_file(tmp_path):
    local = tmp_path / "mark.svg"
    local.write_bytes(svg(path("M10 10 H30 V30 H10 Z")))
    fetch = FakeFetch()
    doc = emblem.EmblemSource(str(local), cache.DiskCache(tmp_path / "cache"), fetch).refresh()
    assert doc["rings"] == [SQUARE]
    assert fetch.calls == []
    assert not os.path.exists(tmp_path / "cache" / emblem.CACHE_KEY)

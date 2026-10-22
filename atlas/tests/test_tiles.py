import io
import os

import numpy as np
import pytest
from PIL import Image

import cache
import geo
import tiles
from fakes import FOREST, LAND, MOTORWAY, WATER, FakeNet, osm_tile


def styled(color):
    return np.asarray(tiles.dark_style(Image.new("RGB", (8, 8), color)))[4, 4]


def test_dark_style_is_neutral_grey_and_keeps_the_size():
    img = Image.open(io.BytesIO(osm_tile(6, 56, 25)))
    out = tiles.dark_style(img)
    assert out.mode == "RGB" and out.size == img.size
    a = np.asarray(out).astype(int)
    assert (a[..., 0] == a[..., 1]).all() and (a[..., 1] == a[..., 2]).all()


def test_dark_style_maps_bright_paper_dark_and_ink_light():
    assert styled((255, 255, 255))[0] <= 30
    assert styled(LAND)[0] <= 40
    assert styled((0, 0, 0))[0] >= 150
    assert styled((100, 100, 100))[0] > styled(LAND)[0] + 40


def test_dark_style_puts_water_at_the_background():
    assert styled(WATER)[0] <= styled(LAND)[0]
    assert styled(WATER)[0] <= 20


def test_dark_style_flattens_green_landcover_but_keeps_roads():
    assert abs(int(styled(FOREST)[0]) - int(styled(LAND)[0])) <= 6
    assert abs(int(styled((200, 250, 204))[0]) - int(styled(LAND)[0])) <= 6
    assert styled(MOTORWAY)[0] > styled(LAND)[0] + 40


def test_dark_style_is_monotonic_in_ink():
    greys = [styled((g, g, g))[0] for g in range(0, 256, 15)]
    assert greys == sorted(greys, reverse=True)


def fake_tile(z, x, y):
    return Image.new("RGB", (256, 256), ((x * 40) % 256, (y * 40) % 256, 0))


def test_stitch_returns_the_mosaic_and_the_extent_box():
    ext = geo.extent_around(139.77, 35.68, 40.0)
    mosaic, box = tiles.stitch(ext, 12, fake_tile)
    covered = geo.tiles_covering(ext, 12)
    nx = len({t[0] for t in covered})
    ny = len({t[1] for t in covered})
    assert mosaic.size == (nx * 256, ny * 256)
    x0, y0, x1, y1 = geo.extent_px(ext, 12)
    assert box[2] - box[0] == pytest.approx(x1 - x0)
    assert box[3] - box[1] == pytest.approx(y1 - y0)
    tx0, ty0 = covered[0]
    assert mosaic.getpixel((0, 0)) == ((tx0 * 40) % 256, (ty0 * 40) % 256, 0)


def test_render_base_is_a_1024x768_jpeg():
    ext = geo.extent_around(139.77, 35.68, 40.0)
    jpeg = tiles.render_base(ext, 12, lambda z, x, y: Image.open(io.BytesIO(osm_tile(z, x, y))))
    img = Image.open(io.BytesIO(jpeg))
    assert img.format == "JPEG" and img.size == (1024, 768)


def test_osm_tiles_are_cached_and_spaced(tmp_path):
    fake = FakeNet()
    slept = []
    now = [0.0]

    def sleep(s):
        slept.append(s)
        now[0] += s

    osm = tiles.OsmTiles(cache.DiskCache(tmp_path), fake, spacing=0.2, clock=lambda: now[0], sleep=sleep)
    img = osm.get(6, 56, 25)
    assert img.size == (256, 256) and img.mode == "RGB"
    osm.get(6, 57, 25)
    osm.get(6, 56, 25)
    assert fake.calls == [
        "https://tile.openstreetmap.org/6/56/25.png",
        "https://tile.openstreetmap.org/6/57/25.png",
    ]
    assert slept == [pytest.approx(0.2)]
    assert (tmp_path / "osm" / "6" / "56" / "25.png").exists()


def test_osm_tiles_younger_than_7_days_are_never_refetched(tmp_path):
    fake = FakeNet()
    c = cache.DiskCache(tmp_path)
    osm = tiles.OsmTiles(c, fake, spacing=0)
    osm.get(12, 3638, 1612)
    mtime = os.stat(c.path("osm/12/3638/1612.png")).st_mtime
    c.clock = lambda: mtime + 6.9 * 86400
    osm.get(12, 3638, 1612)
    assert fake.count("openstreetmap") == 1
    c.clock = lambda: mtime + 7.1 * 86400
    osm.get(12, 3638, 1612)
    assert fake.count("openstreetmap") == 2


def test_osm_serves_stale_tiles_when_offline(tmp_path):
    fake = FakeNet()
    c = cache.DiskCache(tmp_path)
    osm = tiles.OsmTiles(c, fake, spacing=0)
    osm.get(6, 56, 25)
    c.clock = lambda: 1e12
    fake.fail.append("openstreetmap")
    assert osm.get(6, 56, 25).size == (256, 256)
    with pytest.raises(Exception):
        osm.get(6, 1, 1)


def test_osm_rejects_non_images(tmp_path):
    c = cache.DiskCache(tmp_path)
    osm = tiles.OsmTiles(c, lambda url: b"<html>blocked</html>", spacing=0)
    with pytest.raises(Exception):
        osm.get(6, 56, 25)
    assert c.get("osm/6/56/25.png") is None


def test_osm_wraps_tile_x(tmp_path):
    fake = FakeNet()
    osm = tiles.OsmTiles(cache.DiskCache(tmp_path), fake, spacing=0)
    osm.get(2, 5, 1)
    assert fake.calls == ["https://tile.openstreetmap.org/2/1/1.png"]

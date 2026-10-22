import pytest

import geo

TOKYO = (139.77, 35.68)


@pytest.mark.parametrize("lon,lat", [TOKYO, (-0.1, 51.5), (135.75, 35.0), (-74.0, 40.7), (0.0, 0.0)])
@pytest.mark.parametrize("z", [0, 6, 12])
def test_world_px_and_lonlat_round_trip(lon, lat, z):
    x, y = geo.world_px(lon, lat, z)
    lon2, lat2 = geo.lonlat(x, y, z)
    assert lon2 == pytest.approx(lon, abs=1e-9)
    assert lat2 == pytest.approx(lat, abs=1e-9)


def test_world_px_known_values():
    assert geo.world_px(0, 0, 0) == pytest.approx((128.0, 128.0))
    assert geo.world_px(-180, 0, 1) == pytest.approx((0.0, 256.0))
    x, y = geo.world_px(*TOKYO, 12)
    assert (int(x // 256), int(y // 256)) == (3638, 1612)


def test_extent_around_has_the_requested_ground_width_and_4_3():
    ext = geo.extent_around(*TOKYO, 40.0)
    cy = (ext.y0 + ext.y1) / 2
    west = geo.unmerc(ext.x0, cy)
    east = geo.unmerc(ext.x1, cy)
    width = geo.haversine_km(west[1], west[0], east[1], east[0])
    assert width == pytest.approx(40.0, rel=0.01)
    assert (ext.x1 - ext.x0) / (ext.y1 - ext.y0) == pytest.approx(4 / 3)


def test_office_is_the_centre_of_its_local_extent():
    ext = geo.extent_around(*TOKYO, 40.0)
    assert geo.normalized(ext, *TOKYO) == pytest.approx((0.5, 0.5))


def test_extent_bbox_is_4_3_and_contains_the_box():
    ext = geo.extent_bbox(128.0, 30.0, 146.0, 46.0)
    assert (ext.x1 - ext.x0) / (ext.y1 - ext.y0) == pytest.approx(4 / 3)
    for lon in (128.0, 146.0):
        for lat in (30.0, 46.0):
            u, v = geo.normalized(ext, lon, lat)
            assert -1e-9 <= u <= 1 + 1e-9 and -1e-9 <= v <= 1 + 1e-9
    # Japan's box is taller than 4:3 in Mercator, so latitude stays tight
    assert geo.normalized(ext, 137.0, 46.0)[1] == pytest.approx(0.0, abs=1e-9)
    assert geo.normalized(ext, 137.0, 30.0)[1] == pytest.approx(1.0, abs=1e-9)
    assert geo.normalized(ext, 137.0, 38.0)[0] == pytest.approx(0.5)


def test_office_marker_in_the_japan_view():
    ext = geo.extent_bbox(128.0, 30.0, 146.0, 46.0)
    u, v = geo.normalized(ext, *TOKYO)
    assert 0.55 < u < 0.7 and 0.5 < v < 0.75


def test_tiles_covering_a_tiny_extent_is_the_containing_tile():
    ext = geo.extent_around(*TOKYO, 0.01)
    x, y = geo.world_px(*TOKYO, 12)
    assert geo.tiles_covering(ext, 12) == [(int(x // 256), int(y // 256))]


def test_tiles_covering_covers_the_extent_without_waste():
    ext = geo.extent_around(*TOKYO, 40.0)
    tiles = geo.tiles_covering(ext, 12)
    assert 12 <= len(tiles) <= 36
    x0, y0, x1, y1 = geo.extent_px(ext, 12)
    xs = [t[0] for t in tiles]
    ys = [t[1] for t in tiles]
    assert min(xs) * 256 <= x0 < (min(xs) + 1) * 256
    assert (max(xs)) * 256 < x1 <= (max(xs) + 1) * 256
    assert min(ys) * 256 <= y0 < (min(ys) + 1) * 256
    assert (max(ys)) * 256 < y1 <= (max(ys) + 1) * 256
    assert len(set(tiles)) == len(tiles) == (max(xs) - min(xs) + 1) * (max(ys) - min(ys) + 1)


def test_haversine_known_distances():
    assert geo.haversine_km(35.68, 139.77, 35.68, 139.77) == 0
    assert geo.haversine_km(35.68, 139.77, 35.0, 135.75) == pytest.approx(372, rel=0.03)
    assert geo.haversine_km(35.68, 139.77, 48.85, 2.35) == pytest.approx(9712, rel=0.02)

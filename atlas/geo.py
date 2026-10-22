"""Web Mercator helpers: lon/lat <-> world pixels, view extents and tile sets.

An Extent is a rectangle in normalised Web Mercator units: x and y run from 0
to 1 over the whole world, y grows southwards (like image rows).
"""
import math
from collections import namedtuple

TILE = 256
MAX_LAT = 85.05112878
EQUATOR_KM = 2 * math.pi * 6378.137
MEAN_RADIUS_KM = 6371.0088

Extent = namedtuple("Extent", "x0 y0 x1 y1")


def merc(lon, lat):
    lat = max(-MAX_LAT, min(MAX_LAT, lat))
    s = math.sin(math.radians(lat))
    return (lon + 180.0) / 360.0, 0.5 - math.log((1 + s) / (1 - s)) / (4 * math.pi)


def unmerc(x, y):
    return x * 360.0 - 180.0, math.degrees(math.atan(math.sinh(math.pi * (1 - 2 * y))))


def world_px(lon, lat, z):
    x, y = merc(lon, lat)
    n = TILE * 2 ** z
    return x * n, y * n


def lonlat(px, py, z):
    n = TILE * 2 ** z
    return unmerc(px / n, py / n)


def extent_around(lon, lat, width_km, aspect=4 / 3):
    """An extent of the given ground width (measured at its centre) around a point."""
    cx, cy = merc(lon, lat)
    w = width_km / (EQUATOR_KM * math.cos(math.radians(lat)))
    h = w / aspect
    return Extent(cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2)


def extent_bbox(lon0, lat0, lon1, lat1, aspect=4 / 3):
    """The smallest extent of the given aspect containing a lon/lat box, same centre."""
    x0, y1 = merc(lon0, lat0)
    x1, y0 = merc(lon1, lat1)
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    w, h = x1 - x0, y1 - y0
    if w / h < aspect:
        w = h * aspect
    else:
        h = w / aspect
    return Extent(cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2)


def normalized(ext, lon, lat):
    """Image coordinates in [0, 1] of a point inside an extent."""
    x, y = merc(lon, lat)
    return (x - ext.x0) / (ext.x1 - ext.x0), (y - ext.y0) / (ext.y1 - ext.y0)


def extent_px(ext, z):
    n = TILE * 2 ** z
    return ext.x0 * n, ext.y0 * n, ext.x1 * n, ext.y1 * n


def tiles_covering(ext, z):
    """Tiles (x, y) touching the extent, row by row; x is not wrapped."""
    x0, y0, x1, y1 = extent_px(ext, z)
    rows = 2 ** z
    tx0, tx1 = math.floor(x0 / TILE), math.ceil(x1 / TILE) - 1
    ty0, ty1 = max(0, math.floor(y0 / TILE)), min(rows - 1, math.ceil(y1 / TILE) - 1)
    return [(tx, ty) for ty in range(ty0, ty1 + 1) for tx in range(tx0, tx1 + 1)]


def haversine_km(lat1, lon1, lat2, lon2):
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp, dl = p2 - p1, math.radians(lon2 - lon1)
    a = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * MEAN_RADIUS_KM * math.asin(min(1.0, math.sqrt(a)))

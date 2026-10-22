"""OpenStreetMap base maps: polite tile fetching and the dark "tactical" style.

Tiles are cached on disk and never re-fetched while younger than 7 days; they
are fetched one at a time with a pause between requests. The style turns the
standard OSM rendering into neutral greys (the client tints it): paper goes to
near-black, ink (labels, casings, borders) to light grey, water to the
background.
"""
import io
import time

import numpy as np
from PIL import Image

import geo
from net import Spacer

OSM_URL = "https://tile.openstreetmap.org/{z}/{x}/{y}.png"
OSM_MAX_AGE = 7 * 86400
BASE_SIZE = (1024, 768)
JPEG_QUALITY = 86

BACKGROUND = 11  # water and empty sea
PAPER = 17  # land
INK = 205  # the darkest OSM ink ends up here


def _style_lut():
    inv = np.arange(256, dtype=np.float32)
    # OSM land is ~16 after inversion; keep it just above the background and
    # lift everything darker than the paper into a light-grey line range.
    t = np.clip((inv - 14.0) / (190.0 - 14.0), 0.0, 1.0)
    out = PAPER - 4 + (INK - PAPER + 4) * t ** 0.75
    out[inv <= 14] = PAPER - 4 + (inv[inv <= 14] / 14.0) * 4
    return np.clip(np.rint(out), 0, 255).astype(np.uint8)


_LUT = _style_lut()


def dark_style(img):
    """Grayscale, invert, map to the dark range.

    Water is pushed to the background and pale green landcover (forest, parks)
    is flattened to paper, so only lines and labels stand out.
    """
    a = np.asarray(img.convert("RGB"), dtype=np.int32)
    r, g, b = a[..., 0], a[..., 1], a[..., 2]
    lum = (r * 299 + g * 587 + b * 114 + 500) // 1000
    water = (b - r > 30) & (g - r > 15) & (b >= g - 10) & (lum > 150)
    green = (g - r > 10) & (g >= b) & (lum > 140) & ~water
    lum = np.where(green, np.maximum(lum, 236), lum)
    out = _LUT[255 - lum]
    out = np.where(water, np.uint8(BACKGROUND), out)
    return Image.fromarray(np.repeat(out[..., None], 3, axis=2).astype(np.uint8))


def _check_image(data):
    with Image.open(io.BytesIO(data)) as img:
        img.verify()


class OsmTiles:
    def __init__(self, cache, fetch, spacing=0.2, clock=time.monotonic, sleep=time.sleep):
        self.cache = cache
        self.fetch = fetch
        self.spacer = Spacer(spacing, clock, sleep)

    def get(self, z, x, y):
        x %= 2 ** z
        url = OSM_URL.format(z=z, x=x, y=y)

        def download():
            self.spacer.wait()
            return self.fetch(url)

        data = self.cache.fetch("osm/%d/%d/%d.png" % (z, x, y), OSM_MAX_AGE, download, _check_image)
        with Image.open(io.BytesIO(data)) as img:
            return img.convert("RGB")


def stitch(ext, z, get_tile):
    """Mosaic of the tiles covering an extent and the extent's box inside it."""
    covered = geo.tiles_covering(ext, z)
    tx0, ty0 = covered[0]
    tx1, ty1 = covered[-1]
    mosaic = Image.new("RGB", ((tx1 - tx0 + 1) * geo.TILE, (ty1 - ty0 + 1) * geo.TILE))
    for tx, ty in covered:
        tile = get_tile(z, tx, ty)
        if tile.size != (geo.TILE, geo.TILE):
            tile = tile.resize((geo.TILE, geo.TILE))
        mosaic.paste(tile.convert("RGB"), ((tx - tx0) * geo.TILE, (ty - ty0) * geo.TILE))
    x0, y0, x1, y1 = geo.extent_px(ext, z)
    ox, oy = tx0 * geo.TILE, ty0 * geo.TILE
    return mosaic, (x0 - ox, y0 - oy, x1 - ox, y1 - oy)


def render_base(ext, z, get_tile, size=BASE_SIZE):
    """Styled base map of an extent as JPEG bytes."""
    mosaic, box = stitch(ext, z, get_tile)
    styled = dark_style(mosaic).resize(size, Image.LANCZOS, box=box)
    buf = io.BytesIO()
    styled.save(buf, "JPEG", quality=JPEG_QUALITY, optimize=True)
    return buf.getvalue()

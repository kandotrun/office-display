"""Globe textures from NASA GIBS (WMS, EPSG:4326, 2048x1024 equirectangular).

Day: yesterday's VIIRS true colour over Blue Marble (base first, underneath).
VIIRS no-data (missing swaths, polar night) comes back opaque black even in
the composite, so those pixels are filled from a separately fetched static
Blue Marble. Night: VIIRS Black Marble city lights. Everything is kept on
disk; a failed refresh keeps the last good texture.
"""
import hashlib
import io
import time
from datetime import datetime, timedelta, timezone

import numpy as np
from PIL import Image, ImageFilter

WMS = (
    "https://gibs.earthdata.nasa.gov/wms/epsg4326/best/wms.cgi?SERVICE=WMS&REQUEST=GetMap"
    "&VERSION=1.3.0&LAYERS={layers}&STYLES=&CRS=EPSG:4326&BBOX=-90,-180,90,180"
    "&WIDTH=2048&HEIGHT=1024&FORMAT=image/jpeg"
)
BASE_LAYERS = "BlueMarble_ShadedRelief_Bathymetry"
DAY_LAYERS = BASE_LAYERS + ",VIIRS_SNPP_CorrectedReflectance_TrueColor"
NIGHT_LAYERS = "VIIRS_Black_Marble"
NIGHT_TIME = "2016-01-01"
SIZE = (2048, 1024)
STATIC_MAX_AGE = 30 * 86400  # night lights and Blue Marble do not change
DAY_MIN_MEAN = 10  # a blank (black) day texture is an error, not a picture
GAP_LEVEL = 8  # no-data is max channel < 6; real dark ocean is far above
DAY_KEY, DAY_DATE_KEY = "globe/day.jpg", "globe/day.date"
NIGHT_KEY, BASE_KEY = "globe/night.jpg", "globe/base.jpg"


def yesterday_utc(now):
    return (now.astimezone(timezone.utc) - timedelta(days=1)).date().isoformat()


def day_url(date):
    return WMS.format(layers=DAY_LAYERS) + "&TIME=" + date


def night_url():
    return WMS.format(layers=NIGHT_LAYERS) + "&TIME=" + NIGHT_TIME


def base_url():
    return WMS.format(layers=BASE_LAYERS)


def _rgb(data):
    with Image.open(io.BytesIO(data)) as img:
        return img.convert("RGB")


def fill_gaps(day, base):
    """Day JPEG with its opaque-black no-data replaced by the base, feathered."""
    d = _rgb(day)
    gap = np.asarray(d).max(axis=2) < GAP_LEVEL
    if not gap.any():
        return day
    b = _rgb(base)
    if b.size != d.size:
        b = b.resize(d.size, Image.LANCZOS)
    # grow the mask over the JPEG fringe, then soften the seam
    mask = Image.fromarray(gap.astype(np.uint8) * 255)
    mask = mask.filter(ImageFilter.MaxFilter(5)).filter(ImageFilter.GaussianBlur(1.5))
    buf = io.BytesIO()
    Image.composite(b, d, mask).save(buf, "JPEG", quality=88)
    return buf.getvalue()


def normalise(data, min_mean=0):
    """Validated 2048x1024 JPEG bytes (the original bytes when already right)."""
    with Image.open(io.BytesIO(data)) as img:
        img.load()
        fmt, size = img.format, img.size
        rgb = img.convert("RGB")
    if min_mean and float(np.asarray(rgb.convert("L").resize((64, 32))).mean()) < min_mean:
        raise ValueError("blank globe texture")
    if fmt == "JPEG" and size == SIZE:
        return data
    buf = io.BytesIO()
    rgb.resize(SIZE, Image.LANCZOS).save(buf, "JPEG", quality=88)
    return buf.getvalue()


class Globe:
    def __init__(self, cache, fetch, clock=time.time):
        self.cache = cache
        self.fetch = fetch
        self.clock = clock
        self.composed = (None, None)  # (key of the inputs, filled day)

    def _static(self, key, url):
        return self.cache.fetch(key, STATIC_MAX_AGE, lambda: normalise(self.fetch(url)))

    def refresh(self):
        """(day, night, error): the best textures available and the last failure."""
        error = None
        try:
            night = self._static(NIGHT_KEY, night_url())
        except Exception as exc:
            night, error = None, exc
        try:
            base = self._static(BASE_KEY, base_url())
        except Exception as exc:
            base, error = None, exc
        date = yesterday_utc(datetime.fromtimestamp(self.clock(), timezone.utc))
        day = self._day(date)
        if isinstance(day, Exception):
            error = day
            hit = self.cache.get(DAY_KEY)
            day = hit[0] if hit else None
        if day is not None and base is not None:
            day = self._filled(day, base)
        return day, night, error

    def _filled(self, day, base):
        key = hashlib.sha1(day).hexdigest() + hashlib.sha1(base).hexdigest()
        if self.composed[0] != key:
            self.composed = (key, fill_gaps(day, base))
        return self.composed[1]

    def _day(self, date):
        stamp = self.cache.get(DAY_DATE_KEY)
        hit = self.cache.get(DAY_KEY)
        if hit is not None and stamp is not None and stamp[0].decode("ascii", "ignore") == date:
            return hit[0]
        try:
            data = normalise(self.fetch(day_url(date)), min_mean=DAY_MIN_MEAN)
        except Exception as exc:
            return exc
        self.cache.put(DAY_KEY, data)
        self.cache.put(DAY_DATE_KEY, date.encode("ascii"))
        return data

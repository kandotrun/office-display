"""Where a photo was taken, at municipality granularity.

Coordinates are rounded to 0.05 degrees (about 5 km) before any use; only
rounded coordinates reach Nominatim or the disk. Photo names live in memory
only (bounded LRU) and are never written anywhere.
"""
import io
import json
import math
import os
import re
import tempfile
import threading
import time
import urllib.parse
from collections import OrderedDict

from PIL import Image

import geo
from net import Spacer

STEP = 0.05
NEAR_KM = 1500.0
NOMINATIM_URL = (
    "https://nominatim.openstreetmap.org/reverse?format=jsonv2&zoom=10&accept-language=ja"
    "&lat={lat:.2f}&lon={lon:.2f}"
)
PLACE_KEYS = ("city", "town", "village", "county", "municipality", "state", "province", "country")
GPS_IFD = 0x8825
EXIF_IFD = 0x8769
DATETIME_ORIGINAL = 36867
PHOTO_EXTENSIONS = ("jpg", "jpeg", "png")
_SAFE = frozenset(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~")
_FORBIDDEN = set("/\\%?#:")


def validate_name(name):
    """Same rules as the Rust playlist: a plain photo basename, nothing else."""
    if not isinstance(name, str) or not name or len(name.encode("utf-8")) > 255:
        return False
    if name.startswith(".") or ".." in name:
        return False
    if any(ord(c) < 32 or 127 <= ord(c) < 160 or c in _FORBIDDEN for c in name):
        return False
    _, dot, ext = name.rpartition(".")
    return bool(dot) and ext.lower() in PHOTO_EXTENSIONS


def encode_name(name):
    """Percent-encoding exactly like playlist::photo_path."""
    return "".join(chr(b) if b in _SAFE else "%%%02X" % b for b in name.encode("utf-8"))


def decode_segment(segment):
    try:
        return urllib.parse.unquote_to_bytes(segment).decode("utf-8")
    except UnicodeDecodeError:
        return None


def _ratio(value):
    if isinstance(value, tuple):
        if len(value) != 2 or not value[1]:
            return None
        return float(value[0]) / float(value[1])
    try:
        out = float(value)
    except (TypeError, ValueError, ZeroDivisionError):
        return None
    return out if math.isfinite(out) else None


def dms_to_deg(values):
    try:
        parts = [_ratio(v) for v in values]
    except TypeError:
        return None
    if len(parts) != 3 or any(p is None for p in parts):
        return None
    d, m, s = parts
    return d + m / 60.0 + s / 3600.0


def _ref(value):
    if isinstance(value, bytes):
        value = value.decode("ascii", "ignore")
    return str(value or "").strip("\x00 ").upper()


def _exif(data):
    with Image.open(io.BytesIO(data)) as img:
        return img.getexif()


def gps_of(data):
    """(lat, lon) in degrees from EXIF GPS, or None. Not rounded: use place_coords."""
    try:
        gps = _exif(data).get_ifd(GPS_IFD)
        lat = dms_to_deg(gps[2])
        lon = dms_to_deg(gps[4])
        if lat is None or lon is None:
            return None
        if _ref(gps.get(1, "N")) == "S":
            lat = -lat
        if _ref(gps.get(3, "E")) == "W":
            lon = -lon
    except Exception:
        return None
    if not (-90 <= lat <= 90 and -180 <= lon <= 180) or (lat == 0 and lon == 0):
        return None
    return lat, lon


def round_coord(value, step=STEP):
    return round(round(value / step) * step, 2) + 0.0


def place_coords(data):
    """Rounded (lat, lon) of a photo, or None without GPS."""
    gps = gps_of(data)
    if gps is None:
        return None
    return round_coord(gps[0]), round_coord(gps[1])


def taken_of(data):
    """DateTimeOriginal as "YYYY-MM", or None."""
    try:
        value = _exif(data).get_ifd(EXIF_IFD).get(DATETIME_ORIGINAL)
    except Exception:
        return None
    if isinstance(value, bytes):
        value = value.decode("ascii", "ignore")
    m = re.match(r"^(\d{4}):(\d{2})", str(value or ""))
    if not m or int(m.group(1)) < 1900 or not 1 <= int(m.group(2)) <= 12:
        return None
    return "%s-%s" % (m.group(1), m.group(2))


def place_name(address):
    if not isinstance(address, dict):
        return None
    for key in PLACE_KEYS:
        value = address.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


class LRU:
    def __init__(self, capacity):
        self.capacity = capacity
        self.items = OrderedDict()
        self.lock = threading.Lock()

    def __len__(self):
        return len(self.items)

    def lookup(self, key):
        with self.lock:
            if key not in self.items:
                return False, None
            self.items.move_to_end(key)
            return True, self.items[key]

    def put(self, key, value):
        with self.lock:
            self.items[key] = value
            self.items.move_to_end(key)
            while len(self.items) > self.capacity:
                self.items.popitem(last=False)


def coord_key(lat, lon):
    return "%.2f,%.2f" % (lat, lon)


class Geocoder:
    """Reverse geocoding of rounded coordinates, 1 request/s, cached on disk."""

    def __init__(self, path, fetch, interval=1.0, clock=time.monotonic, sleep=time.sleep):
        self.path = str(path)
        self.fetch = fetch
        self.spacer = Spacer(interval, clock, sleep)
        self.lock = threading.Lock()
        self.names = self._load()

    def _load(self):
        try:
            with open(self.path, encoding="utf-8") as f:
                data = json.load(f)
        except (OSError, ValueError):
            return {}
        if not isinstance(data, dict):
            return {}
        return {
            k: v
            for k, v in data.items()
            if re.match(r"^-?\d+\.\d{2},-?\d+\.\d{2}$", str(k)) and (v is None or isinstance(v, str))
        }

    def _save(self):
        folder = os.path.dirname(os.path.abspath(self.path))
        os.makedirs(folder, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=folder, prefix=".tmp-")
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(self.names, f, ensure_ascii=False, sort_keys=True)
        os.replace(tmp, self.path)

    def name(self, lat, lon):
        """Municipality name for an already rounded coordinate (None if unnamed)."""
        key = coord_key(lat, lon)
        with self.lock:
            if key in self.names:
                return self.names[key]
        self.spacer.wait()
        body = json.loads(self.fetch(NOMINATIM_URL.format(lat=lat, lon=lon)))
        name = place_name(body.get("address")) if isinstance(body, dict) else None
        with self.lock:
            self.names[key] = name
            try:
                self._save()
            except OSError:
                pass
        return name


class GeocodeError(Exception):
    """Reverse geocoding failed; temporary, so the result is not memoised."""


class PhotoPlaces:
    """Photo name -> place JSON, memoised in memory only.

    A place always has a non-empty name: photos without GPS and spots without
    a name (open sea) are both "no place".
    """

    def __init__(self, fetch_photo, geocoder, office, capacity=512, near_km=NEAR_KM):
        self.fetch_photo = fetch_photo
        self.geocoder = geocoder
        self.office = office
        self.near_km = near_km
        self.memo = LRU(capacity)

    def lookup(self, name):
        """Place dict, or None for no place.

        Photo fetch errors propagate as they are; geocoding errors as GeocodeError.
        """
        found, value = self.memo.lookup(name)
        if found:
            return value
        data = self.fetch_photo(name)
        coords = place_coords(data)
        result = None
        if coords is not None:
            lat, lon = coords
            try:
                place = self.geocoder.name(lat, lon)
            except Exception as exc:
                raise GeocodeError(str(exc)) from exc
            if place:
                result = {
                    "place": place,
                    "lat": lat,
                    "lon": lon,
                    "taken": taken_of(data),
                    "near": geo.haversine_km(lat, lon, self.office[0], self.office[1]) <= self.near_km,
                }
        self.memo.put(name, result)
        return result

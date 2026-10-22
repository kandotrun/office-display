"""Real-shaped fixtures and a fake network for the atlas tests (no network)."""
import io
import json
import re
from datetime import datetime, timedelta, timezone
from urllib.parse import parse_qs, urlparse

from PIL import Image, ImageDraw
from PIL.TiffImagePlugin import IFDRational

import net

UTC = timezone.utc
NOW = datetime(2026, 10, 22, 9, 5, tzinfo=UTC)
FMT = "%Y%m%d%H%M%S"

LAND = (242, 239, 233)
WATER = (170, 211, 223)
FOREST = (173, 209, 158)
MOTORWAY = (232, 146, 162)
DAY_SEA = (30, 60, 120)
BLUE_MARBLE = (10, 20, 200)


def stamp(t):
    return t.strftime(FMT)


def n1_entries(newest=NOW, count=37):
    """targetTimes_N1.json: observations, newest first, basetime == validtime."""
    out = []
    for i in range(count):
        t = stamp(newest - timedelta(minutes=5 * i))
        out.append({"basetime": t, "validtime": t, "elements": ["hrpns", "hrpns_nd"]})
    return out


def n2_entries(base=NOW, count=12):
    """targetTimes_N2.json: forecasts from the latest basetime, newest validtime first."""
    return [
        {
            "basetime": stamp(base),
            "validtime": stamp(base + timedelta(minutes=5 * k)),
            "elements": ["hrpns", "hrpns_nd"],
        }
        for k in range(count, 0, -1)
    ]


def png(img, **kw):
    buf = io.BytesIO()
    img.save(buf, "PNG", **kw)
    return buf.getvalue()


def jpeg(img, **kw):
    buf = io.BytesIO()
    img.save(buf, "JPEG", **kw)
    return buf.getvalue()


def jma_tile(color=None):
    """A nowcast tile: fully transparent RGBA when dry, palette mode when raining."""
    if color is None:
        return png(Image.new("RGBA", (256, 256), (0, 0, 0, 0)))
    img = Image.new("P", (256, 256), 1)
    img.putpalette([0, 0, 0] + list(color) + [0, 0, 0] * 254)
    return png(img, transparency=0)


def osm_tile(z, x, y):
    img = Image.new("RGB", (256, 256), LAND)
    draw = ImageDraw.Draw(img)
    draw.rectangle([0, 200, 255, 255], fill=WATER)
    draw.line([(0, 100), (255, 100)], fill=(190, 190, 190), width=7)
    draw.line([(0, 100), (255, 100)], fill=(255, 255, 255), width=4)
    draw.text((20, 40), "%d/%d/%d" % (z, x, y), fill=(40, 40, 40))
    return png(img)


def forecast(area_code="130010", temp_point="44132", temps=("14", "21")):
    """bosai/forecast/data/forecast/130000.json, trimmed to the parts atlas reads."""
    return [
        {
            "publishingOffice": "気象庁",
            "reportDatetime": "2026-10-22T11:00:00+09:00",
            "timeSeries": [
                {
                    "timeDefines": [
                        "2026-10-22T11:00:00+09:00",
                        "2026-10-23T00:00:00+09:00",
                        "2026-10-24T00:00:00+09:00",
                    ],
                    "areas": [
                        {
                            "area": {"name": "東京地方", "code": area_code},
                            "weatherCodes": ["200", "201", "101"],
                            "weathers": ["くもり　夜　雨", "くもり　時々　晴れ", "晴れ"],
                            "winds": ["北の風", "北の風", "北の風"],
                        },
                        {
                            "area": {"name": "伊豆諸島北部", "code": "130020"},
                            "weatherCodes": ["313", "200", "100"],
                            "weathers": ["雨　のち　くもり", "くもり", "晴れ"],
                        },
                    ],
                },
                {
                    "timeDefines": ["2026-10-22T12:00:00+09:00", "2026-10-22T18:00:00+09:00"],
                    "areas": [{"area": {"name": "東京地方", "code": area_code}, "pops": ["20", "60"]}],
                },
                {
                    "timeDefines": ["2026-10-23T00:00:00+09:00", "2026-10-23T09:00:00+09:00"][: len(temps)]
                    if len(temps) <= 2
                    else [
                        "2026-10-22T00:00:00+09:00",
                        "2026-10-22T09:00:00+09:00",
                        "2026-10-23T00:00:00+09:00",
                        "2026-10-23T09:00:00+09:00",
                    ],
                    "areas": [
                        {"area": {"name": "東京", "code": temp_point}, "temps": list(temps)},
                        {"area": {"name": "大島", "code": "44172"}, "temps": ["16", "22"]},
                    ],
                },
            ],
        },
        {"publishingOffice": "気象庁", "timeSeries": []},
    ]


def dms(value):
    value = abs(value)
    d = int(value)
    m = int((value - d) * 60)
    s = ((value - d) * 60 - m) * 60
    return (IFDRational(d, 1), IFDRational(m, 1), IFDRational(int(round(s * 100)), 100))


def photo_jpeg(lat=None, lon=None, taken=None, size=(64, 48)):
    img = Image.new("RGB", size, (120, 140, 160))
    exif = Image.Exif()
    if lat is not None:
        gps = exif.get_ifd(0x8825)
        gps[1] = "N" if lat >= 0 else "S"
        gps[2] = dms(lat)
        gps[3] = "E" if lon >= 0 else "W"
        gps[4] = dms(lon)
    if taken is not None:
        exif.get_ifd(0x8769)[36867] = taken
    return jpeg(img, exif=exif)


TILE_RE = re.compile(r"/nowc/(\d{14})/none/(\d{14})/surf/hrpns/(\d+)/(\d+)/(\d+)\.png$")


class FakeNet:
    """Answers every URL atlas uses with real-shaped bodies; records the calls."""

    def __init__(self, rain_from=30, rain_color=(160, 210, 255), photos=None):
        self.calls = []
        self.fail = []
        self.now = NOW  # newest observation in targetTimes
        self.rain_from = rain_from
        self.rain_color = rain_color
        self.photos = dict(photos or {})

    def count(self, part):
        return sum(part in c for c in self.calls)

    def __call__(self, url, **_kw):
        self.calls.append(url)
        for prefix in self.fail:
            if prefix in url:
                raise net.FetchError(url, 503, "fake failure")
        if url.startswith("https://tile.openstreetmap.org/"):
            z, x, y = (int(p) for p in url.rsplit(".", 1)[0].split("/")[-3:])
            return osm_tile(z, x, y)
        if url.endswith("targetTimes_N1.json"):
            return json.dumps(n1_entries(self.now)).encode()
        if url.endswith("targetTimes_N2.json"):
            return json.dumps(n2_entries(self.now)).encode()
        m = TILE_RE.search(url)
        if m:
            valid = datetime.strptime(m.group(2), FMT).replace(tzinfo=UTC)
            wet = self.rain_from is not None and valid >= NOW + timedelta(minutes=self.rain_from)
            return jma_tile(self.rain_color if wet else None)
        if url.endswith("/bosai/forecast/data/forecast/340000.json"):
            body = forecast(area_code="340010", temp_point="67437", temps=("15", "22"))
            return json.dumps(body, ensure_ascii=False).encode()
        if url.endswith("/bosai/forecast/data/forecast/130000.json"):
            return json.dumps(forecast(), ensure_ascii=False).encode()
        if url.startswith("https://gibs.earthdata.nasa.gov/"):
            if "VIIRS_Black_Marble" in url:
                return jpeg(Image.new("RGB", (512, 256), (6, 6, 9)))
            if "VIIRS_SNPP" in url:
                # yesterday's swaths with a no-data gap (opaque black) on the left
                day = Image.new("RGB", (512, 256), DAY_SEA)
                ImageDraw.Draw(day).rectangle([0, 0, 127, 255], fill=(0, 0, 0))
                return jpeg(day)
            return jpeg(Image.new("RGB", (512, 256), BLUE_MARBLE))
        if url.startswith("https://nominatim.openstreetmap.org/reverse"):
            q = parse_qs(urlparse(url).query)
            lat = float(q["lat"][0])
            if abs(lat - 35.0) < 0.01:
                body = {"address": {"city": "京都市", "province": "京都府", "country": "日本"}}
            elif lat > 48:
                body = {"address": {"city": "パリ", "suburb": "パリ", "country": "フランス"}}
            else:
                body = {"error": "Unable to geocode"}
            return json.dumps(body, ensure_ascii=False).encode()
        if url.startswith("http://127.0.0.1:") and "/photos/" in url:
            name = url.split("/photos/", 1)[1]
            if name in self.photos:
                return self.photos[name]
        raise net.FetchError(url, 404, "not found")

"""JMA high-resolution precipitation nowcast (hrpns): frames, levels, overlays.

targetTimes_N1.json lists observations (basetime == validtime), N2 the
forecasts from the latest basetime. Tiles are 256 px PNGs in the JMA palette;
dry pixels are transparent. Not an official API, so every failure is soft.
"""
import io
from collections import namedtuple
from datetime import datetime, timedelta, timezone

import numpy as np
from PIL import Image

import geo

NOWC = "https://www.jma.go.jp/bosai/jmatile/data/nowc/"
N1_URL = NOWC + "targetTimes_N1.json"
N2_URL = NOWC + "targetTimes_N2.json"
TILE_URL = NOWC + "{basetime}/none/{validtime}/surf/hrpns/{z}/{x}/{y}.png"

# JMA palette (alpha 255) and the intensity level of each colour.
PALETTE = np.array(
    [
        (242, 242, 255),  # 0-1 mm/h
        (160, 210, 255),  # 1-5
        (33, 140, 255),  # 5-10
        (0, 65, 255),  # 10-20
        (250, 245, 0),  # 20-30
        (255, 153, 0),  # 30-50
        (255, 40, 0),  # 50-80
        (180, 0, 104),  # 80-
    ],
    dtype=np.int32,
)
# palette index (0 = dry, 1..8 = PALETTE row + 1) -> level 0..6
INDEX_LEVEL = np.array([0, 1, 1, 1, 2, 3, 4, 5, 6], dtype=np.uint8)
CLASSES = ["", "弱い雨", "やや強い雨", "強い雨", "激しい雨", "非常に激しい雨", "猛烈な雨"]
OVERLAY_ALPHA = 210
OVERLAY_SIZE = (512, 384)
# hrpns tiles carry data only at these zooms; odd zooms answer 200 with an
# empty transparent tile, which would silently look like "no rain".
DATA_ZOOMS = (4, 6, 8, 10)
# Observed tiles are kept this long: the 10-minute frame grid moves by 5
# minutes per refresh, so a tile is needed again two refreshes later.
RETAIN = timedelta(minutes=70)

_LUT = np.zeros((len(PALETTE) + 1, 4), dtype=np.uint8)
_LUT[1:, :3] = PALETTE
_LUT[1:, 3] = OVERLAY_ALPHA

Frame = namedtuple("Frame", "basetime validtime at forecast")


def _utc(stamp):
    return datetime.strptime(stamp, "%Y%m%d%H%M%S").replace(tzinfo=timezone.utc)


def parse_target_times(entries, forecast):
    """hrpns frames from a targetTimes list, oldest first, one per validtime."""
    if not isinstance(entries, list):
        return []
    by_valid = {}
    for entry in entries:
        try:
            if "hrpns" not in entry.get("elements", ()):
                continue
            base, valid = str(entry["basetime"]), str(entry["validtime"])
            at = _utc(valid)
            _utc(base)
        except (AttributeError, KeyError, TypeError, ValueError):
            continue
        old = by_valid.get(valid)
        if old is None or old.basetime < base:
            by_valid[valid] = Frame(base, valid, at, bool(forecast))
    return sorted(by_valid.values(), key=lambda f: f.at)


def now_of(past, future):
    """The newest observation that already has a forecast run.

    N1 is published a minute or two before N2; until the new run appears the
    previous observation stays "now", so the loop keeps a full forecast hour.
    """
    if not future:
        return past[-1].at
    run = max(_utc(f.basetime) for f in future)
    ready = [f.at for f in past if f.at <= run]
    return ready[-1] if ready else past[-1].at


def pick_frames(past, future, step=10, span=60):
    """Past `span` minutes, now and `span` minutes of forecast at `step` minutes.

    Returns (frames, now_index) where now_index is the "now" observation.
    """
    if not past:
        return [], None
    now = now_of(past, future)
    observed = {f.at: f for f in past}
    ahead = {f.at: f for f in future if f.at > now}
    frames = [observed[t] for t in (now + timedelta(minutes=m) for m in range(-span, 1, step)) if t in observed]
    now_index = len(frames) - 1
    frames += [ahead[t] for t in (now + timedelta(minutes=m) for m in range(step, span + 1, step)) if t in ahead]
    return frames, now_index


def series_frames(past, future, step=5, count=12):
    """Forecast frames for the next count*step minutes (None where missing)."""
    if not past:
        return []
    now = now_of(past, future)
    ahead = {f.at: f for f in future}
    return [ahead.get(now + timedelta(minutes=step * (i + 1))) for i in range(count)]


def fill_series(levels, start):
    """Missing steps (None) repeat the last known level instead of reading as dry."""
    out, last = [], start
    for value in levels:
        last = last if value is None else value
        out.append(last)
    return out


def tile_url(frame, z, x, y):
    return TILE_URL.format(basetime=frame.basetime, validtime=frame.validtime, z=z, x=x, y=y)


def palette_index(rgba):
    """Nearest JMA colour per pixel: 0 for dry (alpha < 128), else 1..8."""
    a = np.asarray(rgba)
    out = np.zeros(a.shape[:2], dtype=np.uint8)
    if a.ndim != 3 or a.shape[2] != 4:
        return out
    wet = a[..., 3] >= 128
    if not wet.any():
        return out
    rgb = a[wet][:, :3].astype(np.int32)
    dist = ((rgb[:, None, :] - PALETTE[None, :, :]) ** 2).sum(axis=2)
    out[wet] = dist.argmin(axis=1).astype(np.uint8) + 1
    return out


def levels_of(rgba):
    return INDEX_LEVEL[palette_index(rgba)]


def level_of(color):
    rgba = tuple(color) + (255,) * (4 - len(color))
    return int(levels_of(np.array([[rgba]], dtype=np.uint8))[0, 0])


def level_at(levels, x, y):
    """Highest level in the 3x3 neighbourhood of pixel (x, y), clamped to the array."""
    h, w = levels.shape
    x, y = int(x), int(y)
    x0, x1 = max(0, x - 1), min(w, x + 2)
    y0, y1 = max(0, y - 1), min(h, y + 2)
    if x0 >= x1 or y0 >= y1:
        return 0
    return int(levels[y0:y1, x0:x1].max())


def summary(level, series):
    series = list(series)
    if level > 0:
        text = "雨が降っています（%s）" % CLASSES[min(level, 6)]
        if series and series[-1] == 0:
            first_dry = len(series)
            while first_dry > 0 and series[first_dry - 1] == 0:
                first_dry -= 1
            text += "・%d分後にやむ見込み" % (5 * (first_dry + 1))
        return text
    for i, value in enumerate(series):
        if value > 0:
            return "%d分後に%s" % (5 * (i + 1), CLASSES[min(value, 6)])
    return "この先1時間 雨の心配なし"


def _soften(rgba):
    """3x3 box blur in premultiplied alpha, so edges feather without dark fringes."""
    a = rgba.astype(np.float32)
    alpha = a[..., 3:4] / 255.0
    pre = np.concatenate([a[..., :3] * alpha, a[..., 3:4]], axis=2)
    pad = np.pad(pre, ((1, 1), (1, 1), (0, 0)), mode="edge")
    h, w = rgba.shape[:2]
    acc = sum(pad[dy:dy + h, dx:dx + w] for dy in range(3) for dx in range(3)) / 9.0
    out_a = acc[..., 3:4]
    with np.errstate(divide="ignore", invalid="ignore"):
        rgb = np.where(out_a > 0, acc[..., :3] * 255.0 / np.maximum(out_a, 1e-6), 0)
    out = np.concatenate([rgb, out_a], axis=2)
    return np.clip(np.rint(out), 0, 255).astype(np.uint8)


def overlay_image(index, box, size=OVERLAY_SIZE, blur=False):
    """Palette-index mosaic -> RGBA overlay of `box` (mosaic pixels) at `size`."""
    scaled = np.asarray(Image.fromarray(index).resize(size, Image.NEAREST, box=box))
    rgba = _LUT[scaled]
    if blur:
        rgba = _soften(rgba)
    return Image.fromarray(rgba)


def render_frame(get_index, ext, z, frame, size=OVERLAY_SIZE, blur=False):
    """Overlay of an extent for one frame; get_index(frame, z, x, y) gives a tile."""
    covered = geo.tiles_covering(ext, z)
    tx0, ty0 = covered[0]
    tx1, ty1 = covered[-1]
    mosaic = np.zeros(((ty1 - ty0 + 1) * geo.TILE, (tx1 - tx0 + 1) * geo.TILE), dtype=np.uint8)
    for tx, ty in covered:
        ox, oy = (tx - tx0) * geo.TILE, (ty - ty0) * geo.TILE
        mosaic[oy:oy + geo.TILE, ox:ox + geo.TILE] = get_index(frame, z, tx, ty)
    x0, y0, x1, y1 = geo.extent_px(ext, z)
    box = (x0 - tx0 * geo.TILE, y0 - ty0 * geo.TILE, x1 - tx0 * geo.TILE, y1 - ty0 * geo.TILE)
    return overlay_image(mosaic, box, size, blur)


def office_level(get_index, frame, lon, lat, z):
    px, py = geo.world_px(lon, lat, z)
    tx, ty = int(px // geo.TILE), int(py // geo.TILE)
    levels = INDEX_LEVEL[get_index(frame, z, tx, ty)]
    return level_at(levels, px - tx * geo.TILE, py - ty * geo.TILE)


def png_bytes(img):
    buf = io.BytesIO()
    img.save(buf, "PNG")
    return buf.getvalue()


_DRY = np.zeros((geo.TILE, geo.TILE), dtype=np.uint8)
_DRY.flags.writeable = False


class TileSource:
    """JMA tiles as palette-index arrays, memoised across refreshes.

    Observed frames never change, so their tiles are kept for RETAIN and
    fetched once; forecast tiles are dropped as soon as a refresh does not use
    them (every refresh brings a new basetime). Dry tiles share one array.
    """

    def __init__(self, fetch, not_found=(404,)):
        self.fetch = fetch
        self.not_found = not_found
        self.memo = {}  # url -> (frame, index array)
        self.used = set()

    def begin(self):
        self.used = set()

    def end(self, now):
        """Forget tiles this refresh did not use, except recent observations."""
        self.memo = {
            url: (frame, arr)
            for url, (frame, arr) in self.memo.items()
            if url in self.used or (not frame.forecast and frame.at >= now - RETAIN)
        }

    def index(self, frame, z, x, y):
        if z not in DATA_ZOOMS:
            raise ValueError("hrpns has no data at zoom %d" % z)
        rows = 2 ** z
        if not 0 <= y < rows:
            return _DRY
        url = tile_url(frame, z, x % rows, y)
        self.used.add(url)
        hit = self.memo.get(url)
        if hit is None:
            hit = (frame, self._load(url))
            self.memo[url] = hit
        return hit[1]

    def _load(self, url):
        try:
            data = self.fetch(url)
        except Exception as exc:
            if getattr(exc, "status", None) in self.not_found:
                return _DRY
            raise
        with Image.open(io.BytesIO(data)) as img:
            if img.size != (geo.TILE, geo.TILE):
                raise ValueError("unexpected radar tile size %r" % (img.size,))
            index = palette_index(np.asarray(img.convert("RGBA")))
        return index if index.any() else _DRY

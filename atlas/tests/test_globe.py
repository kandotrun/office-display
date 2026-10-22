import io
from datetime import datetime, timezone

import numpy as np
import pytest
from PIL import Image

import cache
import globe
from fakes import BLUE_MARBLE, DAY_SEA, FakeNet, jpeg

UTC = timezone.utc


def test_yesterday_utc():
    assert globe.yesterday_utc(datetime(2026, 10, 22, 0, 30, tzinfo=UTC)) == "2026-10-21"
    assert globe.yesterday_utc(datetime(2026, 3, 1, 23, 59, tzinfo=UTC)) == "2026-02-28"
    # 08:00 in Tokyo is 23:00 UTC of the previous day
    assert globe.yesterday_utc(datetime(2026, 10, 21, 23, 0, tzinfo=UTC)) == "2026-10-20"


def test_day_url():
    url = globe.day_url("2026-10-21")
    assert url.startswith("https://gibs.earthdata.nasa.gov/wms/epsg4326/best/wms.cgi?")
    assert "LAYERS=BlueMarble_ShadedRelief_Bathymetry,VIIRS_SNPP_CorrectedReflectance_TrueColor&" in url
    for part in ("SERVICE=WMS", "REQUEST=GetMap", "VERSION=1.3.0", "CRS=EPSG:4326", "BBOX=-90,-180,90,180",
                 "WIDTH=2048", "HEIGHT=1024", "FORMAT=image/jpeg", "TIME=2026-10-21"):
        assert part in url


def test_night_url():
    url = globe.night_url()
    assert "LAYERS=VIIRS_Black_Marble&" in url and "TIME=2016-01-01" in url


def test_base_url_is_static_blue_marble():
    url = globe.base_url()
    assert "LAYERS=BlueMarble_ShadedRelief_Bathymetry&" in url and "TIME=" not in url
    assert "WIDTH=2048" in url and "HEIGHT=1024" in url


def test_fill_gaps_puts_blue_marble_under_missing_swaths():
    day = Image.new("RGB", (256, 128), DAY_SEA)
    day.paste((0, 0, 0), (0, 0, 64, 128))
    out = globe.fill_gaps(jpeg(day), jpeg(Image.new("RGB", (256, 128), BLUE_MARBLE)))
    a = np.asarray(Image.open(io.BytesIO(out)).convert("RGB")).astype(int)
    assert a.shape == (128, 256, 3)
    assert np.abs(a[64, 20] - BLUE_MARBLE).max() <= 12
    assert np.abs(a[64, 200] - DAY_SEA).max() <= 12
    # dark but real ocean is not a gap
    dark_sea = Image.new("RGB", (256, 128), (6, 14, 40))
    kept = globe.fill_gaps(jpeg(dark_sea), jpeg(Image.new("RGB", (256, 128), BLUE_MARBLE)))
    assert np.abs(np.asarray(Image.open(io.BytesIO(kept)))[64, 128].astype(int) - (6, 14, 40)).max() <= 12


def test_normalise_resizes_to_2048x1024():
    out = globe.normalise(jpeg(Image.new("RGB", (512, 256), (30, 60, 120))))
    assert Image.open(io.BytesIO(out)).size == (2048, 1024)
    exact = jpeg(Image.new("RGB", (2048, 1024), (30, 60, 120)))
    assert globe.normalise(exact) == exact


def test_normalise_rejects_errors_and_blank_days():
    with pytest.raises(Exception):
        globe.normalise(b'<?xml version="1.0"?><ServiceExceptionReport/>')
    with pytest.raises(ValueError):
        globe.normalise(jpeg(Image.new("RGB", (2048, 1024), (0, 0, 0))), min_mean=10)


class Clock:
    def __init__(self, t):
        self.t = t

    def __call__(self):
        return self.t


def test_refresh_fetches_once_per_day_and_keeps_the_last_good(tmp_path):
    fake = FakeNet()
    clock = Clock(datetime(2026, 10, 22, 3, 0, tzinfo=UTC).timestamp())
    g = globe.Globe(cache.DiskCache(tmp_path), fake, clock=clock)
    day, night, error = g.refresh()
    assert error is None
    assert Image.open(io.BytesIO(day)).size == (2048, 1024)
    assert Image.open(io.BytesIO(night)).size == (2048, 1024)
    assert fake.count("TIME=2026-10-21") == 1 and fake.count("VIIRS_Black_Marble") == 1
    assert fake.count("LAYERS=BlueMarble_ShadedRelief_Bathymetry&") == 1
    a = np.asarray(Image.open(io.BytesIO(day)).convert("RGB")).astype(int)
    assert np.abs(a[512, 100] - BLUE_MARBLE).max() <= 12  # the gap is filled
    assert np.abs(a[512, 1500] - DAY_SEA).max() <= 12

    assert g.refresh()[0] == day
    assert len(fake.calls) == 3

    clock.t += 86400
    fake.fail.append("gibs")
    day2, night2, error = g.refresh()
    assert error is not None
    assert day2 == day and night2 == night

    fake.fail.clear()
    again = globe.Globe(cache.DiskCache(tmp_path), fake, clock=clock)
    day3, _, error = again.refresh()
    assert error is None and fake.count("TIME=2026-10-22") == 2


def test_refresh_without_anything_cached_reports_none(tmp_path):
    fake = FakeNet()
    fake.fail.append("gibs")
    g = globe.Globe(cache.DiskCache(tmp_path), fake, clock=Clock(1.8e9))
    day, night, error = g.refresh()
    assert day is None and night is None and error is not None

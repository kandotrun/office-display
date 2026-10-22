import io
from datetime import timedelta

import numpy as np
import pytest
from PIL import Image

import geo
import radar
from fakes import NOW, jma_tile, n1_entries, n2_entries


def frames():
    past = radar.parse_target_times(n1_entries(), forecast=False)
    future = radar.parse_target_times(n2_entries(), forecast=True)
    return past, future


def test_parse_target_times_sorts_oldest_first():
    past, future = frames()
    assert len(past) == 37 and len(future) == 12
    assert past[-1].at == NOW and past[0].at == NOW - timedelta(minutes=180)
    assert past[-1].basetime == past[-1].validtime == "20261022090500"
    assert not any(f.forecast for f in past)
    assert all(f.forecast for f in future)
    assert future[0].at == NOW + timedelta(minutes=5)
    assert all(f.basetime == "20261022090500" for f in future)


def test_parse_target_times_skips_bad_entries():
    entries = [
        {"basetime": "20261022090500", "validtime": "20261022090500", "elements": ["hrpns"]},
        {"basetime": "20261022090000", "validtime": "20261022090000", "elements": ["other"]},
        {"basetime": "nope", "validtime": "20261022085500", "elements": ["hrpns"]},
        {"validtime": "20261022085000", "elements": ["hrpns"]},
        "garbage",
    ]
    out = radar.parse_target_times(entries, forecast=False)
    assert [f.validtime for f in out] == ["20261022090500"]
    assert radar.parse_target_times({"not": "a list"}, forecast=False) == []


def test_pick_frames_gives_13_frames_with_now_at_6():
    past, future = frames()
    picked, now_index = radar.pick_frames(past, future)
    assert len(picked) == 13 and now_index == 6
    assert picked[now_index].at == NOW
    assert [f.at for f in picked] == [NOW + timedelta(minutes=10 * k) for k in range(-6, 7)]
    assert [f.forecast for f in picked] == [False] * 7 + [True] * 6


def test_pick_frames_tolerates_gaps():
    past, future = frames()
    past = [f for f in past if f.at != NOW - timedelta(minutes=30)]
    picked, now_index = radar.pick_frames(past, future[:3])
    assert len(picked) == 7 and now_index == 5
    assert picked[now_index].at == NOW


def test_pick_frames_without_observations():
    assert radar.pick_frames([], frames()[1]) == ([], None)


def test_series_frames_are_the_next_12_five_minute_forecasts():
    past, future = frames()
    series = radar.series_frames(past, future)
    assert [f.at for f in series] == [NOW + timedelta(minutes=5 * (i + 1)) for i in range(12)]
    gappy = radar.series_frames(past, future[1:])
    assert gappy[0] is None and gappy[1].at == NOW + timedelta(minutes=10)


def test_tile_url():
    f = radar.parse_target_times(n2_entries(), forecast=True)[0]
    assert radar.tile_url(f, 10, 909, 403) == (
        "https://www.jma.go.jp/bosai/jmatile/data/nowc/20261022090500/none/"
        "20261022091000/surf/hrpns/10/909/403.png"
    )


@pytest.mark.parametrize(
    "rgba,level",
    [
        ((242, 242, 255, 255), 1),
        ((160, 210, 255, 255), 1),
        ((33, 140, 255, 255), 1),
        ((0, 65, 255, 255), 2),
        ((250, 245, 0, 255), 3),
        ((255, 153, 0, 255), 4),
        ((255, 40, 0, 255), 5),
        ((180, 0, 104, 255), 6),
        ((0, 0, 0, 0), 0),
        ((255, 40, 0, 100), 0),
        ((30, 136, 250, 255), 1),
        ((4, 70, 248, 200), 2),
        ((176, 6, 110, 255), 6),
    ],
)
def test_level_of(rgba, level):
    assert radar.level_of(rgba) == level


def test_levels_of_decodes_a_palette_tile():
    rgba = np.asarray(Image.open(io.BytesIO(jma_tile((250, 245, 0)))).convert("RGBA"))
    levels = radar.levels_of(rgba)
    assert levels.shape == (256, 256) and levels.dtype == np.uint8
    assert set(np.unique(levels)) == {3}
    dry = np.asarray(Image.open(io.BytesIO(jma_tile(None))).convert("RGBA"))
    assert not radar.levels_of(dry).any()


def test_level_at_takes_the_highest_level_in_3x3():
    levels = np.zeros((10, 10), np.uint8)
    levels[4, 6] = 2
    levels[5, 5] = 1
    assert radar.level_at(levels, 5, 5) == 2
    assert radar.level_at(levels, 5.7, 5.2) == 2
    assert radar.level_at(levels, 2, 5) == 0
    levels[0, 0] = 5
    assert radar.level_at(levels, 0, 0) == 5
    assert radar.level_at(levels, 1, 1) == 5
    assert radar.level_at(levels, 9, 9) == 0
    assert radar.level_at(levels, 40, 40) == 0


@pytest.mark.parametrize(
    "level,series,text",
    [
        (0, [0] * 12, "この先1時間 雨の心配なし"),
        (0, [0] * 5 + [1] * 7, "30分後に弱い雨"),
        (0, [0, 0, 3] + [0] * 9, "15分後に強い雨"),
        (0, [6] + [0] * 11, "5分後に猛烈な雨"),
        (2, [2, 2, 1] + [0] * 9, "雨が降っています（やや強い雨）・20分後にやむ見込み"),
        (1, [1] * 12, "雨が降っています（弱い雨）"),
        (1, [1, 0, 1] + [0] * 9, "雨が降っています（弱い雨）・20分後にやむ見込み"),
        (1, [0] * 12, "雨が降っています（弱い雨）・5分後にやむ見込み"),
        (5, [5] * 11 + [0], "雨が降っています（非常に激しい雨）・60分後にやむ見込み"),
    ],
)
def test_summary_rules(level, series, text):
    assert radar.summary(level, series) == text


def test_rain_classes_follow_jma_wording():
    assert radar.CLASSES == ["", "弱い雨", "やや強い雨", "強い雨", "激しい雨", "非常に激しい雨", "猛烈な雨"]


def test_overlay_uses_jma_colours_and_alpha_210():
    index = np.zeros((100, 100), np.uint8)
    index[:, 50:] = 5  # (250,245,0)
    img = radar.overlay_image(index, (0, 0, 100, 100), size=(512, 384))
    assert img.size == (512, 384) and img.mode == "RGBA"
    a = np.asarray(img)
    assert set(np.unique(a[..., 3])) == {0, radar.OVERLAY_ALPHA}
    assert tuple(a[10, 500]) == (250, 245, 0, 210)
    assert tuple(a[10, 10]) == (0, 0, 0, 0)


def test_soft_overlay_feathers_edges_without_darkening():
    index = np.zeros((20, 20), np.uint8)
    index[:, 10:] = 3  # (33,140,255)
    a = np.asarray(radar.overlay_image(index, (0, 0, 20, 20), size=(40, 40), blur=True))
    assert a[..., 3].max() == radar.OVERLAY_ALPHA
    assert 0 < a[20, 19, 3] < radar.OVERLAY_ALPHA
    assert a[20, 0, 3] == 0
    assert tuple(a[20, 19, :3]) == (33, 140, 255)


def fake_index(wet_tile, value=7):
    def get(frame, z, x, y):
        out = np.zeros((256, 256), np.uint8)
        if (z, x, y) == wet_tile:
            out[:] = value
        return out

    return get


def test_render_frame_places_rain_where_the_tile_is():
    office = (139.77, 35.68)
    ext = geo.extent_around(*office, 40.0)
    px, py = geo.world_px(*office, 10)
    tile = (10, int(px // 256), int(py // 256))
    frame = frames()[0][-1]
    img = radar.render_frame(fake_index(tile), ext, 10, frame)
    a = np.asarray(img)
    assert img.size == (512, 384)
    assert tuple(a[192, 256]) == (255, 40, 0, 210)


def test_office_level_samples_the_office_pixel():
    office = (139.77, 35.68)
    px, py = geo.world_px(*office, 10)
    tile = (10, int(px // 256), int(py // 256))
    frame = frames()[0][-1]
    assert radar.office_level(fake_index(tile, 8), frame, *office, 10) == 6
    assert radar.office_level(fake_index((10, 0, 0), 8), frame, *office, 10) == 0


def test_hrpns_tiles_only_carry_data_at_even_zooms():
    # odd zooms answer 200 with an empty transparent tile, so they are refused
    assert radar.DATA_ZOOMS == (4, 6, 8, 10)
    src = radar.TileSource(lambda url: jma_tile(None))
    frame = frames()[0][-1]
    with pytest.raises(ValueError):
        src.index(frame, 5, 27, 12)
    assert src.index(frame, 6, 54, 25).shape == (256, 256)


def test_tile_source_keeps_observed_tiles_for_the_shifting_grid():
    calls = []

    def fetch(url):
        calls.append(url)
        return jma_tile(None)

    src = radar.TileSource(fetch)
    past, future = frames()
    newest = past[-1]
    old = past[-13]  # 60 minutes before the newest observation
    src.begin()
    src.index(old, 6, 54, 25)
    src.index(future[0], 6, 54, 25)
    src.end(newest.at)
    # not used this time: the observation inside the window stays, the forecast goes
    src.begin()
    src.end(newest.at)
    src.begin()
    src.index(old, 6, 54, 25)
    assert len(calls) == 2
    src.index(future[0], 6, 54, 25)
    assert len(calls) == 3
    src.end(newest.at)
    # once it falls out of the 70 minute window an unused observation goes too
    src.begin()
    src.end(newest.at + timedelta(minutes=15))
    src.begin()
    src.index(old, 6, 54, 25)
    assert len(calls) == 4


def test_frames_wait_for_the_forecast_run_when_n2_lags_n1():
    # N1 publishes 09:05 before N2 has the 09:05 run: stay on the 09:00 run
    past = radar.parse_target_times(n1_entries(), forecast=False)
    future = radar.parse_target_times(n2_entries(NOW - timedelta(minutes=5)), forecast=True)
    picked, now_index = radar.pick_frames(past, future)
    assert len(picked) == 13 and now_index == 6
    assert picked[now_index].at == NOW - timedelta(minutes=5)
    series = radar.series_frames(past, future)
    assert all(f is not None for f in series)
    assert series[0].at == NOW


def test_fill_series_carries_the_last_known_level():
    assert radar.fill_series([None, 1, None, 0, None], start=2) == [2, 1, 1, 0, 0]
    assert radar.fill_series([None] * 3, start=0) == [0, 0, 0]
    assert radar.fill_series([3] * 12, start=0) == [3] * 12

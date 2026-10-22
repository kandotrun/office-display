import http.client
import importlib
import io
import json
import threading
from datetime import timedelta

import numpy as np
import pytest
from PIL import Image

import atlas_server as srv
import places
import radar
from fakes import NOW, FakeNet, n1_entries, n2_entries, photo_jpeg

T = NOW.timestamp() + 60
HIROSHIMA = (34.408, 132.4547)
CREDITS = ["© OpenStreetMap contributors", "気象庁", "NASA GIBS"]


class Clock:
    def __init__(self, t):
        self.t = t

    def __call__(self):
        return self.t


def photos():
    return {
        places.encode_name("京都 旅行.jpg"): photo_jpeg(35.012, 135.761, taken="2025:08:14 10:22:33"),
        "paris.jpg": photo_jpeg(48.8566, 2.3522),
        "plain.jpg": photo_jpeg(),
    }


def test_status_types_match_the_rust_client(tmp_path):
    atlas, _ = make_atlas(tmp_path)
    atlas.refresh_maps()
    atlas.refresh_radar()
    atlas.refresh_weather()
    atlas.refresh_globe()
    status = atlas.status()
    assert len(json.dumps(status, ensure_ascii=False).encode()) < 64 * 1024
    for name in ("japan", "local"):
        view = status["views"][name]
        assert isinstance(view["rev"], int) and isinstance(view["now_index"], int)
        assert 0 <= view["now_index"] < len(view["frames"]) <= 13
        assert all(0.0 <= c <= 1.0 for c in view["office"])
    rain = status["rain"]
    assert len(rain["series"]) == 12 and 0 <= rain["level"] <= 6
    assert all(isinstance(v, int) and 0 <= v <= 6 for v in rain["series"])


def make_atlas(tmp_path, fake=None, clock=None):
    fake = fake or FakeNet(photos=photos())
    atlas = srv.Atlas(
        lat=HIROSHIMA[0],
        lon=HIROSHIMA[1],
        cache_dir=str(tmp_path),
        forecast_office="340000",
        forecast_area="340010",
        temp_point="67437",
        fetch=fake,
        fetch_local=fake,
        osm_spacing=0,
        geocode_interval=0,
        clock=clock or Clock(T),
    )
    return atlas, fake


def test_defaults_are_the_hiroshima_office(monkeypatch):
    for key in ("ATLAS_LAT", "ATLAS_LON", "ATLAS_FORECAST_OFFICE", "ATLAS_FORECAST_AREA", "ATLAS_TEMP_POINT"):
        monkeypatch.delenv(key, raising=False)
    try:
        mod = importlib.reload(srv)
        assert (mod.LAT, mod.LON) == HIROSHIMA
        assert (mod.FORECAST_OFFICE, mod.FORECAST_AREA, mod.TEMP_POINT) == ("340000", "340010", "67437")
    finally:
        importlib.reload(srv)


def test_status_before_anything_is_ready(tmp_path):
    atlas, fake = make_atlas(tmp_path)
    assert atlas.status() == {
        "generated_at": "2026-10-22T09:06:00Z",
        "office": {"lat": 34.408, "lon": 132.4547},
        "weather": None,
        "rain": None,
        "views": {"globe": None, "japan": None, "local": None},
        "credits": CREDITS,
    }
    assert fake.calls == []


def snapshot(radar_at=T, weather_at=T):
    past = radar.parse_target_times(n1_entries(), forecast=False)
    future = radar.parse_target_times(n2_entries(), forecast=True)
    frames, now_index = radar.pick_frames(past, future)
    return {
        "office": (35.68, 139.77),
        "weather": {"mood": "cloudy", "label": "くもり", "max": 21, "min": 14},
        "weather_at": weather_at,
        "rain": {"summary": "30分後に弱い雨", "level": 0, "series": [0] * 5 + [1] * 7},
        "radar_at": radar_at,
        "globe": {"rev": 3},
        "views": {
            "japan": {"rev": 12, "frames": frames, "now_index": now_index, "office": (0.6123456, 0.5987654)},
            "local": None,
        },
    }


def test_build_status_shape():
    status = srv.build_status(snapshot(), T)
    assert status["generated_at"] == "2026-10-22T09:06:00Z"
    assert status["weather"] == {"mood": "cloudy", "label": "くもり", "max": 21, "min": 14}
    assert status["rain"] == {"summary": "30分後に弱い雨", "level": 0, "series": [0] * 5 + [1] * 7}
    assert status["views"]["globe"] == {"rev": 3}
    assert status["views"]["local"] is None
    japan = status["views"]["japan"]
    assert japan["rev"] == 12 and japan["now_index"] == 6 and japan["office"] == [0.6123, 0.5988]
    assert japan["frames"][0] == {"at": "2026-10-22T08:05:00Z", "forecast": False}
    assert japan["frames"][6] == {"at": "2026-10-22T09:05:00Z", "forecast": False}
    assert japan["frames"][12] == {"at": "2026-10-22T10:05:00Z", "forecast": True}
    assert status["credits"] == CREDITS
    json.dumps(status)


def test_stale_radar_drops_rain_and_frames_but_keeps_the_map():
    status = srv.build_status(snapshot(radar_at=T - 31 * 60), T)
    assert status["rain"] is None
    assert status["views"]["japan"]["frames"] == []
    # the Rust client parses now_index as usize; it is only checked with frames
    assert status["views"]["japan"]["now_index"] == 0
    assert status["views"]["japan"]["rev"] == 12


def test_stale_weather_is_dropped():
    assert srv.build_status(snapshot(weather_at=T - 7 * 3600), T)["weather"] is None
    assert srv.build_status(snapshot(weather_at=None), T)["weather"] is None


def test_next_rev_only_moves_when_images_change():
    assert srv.next_rev(4, "a", "a") == 4
    assert srv.next_rev(4, "a", "b") == 5
    assert srv.next_rev(0, None, "b") == 1


def png_size(data):
    img = Image.open(io.BytesIO(data))
    return img.format, img.size, img.mode


def test_maps_then_radar(tmp_path):
    atlas, fake = make_atlas(tmp_path)
    atlas.refresh_maps()
    views = atlas.status()["views"]
    assert views["japan"]["rev"] == 1 and views["local"]["rev"] == 1
    assert views["japan"]["frames"] == [] and views["japan"]["now_index"] == 0
    assert views["local"]["office"] == [0.5, 0.5]
    u, v = views["japan"]["office"]
    assert 0.3 < u < 0.37 and 0.7 < v < 0.8
    data, ctype, rev = atlas.image("/map/japan/base.jpg")
    assert ctype == "image/jpeg" and rev == 1
    assert png_size(data)[:2] == ("JPEG", (1024, 768))
    osm_calls = fake.count("tile.openstreetmap.org")
    assert 20 <= osm_calls <= 70

    atlas.refresh_radar()
    status = atlas.status()
    assert status["rain"] == {"summary": "30分後に弱い雨", "level": 0, "series": [0] * 5 + [1] * 7}
    for name in ("japan", "local"):
        view = status["views"][name]
        assert len(view["frames"]) == 13 and view["now_index"] == 6
        assert view["rev"] == 2
        assert view["frames"][6] == {"at": "2026-10-22T09:05:00Z", "forecast": False}
        data, ctype, _ = atlas.image("/map/%s/radar/0.png" % name)
        assert ctype == "image/png" and png_size(data) == ("PNG", (512, 384), "RGBA")
        dry = np.asarray(Image.open(io.BytesIO(atlas.image("/map/%s/radar/6.png" % name)[0])))
        wet = np.asarray(Image.open(io.BytesIO(atlas.image("/map/%s/radar/12.png" % name)[0])))
        assert dry[..., 3].max() == 0
        assert wet[..., 3].max() == radar.OVERLAY_ALPHA
    assert atlas.image("/map/japan/radar/13.png") is None

    tile_calls = fake.count("/jmatile/data/nowc/2026")
    atlas.refresh_radar()
    assert fake.count("/jmatile/data/nowc/2026") == tile_calls
    assert atlas.status()["views"]["japan"]["rev"] == 2

    atlas.refresh_maps()
    assert fake.count("tile.openstreetmap.org") == osm_calls
    assert atlas.status()["views"]["japan"]["rev"] == 2


def test_configured_radar_zooms_carry_data():
    for z in (srv.JAPAN_RADAR_ZOOM, srv.LOCAL_RADAR_ZOOM, srv.OFFICE_RADAR_ZOOM):
        assert z in radar.DATA_ZOOMS


def test_steady_state_radar_fetches_only_new_frames(tmp_path):
    atlas, fake = make_atlas(tmp_path)
    atlas.refresh_maps()
    for step in (0, 5, 10):
        fake.now = NOW + timedelta(minutes=step)
        before = len(fake.calls)
        atlas.refresh_radar()
    newest = (NOW + timedelta(minutes=10)).strftime("%Y%m%d%H%M%S")
    observed = [c for c in fake.calls[before:] if "/nowc/%s/none/%s/" % (newest, newest) not in c
                and "/none/" in c and c.split("/nowc/")[1][:14] == c.split("/none/")[1][:14]]
    assert observed == []
    assert atlas.status()["views"]["japan"]["frames"][6]["at"] == "2026-10-22T09:15:00Z"


def test_radar_failure_keeps_the_previous_state(tmp_path):
    atlas, fake = make_atlas(tmp_path)
    atlas.refresh_radar()
    before = atlas.status()["rain"]
    fake.fail.append("targetTimes")
    with pytest.raises(Exception):
        atlas.refresh_radar()
    assert atlas.status()["rain"] == before


def test_raining_now_at_the_office(tmp_path):
    fake = FakeNet(rain_from=-200, rain_color=(0, 65, 255))
    atlas, _ = make_atlas(tmp_path, fake=fake)
    atlas.refresh_radar()
    rain = atlas.status()["rain"]
    assert rain == {"summary": "雨が降っています（やや強い雨）", "level": 2, "series": [2] * 12}


def test_weather_refresh(tmp_path):
    atlas, fake = make_atlas(tmp_path)
    atlas.refresh_weather()
    assert atlas.status()["weather"] == {"mood": "cloudy", "label": "くもり 夜 雨", "max": 22, "min": 15}
    assert fake.calls == ["https://www.jma.go.jp/bosai/forecast/data/forecast/340000.json"]


def test_globe_refresh(tmp_path):
    atlas, fake = make_atlas(tmp_path)
    atlas.refresh_globe()
    assert atlas.status()["views"]["globe"] == {"rev": 1}
    data, ctype, rev = atlas.image("/globe/day.jpg")
    assert ctype == "image/jpeg" and rev == 1 and png_size(data)[1] == (2048, 1024)
    assert png_size(atlas.image("/globe/night.jpg")[0])[1] == (2048, 1024)
    atlas.refresh_globe()
    assert atlas.status()["views"]["globe"] == {"rev": 1}


@pytest.fixture
def served(tmp_path):
    atlas, fake = make_atlas(tmp_path)
    server = srv.ThreadingHTTPServer(("127.0.0.1", 0), srv.make_handler(atlas))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield atlas, fake, server.server_address[1]
    server.shutdown()
    server.server_close()


def get(port, path):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    conn.request("GET", path)
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, resp, body


def test_http_status_answers_before_anything_is_ready(served):
    _, _, port = served
    code, resp, body = get(port, "/status")
    assert code == 200 and resp.getheader("Content-Type").startswith("application/json")
    status = json.loads(body)
    assert status["views"] == {"globe": None, "japan": None, "local": None}
    assert get(port, "/status?x=1")[0] == 200


def test_http_images(served):
    atlas, _, port = served
    assert get(port, "/map/japan/base.jpg")[0] == 404
    atlas.refresh_maps()
    atlas.refresh_radar()
    code, resp, body = get(port, "/map/local/base.jpg")
    assert code == 200 and resp.getheader("Content-Type") == "image/jpeg"
    assert resp.getheader("X-Atlas-Rev") == "2"
    assert png_size(body)[:2] == ("JPEG", (1024, 768))
    code, resp, body = get(port, "/map/japan/radar/12.png")
    assert code == 200 and resp.getheader("Content-Type") == "image/png"
    for path in ("/map/japan/radar/13.png", "/map/japan/radar/-1.png", "/map/japan/radar/x.png",
                 "/map/world/base.jpg", "/nope", "/globe/day.jpg"):
        assert get(port, path)[0] == 404, path


def test_http_photo_place(served):
    _, fake, port = served
    code, resp, body = get(port, "/photo/" + places.encode_name("京都 旅行.jpg"))
    assert code == 200
    assert json.loads(body) == {"place": "京都市", "lat": 35.0, "lon": 135.75, "taken": "2025-08", "near": True}
    assert "lat=35.00&lon=135.75" in [c for c in fake.calls if "nominatim" in c][0]

    code, _, body = get(port, "/photo/plain.jpg")
    assert code == 404 and json.loads(body) == {}
    code, _, body = get(port, "/photo/missing.jpg")
    assert code == 404 and json.loads(body) == {}
    fake.fail.append("nominatim")
    code, _, body = get(port, "/photo/paris.jpg")
    assert code == 503 and json.loads(body) == {}
    fake.fail.clear()
    code, _, body = get(port, "/photo/paris.jpg")
    assert code == 200 and json.loads(body)["place"] == "パリ"
    for bad in ("a%2Fb.jpg", "%2E%2E.jpg", "a%00.jpg", "%FF.jpg", "movie.mp4"):
        code, _, body = get(port, "/photo/" + bad)
        assert code == 400 and json.loads(body) == {}, bad


def test_http_photo_map(served):
    _, fake, port = served
    code, resp, body = get(port, "/photo/" + places.encode_name("京都 旅行.jpg") + "/map.jpg")
    assert code == 200 and resp.getheader("Content-Type") == "image/jpeg"
    assert png_size(body)[:2] == ("JPEG", (1024, 768))
    assert all("/11/" in c for c in fake.calls if "tile.openstreetmap.org" in c)
    calls = len(fake.calls)
    assert get(port, "/photo/" + places.encode_name("京都 旅行.jpg") + "/map.jpg")[0] == 200
    assert len(fake.calls) == calls
    assert get(port, "/photo/paris.jpg/map.jpg")[0] == 404
    assert get(port, "/photo/plain.jpg/map.jpg")[0] == 404
    assert get(port, "/photo/a/b/map.jpg")[0] == 404

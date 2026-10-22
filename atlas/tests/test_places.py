import json

import pytest

import net
import places
from fakes import FakeNet, photo_jpeg


def test_gps_of_reads_north_east():
    lat, lon = places.gps_of(photo_jpeg(35.0, 135.75))
    assert lat == pytest.approx(35.0, abs=1e-4)
    assert lon == pytest.approx(135.75, abs=1e-4)


def test_gps_of_reads_south_west():
    lat, lon = places.gps_of(photo_jpeg(-33.8688, -70.6693))
    assert lat == pytest.approx(-33.8688, abs=1e-4)
    assert lon == pytest.approx(-70.6693, abs=1e-4)


def test_gps_of_without_gps_or_garbage():
    assert places.gps_of(photo_jpeg()) is None
    assert places.gps_of(photo_jpeg(taken="2025:08:14 10:22:33")) is None
    assert places.gps_of(b"not an image") is None
    assert places.gps_of(b"") is None


def test_gps_of_accepts_plain_rational_tuples():
    assert places.dms_to_deg(((35, 1), (30, 1), (0, 1))) == pytest.approx(35.5)
    assert places.dms_to_deg((35.0, 30.0, 36.0)) == pytest.approx(35.51)
    assert places.dms_to_deg(((35, 0), (0, 1), (0, 1))) is None
    assert places.dms_to_deg((35.0,)) is None


def test_null_island_is_no_gps():
    assert places.gps_of(photo_jpeg(0.0, 0.0)) is None


@pytest.mark.parametrize(
    "value,rounded",
    [(35.0, 35.0), (35.012, 35.0), (35.026, 35.05), (135.774, 135.75), (135.776, 135.8), (-0.01, 0.0), (-33.87, -33.85)],
)
def test_round_coord(value, rounded):
    assert places.round_coord(value) == rounded
    assert str(places.round_coord(value)) != "-0.0"


def test_place_coords_are_rounded():
    assert places.place_coords(photo_jpeg(35.0123, 135.7734)) == (35.0, 135.75)
    assert places.place_coords(photo_jpeg()) is None


def test_taken_of():
    assert places.taken_of(photo_jpeg(35.0, 135.75, taken="2025:08:14 10:22:33")) == "2025-08"
    assert places.taken_of(photo_jpeg(35.0, 135.75)) is None
    assert places.taken_of(photo_jpeg(taken="0000:00:00 00:00:00")) is None
    assert places.taken_of(photo_jpeg(taken="garbage")) is None
    assert places.taken_of(b"nope") is None


@pytest.mark.parametrize(
    "address,name",
    [
        ({"city": "京都市", "province": "京都府", "country": "日本"}, "京都市"),
        ({"city": "渋谷区", "country": "日本"}, "渋谷区"),
        ({"city": "パリ", "suburb": "パリ", "country": "フランス"}, "パリ"),
        ({"town": "箱根町", "county": "足柄下郡", "province": "神奈川県"}, "箱根町"),
        ({"village": "白川村", "country": "日本"}, "白川村"),
        ({"county": "某郡", "state": "某州"}, "某郡"),
        ({"state": "北海道", "country": "日本"}, "北海道"),
        ({"country": "日本"}, "日本"),
        ({"city": "", "country": "日本"}, "日本"),
        ({}, None),
        (None, None),
    ],
)
def test_place_name(address, name):
    assert places.place_name(address) == name


@pytest.mark.parametrize("name", ["IMG_0001.jpg", "京都 旅行.JPG", "a-b_c.~x.jpeg", "scan.png"])
def test_valid_names(name):
    assert places.validate_name(name)


@pytest.mark.parametrize(
    "name",
    ["", "a/b.jpg", "..jpg", "a..b.jpg", ".hidden.jpg", "a\x00.jpg", "a\n.jpg", "a\x7f.jpg",
     "a\\b.jpg", "a%b.jpg", "a?.jpg", "a#.jpg", "a:b.jpg", "x" * 252 + ".jpg", "movie.mp4", "noext", None],
)
def test_invalid_names(name):
    assert not places.validate_name(name)


def test_encode_name_matches_the_rust_client():
    assert places.encode_name("京都 旅行.jpg") == "%E4%BA%AC%E9%83%BD%20%E6%97%85%E8%A1%8C.jpg"
    assert places.encode_name("a-b_c.~x.JPG") == "a-b_c.~x.JPG"


def test_decode_segment():
    assert places.decode_segment("%E4%BA%AC.jpg") == "京.jpg"
    assert places.decode_segment("a%20b.jpg") == "a b.jpg"
    assert places.decode_segment("%FF.jpg") is None


def test_lru_evicts_the_least_recently_used():
    lru = places.LRU(2)
    lru.put("a", 1)
    lru.put("b", None)
    assert lru.lookup("a") == (True, 1)
    lru.put("c", 3)
    assert lru.lookup("b") == (False, None)
    assert lru.lookup("a") == (True, 1)
    assert lru.lookup("c") == (True, 3)
    assert len(lru) == 2


def test_geocoder_caches_on_disk_by_rounded_coords_only(tmp_path):
    fake = FakeNet()
    path = tmp_path / "places.json"
    g = places.Geocoder(path, fake, interval=0)
    assert g.name(35.0, 135.75) == "京都市"
    assert g.name(35.0, 135.75) == "京都市"
    assert fake.count("nominatim") == 1
    url = fake.calls[0]
    assert "lat=35.00&lon=135.75" in url and "zoom=10" in url and "accept-language=ja" in url
    assert "format=jsonv2" in url
    stored = json.loads(path.read_text())
    assert stored == {"35.00,135.75": "京都市"}

    again = places.Geocoder(path, fake, interval=0)
    assert again.name(35.0, 135.75) == "京都市"
    assert fake.count("nominatim") == 1


def test_geocoder_remembers_places_without_a_name(tmp_path):
    fake = FakeNet()
    g = places.Geocoder(tmp_path / "places.json", fake, interval=0)
    assert g.name(10.0, 150.0) is None
    assert g.name(10.0, 150.0) is None
    assert fake.count("nominatim") == 1


def test_geocoder_waits_one_second_between_requests(tmp_path):
    now = [0.0]
    slept = []

    def sleep(s):
        slept.append(s)
        now[0] += s

    g = places.Geocoder(tmp_path / "p.json", FakeNet(), interval=1.0, clock=lambda: now[0], sleep=sleep)
    g.name(35.0, 135.75)
    g.name(48.85, 2.35)
    g.name(35.0, 135.75)
    assert slept == [pytest.approx(1.0)]


def test_geocoder_ignores_a_corrupt_cache_file(tmp_path):
    path = tmp_path / "places.json"
    path.write_text("{broken")
    g = places.Geocoder(path, FakeNet(), interval=0)
    assert g.name(35.0, 135.75) == "京都市"


OFFICE = (34.408, 132.4547)  # Hiroshima


def photo_service(fake):
    def fetch_photo(name):
        return fake("http://127.0.0.1:8081/photos/" + places.encode_name(name))

    return fetch_photo


def test_photo_places_lookup(tmp_path):
    fake = FakeNet(photos={
        "kyoto.jpg": photo_jpeg(35.012, 135.761, taken="2025:08:14 10:22:33"),
        "paris.jpg": photo_jpeg(48.8566, 2.3522),
        "plain.jpg": photo_jpeg(),
    })
    pp = places.PhotoPlaces(photo_service(fake), places.Geocoder(tmp_path / "p.json", fake, interval=0), OFFICE)
    assert pp.lookup("kyoto.jpg") == {"place": "京都市", "lat": 35.0, "lon": 135.75, "taken": "2025-08", "near": True}
    assert pp.lookup("paris.jpg") == {"place": "パリ", "lat": 48.85, "lon": 2.35, "taken": None, "near": False}
    assert pp.lookup("plain.jpg") is None
    calls = len(fake.calls)
    pp.lookup("kyoto.jpg")
    pp.lookup("plain.jpg")
    assert len(fake.calls) == calls


def test_photo_places_never_stores_names_on_disk(tmp_path):
    fake = FakeNet(photos={"secret-trip.jpg": photo_jpeg(35.0, 135.75)})
    pp = places.PhotoPlaces(photo_service(fake), places.Geocoder(tmp_path / "p.json", fake, interval=0), OFFICE)
    pp.lookup("secret-trip.jpg")
    for f in tmp_path.rglob("*"):
        if f.is_file():
            assert b"secret" not in f.read_bytes()


def test_photo_places_geocode_failure_raises_and_is_not_memoised(tmp_path):
    fake = FakeNet(photos={"kyoto.jpg": photo_jpeg(35.0, 135.75)})
    fake.fail.append("nominatim")
    pp = places.PhotoPlaces(photo_service(fake), places.Geocoder(tmp_path / "p.json", fake, interval=0), OFFICE)
    with pytest.raises(places.GeocodeError):
        pp.lookup("kyoto.jpg")
    fake.fail.clear()
    assert pp.lookup("kyoto.jpg")["place"] == "京都市"


def test_photo_places_without_a_name_are_no_place(tmp_path):
    # the client needs a non-empty name; open sea has none
    fake = FakeNet(photos={"sea.jpg": photo_jpeg(10.0, 150.0)})
    pp = places.PhotoPlaces(photo_service(fake), places.Geocoder(tmp_path / "p.json", fake, interval=0), OFFICE)
    assert pp.lookup("sea.jpg") is None
    calls = len(fake.calls)
    assert pp.lookup("sea.jpg") is None
    assert len(fake.calls) == calls


def test_photo_places_unknown_photo_raises_404(tmp_path):
    fake = FakeNet()
    pp = places.PhotoPlaces(photo_service(fake), places.Geocoder(tmp_path / "p.json", fake, interval=0), OFFICE)
    with pytest.raises(net.FetchError) as info:
        pp.lookup("missing.jpg")
    assert info.value.status == 404

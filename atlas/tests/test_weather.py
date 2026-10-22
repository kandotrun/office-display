import pytest

import weather
from fakes import forecast


def test_parse_forecast_reads_label_mood_and_temps():
    assert weather.parse_forecast(forecast(), "130010", "44132") == {
        "mood": "cloudy",
        "label": "くもり 夜 雨",
        "max": 21,
        "min": 14,
    }


def test_parse_forecast_other_area():
    w = weather.parse_forecast(forecast(), "130020", "44172")
    assert w == {"mood": "rain", "label": "雨 のち くもり", "max": 22, "min": 16}


def test_four_temps_use_the_last_pair():
    w = weather.parse_forecast(forecast(temps=("21", "21", "14", "22")), "130010", "44132")
    assert (w["min"], w["max"]) == (14, 22)


def test_missing_or_blank_temps_are_null():
    w = weather.parse_forecast(forecast(), "130010", "99999")
    assert w["label"] == "くもり 夜 雨" and w["max"] is None and w["min"] is None
    w = weather.parse_forecast(forecast(temps=("", "21")), "130010", "44132")
    assert (w["min"], w["max"]) == (None, 21)
    w = weather.parse_forecast(forecast(temps=()), "130010", "44132")
    assert (w["min"], w["max"]) == (None, None)


def test_single_temp_uses_its_time_define():
    assert weather.pick_temps(["21"], ["2026-10-22T09:00:00+09:00"]) == (None, 21)
    assert weather.pick_temps(["14"], ["2026-10-23T00:00:00+09:00"]) == (14, None)


@pytest.mark.parametrize("data", [None, [], [{}], [{"timeSeries": []}], {"a": 1}, "x"])
def test_malformed_forecasts_are_none(data):
    assert weather.parse_forecast(data, "130010", "44132") is None


def test_unknown_area_is_none():
    assert weather.parse_forecast(forecast(), "999999", "44132") is None


@pytest.mark.parametrize(
    "code,mood",
    [("100", "clear"), ("101", "clear"), ("200", "cloudy"), ("313", "rain"), ("400", "snow"), ("", "cloudy"), (None, "cloudy"), ("999", "cloudy")],
)
def test_mood_of(code, mood):
    assert weather.mood_of(code) == mood


def test_normalise_label():
    assert weather.normalise_label("晴れ　朝晩　くもり") == "晴れ 朝晩 くもり"
    assert weather.normalise_label("  くもり　　時々  雨 ") == "くもり 時々 雨"
    assert weather.normalise_label("くもり") == "くもり"

from datetime import date

import pytest

import weather
from fakes import forecast

TODAY = date(2026, 10, 22)  # the report day of the fixtures
TOMORROW = date(2026, 10, 23)


def test_parse_forecast_reads_label_mood_and_temps():
    # 11:00: today's minimum is over, its slot only repeats the maximum
    assert weather.parse_forecast(forecast(), "130010", "44132", TODAY) == {
        "mood": "cloudy",
        "label": "くもり 夜 雨",
        "max": 21,
        "min": None,
    }


def test_parse_forecast_other_area():
    w = weather.parse_forecast(forecast(), "130020", "44172", TODAY)
    assert w == {"mood": "rain", "label": "雨 のち くもり", "max": 22, "min": None}


@pytest.mark.parametrize("report", ["05:00", "11:00"])
def test_day_reports_give_todays_maximum_not_tomorrows(report):
    w = weather.parse_forecast(forecast(report=report, temps=("21", "21", "14", "25")), "130010", "44132", TODAY)
    assert (w["min"], w["max"]) == (None, 21)
    assert w["label"] == "くもり 夜 雨"


def test_the_evening_report_has_nothing_for_today():
    # 17:00 lists tomorrow only; tomorrow's values never stand in for today's
    w = weather.parse_forecast(forecast(report="17:00", temps=("14", "25")), "130010", "44132", TODAY)
    assert (w["min"], w["max"]) == (None, None)
    assert w["label"] == "くもり 夜 雨"


def test_after_midnight_the_evening_report_is_todays_forecast():
    # yesterday's 17:00 report read at 01:00: its "tomorrow" is today, a real
    # minimum and maximum, and the label follows the same day
    w = weather.parse_forecast(forecast(report="17:00", temps=("14", "25")), "130010", "44132", TOMORROW)
    assert w == {"mood": "clear", "label": "晴れ 時々 くもり", "max": 25, "min": 14}


def test_a_day_report_read_the_next_day_gives_that_days_pair():
    w = weather.parse_forecast(forecast(report="11:00", temps=("21", "21", "14", "25")), "130010", "44132", TOMORROW)
    assert (w["min"], w["max"]) == (14, 25)


def test_missing_or_blank_temps_are_null():
    w = weather.parse_forecast(forecast(), "130010", "99999", TODAY)
    assert w["label"] == "くもり 夜 雨" and w["max"] is None and w["min"] is None
    w = weather.parse_forecast(forecast(temps=("", "", "14", "22")), "130010", "44132", TODAY)
    assert (w["min"], w["max"]) == (None, None)
    w = weather.parse_forecast(forecast(temps=()), "130010", "44132", TODAY)
    assert (w["min"], w["max"]) == (None, None)


def test_pick_temps_goes_by_the_jst_date_of_each_define():
    assert weather.pick_temps(["21"], ["2026-10-22T09:00:00+09:00"], TODAY) == (None, 21)
    # 00:00 UTC is 09:00 JST
    assert weather.pick_temps(["21"], ["2026-10-22T00:00:00+00:00"], TODAY) == (None, 21)
    assert weather.pick_temps(["21"], ["2026-10-23T09:00:00+09:00"], TODAY) == (None, None)
    # a minimum counts only from a report issued before that day
    assert weather.pick_temps(["14"], ["2026-10-23T00:00:00+09:00"], TOMORROW, issued=TODAY) == (14, None)
    assert weather.pick_temps(["14"], ["2026-10-23T00:00:00+09:00"], TOMORROW, issued=TOMORROW) == (None, None)
    assert weather.pick_temps(["14"], ["2026-10-23T00:00:00+09:00"], TOMORROW) == (None, None)
    assert weather.pick_temps(["14", "x"], ["nonsense", None], TODAY) == (None, None)
    assert weather.pick_temps(None, None, TODAY) == (None, None)


def test_jst_date_of_an_epoch():
    # 2026-10-22 15:30 UTC is already the 23rd in Japan
    assert weather.jst_date(1792683000) == TOMORROW
    assert weather.jst_date(1792683000 - 3600) == TODAY


@pytest.mark.parametrize("data", [None, [], [{}], [{"timeSeries": []}], {"a": 1}, "x"])
def test_malformed_forecasts_are_none(data):
    assert weather.parse_forecast(data, "130010", "44132", TODAY) is None


def test_unknown_area_is_none():
    assert weather.parse_forecast(forecast(), "999999", "44132", TODAY) is None


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

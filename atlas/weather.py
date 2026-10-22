"""JMA forecast (bosai/forecast/data/forecast/<office>.json): label, mood, temps.

Everything is picked for one JST calendar day ("today" of the caller), by the
dates in timeDefines, never by position: the temperature series of the 05:00
and 11:00 reports is [today 09:00, today 00:00, tomorrow 00:00, tomorrow
09:00], that of the 17:00 report only [tomorrow 00:00, tomorrow 09:00].
"""
from datetime import datetime, timedelta, timezone

JST = timezone(timedelta(hours=9))
FORECAST_URL = "https://www.jma.go.jp/bosai/forecast/data/forecast/{office}.json"
MOODS = {"1": "clear", "2": "cloudy", "3": "rain", "4": "snow"}


def mood_of(code):
    """Weather code first digit: 1 clear, 2 cloudy, 3 rain, 4 snow."""
    return MOODS.get(str(code or "")[:1], "cloudy")


def normalise_label(text):
    """Full-width spaces become single ASCII spaces."""
    return " ".join(str(text).replace("　", " ").split())


def _temp(value):
    try:
        return int(round(float(str(value).strip())))
    except (TypeError, ValueError):
        return None


def jst_date(epoch):
    """The JST calendar day of an epoch timestamp."""
    return datetime.fromtimestamp(epoch, JST).date()


def _jst(stamp):
    """A timeDefines / reportDatetime stamp as an aware JST datetime, or None."""
    try:
        t = datetime.fromisoformat(str(stamp))
    except ValueError:
        return None
    return t.astimezone(JST) if t.tzinfo is not None else None


def pick_temps(temps, defines, today, issued=None):
    """Today's (min, max): the entries dated `today` (JST) at 00:00 and 09:00.

    The 09:00 entry is the daytime maximum. A report issued on `today` itself
    (05:00, 11:00) has no minimum for it: the morning is over and the 00:00
    slot only repeats the maximum. So the 00:00 entry counts only when the
    report was issued before `today` (yesterday's 17:00 report after
    midnight). Anything for another day is ignored, never a stand-in.
    """
    tmin = tmax = None
    for value, define in zip(temps or (), defines or ()):
        t = _jst(define)
        if t is None or t.date() != today or t.minute or t.second:
            continue
        if t.hour == 9:
            tmax = _temp(value)
        elif t.hour == 0 and issued is not None and issued < today:
            tmin = _temp(value)
    return tmin, tmax


def _day_index(defines, today):
    """Index of the first timeDefine on `today` (JST), else 0."""
    for i, define in enumerate(defines or ()):
        t = _jst(define)
        if t is not None and t.date() == today:
            return i
    return 0


def _area(areas, code):
    for area in areas:
        if isinstance(area, dict) and (area.get("area") or {}).get("code") == code:
            return area
    return None


def parse_forecast(data, area_code, temp_point, today):
    """{"mood", "label", "max", "min"} for an area on `today` (a JST date), or
    None when the area is not there."""
    try:
        report = data[0]
        series = report["timeSeries"]
        area = _area(series[0]["areas"], area_code)
        i = _day_index(series[0].get("timeDefines"), today)
        code = area["weatherCodes"][i]
        label = normalise_label(area["weathers"][i])
    except (LookupError, TypeError, AttributeError):
        return None
    issued = _jst(report.get("reportDatetime"))
    tmin = tmax = None
    try:
        temps = series[2]
        point = _area(temps["areas"], temp_point)
        if point is not None:
            tmin, tmax = pick_temps(
                point.get("temps"), temps.get("timeDefines"), today, issued.date() if issued else None
            )
    except (LookupError, TypeError, AttributeError):
        pass
    return {"mood": mood_of(code), "label": label, "max": tmax, "min": tmin}

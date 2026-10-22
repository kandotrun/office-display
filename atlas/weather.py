"""JMA forecast (bosai/forecast/data/forecast/<office>.json): label, mood, temps."""

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


def pick_temps(temps, defines=()):
    """(min, max) from a temps list: 00:00 entries are minima, 09:00 maxima.

    With 2 or 4 entries this is the last pair; a lone entry uses its time.
    """
    temps = list(temps or [])
    defines = list(defines or [])
    if len(temps) >= 2:
        return _temp(temps[-2]), _temp(temps[-1])
    if len(temps) == 1:
        hour = str(defines[0])[11:13] if len(defines) == 1 else ""
        return (None, _temp(temps[0])) if hour == "09" else (_temp(temps[0]), None)
    return None, None


def _area(areas, code):
    for area in areas:
        if isinstance(area, dict) and (area.get("area") or {}).get("code") == code:
            return area
    return None


def parse_forecast(data, area_code, temp_point):
    """{"mood", "label", "max", "min"} for an area, or None when it is not there."""
    try:
        series = data[0]["timeSeries"]
        area = _area(series[0]["areas"], area_code)
        code = area["weatherCodes"][0]
        label = normalise_label(area["weathers"][0])
    except (LookupError, TypeError, AttributeError):
        return None
    tmin = tmax = None
    try:
        temps = series[2]
        point = _area(temps["areas"], temp_point)
        if point is not None:
            tmin, tmax = pick_temps(point.get("temps"), temps.get("timeDefines"))
    except (LookupError, TypeError, AttributeError):
        pass
    return {"mood": mood_of(code), "label": label, "max": tmax, "min": tmin}

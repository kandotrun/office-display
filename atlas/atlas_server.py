#!/usr/bin/env python3
"""atlas: the display's only window to the internet.

Prepares imagery and facts for the info strip of photo-frame-rs and serves
them on loopback:

* base maps (OpenStreetMap, dark neutral style) for Japan and the office area,
* rain radar overlays and the office rain summary (JMA nowcast),
* today's weather (JMA forecast),
* globe day / night textures (NASA GIBS),
* municipality-level places of photos (EXIF GPS + Nominatim).

Everything is fetched by background refreshers; /status answers immediately
with null for anything that is not ready yet.
"""
import hashlib
import json
import os
import threading
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import cache
import geo
import globe
import net
import places
import radar
import tiles
import weather

# Defaults: the office in Hiroshima (public), its JMA forecast area 340010
# (南部) and the AMeDAS point 67437 (広島) for temperatures.
LAT = float(os.environ.get("ATLAS_LAT", "34.4080"))
LON = float(os.environ.get("ATLAS_LON", "132.4547"))
PORT = int(os.environ.get("ATLAS_PORT", "8099"))
PHOTO_PORT = int(os.environ.get("ATLAS_PHOTO_PORT", "8081"))
CACHE_DIR = os.path.expanduser(os.environ.get("ATLAS_CACHE_DIR", "~/.cache/atlas"))
FORECAST_OFFICE = os.environ.get("ATLAS_FORECAST_OFFICE", "340000")
FORECAST_AREA = os.environ.get("ATLAS_FORECAST_AREA", "340010")
TEMP_POINT = os.environ.get("ATLAS_TEMP_POINT", "67437")

CREDITS = ["© OpenStreetMap contributors", "気象庁", "NASA GIBS"]

# (interval on success, retry after a failure) in seconds
RADAR_EVERY = (300, 60)
WEATHER_EVERY = (1800, 300)
GLOBE_EVERY = (3600, 1800)  # the day texture changes once per UTC day
MAPS_EVERY = (86400, 600)  # tiles stay cached for 7 days, so this is cheap

RADAR_STALE = 30 * 60
WEATHER_STALE = 6 * 3600

JAPAN_BOX = (128.0, 30.0, 146.0, 46.0)
LOCAL_KM = 40.0
PHOTO_KM = 30.0
JAPAN_OSM_ZOOM, LOCAL_OSM_ZOOM, PHOTO_OSM_ZOOM = 6, 12, 11
# hrpns has data only at even zooms (radar.DATA_ZOOMS). z6 is the nearest to
# the 512 px overlay for Japan; z4 is blocky and over-generalised.
JAPAN_RADAR_ZOOM, LOCAL_RADAR_ZOOM = 6, 10
OFFICE_RADAR_ZOOM = 10


def iso(t):
    """UTC "YYYY-MM-DDTHH:MM:SSZ" from a datetime or an epoch timestamp."""
    if not isinstance(t, datetime):
        t = datetime.fromtimestamp(t, timezone.utc)
    return t.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def digest(*parts):
    h = hashlib.sha1()
    for part in parts:
        h.update(b"%d:" % len(part))
        h.update(part)
    return h.hexdigest()


def next_rev(rev, old, new):
    return rev if old == new else rev + 1


def build_status(snap, now):
    """The /status document from a state snapshot (pure)."""
    radar_fresh = snap["radar_at"] is not None and now - snap["radar_at"] <= RADAR_STALE
    weather_fresh = snap["weather_at"] is not None and now - snap["weather_at"] <= WEATHER_STALE
    views = {"globe": dict(snap["globe"]) if snap["globe"] else None}
    for name in ("japan", "local"):
        view = snap["views"].get(name)
        if view is None:
            views[name] = None
            continue
        frames = view["frames"] if radar_fresh else []
        views[name] = {
            "rev": view["rev"],
            "frames": [{"at": iso(f.at), "forecast": bool(f.forecast)} for f in frames],
            # always an integer (the client parses usize); only meaningful with frames
            "now_index": view["now_index"] if frames else 0,
            "office": [round(view["office"][0], 4), round(view["office"][1], 4)],
        }
    lat, lon = snap["office"]
    return {
        "generated_at": iso(now),
        "office": {"lat": lat, "lon": lon},
        "weather": dict(snap["weather"]) if weather_fresh and snap["weather"] else None,
        "rain": dict(snap["rain"]) if radar_fresh and snap["rain"] else None,
        "views": views,
        "credits": list(CREDITS),
    }


class MapView:
    def __init__(self, name, extent, osm_zoom, radar_zoom, blur, office):
        self.name = name
        self.extent = extent
        self.osm_zoom = osm_zoom
        self.radar_zoom = radar_zoom
        self.blur = blur
        self.office = office
        self.base = None
        self.overlays = []
        self.frames = []
        self.now_index = None
        self.rev = 0
        self.digest = None

    def _bump(self):
        new = digest(self.base or b"", *self.overlays)
        self.rev = next_rev(self.rev, self.digest, new)
        self.digest = new

    def set_base(self, jpeg):
        self.base = jpeg
        self._bump()

    def set_radar(self, frames, now_index, overlays):
        self.frames, self.now_index, self.overlays = list(frames), now_index, list(overlays)
        self._bump()


class Atlas:
    def __init__(
        self,
        lat=LAT,
        lon=LON,
        cache_dir=CACHE_DIR,
        fetch=net.fetch,
        fetch_local=net.fetch_local,
        photo_port=PHOTO_PORT,
        forecast_office=FORECAST_OFFICE,
        forecast_area=FORECAST_AREA,
        temp_point=TEMP_POINT,
        osm_spacing=0.2,
        geocode_interval=1.0,
        clock=time.time,
    ):
        self.lat, self.lon = lat, lon
        self.fetch = fetch
        self.fetch_local = fetch_local
        self.photo_port = photo_port
        self.forecast_url = weather.FORECAST_URL.format(office=forecast_office)
        self.forecast_area = forecast_area
        self.temp_point = temp_point
        self.clock = clock
        self.lock = threading.Lock()

        disk = cache.DiskCache(cache_dir)
        self.osm = tiles.OsmTiles(disk, fetch, spacing=osm_spacing)
        self.radar_tiles = radar.TileSource(fetch)
        self.globe = globe.Globe(disk, fetch, clock=clock)
        geocoder = places.Geocoder(os.path.join(cache_dir, "places.json"), fetch, interval=geocode_interval)
        self.places = places.PhotoPlaces(self._photo_bytes, geocoder, (lat, lon))
        self.photo_maps = places.LRU(16)

        japan = geo.extent_bbox(*JAPAN_BOX)
        local = geo.extent_around(lon, lat, LOCAL_KM)
        self.views = {
            "japan": MapView("japan", japan, JAPAN_OSM_ZOOM, JAPAN_RADAR_ZOOM, True, geo.normalized(japan, lon, lat)),
            "local": MapView("local", local, LOCAL_OSM_ZOOM, LOCAL_RADAR_ZOOM, True, geo.normalized(local, lon, lat)),
        }
        self.weather = None
        self.weather_at = None
        self.rain = None
        self.radar_at = None
        self.globe_day = None
        self.globe_night = None
        self.globe_rev = 0
        self.globe_digest = None

    # -- state -------------------------------------------------------------

    def snapshot(self):
        with self.lock:
            return {
                "office": (self.lat, self.lon),
                "weather": self.weather,
                "weather_at": self.weather_at,
                "rain": self.rain,
                "radar_at": self.radar_at,
                "globe": {"rev": self.globe_rev} if self.globe_day and self.globe_night else None,
                "views": {
                    name: (
                        {"rev": v.rev, "frames": v.frames, "now_index": v.now_index, "office": v.office}
                        if v.base is not None
                        else None
                    )
                    for name, v in self.views.items()
                },
            }

    def status(self):
        return build_status(self.snapshot(), self.clock())

    # -- refreshers ----------------------------------------------------------

    def refresh_maps(self):
        failures = []
        for view in self.views.values():
            try:
                jpeg = tiles.render_base(view.extent, view.osm_zoom, self.osm.get)
            except Exception as exc:
                failures.append("%s: %r" % (view.name, exc))
                continue
            with self.lock:
                view.set_base(jpeg)
        if failures:
            raise RuntimeError("base maps failed: " + "; ".join(failures))

    def refresh_radar(self):
        past = radar.parse_target_times(json.loads(self.fetch(radar.N1_URL)), forecast=False)
        future = radar.parse_target_times(json.loads(self.fetch(radar.N2_URL)), forecast=True)
        frames, now_index = radar.pick_frames(past, future)
        if not frames:
            raise RuntimeError("no radar frames in targetTimes")
        src = self.radar_tiles
        src.begin()
        overlays = {}
        for view in self.views.values():
            overlays[view.name] = [
                radar.png_bytes(radar.render_frame(src.index, view.extent, view.radar_zoom, f, blur=view.blur))
                for f in frames
            ]
        level = radar.office_level(src.index, frames[now_index], self.lon, self.lat, OFFICE_RADAR_ZOOM)
        ahead = radar.series_frames(past, future)
        series = radar.fill_series(
            [radar.office_level(src.index, f, self.lon, self.lat, OFFICE_RADAR_ZOOM) if f else None for f in ahead],
            start=level,
        )
        # newest observed, not "now": tiles fetched ahead of N2 are kept too
        src.end(past[-1].at)
        rain = {"summary": radar.summary(level, series), "level": level, "series": series}
        with self.lock:
            for view in self.views.values():
                view.set_radar(frames, now_index, overlays[view.name])
            self.rain = rain
            self.radar_at = self.clock()

    def refresh_weather(self):
        data = json.loads(self.fetch(self.forecast_url))
        parsed = weather.parse_forecast(data, self.forecast_area, self.temp_point)
        if parsed is None:
            raise RuntimeError("forecast area %s not found" % self.forecast_area)
        with self.lock:
            self.weather = parsed
            self.weather_at = self.clock()

    def refresh_globe(self):
        day, night, error = self.globe.refresh()
        with self.lock:
            self.globe_day, self.globe_night = day, night
            if day and night:
                new = digest(day, night)
                self.globe_rev = next_rev(self.globe_rev, self.globe_digest, new)
                self.globe_digest = new
        if error is not None:
            raise error

    # -- images ------------------------------------------------------------

    def image(self, path):
        """(bytes, content type, rev) for an image path, or None."""
        parts = path.strip("/").split("/")
        with self.lock:
            if len(parts) == 2 and parts[0] == "globe" and parts[1] in ("day.jpg", "night.jpg"):
                data = self.globe_day if parts[1] == "day.jpg" else self.globe_night
                ready = self.globe_day and self.globe_night
                return (data, "image/jpeg", self.globe_rev) if ready else None
            if len(parts) < 3 or parts[0] != "map" or parts[1] not in self.views:
                return None
            view = self.views[parts[1]]
            if parts[2:] == ["base.jpg"] and view.base is not None:
                return view.base, "image/jpeg", view.rev
            if len(parts) == 4 and parts[2] == "radar" and parts[3].endswith(".png"):
                index = parts[3][:-4]
                if index.isdigit() and int(index) < len(view.overlays):
                    return view.overlays[int(index)], "image/png", view.rev
        return None

    # -- photo places ------------------------------------------------------

    def _photo_bytes(self, name):
        url = "http://127.0.0.1:%d/photos/%s" % (self.photo_port, places.encode_name(name))
        return self.fetch_local(url)

    def _place(self, segment):
        """(status, place or None) for a percent-encoded photo name."""
        name = places.decode_segment(segment)
        if not places.validate_name(name):
            return 400, None
        try:
            place = self.places.lookup(name)
        except net.FetchError as exc:
            return (404 if exc.status == 404 else 502), None
        except places.GeocodeError:
            return 503, None
        return (200, place) if place else (404, None)

    def photo(self, segment):
        code, place = self._place(segment)
        return code, place or {}

    def photo_map(self, segment):
        """(status, jpeg or None): the base map around a near photo place."""
        code, place = self._place(segment)
        if code != 200:
            return code, None
        if not place["near"]:
            return 404, None
        key = (place["lat"], place["lon"])
        found, jpeg = self.photo_maps.lookup(key)
        if not found:
            ext = geo.extent_around(place["lon"], place["lat"], PHOTO_KM)
            try:
                jpeg = tiles.render_base(ext, PHOTO_OSM_ZOOM, self.osm.get)
            except Exception:
                return 502, None
            self.photo_maps.put(key, jpeg)
        return 200, jpeg


def make_handler(atlas):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def log_message(self, *args):
            pass

        def _send(self, code, body, ctype, headers=()):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            for key, value in headers:
                self.send_header(key, value)
            self.end_headers()
            self.wfile.write(body)

        def _json(self, code, doc):
            body = json.dumps(doc, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            self._send(code, body, "application/json; charset=utf-8")

        def do_GET(self):
            path = self.path.split("?", 1)[0]
            try:
                if path == "/status":
                    return self._json(200, atlas.status())
                if path.startswith("/photo/"):
                    return self._photo(path[len("/photo/"):])
                hit = atlas.image(path)
                if hit is None:
                    return self._json(404, {})
                data, ctype, rev = hit
                return self._send(200, data, ctype, [("X-Atlas-Rev", str(rev))])
            except (BrokenPipeError, ConnectionResetError):
                return
            except Exception as exc:
                print("atlas: %s failed: %r" % (path[:64], exc), flush=True)
                return self._json(500, {})

        def _photo(self, rest):
            if rest.endswith("/map.jpg"):
                segment = rest[: -len("/map.jpg")]
                if "/" in segment:
                    return self._json(404, {})
                code, jpeg = atlas.photo_map(segment)
                if jpeg is None:
                    return self._json(code, {})
                return self._send(200, jpeg, "image/jpeg")
            if "/" in rest:
                return self._json(404, {})
            code, doc = atlas.photo(rest)
            return self._json(code, doc)

    return Handler


def start_loop(name, refresh, every):
    """Run refresh forever in its own thread; a failure is logged once per streak."""
    interval, retry = every

    def loop():
        failing = False
        while True:
            try:
                refresh()
                if failing:
                    print("atlas: %s recovered" % name, flush=True)
                failing = False
                wait = interval
            except Exception as exc:
                if not failing:
                    print("atlas: %s failed: %r" % (name, exc), flush=True)
                failing = True
                wait = retry
            time.sleep(wait)

    thread = threading.Thread(target=loop, name=name, daemon=True)
    thread.start()
    return thread


def main():
    os.makedirs(CACHE_DIR, exist_ok=True)
    atlas = Atlas()
    start_loop("maps", atlas.refresh_maps, MAPS_EVERY)
    start_loop("radar", atlas.refresh_radar, RADAR_EVERY)
    start_loop("weather", atlas.refresh_weather, WEATHER_EVERY)
    start_loop("globe", atlas.refresh_globe, GLOBE_EVERY)
    server = ThreadingHTTPServer(("127.0.0.1", PORT), make_handler(atlas))
    print("atlas on 127.0.0.1:%d (cache %s)" % (PORT, CACHE_DIR), flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()

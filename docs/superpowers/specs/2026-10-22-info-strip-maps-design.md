# Info strip: globe, rain radar, maps and photo places

Status: approved design, 2026-10-22

## Goal

Give the 2F display more character without taking the focus away from the
photos:

- a permanent info strip on the right with clock, today's weather, a map card
  and a rain summary
- the map card cycles through the globe, rain over Japan, the office
  neighbourhood (OpenStreetMap + rain), and where the shown photo was taken
- the whole UI changes mood with the time of day and the weather
- more motion: staggered card entrances, a rain radar loop, gentle globe
  rotation and view crossfades

## Decisions

| Topic | Decision |
|---|---|
| Placement | Permanent strip on the right, about 30% of the width. Collage keeps the left 70% and the live survey tile. |
| Network | Only a new loopback service, `atlas`, talks to the internet. `photo-frame-rs` stays loopback-only. |
| Rendering | `atlas` prepares imagery and facts; `photo-frame-rs` draws everything (text, animation, theme). |
| Views | Globe, rain over Japan, office neighbourhood, photo place. |
| Photo places | Municipality granularity. Coordinates are rounded to 0.05° before any use. |
| Mood | Time of day and weather both change the palette. |

## Screen

```
┌───────────────────────────────────────────┬────────────────────┐
│ ┌──────────────┐ ┌──────────┐             │ 14:32              │
│ │              │ │  LIVE    │             │ 10月22日 水曜日     │
│ │   photo      │ │  survey  │             │ くもり のち 雨 23/17 │
│ │  [京都市 ·   │ ├──────────┤             │ ┌────────────────┐ │
│ │   2025.08]   │ │  photo   │             │ │ map card       │ │
│ └──────────────┘ └──────────┘             │ │ 雨雲 · 日本 14:25│ │
│                                           │ └──── • ━ • • ───┘ │
│                                           │ 30分後に弱い雨      │
│                                           │ ▁▁▁▁▁▁▃▅▆▇▅▃       │
│                                           │ TSUQREA · credits  │
└───────────────────────────────────────────┴────────────────────┘
```

- Strip width: `clamp(viewport_w * 0.30, 360, 640)` logical points, flush right
  with the same outer margin the collage uses.
- The collage gets the remaining area with equal outer margins. The fixed
  80 pt header and 48 pt footer reservations go away: the brand moves to the
  bottom of the strip and the debug footer
  (`N photos / available / skipped`) moves to `/status` only.
- The clock leaves the top-right corner and becomes the top of the strip.
- The map card is 4:3 and takes the width of the strip.

Strip blocks, top to bottom: clock and date, weather line, map card (view
title, frame time, view dots), rain summary with a 60 minute bar, brand and
credits. Credits are required by the data licences and always visible:
`© OpenStreetMap contributors · 気象庁 · NASA GIBS`.

`--no-strip` restores the previous full-width layout.

## Architecture

```
internet ── OSM tiles · JMA nowcast and forecast · NASA GIBS · Nominatim
               │  (atlas only, disk cache)
atlas  (Python, numpy + Pillow, 127.0.0.1:8099)
               │  loopback HTTP, no query strings
photo-frame-rs (Rust/egui)
  atlas.rs   worker: poll /status, fetch images when a rev changes, decode
  strip.rs   pure: strip/collage split, block rects, view cycle, radar timing
  theme.rs   pure: sun position, palette from sun elevation and weather mood
  globe.rs   pure: orthographic sphere mesh with day/night weights
  main.rs    wiring and painting
```

Port 8097 is the hand detector and 8098 is speech, so atlas uses 8099.

### atlas endpoints

| Path | Body |
|---|---|
| `GET /status` | JSON below |
| `GET /map/japan/base.jpg`, `GET /map/local/base.jpg` | 1024x768 base map, neutral dark grey |
| `GET /map/japan/radar/<i>.png`, `GET /map/local/radar/<i>.png` | 512x384 RGBA rain overlay for frame `i` |
| `GET /globe/day.jpg`, `GET /globe/night.jpg` | 2048x1024 equirectangular textures |
| `GET /photo/<name>` | photo place JSON, `404` when the photo has no GPS |
| `GET /photo/<name>/map.jpg` | 1024x768 municipality-scale base map centred on the rounded place |

`<name>` is a photo basename, percent-encoded exactly like
`playlist::photo_path`. atlas fetches the photo bytes itself from
`127.0.0.1:8081/photos/<name>`.

`/status`:

```json
{
  "generated_at": "2026-10-22T05:30:00Z",
  "weather": {"mood": "cloudy", "label": "くもり 夜 雨", "max": 23, "min": 17},
  "rain": {"summary": "30分後に弱い雨", "level": 1, "series": [0,0,0,0,0,0,1,1,2,2,1,1]},
  "views": {
    "globe": {"rev": 3},
    "japan": {"rev": 12, "frames": [{"at": "2026-10-22T04:30:00Z", "forecast": false}], "now_index": 6, "office": [0.52, 0.58]},
    "local": {"rev": 12, "frames": [], "now_index": 6, "office": [0.5, 0.5]}
  },
  "credits": ["© OpenStreetMap contributors", "気象庁", "NASA GIBS"]
}
```

- `weather`, `rain` and each entry of `views` may be `null`. A `null` view is
  not ready and is skipped by the cycle.
- `mood` is one of `clear`, `cloudy`, `rain`, `snow`.
- `rain.series` is 12 steps of 5 minutes (the next 60 minutes) as intensity
  levels `0..=6`. `level` is the level now.
- `office` is the office marker in normalised image coordinates.
- `rev` changes whenever any image of that view changes. Rust fetches images
  only when a rev changes.

Photo place:

```json
{"place": "京都市", "lat": 35.0, "lon": 135.75, "taken": "2025-08", "near": true}
```

`near` is true within 1500 km of the office; far places are shown on the globe
instead of a map.

### Data sources

| Source | Use | Refresh | Notes |
|---|---|---|---|
| `tile.openstreetmap.org` | base maps | disk cache, at least 7 days | identifying User-Agent, no bulk download; tiles are styled to dark grey in atlas |
| JMA `bosai/jmatile/data/nowc` (`targetTimes_N1/N2.json`, `hrpns` tiles) | rain frames and the office rain series | every 5 min | not an official API; failure makes rain `null` |
| JMA `bosai/forecast/data/forecast/<office>.json` | weather label, mood, temperatures | every 30 min | weather code first digit gives the mood |
| NASA GIBS WMS (EPSG:4326) | globe day (yesterday's VIIRS true colour over Blue Marble) and night lights | daily | keep the last good texture |
| Nominatim reverse | municipality name | per rounded coordinate, cached on disk | 1 request/s at most, `accept-language=ja`, `zoom=10` |

Configuration comes from the environment and never from the repository:
`ATLAS_LAT`, `ATLAS_LON`, `ATLAS_FORECAST_OFFICE`, `ATLAS_FORECAST_AREA`,
`ATLAS_USER_AGENT`, `ATLAS_CACHE_DIR` (systemd `CacheDirectory=atlas`).

### Views

| View | Extent | Content |
|---|---|---|
| Globe | orthographic, centred near the office longitude | day texture, night lights on the dark side, terminator from the real sun position, office dot |
| Rain · Japan | lon 128–146, lat 30–46 | base + radar loop |
| Neighbourhood | about 40 km wide around the office | base (OSM z12) + radar loop |
| Photo place | about 30 km wide around the rounded place, or the globe when far | base + ring marker and place name |

Radar frames: past 60 minutes and forecast 60 minutes at 10 minute steps, 13
frames. Rain intensity comes from the JMA palette sampled at the office over a
3x3 pixel neighbourhood (the highest level wins).

Rain summary rules, in order:

1. raining now: `雨が降っています（<class>）`, plus `<m>分後にやむ見込み` when the series ends dry
2. rain in the series: `<m>分後に<class>`
3. otherwise: `この先1時間 雨の心配なし`

Classes follow JMA wording: `弱い雨` (<10 mm/h), `やや強い雨`, `強い雨`,
`激しい雨`, `非常に激しい雨`, `猛烈な雨`.

## Mood

`theme.rs` computes the sun elevation at the office from UTC time and the
office coordinates. `photo-frame-rs` takes them as `--lat/--lon`, injected by
`install-units.sh` from the same environment as atlas, so the time-of-day
palette keeps working while atlas is down.

Palette anchors (bg, surface, border, text, dim, accent), interpolated by sun
elevation:

| Anchor | Elevation | Character |
|---|---|---|
| night | ≤ −12° | deep blue-black, cool accent |
| twilight | −3° | violet tint |
| golden | 6° | warm brown, orange accent |
| day | ≥ 20° | today's slate palette |

Weather mood modifies the interpolated palette: `rain` mixes toward a cool
blue and darkens the background, `cloudy` lowers saturation, `snow` lifts the
borders toward pale blue, `clear` leaves it. Rain expected in the next 60
minutes counts as `rain`. The applied palette eases toward the target over
60 seconds so the room never sees a jump.

When rain is expected the cycle doubles the neighbourhood dwell and the rain
summary uses the accent colour.

## Motion

- Collage cards enter one by one: card `i` starts `0.12 s * i` after the
  commit, fades over the existing 0.9 s and rises 14 pt with an ease-out.
- View cycle: 15 s per ready view in the order globe → Japan → neighbourhood →
  photo place (only while a photo with a place is in the scene); 0.8 s
  crossfade with a 1.04 → 1.0 zoom.
- Radar loop: 0.5 s per frame, crossfaded, holding 2 s on `now` and 1.5 s at
  the end.
- Globe: slow ±20° longitude drift; moving to a far photo place follows the
  great circle over 2 s.
- Repaint at up to 30 fps only while something in the strip is animating;
  otherwise the current 1 s cadence remains.

## Photo places

- atlas reads EXIF GPS and `DateTimeOriginal` with Pillow.
- Coordinates are rounded to 0.05° (about 5 km) before display, geocoding or
  caching. Only the rounded coordinate is ever sent to Nominatim.
- The name → place mapping lives in memory only (bounded LRU). The disk cache
  is keyed by rounded coordinates and holds no photo names.
- The collage card of a photo with a place gets a small label
  `京都市 · 2025.08`; while it is shown the cycle adds the photo place view.
- Photos without GPS get no label and no view.

## Failure behaviour

| Failure | Behaviour |
|---|---|
| atlas down | strip shows clock, date and time-of-day palette; map card shows `地図を準備中`; collage unaffected |
| JMA nowcast fails | maps without radar, `雨雲データなし`, no rain summary |
| JMA forecast fails | weather line hidden, mood from radar only |
| OSM fails | serve stale cache; without cache the view is not ready |
| GIBS fails | keep the last good texture; without one the globe view is not ready |
| No GPS | no label, no photo view |
| Oversized or invalid response | rejected by bounded parsing in Rust; previous textures stay |

`/status` of `photo-frame-rs` gains `atlas_state`, `strip_view`,
`theme_phase` and `theme_mood`.

## Testing

Rust (`cargo test`):

- sun elevation against known values (e.g. Tokyo solar noon on an equinox)
- palette interpolation is continuous and hits the anchors
- strip and collage rects never overlap and stay inside the viewport at 4K,
  1080p, 960x540 and 640x360
- view cycle skips views that are not ready, honours dwell and crossfade, and
  doubles the neighbourhood when rain is expected
- radar frame index and blend over time, including holds
- globe mesh: centre maps to centre, back hemisphere is culled, night weight
  follows the subsolar point
- atlas `/status` parsing: valid, missing fields, wrong types, oversize
- card stagger timing

Python (`pytest`, run through `uv`):

- lon/lat ↔ tile and pixel conversions, view extents
- JMA colour → level, summary text rules, `targetTimes` parsing
- forecast parsing and code → mood
- EXIF GPS rational → decimal and rounding
- disk cache expiry (7 day minimum for OSM)
- HTTP handler with a fake fetcher

Visual check on the Mac: a dev-only fake photo manager on 8081 serving
generated images (some with GPS), real atlas on 8099, and screenshots of the
native window.

## Delivery

Each stage leaves the display working.

1. Strip skeleton, theme, UI refresh, card stagger (no network)
2. atlas with Japan and neighbourhood rain, weather and the rain summary; Rust map card
3. Globe
4. Photo places

## Out of scope

- Hosting our own tile server
- Typhoon tracks, warnings, temperature maps
- Showing exact photo locations

//! Loopback client for the atlas service: map, radar, globe and photo-place imagery.
use crate::{decode, http, playlist, strip::View};
use eframe::egui::ColorImage;
use serde::Deserialize;
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const ATLAS_PORT: u16 = 8099;
pub const MAX_STATUS_BYTES: usize = 64 * 1024;
pub const MAX_PLACE_BYTES: usize = 4 * 1024;
pub const MAX_FRAMES: usize = 13;
pub const MAX_TEXTURE_SIDE: usize = 4096;
const QUEUE: usize = 8;

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Status {
    pub generated_at: String,
    #[serde(default)]
    pub weather: Option<Weather>,
    #[serde(default)]
    pub rain: Option<Rain>,
    pub views: Views,
    #[serde(default)]
    pub credits: Vec<String>,
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Weather {
    pub mood: String,
    pub label: String,
    #[serde(default)]
    pub max: Option<i32>,
    #[serde(default)]
    pub min: Option<i32>,
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Rain {
    pub summary: String,
    pub level: u8,
    pub series: Vec<u8>,
}
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Views {
    #[serde(default)]
    pub globe: Option<GlobeView>,
    #[serde(default)]
    pub japan: Option<MapView>,
    #[serde(default)]
    pub local: Option<MapView>,
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct GlobeView {
    pub rev: u64,
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct MapView {
    pub rev: u64,
    pub frames: Vec<Frame>,
    pub now_index: usize,
    pub office: [f32; 2],
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Frame {
    pub at: String,
    pub forecast: bool,
}
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Place {
    pub place: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub taken: Option<String>,
    pub near: bool,
}
impl Rain {
    /// Rain now or anywhere in the next hour.
    pub fn expected(&self) -> bool {
        self.level > 0 || self.series.iter().any(|l| *l > 0)
    }
}
fn short(s: &str) -> bool {
    s.len() <= 256
}
pub fn parse_status(bytes: &[u8]) -> Result<Status, &'static str> {
    if bytes.len() > MAX_STATUS_BYTES {
        return Err("atlas_status_too_large");
    }
    let s: Status = serde_json::from_slice(bytes).map_err(|_| "atlas_status_json")?;
    if !short(&s.generated_at) || s.credits.len() > 8 || !s.credits.iter().all(|c| short(c)) {
        return Err("atlas_status_strings");
    }
    if let Some(w) = &s.weather
        && (!short(&w.label) || !short(&w.mood))
    {
        return Err("atlas_status_strings");
    }
    if let Some(r) = &s.rain
        && (!short(&r.summary) || r.series.len() != 12 || r.level > 6 || r.series.iter().any(|l| *l > 6))
    {
        return Err("atlas_status_rain");
    }
    for v in [&s.views.japan, &s.views.local].into_iter().flatten() {
        if v.frames.len() > MAX_FRAMES
            || (!v.frames.is_empty() && v.now_index >= v.frames.len())
            || !v.office.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c))
            || !v.frames.iter().all(|f| short(&f.at))
        {
            return Err("atlas_status_view");
        }
    }
    Ok(s)
}
pub fn parse_place(bytes: &[u8]) -> Result<Place, &'static str> {
    if bytes.len() > MAX_PLACE_BYTES {
        return Err("atlas_place_too_large");
    }
    let p: Place = serde_json::from_slice(bytes).map_err(|_| "atlas_place_json")?;
    if p.place.is_empty()
        || p.place.len() > 128
        || !(-90.0..=90.0).contains(&p.lat)
        || !(-180.0..=180.0).contains(&p.lon)
        || p.taken.as_ref().is_some_and(|t| t.len() > 16)
    {
        return Err("atlas_place_invalid");
    }
    Ok(p)
}
/// `/photo/<same escaping as /photos/>`.
pub fn place_path(name: &str) -> Result<String, &'static str> {
    let path = playlist::photo_path(name)?;
    Ok(path.replacen("/photos/", "/photo/", 1))
}

pub enum AtlasEvent {
    Status(Status),
    Map {
        view: View,
        /// The frames and marker these images were rendered for.
        meta: MapView,
        base: ColorImage,
        radar: Vec<ColorImage>,
    },
    Globe {
        rev: u64,
        day: ColorImage,
        night: ColorImage,
    },
    Place {
        name: String,
        place: Option<Place>,
        map: Option<ColorImage>,
    },
    Unavailable(&'static str),
}
pub type AtlasSlot = Arc<Mutex<VecDeque<AtlasEvent>>>;
pub type Wake = Arc<Mutex<Option<eframe::egui::Context>>>;

/// Images of one view must all come from the rev `/status` announced; atlas
/// tags each with `X-Atlas-Rev`, and a mismatch means it refreshed mid-fetch.
pub fn same_rev(response: &http::Response, rev: Option<u64>) -> Result<(), &'static str> {
    match (rev, response.header("X-Atlas-Rev").map(str::parse::<u64>)) {
        (None, _) | (Some(_), None) => Ok(()),
        (Some(want), Some(Ok(got))) if want == got => Ok(()),
        _ => Err("atlas_rev_changed"),
    }
}
fn image(port: u16, path: &str) -> Result<ColorImage, &'static str> {
    image_at(port, path, None)
}
fn image_at(port: u16, path: &str, rev: Option<u64>) -> Result<ColorImage, &'static str> {
    let response = http::get(port, path, decode::MAX_IMAGE_BYTES)?;
    http::require_type(&response, &["image/jpeg", "image/png"])?;
    same_rev(&response, rev)?;
    let decoded = image::load_from_memory(&response.body).map_err(|_| "atlas_image_decode")?;
    if decoded.width() as usize > MAX_TEXTURE_SIDE || decoded.height() as usize > MAX_TEXTURE_SIDE {
        return Err("atlas_image_too_large");
    }
    let rgba = decoded.to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}
fn map_images(port: u16, key: &str, rev: u64, frames: usize) -> Result<(ColorImage, Vec<ColorImage>), &'static str> {
    let base = image_at(port, &format!("/map/{key}/base.jpg"), Some(rev))?;
    let radar = (0..frames.min(MAX_FRAMES))
        .map(|i| image_at(port, &format!("/map/{key}/radar/{i}.png"), Some(rev)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((base, radar))
}
fn place(port: u16, name: &str) -> Result<(Option<Place>, Option<ColorImage>), &'static str> {
    let path = place_path(name)?;
    let response = http::get(port, &path, MAX_PLACE_BYTES);
    let place = match response {
        Ok(r) => parse_place(&r.body)?,
        // Only a definite answer is remembered; 5xx and timeouts are retried.
        Err("http_not_found" | "http_bad_request") => return Ok((None, None)),
        Err(e) => return Err(e),
    };
    // A near place without its map is not done yet: fail so it is asked again.
    let map = if place.near {
        Some(image(port, &format!("{path}/map.jpg"))?)
    } else {
        None
    };
    Ok((Some(place), map))
}
/// Polls `/status`; fetches imagery only when a view's rev changes. Scene photo
/// names arrive on the returned sender and are looked up once each.
pub fn start(
    port: u16,
    stop: Arc<AtomicBool>,
    wake: Wake,
) -> std::io::Result<(AtlasSlot, SyncSender<Vec<String>>, JoinHandle<()>)> {
    let slot: AtlasSlot = Arc::new(Mutex::new(VecDeque::new()));
    let producer = slot.clone();
    let (names_tx, names_rx): (SyncSender<Vec<String>>, Receiver<Vec<String>>) = mpsc::sync_channel(4);
    let handle = thread::Builder::new().name("atlas".into()).spawn(move || {
        let push = |event: AtlasEvent| {
            if let Ok(mut q) = producer.lock() {
                if q.len() == QUEUE {
                    q.pop_front();
                }
                q.push_back(event);
            }
            if let Ok(ctx) = wake.lock()
                && let Some(ctx) = ctx.as_ref()
            {
                ctx.request_repaint();
            }
        };
        let mut revs: [Option<u64>; 3] = [None; 3];
        let mut next_poll = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            match names_rx.try_recv() {
                Ok(names) => {
                    // The HUD only sends names it has no usable answer for; a failed
                    // look-up sends nothing, so the name is asked again on a later scene.
                    let mut seen = HashSet::new();
                    for name in names.into_iter().filter(|n| seen.insert(n.clone())).take(4) {
                        if let Ok((place, map)) = place(port, &name) {
                            push(AtlasEvent::Place { name, place, map });
                        }
                    }
                    continue;
                }
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            if Instant::now() < next_poll {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            let status = http::get(port, "/status", MAX_STATUS_BYTES)
                .and_then(|r| parse_status(&r.body));
            match status {
                Ok(status) => {
                    next_poll = Instant::now() + Duration::from_secs(30);
                    let maps = [
                        (0, View::Japan, "japan", status.views.japan.clone()),
                        (1, View::Local, "local", status.views.local.clone()),
                    ];
                    push(AtlasEvent::Status(status.clone()));
                    for (i, view, key, v) in maps {
                        let Some(v) = v else { continue };
                        if revs[i] == Some(v.rev) {
                            continue;
                        }
                        if let Ok((base, radar)) = map_images(port, key, v.rev, v.frames.len()) {
                            revs[i] = Some(v.rev);
                            push(AtlasEvent::Map {
                                view,
                                meta: v,
                                base,
                                radar,
                            });
                        }
                    }
                    if let Some(g) = &status.views.globe
                        && revs[2] != Some(g.rev)
                        && let (Ok(day), Ok(night)) = (
                            image_at(port, "/globe/day.jpg", Some(g.rev)),
                            image_at(port, "/globe/night.jpg", Some(g.rev)),
                        )
                    {
                        revs[2] = Some(g.rev);
                        push(AtlasEvent::Globe {
                            rev: g.rev,
                            day,
                            night,
                        });
                    }
                }
                Err(e) => {
                    next_poll = Instant::now() + Duration::from_secs(5);
                    push(AtlasEvent::Unavailable(e));
                }
            }
        }
        if let Ok(mut q) = producer.lock() {
            q.clear();
        }
    })?;
    Ok((slot, names_tx, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    const STATUS: &str = r#"{
      "generated_at": "2026-10-22T05:30:00Z",
      "office": {"lat": 35.68, "lon": 139.77},
      "weather": {"mood": "cloudy", "label": "くもり 夜 雨", "max": 23, "min": 17},
      "rain": {"summary": "30分後に弱い雨", "level": 0, "series": [0,0,0,0,0,1,1,2,2,1,1,0]},
      "views": {
        "globe": {"rev": 3},
        "japan": {"rev": 12, "frames": [{"at": "2026-10-22T04:30:00Z", "forecast": false},
                                        {"at": "2026-10-22T04:40:00Z", "forecast": true}],
                  "now_index": 0, "office": [0.52, 0.58]},
        "local": null
      },
      "credits": ["© OpenStreetMap contributors", "気象庁", "NASA GIBS"]
    }"#;
    #[test]
    fn parses_a_full_status() {
        let s = parse_status(STATUS.as_bytes()).unwrap();
        assert_eq!(s.weather.as_ref().unwrap().max, Some(23));
        assert!(s.rain.as_ref().unwrap().expected());
        assert_eq!(s.views.globe, Some(GlobeView { rev: 3 }));
        assert_eq!(s.views.japan.as_ref().unwrap().frames.len(), 2);
        assert_eq!(s.views.local, None);
        assert_eq!(s.credits.len(), 3);
    }
    #[test]
    fn nothing_ready_is_valid() {
        let s = parse_status(br#"{"generated_at":"x","weather":null,"rain":null,"views":{"globe":null,"japan":null,"local":null},"credits":[]}"#).unwrap();
        assert_eq!(s.views, Views::default());
    }
    #[test]
    fn rejects_bad_shapes() {
        let bad = |from: &str, to: &str| parse_status(STATUS.replace(from, to).as_bytes());
        assert!(bad("[0,0,0,0,0,1,1,2,2,1,1,0]", "[0,1]").is_err());
        assert!(bad("[0,0,0,0,0,1,1,2,2,1,1,0]", "[0,0,0,0,0,1,1,2,2,1,1,7]").is_err());
        assert!(bad("\"now_index\": 0", "\"now_index\": 2").is_err());
        assert!(bad("[0.52, 0.58]", "[1.5, 0.58]").is_err());
        assert!(bad("\"rev\": 3", "\"rev\": \"3\"").is_err());
        assert!(parse_status(b"not json").is_err());
        assert!(parse_status(&vec![b' '; MAX_STATUS_BYTES + 1]).is_err());
        let long = "x".repeat(300);
        assert!(bad("くもり 夜 雨", &long).is_err());
    }
    #[test]
    fn images_from_another_rev_are_rejected() {
        let with = |h: Option<&str>| http::Response {
            headers: h.map(|v| vec![("X-Atlas-Rev".to_string(), v.to_string())]).unwrap_or_default(),
            body: vec![],
        };
        assert_eq!(same_rev(&with(Some("12")), Some(12)), Ok(()));
        assert_eq!(same_rev(&with(Some("13")), Some(12)), Err("atlas_rev_changed"));
        assert_eq!(same_rev(&with(Some("x")), Some(12)), Err("atlas_rev_changed"));
        assert_eq!(same_rev(&with(None), Some(12)), Ok(()), "older atlas without the header");
        assert_eq!(same_rev(&with(Some("13")), None), Ok(()));
    }
    #[test]
    fn places_parse_and_paths_escape() {
        let p = parse_place(r#"{"place":"京都市","lat":35.0,"lon":135.75,"taken":"2025-08","near":true}"#.as_bytes()).unwrap();
        assert_eq!((p.place.as_str(), p.near), ("京都市", true));
        assert!(parse_place(br#"{"place":"","lat":0,"lon":0,"near":false}"#).is_err());
        assert!(parse_place(br#"{"place":"x","lat":95,"lon":0,"near":false}"#).is_err());
        assert_eq!(place_path("夏 1.jpg").unwrap(), "/photo/%E5%A4%8F%201.jpg");
        assert!(place_path("../x.jpg").is_err());
    }
}

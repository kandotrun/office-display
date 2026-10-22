//! Strip state: atlas textures, the view cycle and the eased palette.
use crate::{
    atlas::{self, AtlasEvent, AtlasSlot},
    globe,
    strip::{self, Split, View},
    strip_paint::{self, CardState, GlobeLayer, MapCard, MapLayer, PlaceLayer, StripInfo},
    theme::{self, Eased, Mood, Palette, Sun},
};
use eframe::egui::{self, Pos2, TextureHandle, TextureOptions};
use std::{
    collections::{HashMap, VecDeque},
    sync::mpsc::SyncSender,
};

pub struct MapTex {
    pub rev: u64,
    pub base: TextureHandle,
    pub radar: Vec<TextureHandle>,
}
pub struct PlaceEntry {
    pub place: Option<atlas::Place>,
    pub map: Option<TextureHandle>,
}
const PLACE_LIMIT: usize = 128;

pub struct Hud {
    slot: Option<AtlasSlot>,
    names: Option<SyncSender<Vec<String>>>,
    pub status: Option<atlas::Status>,
    pub state: &'static str,
    maps: [Option<MapTex>; 2],
    globe: Option<(TextureHandle, TextureHandle)>,
    places: HashMap<String, PlaceEntry>,
    place_order: VecDeque<String>,
    sent: Vec<String>,
    cycle: strip::Cycle,
    eased: Option<Eased>,
    rain_series: Vec<u8>,
    rain_since: f64,
    pub view: &'static str,
    pub phase: &'static str,
    pub mood: Option<Mood>,
}
impl Default for Hud {
    fn default() -> Self {
        Self::new(None, None)
    }
}
impl Hud {
    pub fn new(slot: Option<AtlasSlot>, names: Option<SyncSender<Vec<String>>>) -> Self {
        Self {
            state: if slot.is_some() { "connecting" } else { "disabled" },
            slot,
            names,
            status: None,
            maps: [None, None],
            globe: None,
            places: HashMap::new(),
            place_order: VecDeque::new(),
            sent: Vec::new(),
            cycle: strip::Cycle::default(),
            eased: None,
            rain_series: Vec::new(),
            rain_since: 0.0,
            view: "preparing",
            phase: "day",
            mood: None,
        }
    }
    pub fn receive(&mut self, ctx: &egui::Context, now: f64) {
        let events: Vec<AtlasEvent> = self
            .slot
            .as_ref()
            .and_then(|s| s.lock().ok().map(|mut q| q.drain(..).collect()))
            .unwrap_or_default();
        let load = |name: &str, image: egui::ColorImage| ctx.load_texture(name, image, TextureOptions::LINEAR);
        for event in events {
            match event {
                AtlasEvent::Status(status) => {
                    let series = status.rain.as_ref().map(|r| r.series.clone()).unwrap_or_default();
                    if series != self.rain_series {
                        self.rain_series = series;
                        self.rain_since = now;
                    }
                    let v = &status.views;
                    let ready = [v.globe.is_some(), v.japan.is_some(), v.local.is_some()];
                    self.state = if ready.iter().all(|r| *r) && status.rain.is_some() {
                        "ready"
                    } else {
                        "partial"
                    };
                    self.status = Some(status);
                }
                AtlasEvent::Map { view, rev, base, radar } => {
                    let i = usize::from(view == View::Local);
                    self.maps[i] = Some(MapTex {
                        rev,
                        base: load("atlas-base", base),
                        radar: radar.into_iter().map(|r| load("atlas-radar", r)).collect(),
                    });
                }
                AtlasEvent::Globe { day, night, .. } => {
                    self.globe = Some((load("globe-day", day), load("globe-night", night)));
                }
                AtlasEvent::Place { name, place, map } => {
                    if self.place_order.len() == PLACE_LIMIT
                        && let Some(old) = self.place_order.pop_front()
                    {
                        self.places.remove(&old);
                    }
                    self.place_order.push_back(name.clone());
                    let map = map.map(|m| load("place-map", m));
                    self.places.insert(name, PlaceEntry { place, map });
                }
                AtlasEvent::Unavailable(_) => self.state = "unavailable",
            }
        }
    }
    /// Ask atlas about the photos in a new scene (once per distinct scene).
    pub fn scene(&mut self, names: &[String]) {
        if names == self.sent.as_slice() {
            return;
        }
        self.sent = names.to_vec();
        if let Some(tx) = &self.names {
            let _ = tx.try_send(names.to_vec());
        }
    }
    pub fn place(&self, name: &str) -> Option<&atlas::Place> {
        self.places.get(name)?.place.as_ref()
    }
    fn rain_expected(&self) -> bool {
        self.status
            .as_ref()
            .and_then(|s| s.rain.as_ref())
            .is_some_and(atlas::Rain::expected)
    }
    pub fn palette(&mut self, now: f64, unix: f64, lat: f64, lon: f64) -> (Palette, Sun) {
        let sun = theme::sun(unix, lat, lon);
        let forecast = self
            .status
            .as_ref()
            .and_then(|s| s.weather.as_ref())
            .and_then(|w| Mood::parse(&w.mood));
        self.mood = if self.rain_expected() { Some(Mood::Rain) } else { forecast };
        self.phase = theme::phase(sun.elevation_deg);
        let target = theme::palette(sun.elevation_deg, self.mood);
        let eased = self.eased.get_or_insert_with(|| Eased::new(target, now));
        (eased.step(target, now), sun)
    }
    /// First photo in the scene that has a place we can show.
    fn scene_place(&self) -> Option<(&atlas::Place, Option<&TextureHandle>)> {
        self.sent.iter().find_map(|n| {
            let e = self.places.get(n)?;
            let p = e.place.as_ref()?;
            (e.map.is_some() || self.globe.is_some()).then_some((p, e.map.as_ref()))
        })
    }
    fn ready(&self) -> Vec<View> {
        let mut ready = Vec::new();
        if self.globe.is_some() {
            ready.push(View::Globe);
        }
        if self.maps[0].is_some() {
            ready.push(View::Japan);
        }
        if self.maps[1].is_some() {
            ready.push(View::Local);
        }
        if self.scene_place().is_some() {
            ready.push(View::Photo);
        }
        ready
    }
    fn globe_card<'a>(
        &'a self,
        view: (f64, f64),
        marker: Option<(f64, f64, &'a str)>,
        sun: Sun,
        office: (f64, f64),
    ) -> Option<MapCard<'a>> {
        let (day, night) = self.globe.as_ref()?;
        Some(MapCard::Globe(GlobeLayer { day, night, view, sun, office, marker }))
    }
    fn card(&self, view: View, elapsed: f64, now: f64, sun: Sun, office: (f64, f64)) -> MapCard<'_> {
        let map = |i: usize| -> Option<MapCard<'_>> {
            let tex = self.maps[i].as_ref()?;
            let v = self.status.as_ref().and_then(|s| if i == 0 { s.views.japan.as_ref() } else { s.views.local.as_ref() })?;
            Some(MapCard::Map(MapLayer {
                base: &tex.base,
                radar: if tex.radar.len() == v.frames.len() { &tex.radar } else { &[] },
                frames: &v.frames,
                now_index: v.now_index,
                office: v.office,
            }))
        };
        // The globe drifts ±20° around the office; far photo places pull it there.
        let home = (office.0 * 0.7, office.1 + 20.0 * (now * std::f64::consts::TAU / 90.0).sin());
        let globe = |centre, marker| self.globe_card(centre, marker, sun, office);
        let card = match view {
            View::Globe => globe(home, None),
            View::Japan => map(0),
            View::Local => map(1),
            View::Photo => self.scene_place().and_then(|(p, tex)| match tex {
                Some(t) if p.near => Some(MapCard::Place(PlaceLayer { map: t, place: &p.place })),
                _ => {
                    let t = ((elapsed / 2.0).clamp(0.0, 1.0) * std::f64::consts::PI / 2.0).sin();
                    let centre = globe::slerp_lonlat((office.0 * 0.7, office.1), (p.lat, p.lon), t);
                    globe(centre, Some((p.lat, p.lon, p.place.as_str())))
                }
            }),
        };
        card.unwrap_or(MapCard::Preparing)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        painter: &egui::Painter,
        origin: Pos2,
        split: &Split,
        palette: &Palette,
        sun: Sun,
        now: f64,
        office: (f64, f64),
    ) {
        let ready = self.ready();
        let frame = self.cycle.update(now, &ready, self.rain_expected());
        self.view = frame.map_or("preparing", |f| f.view.name());
        let current = frame.map_or(MapCard::Preparing, |f| self.card(f.view, f.elapsed, now, sun, office));
        let previous = frame
            .and_then(|f| f.previous)
            .map(|v| self.card(v, strip::DWELL_SECONDS, now, sun, office));
        let status = self.status.as_ref();
        let info = StripInfo {
            weather: status.and_then(|s| s.weather.as_ref()),
            rain: status.and_then(|s| s.rain.as_ref()),
            rain_since: self.rain_since,
            sun_elevation: sun.elevation_deg,
        };
        let blocks = strip::blocks(split.strip);
        strip_paint::paint_strip(
            painter,
            origin,
            split.strip,
            &blocks,
            palette,
            chrono::Local::now(),
            now,
            CardState { frame, current, previous, ready: &ready },
            &info,
        );
    }
}

//! Strip state: atlas status and places, the honeypot snapshot, the
//! globe textures and the eased palette.
use crate::{
    atlas::{self, AtlasEvent, AtlasSlot},
    honeypot::{self, HoneypotEvent, HoneypotSlot},
    strip::{self, Split},
    strip_paint::{self, HoneypotLayer, StripInfo, SurveyLayer, SurveyPanel},
    survey::{self, SurveyEvent, SurveySlot},
    theme::{self, Eased, Mood, Palette, Sun},
};
use eframe::egui::{self, Pos2, TextureHandle, TextureOptions};
use std::{
    collections::{HashMap, VecDeque},
    sync::mpsc::SyncSender,
};

pub struct PlaceEntry {
    pub place: Option<atlas::Place>,
    pub map: Option<TextureHandle>,
}
const PLACE_LIMIT: usize = 128;
/// Place maps are 1024x768 RGBA (3 MiB); only this many stay on the GPU. A
/// dropped map is fetched again the next time its photo is on show.
const PLACE_MAPS: usize = 8;
/// Weather and rain older than this are no longer shown as current.
const STATUS_STALE_SECONDS: f64 = 1800.0;
/// A honeypot snapshot older than this is no longer shown as current.
const HONEYPOT_STALE_SECONDS: f64 = 120.0;
/// A survey reading older than this is no longer shown as current.
const SURVEY_STALE_SECONDS: f64 = 10.0;
/// Motion history for the survey sparkline: a sample every few
/// seconds, capped to roughly the last ten minutes.
const HISTORY_SAMPLE_SECONDS: f64 = 3.0;
const HISTORY_CAP: usize = 200;
/// Missing places are asked for again after this long.
const PLACE_RETRY_SECONDS: f64 = 30.0;

pub struct Hud {
    slot: Option<AtlasSlot>,
    names: Option<SyncSender<Vec<String>>>,
    hp_slot: Option<HoneypotSlot>,
    hp: Option<honeypot::Status>,
    hp_at: f64,
    survey_slot: Option<SurveySlot>,
    survey: Option<survey::Reading>,
    survey_at: f64,
    motion_history: VecDeque<(f64, f32)>,
    pub status: Option<atlas::Status>,
    pub state: &'static str,
    globe: Option<(TextureHandle, TextureHandle)>,
    places: HashMap<String, PlaceEntry>,
    place_order: VecDeque<String>,
    sent: Vec<String>,
    sent_at: f64,
    /// Photo names of the scene on screen.
    shown: Vec<String>,
    status_at: f64,
    eased: Option<Eased>,
    pub view: &'static str,
    pub phase: &'static str,
    pub mood: Option<Mood>,
}
impl Default for Hud {
    fn default() -> Self {
        Self::new(None, None, None, None)
    }
}
impl Hud {
    pub fn new(
        slot: Option<AtlasSlot>,
        names: Option<SyncSender<Vec<String>>>,
        hp_slot: Option<HoneypotSlot>,
        survey_slot: Option<SurveySlot>,
    ) -> Self {
        Self {
            state: if slot.is_some() { "connecting" } else { "disabled" },
            slot,
            names,
            hp_slot,
            hp: None,
            hp_at: f64::NEG_INFINITY,
            survey_slot,
            survey: None,
            survey_at: f64::NEG_INFINITY,
            motion_history: VecDeque::new(),
            status: None,
            globe: None,
            places: HashMap::new(),
            place_order: VecDeque::new(),
            sent: Vec::new(),
            sent_at: f64::NEG_INFINITY,
            shown: Vec::new(),
            status_at: f64::NEG_INFINITY,
            eased: None,
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
                    self.state = "ready";
                    self.status = Some(status);
                    self.status_at = now;
                }
                // Map imagery belonged to the retired strip cycle; the
                // atlas client still fetches it, the strip drops it.
                AtlasEvent::Map { .. } => {}
                AtlasEvent::Globe { day, night, .. } => {
                    self.globe = Some((load("globe-day", day), load("globe-night", night)));
                }
                AtlasEvent::Place { name, place, map } => {
                    self.place_order.retain(|n| *n != name);
                    if self.place_order.len() == PLACE_LIMIT
                        && let Some(old) = self.place_order.pop_front()
                    {
                        self.places.remove(&old);
                    }
                    self.place_order.push_back(name.clone());
                    let map = map.map(|m| load("place-map", m));
                    self.places.insert(name, PlaceEntry { place, map });
                    self.trim_place_maps();
                }
                AtlasEvent::Unavailable(_) => self.state = "unavailable",
            }
        }
        let hp_events: Vec<HoneypotEvent> = self
            .hp_slot
            .as_ref()
            .and_then(|s| s.lock().ok().map(|mut q| q.drain(..).collect()))
            .unwrap_or_default();
        for event in hp_events {
            if let HoneypotEvent::Status(status) = event {
                self.hp = Some(status);
                self.hp_at = now;
            }
        }
        let survey_events: Vec<SurveyEvent> = self
            .survey_slot
            .as_ref()
            .and_then(|s| s.lock().ok().map(|mut q| q.drain(..).collect()))
            .unwrap_or_default();
        for event in survey_events {
            if let SurveyEvent::Reading(reading) = event {
                self.survey = Some(reading);
                self.survey_at = now;
                let due = self
                    .motion_history
                    .back()
                    .is_none_or(|(t, _)| now - t >= HISTORY_SAMPLE_SECONDS);
                if due {
                    self.motion_history.push_back((now, reading.motion as f32));
                    while self.motion_history.len() > HISTORY_CAP {
                        self.motion_history.pop_front();
                    }
                }
            }
        }
        // Old weather must not pass for current; the maps themselves stay.
        if now - self.status_at > STATUS_STALE_SECONDS
            && let Some(s) = self.status.as_mut()
        {
            s.weather = None;
            s.rain = None;
            self.state = "unavailable";
        }
    }
    fn trim_place_maps(&mut self) {
        let mut with_map = self.place_order.iter().filter(|n| self.places.get(*n).is_some_and(|e| e.map.is_some())).count();
        for name in &self.place_order {
            if with_map <= PLACE_MAPS {
                break;
            }
            if let Some(e) = self.places.get_mut(name)
                && e.map.take().is_some()
            {
                with_map -= 1;
            }
        }
    }
    /// Scene photos without a usable answer yet: never asked, or near with no map.
    fn missing(&self, names: &[String]) -> Vec<String> {
        names
            .iter()
            .filter(|n| match self.places.get(*n) {
                None => true,
                Some(e) => e.place.as_ref().is_some_and(|p| p.near) && e.map.is_none(),
            })
            .cloned()
            .collect()
    }
    /// Ask atlas about scene photos it has not answered for: at once for a new
    /// set, and again every `PLACE_RETRY_SECONDS` while answers are missing.
    pub fn scene(&mut self, names: &[String], now: f64) {
        let missing = self.missing(names);
        self.shown = names.to_vec();
        if missing.is_empty() || (missing == self.sent && now - self.sent_at < PLACE_RETRY_SECONDS) {
            return;
        }
        if let Some(tx) = &self.names
            && tx.try_send(missing.clone()).is_ok()
        {
            self.sent = missing;
            self.sent_at = now;
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
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        painter: &egui::Painter,
        origin: Pos2,
        split: &Split,
        palette: &Palette,
        sun: Sun,
        now: f64,
        survey_tex: Option<&TextureHandle>,
    ) {
        let hp_fresh = self.hp.is_some() && now - self.hp_at <= HONEYPOT_STALE_SECONDS;
        self.view = if hp_fresh { "honeypot" } else { "preparing" };
        let layer = if hp_fresh {
            self.hp.as_ref().map(|status| HoneypotLayer {
                status,
                globe: self.globe.as_ref().map(|(d, n)| (d, n)),
                sun,
            })
        } else {
            None
        };
        let survey_fresh = self.survey.is_some() && now - self.survey_at <= SURVEY_STALE_SECONDS;
        let history = self.motion_history.make_contiguous();
        let stats = if survey_fresh {
            self.survey.as_ref().map(|r| SurveyLayer {
                motion: r.motion,
                db: r.db,
                faces: r.faces,
                history,
            })
        } else {
            None
        };
        let panel = SurveyPanel {
            tex: survey_tex,
            stats,
        };
        let status = self.status.as_ref();
        let info = StripInfo {
            weather: status.and_then(|s| s.weather.as_ref()),
            rain: status.and_then(|s| s.rain.as_ref()),
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
            &info,
            layer.as_ref(),
            &panel,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex, mpsc};
    fn fixture() -> (Hud, AtlasSlot, mpsc::Receiver<Vec<String>>, egui::Context) {
        let slot: AtlasSlot = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, rx) = mpsc::sync_channel(16);
        (Hud::new(Some(slot.clone()), Some(tx), None, None), slot, rx, egui::Context::default())
    }
    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }
    fn place(near: bool) -> Option<atlas::Place> {
        Some(atlas::Place { place: "京都市".into(), lat: 35.0, lon: 135.75, taken: None, near })
    }
    fn image() -> egui::ColorImage {
        egui::ColorImage::new([2, 2], vec![egui::Color32::WHITE; 4])
    }
    #[test]
    fn asks_only_for_missing_places_and_retries_later() {
        let (mut hud, slot, rx, ctx) = fixture();
        hud.scene(&names(&["a.jpg", "b.jpg", "c.jpg"]), 0.0);
        assert_eq!(rx.try_recv().unwrap(), names(&["a.jpg", "b.jpg", "c.jpg"]));
        hud.scene(&names(&["a.jpg", "b.jpg", "c.jpg"]), 1.0);
        assert!(rx.try_recv().is_err(), "no repeat within the retry window");
        // a: no GPS (definite), b: near with its map. c never answered (atlas failed).
        slot.lock().unwrap().extend([
            AtlasEvent::Place { name: "a.jpg".into(), place: None, map: None },
            AtlasEvent::Place { name: "b.jpg".into(), place: place(true), map: Some(image()) },
        ]);
        hud.receive(&ctx, 2.0);
        assert!(hud.place("b.jpg").is_some() && hud.place("a.jpg").is_none());
        hud.scene(&names(&["a.jpg", "b.jpg", "c.jpg"]), 3.0);
        assert_eq!(rx.try_recv().unwrap(), names(&["c.jpg"]), "only the unanswered one");
        hud.scene(&names(&["a.jpg", "b.jpg", "c.jpg"]), 10.0);
        assert!(rx.try_recv().is_err());
        hud.scene(&names(&["a.jpg", "b.jpg", "c.jpg"]), 3.0 + PLACE_RETRY_SECONDS);
        assert_eq!(rx.try_recv().unwrap(), names(&["c.jpg"]), "retried");
    }
    #[test]
    fn keeps_few_place_maps_and_refetches_dropped_ones() {
        let (mut hud, slot, rx, ctx) = fixture();
        for i in 0..12 {
            slot.lock().unwrap().push_back(AtlasEvent::Place {
                name: format!("{i}.jpg"),
                place: place(true),
                map: Some(image()),
            });
            hud.receive(&ctx, i as f64);
        }
        let maps = hud.places.values().filter(|e| e.map.is_some()).count();
        assert_eq!(maps, PLACE_MAPS);
        assert!(hud.places["0.jpg"].map.is_none() && hud.places["11.jpg"].map.is_some());
        hud.scene(&names(&["0.jpg", "11.jpg"]), 20.0);
        assert_eq!(rx.try_recv().unwrap(), names(&["0.jpg"]), "dropped map is fetched again");
    }
    #[test]
    fn stale_weather_and_rain_are_cleared() {
        let (mut hud, slot, _rx, ctx) = fixture();
        let status = atlas::parse_status(
            r#"{"generated_at":"x","weather":{"mood":"rain","label":"雨","max":20,"min":15},
                "rain":{"summary":"雨","level":2,"series":[2,2,2,2,2,2,2,2,2,2,2,2]},
                "views":{"globe":null,"japan":null,"local":null},"credits":[]}"#
                .as_bytes(),
        )
        .unwrap();
        slot.lock().unwrap().push_back(AtlasEvent::Status(status));
        hud.receive(&ctx, 100.0);
        assert!(hud.rain_expected());
        hud.receive(&ctx, 100.0 + STATUS_STALE_SECONDS + 1.0);
        let s = hud.status.as_ref().unwrap();
        assert!(s.weather.is_none() && s.rain.is_none() && !hud.rain_expected());
        assert_eq!(hud.state, "unavailable");
    }
}

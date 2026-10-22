use crate::{
    decode::{self, DecodedPhoto},
    http,
    layout::{self, Layout},
    playlist::{self, Playlist},
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Screen-mirrored left/right input is mapped by the caller, not by the source worker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Navigation {
    #[default]
    Next,
    Previous,
}
#[derive(Clone, Copy)]
pub struct Request {
    pub refresh: bool,
    pub navigation: Navigation,
    pub width: f32,
    pub height: f32,
    pub camera: bool,
}
#[derive(Clone, Copy, Default)]
pub struct Counts {
    pub photos: usize,
    pub skipped_videos: usize,
    pub rejected_entries: usize,
    pub generation: u64,
    pub loaded_total: u64,
    pub decode_failures: u64,
    pub history_len: usize,
    pub history_position: usize,
    pub scene_id: u64,
}
pub enum Outcome {
    Scene {
        photos: Vec<DecodedPhoto>,
        layout: Layout,
        viewport: [f32; 2],
        counts: Counts,
    },
    Empty(Counts),
    Metadata(Counts),
    NoChange {
        reason: &'static str,
        counts: Counts,
    },
    Failed {
        reason: &'static str,
        counts: Counts,
    },
}
pub struct PhotoWorker {
    pub tx: SyncSender<Request>,
    pub rx: Receiver<Outcome>,
    pub handle: JoinHandle<()>,
}
trait Source {
    fn list(&mut self) -> Result<Playlist, &'static str>;
    fn photo(&mut self, name: &str) -> Result<DecodedPhoto, &'static str>;
}
struct LoopbackSource {
    port: u16,
    backdrops: bool,
}
impl Source for LoopbackSource {
    fn list(&mut self) -> Result<Playlist, &'static str> {
        let response = http::get(self.port, "/api/photos", playlist::MAX_JSON_BYTES)?;
        http::require_type(&response, &["application/json"])?;
        playlist::parse(&response.body)
    }
    fn photo(&mut self, name: &str) -> Result<DecodedPhoto, &'static str> {
        let response = http::get(
            self.port,
            &playlist::photo_path(name)?,
            decode::MAX_IMAGE_BYTES,
        )?;
        http::require_type(&response, &["image/jpeg", "image/png"])?;
        let mut photo = decode::decode(&response.body, self.backdrops, false)?;
        photo.name = name.to_string();
        Ok(photo)
    }
}
const HISTORY_LIMIT: usize = 32;
const SCENE_PHOTO_LIMIT: usize = 4;

// Process-lifetime metadata only: never a decoded image/texture or an image-byte cache.
// Names are strict validated basenames, in the returned photo vector's order; placement
// indices are a permutation of that vector. At most 32 * 4 * 255 basename bytes.
struct SavedScene {
    names: Vec<String>,
    layout: Layout,
    viewport: [f32; 2],
    scene_id: u64,
}
#[derive(Default)]
struct PhotoState {
    playlist: Playlist,
    // Fresh-source cursor is independent of the currently displayed history position.
    cursor: usize,
    generation: u64,
    loaded_total: u64,
    decode_failures: u64,
    previous_layout: &'static str,
    committed: bool,
    history: VecDeque<SavedScene>,
    history_position: usize,
    last_scene_id: u64,
}
fn valid_viewport([width, height]: [f32; 2]) -> bool {
    width.is_finite() && height.is_finite() && width >= 240.0 && height >= 180.0
}
fn camera_exclusion(request: Request) -> Option<layout::Rect> {
    request
        .camera
        .then(|| layout::camera_rect(request.width, request.height).inset(-10.0))
}
fn valid_layout(layout: &Layout, aspects: &[f32], request: Request) -> bool {
    if !valid_viewport([request.width, request.height])
        || aspects.is_empty()
        || aspects.len() > SCENE_PHOTO_LIMIT
        || layout.placements.len() != aspects.len()
        || !layout.score.is_finite()
    {
        return false;
    }
    let inside = |r: layout::Rect, container: layout::Rect| {
        [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite())
            && r.w > 0.0
            && r.h > 0.0
            && r.x >= container.x - 0.05
            && r.y >= container.y - 0.05
            && r.x + r.w <= container.x + container.w + 0.05
            && r.y + r.h <= container.y + container.h + 0.05
    };
    let bounds = layout::Rect {
        x: 0.0,
        y: 80.0,
        w: request.width,
        h: request.height - 128.0,
    };
    let camera = camera_exclusion(request);
    let mut seen = [false; SCENE_PHOTO_LIMIT];
    for p in &layout.placements {
        if p.index >= aspects.len() || seen[p.index] {
            return false;
        }
        seen[p.index] = true;
        let aspect = aspects[p.index];
        if !aspect.is_finite()
            || aspect <= 0.0
            || !inside(p.card, bounds)
            || !inside(p.foreground, p.card)
            || (p.foreground.w / p.foreground.h - aspect).abs() > 0.001 * aspect.max(1.0)
            || camera.is_some_and(|c| layout::intersects(p.foreground, c))
        {
            return false;
        }
    }
    true
}
fn restored_layout(saved: &SavedScene, aspects: &[f32], request: Request) -> Option<Layout> {
    let viewport = [request.width, request.height];
    if !valid_viewport(viewport) || !valid_viewport(saved.viewport) {
        return None;
    }
    let mut result = saved.layout.clone();
    if viewport != saved.viewport {
        // Resize the saved template rather than choose a smaller, different combination.
        // Map cards within the clock/footer-safe area, then refit each original photo.
        let old_outer = (saved.viewport[0] * 0.008).clamp(16.0, 36.0);
        let new_outer = (request.width * 0.008).clamp(16.0, 36.0);
        let sx = (request.width - 2.0 * new_outer) / (saved.viewport[0] - 2.0 * old_outer);
        let sy = (request.height - 128.0) / (saved.viewport[1] - 128.0);
        for p in &mut result.placements {
            let aspect = *aspects.get(p.index)?;
            p.card = layout::Rect {
                x: new_outer + (p.card.x - old_outer) * sx,
                y: 80.0 + (p.card.y - 80.0) * sy,
                w: p.card.w * sx,
                h: p.card.h * sy,
            };
            p.foreground = layout::safe_foreground(p.card, aspect, camera_exclusion(request));
        }
    }
    valid_layout(&result, aspects, request).then_some(result)
}
impl PhotoState {
    fn counts(&self) -> Counts {
        Counts {
            photos: self.playlist.photos.len(),
            skipped_videos: self.playlist.skipped_videos,
            rejected_entries: self.playlist.rejected_entries,
            generation: self.generation,
            loaded_total: self.loaded_total,
            decode_failures: self.decode_failures,
            history_len: self.history.len(),
            history_position: self.history_position,
            scene_id: self
                .history
                .get(self.history_position)
                .map_or(0, |s| s.scene_id),
        }
    }
    fn clear_history(&mut self) {
        self.history.clear();
        self.history_position = 0;
    }
    fn restore(
        &mut self,
        position: usize,
        request: Request,
        source: &mut impl Source,
        stop: &AtomicBool,
    ) -> Outcome {
        let saved = &self.history[position];
        let mut photos = Vec::with_capacity(saved.names.len());
        for name in &saved.names {
            if stop.load(Ordering::Relaxed) {
                return Outcome::Failed {
                    reason: "stopping",
                    counts: self.counts(),
                };
            }
            // Defense in depth: history cannot turn a basename into a URL/file API.
            if playlist::kind(name) != Some(playlist::Kind::Photo) {
                return Outcome::Failed {
                    reason: "invalid_photo_name",
                    counts: self.counts(),
                };
            }
            match source.photo(name) {
                Ok(photo) => {
                    photos.push(photo);
                    self.loaded_total = self.loaded_total.saturating_add(1);
                }
                Err(_) => {
                    self.decode_failures = self.decode_failures.saturating_add(1);
                    // No partial replacement, skipped name, fresh-cursor move or pointer move.
                    return Outcome::Failed {
                        reason: "history_photo_unavailable",
                        counts: self.counts(),
                    };
                }
            }
        }
        if stop.load(Ordering::Relaxed) {
            return Outcome::Failed {
                reason: "stopping",
                counts: self.counts(),
            };
        }
        let aspects: Vec<_> = photos.iter().map(|p| p.aspect).collect();
        let Some(layout) = restored_layout(saved, &aspects, request) else {
            return Outcome::Failed {
                reason: "history_layout_inconsistent",
                counts: self.counts(),
            };
        };
        self.history_position = position;
        self.previous_layout = layout.name;
        Outcome::Scene {
            photos,
            layout,
            viewport: [request.width, request.height],
            counts: self.counts(),
        }
    }
    fn process(
        &mut self,
        request: Request,
        source: &mut impl Source,
        stop: &AtomicBool,
    ) -> Outcome {
        // Refresh always wins over navigation, including Previous.
        let candidate = if request.refresh {
            match source.list() {
                Ok(p) => Some(p),
                Err(reason) => {
                    return Outcome::Failed {
                        reason,
                        counts: self.counts(),
                    };
                }
            }
        } else {
            None
        };
        if stop.load(Ordering::Relaxed) {
            return Outcome::Failed {
                reason: "stopping",
                counts: self.counts(),
            };
        }
        if let Some(p) = &candidate {
            if p.photos.is_empty() {
                if !self.committed || !self.playlist.photos.is_empty() {
                    self.generation = self.generation.saturating_add(1);
                }
                self.playlist = p.clone();
                self.cursor = 0;
                self.previous_layout = "";
                self.clear_history();
                self.committed = true;
                return Outcome::Empty(self.counts());
            }
            if self.committed && p.photos == self.playlist.photos {
                self.playlist.skipped_videos = p.skipped_videos;
                self.playlist.rejected_entries = p.rejected_entries;
                return Outcome::Metadata(self.counts());
            }
        } else {
            if self.history.is_empty() {
                return Outcome::NoChange {
                    reason: "history_empty",
                    counts: self.counts(),
                };
            }
            match request.navigation {
                Navigation::Previous if self.history_position == 0 => {
                    return Outcome::NoChange {
                        reason: "history_start",
                        counts: self.counts(),
                    };
                }
                Navigation::Previous => {
                    return self.restore(self.history_position - 1, request, source, stop);
                }
                Navigation::Next if self.history_position + 1 < self.history.len() => {
                    return self.restore(self.history_position + 1, request, source, stop);
                }
                Navigation::Next => {}
            }
        }
        let playlist = candidate.as_ref().unwrap_or(&self.playlist);
        let cursor = if candidate.is_some() { 0 } else { self.cursor };
        let indices = playlist::batch_indices(playlist.photos.len(), cursor);
        let mut photos = Vec::with_capacity(indices.len());
        let mut names = Vec::with_capacity(indices.len());
        let mut positions = Vec::with_capacity(indices.len());
        for (position, index) in indices.iter().enumerate() {
            if stop.load(Ordering::Relaxed) {
                return Outcome::Failed {
                    reason: "stopping",
                    counts: self.counts(),
                };
            }
            let name = &playlist.photos[*index];
            let result = if playlist::kind(name) == Some(playlist::Kind::Photo) {
                source.photo(name)
            } else {
                Err("invalid_photo_name")
            };
            match result {
                Ok(photo) => {
                    photos.push(photo);
                    names.push(name.clone());
                    positions.push(position);
                    self.loaded_total = self.loaded_total.saturating_add(1);
                }
                Err(_) => self.decode_failures = self.decode_failures.saturating_add(1),
            }
        }
        if stop.load(Ordering::Relaxed) {
            return Outcome::Failed {
                reason: "stopping",
                counts: self.counts(),
            };
        }
        let aspects: Vec<_> = photos.iter().map(|p| p.aspect).collect();
        let chosen = valid_viewport([request.width, request.height])
            .then(|| {
                layout::choose(
                    &aspects,
                    request.width,
                    request.height,
                    camera_exclusion(request),
                    self.previous_layout,
                )
            })
            .flatten()
            // choose selects the first count successful decodes; its placements permute
            // ONLY 0..count. Check this contract before truncating either photos or names.
            .filter(|l| {
                l.placements.len() <= aspects.len()
                    && valid_layout(l, &aspects[..l.placements.len()], request)
            });
        let Some(layout) = chosen else {
            if candidate.is_none() && !self.playlist.photos.is_empty() {
                self.cursor = (cursor + indices.len()) % self.playlist.photos.len();
            }
            return Outcome::Failed {
                reason: "no_usable_photos",
                counts: self.counts(),
            };
        };
        let count = layout.placements.len();
        photos.truncate(count);
        names.truncate(count);
        let advance = positions[count - 1] + 1;
        if let Some(candidate) = candidate {
            let committed = playlist::commit_refresh(&mut self.playlist, candidate, count);
            debug_assert!(committed);
            self.generation = self.generation.saturating_add(1);
            self.committed = true;
            // Drop all previous-list basenames only once the replacement scene is usable.
            self.clear_history();
        }
        self.cursor = (cursor + advance) % self.playlist.photos.len();
        self.previous_layout = layout.name;
        self.last_scene_id = self.last_scene_id.saturating_add(1);
        if self.history.len() == HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history.push_back(SavedScene {
            names,
            layout: layout.clone(),
            viewport: [request.width, request.height],
            scene_id: self.last_scene_id,
        });
        self.history_position = self.history.len() - 1;
        Outcome::Scene {
            photos,
            layout,
            viewport: [request.width, request.height],
            counts: self.counts(),
        }
    }
}
pub fn start_photos(
    port: u16,
    backdrops: bool,
    stop: Arc<AtomicBool>,
) -> std::io::Result<PhotoWorker> {
    let (tx, commands) = mpsc::sync_channel::<Request>(1);
    let (results, rx) = mpsc::sync_channel(1);
    let handle = thread::Builder::new()
        .name("photo-source".into())
        .spawn(move || {
            let mut state = PhotoState::default();
            let mut source = LoopbackSource { port, backdrops };
            while !stop.load(Ordering::Relaxed) {
                match commands.recv_timeout(Duration::from_millis(100)) {
                    Ok(request) => {
                        let result = state.process(request, &mut source, &stop);
                        if stop.load(Ordering::Relaxed) || results.try_send(result).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })?;
    Ok(PhotoWorker { tx, rx, handle })
}

pub struct CameraFrame {
    pub photo: DecodedPhoto,
    pub captured: Instant,
    pub sequence: u64,
}
pub enum CameraEvent {
    Frame(CameraFrame),
    Unavailable(&'static str),
}
pub type CameraSlot = Arc<Mutex<Option<CameraEvent>>>;
pub const CAMERA_STALE: Duration = Duration::from_secs(3);
pub fn camera_freshness(
    response: &http::Response,
    elapsed: Duration,
    previous: Option<u64>,
) -> Result<(Instant, u64), &'static str> {
    let sequence = response
        .header("X-Frame-Sequence")
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or("camera_sequence_missing")?;
    let age = response
        .header("X-Frame-Age-Seconds")
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 60.0)
        .ok_or("camera_timestamp_unverified")?;
    let age = Duration::from_secs_f64(age).saturating_add(elapsed);
    if age >= CAMERA_STALE {
        return Err("camera_stale");
    }
    if sequence == 0 || previous.is_some_and(|last| sequence <= last) {
        return Err("camera_sequence_frozen_or_reversed");
    }
    let captured = Instant::now()
        .checked_sub(age)
        .ok_or("camera_timestamp_invalid")?;
    Ok((captured, sequence))
}
pub type CameraWake = Arc<Mutex<Option<eframe::egui::Context>>>;
pub fn start_camera(
    stop: Arc<AtomicBool>,
    wake: CameraWake,
    port: u16,
) -> std::io::Result<(CameraSlot, JoinHandle<()>)> {
    let slot = Arc::new(Mutex::new(None));
    let producer = slot.clone();
    let handle = thread::Builder::new()
        .name("shared-mjpeg-preview".into())
        .spawn(move || {
            let mut previous;
            while !stop.load(Ordering::Relaxed) {
                // A reconnected producer may restart its sequence; accept it again.
                previous = None;
                let mut stream = crate::mjpeg::PreviewStream::open(port, &stop).ok();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let result = stream
                        .as_mut()
                        .ok_or("camera_preview_unavailable")
                        .and_then(|s| s.next(previous, &stop));
                    let failed = result.is_err();
                    let event = match result {
                        Ok(frame) => {
                            previous = Some(frame.sequence);
                            CameraEvent::Frame(CameraFrame {
                                photo: frame.photo,
                                captured: frame.captured,
                                sequence: frame.sequence,
                            })
                        }
                        Err(e) => CameraEvent::Unavailable(e),
                    };
                    if let Ok(mut current) = producer.lock() {
                        *current = Some(event);
                    }
                    // One overwritten latest slot. No backlog even if rendering lags.
                    if let Ok(ctx) = wake.lock()
                        && let Some(ctx) = ctx.as_ref()
                    {
                        ctx.request_repaint();
                    }
                    if failed {
                        break;
                    }
                }
                let backoff = Instant::now();
                while backoff.elapsed() < Duration::from_millis(500)
                    && !stop.load(Ordering::Relaxed)
                {
                    thread::sleep(Duration::from_millis(25));
                }
            }
            if let Ok(mut current) = producer.lock() {
                *current = None;
            }
        })?;
    Ok((slot, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        list: Result<Playlist, &'static str>,
        fail_photos: bool,
    }
    impl Source for Fake {
        fn list(&mut self) -> Result<Playlist, &'static str> {
            self.list.clone()
        }
        fn photo(&mut self, _: &str) -> Result<DecodedPhoto, &'static str> {
            if self.fail_photos {
                Err("failed")
            } else {
                Ok(DecodedPhoto {
                    image: eframe::egui::ColorImage::new(
                        [2, 2],
                        vec![eframe::egui::Color32::WHITE; 4],
                    ),
                    backdrop: None,
                    aspect: 1.0,
                    name: String::new(),
                })
            }
        }
    }
    fn request() -> Request {
        Request {
            refresh: true,
            navigation: Navigation::Next,
            width: 1920.0,
            height: 1080.0,
            camera: true,
        }
    }
    #[test]
    fn refresh_and_decode_failures_retain_committed_playlist_then_empty_clears() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Fake {
            list: playlist::parse(br#"["a.jpg","b.jpg"]"#),
            fail_photos: false,
        };
        assert!(matches!(
            s.process(request(), &mut f, &stop),
            Outcome::Scene { .. }
        ));
        let old = s.playlist.clone();
        let generation = s.generation;
        f.list = Err("bad_response");
        assert!(matches!(
            s.process(request(), &mut f, &stop),
            Outcome::Failed { .. }
        ));
        assert_eq!(s.playlist, old);
        f.list = playlist::parse(br#"["new.jpg"]"#);
        f.fail_photos = true;
        assert!(matches!(
            s.process(request(), &mut f, &stop),
            Outcome::Failed { .. }
        ));
        assert_eq!(s.playlist, old);
        assert_eq!(s.generation, generation);
        f.list = playlist::parse(b"[]");
        assert!(matches!(
            s.process(request(), &mut f, &stop),
            Outcome::Empty(_)
        ));
        assert!(s.playlist.photos.is_empty());
        assert_eq!(s.generation, generation + 1);
    }
    #[test]
    fn unchanged_refresh_does_not_reload_or_reset_playlist_cursor() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Fake {
            list: playlist::parse(br#"["a.jpg","b.jpg","c.jpg","d.jpg","e.jpg"]"#),
            fail_photos: false,
        };
        s.process(request(), &mut f, &stop);
        let cursor = s.cursor;
        let loaded = s.loaded_total;
        f.fail_photos = true;
        assert!(matches!(
            s.process(request(), &mut f, &stop),
            Outcome::Metadata(_)
        ));
        assert_eq!(cursor, s.cursor);
        assert_eq!(loaded, s.loaded_total);
    }
    fn navigate(navigation: Navigation) -> Request {
        Request {
            refresh: false,
            navigation,
            ..request()
        }
    }
    struct Scripted {
        list: Result<Playlist, &'static str>,
        list_calls: usize,
        calls: Vec<String>,
        aspects: std::collections::HashMap<String, f32>,
        identities: std::collections::HashMap<String, u8>,
        failures: std::collections::HashSet<String>,
        stop_after: Option<(usize, Arc<AtomicBool>)>,
    }
    impl Scripted {
        fn new(length: usize) -> Self {
            let names: Vec<_> = (0..length).map(|n| format!("{n}.jpg")).collect();
            Self {
                list: playlist::parse(&serde_json::to_vec(&names).unwrap()),
                list_calls: 0,
                calls: Vec::new(),
                aspects: Default::default(),
                identities: names
                    .into_iter()
                    .enumerate()
                    .map(|(n, s)| (s, n as u8))
                    .collect(),
                failures: Default::default(),
                stop_after: None,
            }
        }
    }
    impl Source for Scripted {
        fn list(&mut self) -> Result<Playlist, &'static str> {
            self.list_calls += 1;
            self.list.clone()
        }
        fn photo(&mut self, name: &str) -> Result<DecodedPhoto, &'static str> {
            self.calls.push(name.to_owned());
            if let Some((limit, stop)) = &self.stop_after
                && self.calls.len() >= *limit
            {
                stop.store(true, Ordering::Relaxed);
            }
            if self.failures.contains(name) {
                return Err("deleted_or_fetch_failed");
            }
            let id = self.identities.get(name).copied().unwrap_or(250);
            Ok(DecodedPhoto {
                image: eframe::egui::ColorImage::new(
                    [2, 2],
                    vec![eframe::egui::Color32::from_rgb(id, 17, 29); 4],
                ),
                backdrop: None,
                aspect: self.aspects.get(name).copied().unwrap_or(1.0),
                name: name.to_string(),
            })
        }
    }
    #[derive(Debug, PartialEq)]
    struct Fingerprint {
        pixels: Vec<eframe::egui::Color32>,
        template: &'static str,
        placements: Vec<(usize, layout::Rect, layout::Rect)>,
        viewport: [f32; 2],
        scene_id: u64,
    }
    fn fingerprint(outcome: Outcome) -> Fingerprint {
        let Outcome::Scene {
            photos,
            layout,
            viewport,
            counts,
        } = outcome
        else {
            panic!("expected a complete scene");
        };
        Fingerprint {
            pixels: photos.iter().map(|p| p.image.pixels[0]).collect(),
            template: layout.name,
            placements: layout
                .placements
                .iter()
                .map(|p| (p.index, p.card, p.foreground))
                .collect(),
            viewport,
            scene_id: counts.scene_id,
        }
    }
    fn history_state(s: &PhotoState) -> (usize, usize, u64, usize, u64, Vec<Vec<String>>) {
        (
            s.history.len(),
            s.history_position,
            s.counts().scene_id,
            s.cursor,
            s.generation,
            s.history.iter().map(|v| v.names.clone()).collect(),
        )
    }
    // Force the real chooser to use 2/3/4 of FOUR successful decodes, not merely
    // different playlist lengths. No production selection override is added for tests.
    fn force_count(s: &PhotoState, f: &mut Scripted, count: usize) {
        let request = request();
        let options: [f32; 8] = [0.01, 0.15, 0.4, 0.65, 1.0, 1.5, 3.0, 100.0];
        let mut found = None;
        for code in 0..4096usize {
            let aspects: Vec<_> = (0..4).map(|n| options[(code >> (n * 3)) & 7]).collect();
            if layout::choose(
                &aspects,
                request.width,
                request.height,
                camera_exclusion(request),
                s.previous_layout,
            )
            .is_some_and(|l| l.placements.len() == count)
            {
                found = Some(aspects);
                break;
            }
        }
        let aspects =
            found.unwrap_or_else(|| panic!("fixture must exercise template size {count}"));
        let playlist = if s.committed {
            &s.playlist
        } else {
            f.list.as_ref().unwrap()
        };
        let cursor = if s.committed { s.cursor } else { 0 };
        for (index, aspect) in playlist::batch_indices(playlist.photos.len(), cursor)
            .into_iter()
            .zip(aspects)
        {
            f.aspects.insert(playlist.photos[index].clone(), aspect);
        }
    }
    #[test]
    fn navigation_boundaries_are_noops_without_fetches() {
        assert_eq!(Navigation::default(), Navigation::Next);
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Scripted::new(8);
        for direction in [Navigation::Previous, Navigation::Next] {
            assert!(matches!(
                s.process(navigate(direction), &mut f, &stop),
                Outcome::NoChange {
                    reason: "history_empty",
                    counts: Counts {
                        history_len: 0,
                        history_position: 0,
                        scene_id: 0,
                        ..
                    }
                }
            ));
        }
        assert!(f.calls.is_empty());
        assert_eq!(f.list_calls, 0);
        fingerprint(s.process(request(), &mut f, &stop));
        let before = history_state(&s);
        let calls = f.calls.len();
        assert!(matches!(
            s.process(navigate(Navigation::Previous), &mut f, &stop),
            Outcome::NoChange {
                reason: "history_start",
                ..
            }
        ));
        assert_eq!(history_state(&s), before);
        assert_eq!(f.calls.len(), calls);
    }
    #[test]
    fn variable_layouts_roundtrip_actual_selected_names_and_preserve_fresh_cursor() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Scripted::new(16);
        let mut scenes = Vec::new();
        for count in [2, 3, 4] {
            force_count(&s, &mut f, count);
            let cursor = s.cursor;
            let old_calls = f.calls.len();
            let r = if s.committed {
                navigate(Navigation::Next)
            } else {
                request()
            };
            let scene = fingerprint(s.process(r, &mut f, &stop));
            assert_eq!(scene.pixels.len(), count);
            assert_eq!(f.calls.len() - old_calls, 4);
            let expected: Vec<_> = (cursor..cursor + count)
                .map(|i| format!("{i}.jpg"))
                .collect();
            assert_eq!(s.history.back().unwrap().names, expected);
            assert_eq!(s.cursor, cursor + count);
            scenes.push(scene);
        }
        let cursor = s.cursor;
        let generation = s.generation;
        for (direction, target) in [
            (Navigation::Previous, 1),
            (Navigation::Previous, 0),
            (Navigation::Next, 1),
            (Navigation::Next, 2),
        ] {
            let old_calls = f.calls.len();
            assert_eq!(
                fingerprint(s.process(navigate(direction), &mut f, &stop)),
                scenes[target]
            );
            assert_eq!(&f.calls[old_calls..], s.history[target].names);
            assert_eq!(s.cursor, cursor);
            assert_eq!(s.generation, generation);
            assert_eq!(s.history.len(), 3);
            assert_eq!(s.history_position, target);
            assert_eq!(s.last_scene_id, 3);
        }
        let old_calls = f.calls.len();
        let scene = fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop));
        assert_eq!(scene.scene_id, 4);
        assert_eq!(f.calls[old_calls], format!("{cursor}.jpg"));
        assert!(f.calls.len() - old_calls <= 4);
    }
    #[test]
    fn partial_decode_stores_only_selected_successful_names_and_positions() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Scripted::new(12);
        f.failures.extend(["0.jpg".to_owned(), "2.jpg".to_owned()]);
        let first = fingerprint(s.process(request(), &mut f, &stop));
        assert_eq!(s.history[0].names, ["1.jpg", "3.jpg"]);
        assert_eq!(s.cursor, 4);
        assert_eq!(
            first.pixels.iter().map(|p| p.r()).collect::<Vec<_>>(),
            [1, 3]
        );
        fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop));
        let calls = f.calls.len();
        assert_eq!(
            fingerprint(s.process(navigate(Navigation::Previous), &mut f, &stop)),
            first
        );
        assert_eq!(&f.calls[calls..], ["1.jpg", "3.jpg"]);
        assert_eq!(s.decode_failures, 2);
    }
    #[test]
    fn history_is_bounded_and_front_eviction_corrects_position() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Scripted::new(8);
        for id in 1..=40 {
            let r = if id == 1 {
                request()
            } else {
                navigate(Navigation::Next)
            };
            let calls = f.calls.len();
            let scene = fingerprint(s.process(r, &mut f, &stop));
            assert_eq!(scene.scene_id, id);
            assert_eq!(s.history.len(), (id as usize).min(HISTORY_LIMIT));
            assert_eq!(s.history_position, s.history.len() - 1);
            assert!(f.calls.len() - calls <= SCENE_PHOTO_LIMIT);
            assert!(
                s.history
                    .iter()
                    .all(|v| v.names.len() <= 4 && v.names.iter().all(|n| n.len() <= 255))
            );
        }
        assert_eq!(s.history.front().unwrap().scene_id, 9);
        for id in (9..40).rev() {
            assert_eq!(
                fingerprint(s.process(navigate(Navigation::Previous), &mut f, &stop)).scene_id,
                id
            );
        }
        assert_eq!(s.history_position, 0);
        assert!(matches!(
            s.process(navigate(Navigation::Previous), &mut f, &stop),
            Outcome::NoChange {
                reason: "history_start",
                ..
            }
        ));
        assert_eq!(s.history.len(), 32);
    }
    #[test]
    fn unchanged_refresh_preserves_rewound_pointer_and_ignores_navigation() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Scripted::new(16);
        fingerprint(s.process(request(), &mut f, &stop));
        fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop));
        fingerprint(s.process(navigate(Navigation::Previous), &mut f, &stop));
        let before = history_state(&s);
        let calls = f.calls.len();
        let loaded = s.loaded_total;
        f.list.as_mut().unwrap().skipped_videos = 7;
        f.list.as_mut().unwrap().rejected_entries = 2;
        assert!(matches!(
            s.process(
                Request {
                    navigation: Navigation::Previous,
                    ..request()
                },
                &mut f,
                &stop
            ),
            Outcome::Metadata(Counts {
                skipped_videos: 7,
                rejected_entries: 2,
                ..
            })
        ));
        assert_eq!(history_state(&s), before);
        assert_eq!(f.calls.len(), calls);
        assert_eq!(s.loaded_total, loaded);
        assert_eq!(f.list_calls, 2);
    }
    #[test]
    fn changed_refresh_is_transactional_empty_clears_and_ids_do_not_restart() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Scripted::new(12);
        fingerprint(s.process(request(), &mut f, &stop));
        fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop));
        fingerprint(s.process(navigate(Navigation::Previous), &mut f, &stop));
        let before = history_state(&s);
        let old_playlist = s.playlist.clone();
        f.list = Err("list_unavailable");
        assert!(matches!(
            s.process(request(), &mut f, &stop),
            Outcome::Failed {
                reason: "list_unavailable",
                ..
            }
        ));
        assert_eq!(history_state(&s), before);
        f.list = playlist::parse(br#"["new0.jpg","new1.jpg"]"#);
        f.failures
            .extend(["new0.jpg".to_owned(), "new1.jpg".to_owned()]);
        assert!(matches!(
            s.process(request(), &mut f, &stop),
            Outcome::Failed {
                reason: "no_usable_photos",
                ..
            }
        ));
        assert_eq!(history_state(&s), before);
        assert_eq!(s.playlist, old_playlist);
        f.failures.clear();
        assert!(matches!(
            s.process(
                Request {
                    width: 100.0,
                    ..request()
                },
                &mut f,
                &stop
            ),
            Outcome::Failed { .. }
        ));
        assert_eq!(history_state(&s), before);
        let new = fingerprint(s.process(
            Request {
                navigation: Navigation::Previous,
                ..request()
            },
            &mut f,
            &stop,
        ));
        assert_eq!(new.scene_id, 3);
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.history_position, 0);
        assert_eq!(s.history[0].names, ["new0.jpg", "new1.jpg"]);
        assert_eq!(s.playlist.photos, ["new0.jpg", "new1.jpg"]);
        assert_eq!(s.generation, 2);
        f.list = playlist::parse(br#"["movie.mp4"]"#);
        assert!(matches!(
            s.process(request(), &mut f, &stop),
            Outcome::Empty(Counts {
                history_len: 0,
                history_position: 0,
                scene_id: 0,
                photos: 0,
                skipped_videos: 1,
                ..
            })
        ));
        assert!(s.history.is_empty());
        assert!(s.playlist.photos.is_empty());
        assert_eq!(s.cursor, 0);
        assert_eq!(s.generation, 3);
        f.list = playlist::parse(br#"["after.jpg"]"#);
        assert_eq!(fingerprint(s.process(request(), &mut f, &stop)).scene_id, 4);
    }
    #[test]
    fn deleted_history_photo_and_partial_back_redo_failure_retain_full_scene() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Scripted::new(16);
        force_count(&s, &mut f, 4);
        let first = fingerprint(s.process(request(), &mut f, &stop));
        force_count(&s, &mut f, 4);
        let current = fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop));
        let before = history_state(&s);
        let old_playlist = s.playlist.clone();
        let deleted = s.history[0].names[1].clone();
        f.failures.insert(deleted.clone());
        let calls = f.calls.len();
        assert!(matches!(
            s.process(navigate(Navigation::Previous), &mut f, &stop),
            Outcome::Failed {
                reason: "history_photo_unavailable",
                counts: Counts {
                    history_position: 1,
                    scene_id: 2,
                    ..
                }
            }
        ));
        assert_eq!(f.calls.len() - calls, 2); // Stop immediately, not a partial 3-photo scene.
        assert_eq!(history_state(&s), before);
        assert_eq!(s.playlist, old_playlist);
        assert_eq!(s.counts().scene_id, current.scene_id);
        f.failures.remove(&deleted);
        assert_eq!(
            fingerprint(s.process(navigate(Navigation::Previous), &mut f, &stop)),
            first
        );
        let before_redo = history_state(&s);
        f.failures.insert(s.history[1].names[3].clone());
        let calls = f.calls.len();
        assert!(matches!(
            s.process(navigate(Navigation::Next), &mut f, &stop),
            Outcome::Failed { .. }
        ));
        assert_eq!(f.calls.len() - calls, 4);
        assert_eq!(history_state(&s), before_redo);
        f.failures.insert(s.history[1].names[0].clone());
        let calls = f.calls.len();
        assert!(matches!(
            s.process(navigate(Navigation::Next), &mut f, &stop),
            Outcome::Failed { .. }
        ));
        assert_eq!(f.calls.len() - calls, 1);
        assert_eq!(history_state(&s), before_redo);
        f.failures.clear();
        assert_eq!(
            fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop)),
            current
        );
    }
    #[test]
    fn changed_aspect_or_invalid_layout_cannot_partially_restore() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Scripted::new(12);
        fingerprint(s.process(request(), &mut f, &stop));
        fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop));
        let before = history_state(&s);
        let name = s.history[0].names[0].clone();
        f.aspects.insert(name.clone(), 2.75);
        assert!(matches!(
            s.process(navigate(Navigation::Previous), &mut f, &stop),
            Outcome::Failed {
                reason: "history_layout_inconsistent",
                ..
            }
        ));
        assert_eq!(history_state(&s), before);
        f.aspects.remove(&name);
        s.history[0].layout.placements[0].foreground.x = -100.0;
        assert!(matches!(
            s.process(navigate(Navigation::Previous), &mut f, &stop),
            Outcome::Failed {
                reason: "history_layout_inconsistent",
                ..
            }
        ));
        assert_eq!(history_state(&s), before);
    }
    #[test]
    fn resize_restores_all_original_photos_and_template_without_camera_overlap() {
        let stop = AtomicBool::new(false);
        for count in [2, 3, 4] {
            let mut s = PhotoState::default();
            let mut f = Scripted::new(16);
            force_count(&s, &mut f, count);
            let first = fingerprint(s.process(request(), &mut f, &stop));
            fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop));
            for [width, height] in [
                [640.0, 360.0],
                [960.0, 540.0],
                [3840.0, 2160.0],
                [540.0, 960.0],
            ] {
                let r = Request {
                    width,
                    height,
                    ..navigate(Navigation::Previous)
                };
                let out = s.process(r, &mut f, &stop);
                let Outcome::Scene { photos, layout, .. } = &out else {
                    panic!("resized history must restore")
                };
                assert_eq!(photos.len(), count);
                assert!(valid_layout(
                    layout,
                    &photos.iter().map(|p| p.aspect).collect::<Vec<_>>(),
                    r
                ));
                let resized = fingerprint(out);
                assert_eq!(resized.pixels, first.pixels);
                assert_eq!(resized.template, first.template);
                assert_eq!(resized.scene_id, first.scene_id);
                assert_eq!(resized.viewport, [width, height]);
                assert_eq!(s.history[0].viewport, first.viewport);
                fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop));
            }
            assert_eq!(
                fingerprint(s.process(navigate(Navigation::Previous), &mut f, &stop)),
                first
            );
        }
    }
    #[test]
    fn failed_fresh_batch_skips_cursor_but_never_records_or_moves_history() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let mut f = Scripted::new(16);
        fingerprint(s.process(request(), &mut f, &stop));
        let before = history_state(&s);
        let old_cursor = s.cursor;
        f.failures.extend(s.playlist.photos.clone());
        let calls = f.calls.len();
        assert!(matches!(
            s.process(navigate(Navigation::Next), &mut f, &stop),
            Outcome::Failed {
                reason: "no_usable_photos",
                ..
            }
        ));
        assert_eq!(s.cursor, (old_cursor + 4) % s.playlist.photos.len());
        let after = history_state(&s);
        assert_eq!(
            (after.0, after.1, after.2, after.4, after.5),
            (before.0, before.1, before.2, before.4, before.5)
        );
        assert_eq!(f.calls.len() - calls, 4);
    }
    #[test]
    fn shutdown_mid_or_final_decode_never_commits_or_consumes_history() {
        for restored in [false, true] {
            for after in [2, 4] {
                let stop = Arc::new(AtomicBool::new(false));
                let mut s = PhotoState::default();
                let mut f = Scripted::new(16);
                force_count(&s, &mut f, 4);
                fingerprint(s.process(request(), &mut f, &stop));
                force_count(&s, &mut f, 4);
                fingerprint(s.process(navigate(Navigation::Next), &mut f, &stop));
                let before = history_state(&s);
                let calls = f.calls.len();
                f.stop_after = Some((calls + after, stop.clone()));
                let direction = if restored {
                    Navigation::Previous
                } else {
                    Navigation::Next
                };
                assert!(matches!(
                    s.process(navigate(direction), &mut f, &stop),
                    Outcome::Failed {
                        reason: "stopping",
                        ..
                    }
                ));
                assert_eq!(f.calls.len() - calls, after);
                assert_eq!(history_state(&s), before);
            }
        }
    }
    #[test]
    fn maximal_basenames_remain_metadata_bounded_and_replay_refetches() {
        let stop = AtomicBool::new(false);
        let mut s = PhotoState::default();
        let names: Vec<_> = (0..4)
            .map(|n| format!("{n}{}.jpg", "a".repeat(250)))
            .collect();
        assert!(names.iter().all(|n| n.len() == 255));
        let mut f = Scripted::new(0);
        f.list = playlist::parse(&serde_json::to_vec(&names).unwrap());
        for id in 1..=34 {
            let r = if id == 1 {
                request()
            } else {
                navigate(Navigation::Next)
            };
            fingerprint(s.process(r, &mut f, &stop));
        }
        assert!(
            s.history
                .iter()
                .flat_map(|h| &h.names)
                .map(String::len)
                .sum::<usize>()
                <= 32 * 4 * 255
        );
        let loaded = s.loaded_total;
        let calls = f.calls.len();
        let expected = s.history[s.history_position - 1].names.clone();
        fingerprint(s.process(navigate(Navigation::Previous), &mut f, &stop));
        assert_eq!(&f.calls[calls..], expected);
        assert_eq!(s.loaded_total - loaded, expected.len() as u64);
    }
    #[test]
    fn camera_rejects_stale_unverified_and_frozen() {
        let response = |headers: &str| {
            http::parse_response(format!("HTTP/1.1 200 OK\r\n{headers}\r\n").as_bytes(), 100)
                .unwrap()
        };
        let fresh = response("X-Frame-Sequence: 7\r\nX-Frame-Age-Seconds: 0.2\r\n");
        let (t, n) = camera_freshness(&fresh, Duration::from_millis(100), None).unwrap();
        assert_eq!(n, 7);
        assert!(t.elapsed() >= Duration::from_millis(299));
        assert!(camera_freshness(&fresh, Duration::ZERO, Some(7)).is_err());
        assert!(camera_freshness(&fresh, Duration::ZERO, Some(8)).is_err());
        assert!(
            camera_freshness(
                &response("X-Frame-Sequence: 0\r\nX-Frame-Age-Seconds: 0.1\r\n"),
                Duration::ZERO,
                None
            )
            .is_err()
        );
        assert!(camera_freshness(&fresh, Duration::from_secs(3), None).is_err());
        for h in [
            "X-Frame-Sequence: 7\r\n",
            "X-Frame-Sequence: 7\r\nX-Frame-Age-Seconds: NaN\r\n",
            "X-Frame-Sequence: 7\r\nX-Frame-Age-Seconds: 5\r\n",
        ] {
            assert!(camera_freshness(&response(h), Duration::ZERO, None).is_err());
        }
    }
    #[test]
    fn disconnected_idle_worker_exits() {
        let stop = Arc::new(AtomicBool::new(false));
        let w = start_photos(1, false, stop).unwrap();
        let start = Instant::now();
        drop(w.tx);
        drop(w.rx);
        w.handle.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}

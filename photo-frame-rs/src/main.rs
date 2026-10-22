#![forbid(unsafe_code)]

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2,
    ViewportCommand,
};
use photo_frame_rs::{
    Timeline, art, atlas, depth,
    space::{self, Camera, V3},
    decode::DecodedPhoto,
    hud::Hud,
    strip, strip_paint,
    theme::Palette,
    hand_overlay,
    layout::{self, Layout, Rect},
    navigation::{self, Decision},
    status::{self, SharedStatus, Status},
    worker::{self, CameraEvent, CameraSlot, Counts, Navigation, Outcome, PhotoWorker, Request},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Config {
    fullscreen: bool,
    camera: bool,
    live_tile: bool,
    camera_port: u16,
    gestures: bool,
    gesture_controls: bool,
    backdrops: bool,
    run_seconds: Option<f64>,
    status_port: u16,
    scale: f32,
    strip: bool,
    lat: f64,
    lon: f64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            fullscreen: false,
            camera: false,
            live_tile: false,
            camera_port: 8082,
            gestures: false,
            gesture_controls: false,
            backdrops: true,
            run_seconds: None,
            status_port: 8096,
            scale: 1.0,
            strip: true,
            // The 2F office, 広島市西区楠木町.
            lat: 34.4080,
            lon: 132.4547,
        }
    }
}
fn config() -> Result<Option<Config>, &'static str> {
    config_from(std::env::args().skip(1))
}
fn config_from(args: impl IntoIterator<Item = String>) -> Result<Option<Config>, &'static str> {
    let mut c = Config::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fullscreen" => c.fullscreen = true,
            "--camera" => c.camera = true,
            "--live-tile" => {
                c.live_tile = true;
                c.camera = true;
            }
            "--no-live-tile" => c.live_tile = false,
            "--camera-port" => {
                c.camera_port = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|p| *p >= 1024)
                    .ok_or("invalid_camera_port")?;
            }
            "--gesture-controls" => {
                c.gesture_controls = true;
                c.gestures = true;
                c.camera = true;
            }
            "--no-gesture-controls" => c.gesture_controls = false,
            "--gestures" => {
                c.gestures = true;
                c.camera = true;
            }
            "--no-camera" => c.camera = false,
            "--no-backdrops" => c.backdrops = false,
            "--no-strip" => c.strip = false,
            "--lat" => {
                c.lat = args
                    .next()
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|v| (-90.0..=90.0).contains(v))
                    .ok_or("invalid_lat")?;
            }
            "--lon" => {
                c.lon = args
                    .next()
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|v| (-180.0..=180.0).contains(v))
                    .ok_or("invalid_lon")?;
            }
            "--run-seconds" => {
                let n = args
                    .next()
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|v| v.is_finite() && *v >= 1.0 && *v <= 3600.0)
                    .ok_or("invalid_run_seconds")?;
                c.run_seconds = Some(n);
            }
            "--status-port" => {
                c.status_port = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|p| matches!(p, 8096 | 8098..=8199))
                    .ok_or("invalid_status_port")?;
            }
            "--scale" => {
                c.scale = args
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .filter(|v| v.is_finite() && (0.75..=3.0).contains(v))
                    .ok_or("invalid_scale")?;
            }
            "--help" => {
                println!(
                    "Native Rust photo-frame prototype\n--fullscreen (default: windowed)\n--camera | --no-camera (default: disabled; shared loopback MJPEG at source rate, target 15fps)\n--camera-port N (loopback MJPEG source port; default 8082)\n--live-tile | --no-live-tile (draw the live camera into the largest collage card instead of the corner preview; implies camera)\n--run-seconds 1..3600 (automatic window close)\n--status-port 8096 or 8098..8199 (GET /health /status, loopback only)\n--scale 0.75..3 (physical pixels per logical egui point; default 1)\n--no-backdrops (disable small RAM-only blurred covers)\n--gestures (display-only overlay; implies camera mirror)\n--gesture-controls (explicit native photo navigation: screen left=next,right=previous; implies gestures/camera)\n--no-gesture-controls (display only)\n--no-strip (full-width collage without the info strip / atlas)\n--lat D --lon D (office position for the sun and the map marker; default 34.4080 132.4547 (Hiroshima office))\nPhoto source fixed: loopback 8081; atlas loopback 8099. No external URLs, credential, file input, HTTP write controls, video or gaze."
                );
                return Ok(None);
            }
            _ => return Err("invalid_argument"),
        }
    }
    if c.gestures && !c.camera {
        return Err("gestures_require_camera");
    }
    if c.live_tile && !c.camera {
        return Err("live_tile_requires_camera");
    }
    Ok(Some(c))
}
struct Runtime {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Absolute grace, not a per-thread accumulating wait. Dropping a tardy handle
        // cannot delay process exit indefinitely; OS reclaims it on main return.
        let deadline = Instant::now() + Duration::from_secs(5);
        for handle in self.handles.drain(..) {
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}
struct GpuPhoto {
    texture: TextureHandle,
    backdrop: Option<TextureHandle>,
    aspect: f32,
    name: String,
    /// Average colour, used to tint the light in the sky.
    tone: [u8; 3],
}
struct Scene {
    photos: Vec<GpuPhoto>,
    layout: Layout,
    size: [f32; 2],
}
impl Scene {
    fn bytes(&self) -> usize {
        self.photos
            .iter()
            .map(|p| {
                p.texture.byte_size() + p.backdrop.as_ref().map_or(0, TextureHandle::byte_size)
            })
            .sum()
    }
    fn textures(&self) -> usize {
        self.photos
            .iter()
            .map(|p| 1 + usize::from(p.backdrop.is_some()))
            .sum()
    }
    fn resize(&mut self, size: [f32; 2], camera: bool) {
        if self.size != size {
            let aspects: Vec<_> = self.photos.iter().map(|p| p.aspect).collect();
            let reserve = camera.then(|| layout::camera_rect(size[0], size[1]).inset(-10.0));
            if let Some(layout) =
                layout::choose(&aspects, size[0], size[1], reserve, self.layout.name)
            {
                self.layout = layout;
                self.size = size;
            }
        }
    }
}
/// Mean colour over a sparse sample of pixels.
fn tone(image: &egui::ColorImage) -> [u8; 3] {
    let step = (image.pixels.len() / 2048).max(1);
    let (mut sum, mut n) = ([0u64; 3], 0u64);
    for p in image.pixels.iter().step_by(step) {
        sum[0] += p.r() as u64;
        sum[1] += p.g() as u64;
        sum[2] += p.b() as u64;
        n += 1;
    }
    sum.map(|v| (v / n.max(1)) as u8)
}
fn upload(ctx: &egui::Context, photos: Vec<DecodedPhoto>, layout: Layout, size: [f32; 2]) -> Scene {
    let photos = photos
        .into_iter()
        .map(|photo| GpuPhoto {
            tone: tone(&photo.image),
            texture: ctx.load_texture("photo-slot", photo.image, TextureOptions::LINEAR),
            backdrop: photo
                .backdrop
                .map(|b| ctx.load_texture("backdrop-slot", b, TextureOptions::LINEAR)),
            aspect: photo.aspect,
            name: photo.name,
        })
        .collect();
    Scene {
        photos,
        layout,
        size,
    }
}
fn rect(r: Rect, origin: Pos2) -> egui::Rect {
    egui::Rect::from_min_size(origin + Vec2::new(r.x, r.y), Vec2::new(r.w, r.h))
}
fn cover_uv(texture: Vec2, target: Vec2) -> egui::Rect {
    let source = texture.x / texture.y;
    let target = target.x / target.y;
    if source > target {
        let w = target / source;
        egui::Rect::from_min_max(
            Pos2::new((1.0 - w) / 2.0, 0.),
            Pos2::new((1.0 + w) / 2.0, 1.),
        )
    } else {
        let h = source / target;
        egui::Rect::from_min_max(
            Pos2::new(0., (1.0 - h) / 2.0),
            Pos2::new(1., (1.0 + h) / 2.0),
        )
    }
}
/// One card's 3D pose for this frame.
#[derive(Clone, Copy)]
struct CardPose {
    alpha: f32,
    yaw: f32,
    z: f32,
    zoom: f32,
}
/// A screen rect lifted onto its card's pose (turned about the card centre).
fn lift(r: egui::Rect, card: egui::Rect, pose: CardPose) -> [V3; 4] {
    let c = space::v3(card.center().x, card.center().y, 0.0);
    space::rect_corners(r.min.x, r.min.y, r.width(), r.height())
        .map(|p| p.turn(c, pose.yaw, 0.0).add(space::v3(0.0, 0.0, pose.z)))
}
fn uv_of(r: egui::Rect) -> [[f32; 2]; 2] {
    [[r.min.x, r.min.y], [r.max.x, r.max.y]]
}
/// Screen bounding box of projected corners.
fn screen_box(cam: &Camera, c: &[V3; 4]) -> Option<egui::Rect> {
    let pts: Option<Vec<Pos2>> = c.iter().map(|p| depth::project(cam, *p)).collect();
    pts.map(|p| egui::Rect::from_points(&p))
}
/// White print border around a photo, scaled to its size.
fn matte(r: egui::Rect) -> f32 {
    (r.width().min(r.height()) * 0.028).clamp(5.0, 14.0)
}
/// A light glancing across a print: a soft diagonal band at `progress` 0..1.
fn glare(painter: &egui::Painter, cam: &Camera, c: &[V3; 4], progress: f32, alpha: f32) {
    let centre = -0.35 + 1.7 * progress;
    let width = 0.22;
    let rows = 6;
    let mut mesh = egui::epaint::Mesh::default();
    for j in 0..=rows {
        let v = j as f32 / rows as f32;
        let skew = 0.3 * (v - 0.5);
        for (du, a) in [(-width, 0.0), (0.0, 0.32), (width, 0.0)] {
            let u = (centre + skew + du).clamp(0.0, 1.0);
            let p = depth::project(cam, space::on_quad(c, u, v)).unwrap_or(Pos2::ZERO);
            mesh.colored_vertex(p, Color32::from_white_alpha((a * alpha * 255.0) as u8));
        }
    }
    for j in 0..rows as u32 {
        let (a, b) = (3 * j, 3 * (j + 1));
        for i in 0..2 {
            mesh.add_triangle(a + i, a + i + 1, b + i);
            mesh.add_triangle(a + i + 1, b + i, b + i + 1);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}
/// Draw a scene in 3D as floating prints. `skip_index` leaves one card for the
/// live camera, drawn by the caller with the same pose.
#[allow(clippy::too_many_arguments)]
fn paint_scene(
    painter: &egui::Painter,
    scene: &Scene,
    origin: Pos2,
    cam: &Camera,
    skip_index: Option<usize>,
    palette: &Palette,
    hud: &Hud,
    now: f64,
    pose: impl Fn(usize) -> CardPose,
    glance: impl Fn(usize) -> Option<f32>,
) -> usize {
    let mut draws = 0;
    for (order, p) in scene.layout.placements.iter().enumerate() {
        let pose = pose(order);
        if pose.alpha <= 0.0 || skip_index == Some(p.index) {
            continue;
        }
        let a = pose.alpha;
        let photo = &scene.photos[p.index];
        let card = rect(p.card, origin);
        let fg = rect(p.foreground, origin);
        let print = lift(fg.expand(matte(fg)), card, pose);
        depth::shadow(painter, cam, &print, a * 1.6);
        depth::filled(painter, cam, &print, Color32::from_rgba_unmultiplied(250, 248, 244, (a * 255.0) as u8));
        let half = 0.5 / pose.zoom.max(1.0);
        let image = lift(fg, card, pose);
        depth::textured(
            painter,
            cam,
            &image,
            photo.texture.id(),
            [[0.5 - half, 0.5 - half], [0.5 + half, 0.5 + half]],
            Color32::from_white_alpha((a * 255.0) as u8),
            10,
        );
        if let Some(g) = glance(order) {
            glare(painter, cam, &image, g, a);
        }
        if let (Some(place), Some(screen)) = (hud.place(&photo.name), screen_box(cam, &image)) {
            strip_paint::place_chip(painter, screen, palette, place, now, a);
        }
        draws += 1;
    }
    draws
}
/// Fit a rect inside a box without cropping (letterbox).
fn contain_rect(box_: egui::Rect, aspect: f32) -> egui::Rect {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let mut w = box_.width();
    let mut h = w / aspect;
    if h > box_.height() {
        h = box_.height();
        w = h * aspect;
    }
    egui::Rect::from_center_size(box_.center(), Vec2::new(w, h))
}

/// Pick the collage card that the live camera should occupy: the largest card,
/// preferring one whose shape is closest to the camera's aspect so the crop is
/// gentle.
fn live_tile_placement<'a>(scene: &'a Scene, aspect: f32) -> Option<&'a layout::Placement> {
    let max_area = scene
        .layout
        .placements
        .iter()
        .map(|p| p.card.area())
        .fold(0.0_f32, f32::max);
    if max_area <= 0.0 {
        return None;
    }
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16. / 9.
    };
    // Shape matters more than size here: a square artwork in a tall card wastes
    // most of the card, so prefer the card whose shape is closest to the source.
    let score = |p: &layout::Placement| {
        let card_aspect = (p.card.w / p.card.h.max(1.0)).max(0.01);
        let shape = 1.0 / (1.0 + (card_aspect / aspect).ln().abs());
        let size = (p.card.area() / max_area).sqrt();
        shape * 0.75 + size * 0.25
    };
    scene
        .layout
        .placements
        .iter()
        .max_by(|a, b| score(a).total_cmp(&score(b)))
}

struct Feeds {
    camera: Option<CameraSlot>,
    hand: Option<hand_overlay::Slot>,
    atlas: Option<(atlas::AtlasSlot, SyncSender<Vec<String>>)>,
}
struct App {
    config: Config,
    stop: Arc<AtomicBool>,
    state: SharedStatus,
    tx: SyncSender<Request>,
    rx: Receiver<Outcome>,
    camera_slot: Option<CameraSlot>,
    hand_slot: Option<hand_overlay::Slot>,
    hand_view: hand_overlay::View,
    camera_texture: Option<TextureHandle>,
    camera_captured: Option<Instant>,
    camera_aspect: f32,
    current: Option<Scene>,
    previous: Option<Scene>,
    timeline: Timeline,
    started: Instant,
    pending: bool,
    next_refresh: f64,
    next_retry: f64,
    counts: Counts,
    empty: bool,
    source_error: Option<&'static str>,
    focused: bool,
    action_gate: navigation::Gate,
    pending_navigation: Option<Navigation>,
    scene_committed: Option<Instant>,
    navigation_dispatched: u64,
    navigation_commits: u64,
    navigation_failures: u64,
    navigation_last_action: Option<&'static str>,
    navigation_last_result: &'static str,
    navigation_notice: Option<(&'static str, f64)>,
    hud: Hud,
}
impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        stop: Arc<AtomicBool>,
        state: SharedStatus,
        tx: SyncSender<Request>,
        rx: Receiver<Outcome>,
        feeds: Feeds,
    ) -> Self {
        cc.egui_ctx.set_pixels_per_point(config.scale);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let jp = hand_overlay::install_japanese_font(&cc.egui_ctx);
        if let Ok(mut s) = state.lock() {
            s.renderer_initialized = cc.gl.is_some();
            s.japanese_font_loaded = jp;
        }
        Self {
            config,
            stop,
            state,
            tx,
            rx,
            camera_slot: feeds.camera,
            hand_slot: feeds.hand,
            hand_view: hand_overlay::View::default(),
            camera_texture: None,
            camera_captured: None,
            camera_aspect: 16. / 9.,
            current: None,
            previous: None,
            timeline: Timeline::default(),
            started: Instant::now(),
            pending: false,
            next_refresh: 0.,
            next_retry: 0.,
            counts: Counts::default(),
            empty: false,
            source_error: None,
            focused: false,
            action_gate: navigation::Gate::default(),
            pending_navigation: None,
            scene_committed: None,
            navigation_dispatched: 0,
            navigation_commits: 0,
            navigation_failures: 0,
            navigation_last_action: None,
            navigation_last_result: "idle",
            navigation_notice: None,
            hud: feeds
                .atlas
                .map_or_else(Hud::default, |(slot, names)| Hud::new(Some(slot), Some(names))),
        }
    }
    /// Whether the layout must reserve space for the floating corner preview.
    /// In live-tile mode the camera occupies a collage card instead, so the
    /// photos get the whole canvas.
    fn reserved_preview(&self) -> bool {
        self.config.camera && !self.config.live_tile
    }
    fn receive(&mut self, ctx: &egui::Context, size: [f32; 2], now: f64) {
        let was_pending = self.pending;
        if self
            .navigation_notice
            .is_some_and(|(_, at)| now < at || now - at > 3.)
        {
            self.navigation_notice = None;
        }
        if let Some(notice) = self.hand_slot.as_ref().and_then(|s| s.lock().ok()?.take()) {
            self.hand_view.apply(notice, now);
        }
        self.hand_view.expire(now);
        self.action_gate
            .source(self.hand_view.instance(), self.hand_view.counter());
        let received = self.rx.try_recv();
        if matches!(&received, Err(std::sync::mpsc::TryRecvError::Disconnected)) && self.pending {
            self.pending = false;
            self.source_error = Some("worker_unavailable");
            self.next_retry = now + 3.;
            if self.pending_navigation.take().is_some() {
                self.navigation_failures += 1;
                self.navigation_last_result = "worker_unavailable";
                self.navigation_notice = Some(("読み込めませんでした · 現在の写真を保持", now));
            }
        }
        if let Ok(outcome) = received {
            let manual = self.pending_navigation.take();
            self.pending = false;
            self.next_retry = now + 3.0;
            match outcome {
                Outcome::Scene {
                    photos,
                    layout,
                    viewport,
                    counts,
                } => {
                    self.counts = counts;
                    self.previous = None;
                    let scene = upload(ctx, photos, layout, viewport);
                    self.previous = self.current.replace(scene);
                    self.timeline.commit(now);
                    self.scene_committed = Some(Instant::now());
                    if let Some(action) = manual {
                        self.navigation_commits += 1;
                        self.navigation_last_result = "committed";
                        self.navigation_notice = Some((
                            if action == Navigation::Next {
                                "次の写真へ"
                            } else {
                                "前の写真へ"
                            },
                            now,
                        ));
                    }
                    self.empty = false;
                    self.source_error = None;
                    if let Ok(mut s) = self.state.lock() {
                        s.content_generation += 1;
                    }
                }
                Outcome::Empty(counts) => {
                    self.counts = counts;
                    self.current = None;
                    self.previous = None;
                    self.empty = true;
                    self.scene_committed = None;
                    if manual.is_some() {
                        self.navigation_last_result = "empty";
                        self.navigation_notice = Some(("写真がありません", now));
                    }
                    self.source_error = None;
                    if let Ok(mut s) = self.state.lock()
                        && s.content_state != "empty"
                    {
                        s.content_generation += 1;
                    }
                }
                Outcome::NoChange { reason, counts } => {
                    self.counts = counts;
                    self.source_error = None;
                    if manual.is_some() {
                        self.navigation_last_result = reason;
                        self.navigation_notice = Some((
                            if reason == "history_start" {
                                "これより前の履歴はありません"
                            } else {
                                "写真を切り替えられません"
                            },
                            now,
                        ));
                    }
                }
                Outcome::Metadata(counts) => {
                    self.counts = counts;
                    self.source_error = None;
                    if manual.is_some() {
                        self.navigation_last_result = "no_change";
                        self.navigation_notice = Some(("写真は変更されていません", now));
                    }
                }
                Outcome::Failed { reason, counts } => {
                    self.counts = counts;
                    self.source_error = Some(reason);
                    if manual.is_some() {
                        self.navigation_failures += 1;
                        self.navigation_last_result = "failed";
                        self.navigation_notice =
                            Some(("読み込めませんでした · 現在の写真を保持", now));
                    }
                }
            }
        }
        if self.timeline.alpha(now) >= 1.0 {
            self.previous = None;
        }
        let reserve_preview = self.reserved_preview();
        for scene in [&mut self.current, &mut self.previous]
            .into_iter()
            .flatten()
        {
            scene.resize(size, reserve_preview);
        }
        let event = self
            .camera_slot
            .as_ref()
            .and_then(|slot| slot.lock().ok()?.take());
        if let Some(event) = event {
            match event {
                CameraEvent::Frame(frame) => {
                    if frame.captured.elapsed() < worker::CAMERA_STALE {
                        self.camera_aspect = frame.photo.aspect;
                        if let Ok(mut s) = self.state.lock() {
                            s.camera_dimensions = frame.photo.image.size;
                        }
                        if let Some(texture) = &mut self.camera_texture {
                            texture.set(frame.photo.image, TextureOptions::LINEAR);
                        } else {
                            self.camera_texture = Some(ctx.load_texture(
                                "camera-current",
                                frame.photo.image,
                                TextureOptions::LINEAR,
                            ));
                        }
                        self.camera_captured = Some(frame.captured);
                        if let Ok(mut s) = self.state.lock() {
                            s.camera_updates += 1;
                            s.camera_state = "live";
                            s.camera_timestamp_verified = true;
                        }
                    }
                }
                CameraEvent::Unavailable(reason) => {
                    self.camera_texture = None;
                    self.camera_captured = None;
                    if let Ok(mut s) = self.state.lock() {
                        s.camera_state = reason;
                        s.camera_timestamp_verified = false;
                    }
                }
            }
        }
        if self
            .camera_captured
            .is_some_and(|t| t.elapsed() >= worker::CAMERA_STALE)
        {
            self.camera_texture = None;
            self.camera_captured = None;
            if let Ok(mut s) = self.state.lock() {
                s.camera_state = "camera_stale";
                s.camera_timestamp_verified = false;
            }
        }
        if let Some(swipe) = self.hand_view.take_swipe() {
            let available = navigation::Availability {
                enabled: self.config.gesture_controls,
                source_live: self.hand_view.live(),
                camera_live: self.camera_texture.is_some()
                    && self
                        .camera_captured
                        .is_some_and(|t| t.elapsed() < worker::CAMERA_STALE),
                scene_ready: self.current.is_some() && !self.empty,
                busy: was_pending || self.pending,
                has_previous: self.counts.history_position > 0,
                scene_committed: self.scene_committed,
            };
            self.navigation_last_action =
                Some(if swipe.direction == hand_overlay::Direction::ImageLeft {
                    "next"
                } else {
                    "previous"
                });
            match self.action_gate.offer(&swipe, now, available) {
                Decision::Navigate(action) => {
                    let request = Request {
                        refresh: false,
                        navigation: action,
                        width: size[0],
                        height: size[1],
                        camera: self.reserved_preview(),
                    };
                    if self.tx.try_send(request).is_ok() {
                        self.pending = true;
                        self.pending_navigation = Some(action);
                        self.navigation_dispatched += 1;
                        self.navigation_last_result = "loading";
                        self.navigation_notice = Some((
                            if action == Navigation::Next {
                                "次の写真を読み込み中…"
                            } else {
                                "前の写真を読み込み中…"
                            },
                            now,
                        ));
                    } else {
                        self.next_retry = now + 3.;
                        self.source_error = Some("worker_unavailable");
                        self.navigation_failures += 1;
                        self.navigation_last_result = "worker_unavailable";
                        self.navigation_notice =
                            Some(("読み込めませんでした · 現在の写真を保持", now));
                    }
                }
                Decision::Ignore(reason) => {
                    self.navigation_last_result = reason;
                    if self.config.gesture_controls {
                        self.navigation_notice = Some((
                            match reason {
                                "history_start" => "これより前の履歴はありません",
                                "loading_busy" => "切替中です · 少し待ってもう一度",
                                "scene_changed" => "写真が切り替わりました · もう一度",
                                "input_unavailable" => "手とカメラの入力を確認中…",
                                "photos_not_ready" => "写真を準備中…",
                                "rate_limited" => "一度手を下げて、もう一度",
                                _ => "操作を見送りました · もう一度",
                            },
                            now,
                        ));
                    }
                }
            }
        }
        if !self.pending && self.previous.is_none() && now >= self.next_retry {
            let refresh = now >= self.next_refresh;
            let next = self.current.is_some() && self.timeline.due(now);
            if refresh || next {
                let request = Request {
                    refresh,
                    navigation: Navigation::Next,
                    width: size[0],
                    height: size[1],
                    camera: self.reserved_preview(),
                };
                if self.tx.try_send(request).is_ok() {
                    self.pending = true;
                    if refresh {
                        self.next_refresh = now + 30.;
                    }
                } else {
                    self.next_retry = now + 3.;
                    self.source_error = Some("worker_unavailable");
                }
            }
        }
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = self.started.elapsed().as_secs_f64();
        if !self.focused {
            ctx.send_viewport_cmd(ViewportCommand::Focus);
            self.focused = true;
        }
        if self.config.run_seconds.is_some_and(|n| now >= n) {
            self.stop.store(true, Ordering::Relaxed);
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        let mut draws = 0;
        let mut camera_drawn = false;
        let mut size = [0., 0.];
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        self.hud.receive(ctx, now);
        let (palette, sun) = self.hud.palette(now, unix, self.config.lat, self.config.lon);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(strip_paint::color(palette.sky_bottom)))
            .show(ctx, |ui| {
                let canvas = ui.max_rect();
                size = [canvas.width(), canvas.height()];
                // With the strip, the collage lives in a virtual viewport drawn shifted.
                let split = self.config.strip.then(|| strip::split(size[0], size[1]));
                let (scene_size, origin) = split.map_or((size, canvas.min), |s| {
                    (s.virtual_viewport, canvas.min + Vec2::new(s.offset[0], s.offset[1]))
                });
                self.receive(ctx, scene_size, now);
                let painter = ui.painter_at(canvas);
                let tones: Vec<[u8; 3]> = self
                    .current
                    .as_ref()
                    .map(|c| c.photos.iter().map(|p| p.tone).collect())
                    .unwrap_or_default();
                art::sky(&painter, canvas, &palette);
                art::stars(&painter, canvas, art::night(sun.elevation_deg), now);
                art::aurora(&painter, canvas, &tones, &palette, now);
                art::weather(&painter, canvas, self.hud.mood, now);
                let alpha = self.timeline.alpha(now);
                let area = split.map_or(canvas, |s| {
                    egui::Rect::from_min_size(
                        canvas.min + Vec2::new(s.collage.x, s.collage.y),
                        Vec2::new(s.collage.w, s.collage.h),
                    )
                });
                let (yaw, pitch) = space::sway(now);
                let since = self.timeline.committed_at.map_or(f64::MAX, |t| now - t);
                let kick = 1.0 - space::ease_out((since / 1.6) as f32);
                let side = if self.counts.scene_id % 2 == 0 { 1.0 } else { -1.0 };
                let cam = Camera {
                    yaw: yaw + side * 0.09 * kick,
                    pitch,
                    // Sit the collage slightly back so the sway never pushes a print
                    // into the strip; the scene change starts further away still.
                    dolly: area.width() * 0.07 + 340.0 * kick,
                    ..Camera::new([area.center().x, area.center().y], area.width() * 1.1)
                };
                art::bokeh(&painter, &cam, area, &palette, &tones, now);
                let push_in = self.timeline.push_in(now);
                let current_pose = |order: usize| {
                    let m = self.timeline.card(now, order);
                    let (yaw, z) = space::fly_in(m.alpha, order);
                    CardPose {
                        alpha: m.alpha,
                        yaw,
                        z: z + 22.0 * (now as f32 * 0.6 + order as f32 * 1.7).sin(),
                        zoom: push_in,
                    }
                };
                let previous_pose = |order: usize| {
                    let (yaw, z) = space::fly_out(alpha, order);
                    CardPose {
                        alpha: 1.0 - alpha,
                        yaw,
                        z,
                        zoom: 1.0 + photo_frame_rs::KEN_BURNS,
                    }
                };
                let live_target = self.config.live_tile.then(|| {
                    self.current
                        .as_ref()
                        .and_then(|s| live_tile_placement(s, self.camera_aspect))
                });
                let live_target = live_target.flatten();
                let live_skip =
                    live_target.and_then(|p| self.camera_texture.is_some().then_some(p.index));
                if let Some(previous) = &self.previous {
                    draws += paint_scene(&painter, previous, origin, &cam, None, &palette, &self.hud, now, previous_pose, |_| None);
                }
                if let Some(current) = &self.current {
                    draws += paint_scene(&painter, current, origin, &cam, live_skip, &palette, &self.hud, now, current_pose, |order| {
                        self.timeline.glare(now, order)
                    });
                } else {
                    painter.text(
                        canvas.center(),
                        Align2::CENTER_CENTER,
                        if self.empty {
                            "No photos available"
                        } else if self.source_error.is_some() {
                            "Photo source unavailable - retrying"
                        } else {
                            "Loading photos..."
                        },
                        FontId::proportional(28.),
                        strip_paint::color(palette.dim),
                    );
                }
                if let Some(split) = &split {
                    let names: Vec<String> = self
                        .current
                        .as_ref()
                        .map(|c| c.photos.iter().map(|p| p.name.clone()).collect())
                        .unwrap_or_default();
                    self.hud.scene(&names);
                    self.hud.paint(
                        &painter,
                        canvas.min,
                        split,
                        &palette,
                        sun,
                        now,
                        (self.config.lat, self.config.lon),
                    );
                }
                if !self.config.strip {
                painter.text(
                    origin + Vec2::new(28., 16.),
                    Align2::LEFT_TOP,
                    "TSUQREA",
                    FontId::proportional(24.),
                    Color32::from_rgb(236, 242, 247),
                );
                painter.text(
                    origin + Vec2::new(28., 45.),
                    Align2::LEFT_TOP,
                    "PHOTO FRAME / NATIVE RUST",
                    FontId::proportional(11.),
                    Color32::from_rgb(108, 128, 146),
                );
                let clock = chrono::Local::now();
                painter.text(
                    origin + Vec2::new(size[0] - 28., 10.),
                    Align2::RIGHT_TOP,
                    clock.format("%H:%M").to_string(),
                    FontId::monospace(32.),
                    Color32::from_rgb(236, 242, 247),
                );
                painter.text(
                    origin + Vec2::new(size[0] - 28., 49.),
                    Align2::RIGHT_TOP,
                    clock.format("%Y-%m-%d  %a").to_string(),
                    FontId::monospace(12.),
                    Color32::from_rgb(132, 151, 167),
                );
                let count = self
                    .current
                    .as_ref()
                    .map_or(0, |s| s.layout.placements.len());
                let suffix = if self.source_error.is_some() && self.current.is_some() {
                    " / source unavailable (last valid scene)"
                } else if self.source_error.is_some() {
                    " / source unavailable - retrying"
                } else {
                    ""
                };
                painter.text(
                    origin + Vec2::new(28., size[1] - 24.),
                    Align2::LEFT_CENTER,
                    format!(
                        "{count} photos / {} available / {} videos skipped{suffix}",
                        self.counts.photos, self.counts.skipped_videos
                    ),
                    FontId::proportional(13.),
                    Color32::from_rgb(132, 151, 167),
                );
                }
                if self.config.camera {
                    if self.config.live_tile {
                        if let (Some(placement), Some(texture)) =
                            (live_target, self.camera_texture.as_ref())
                        {
                            {
                                let card = rect(placement.card, origin);
                                let size = texture.size_vec2();
                                let target = contain_rect(
                                    card.shrink(8.0),
                                    size.x / size.y.max(1.0),
                                );
                                let order = self.current.as_ref().map_or(0, |c| {
                                    c.layout
                                        .placements
                                        .iter()
                                        .position(|q| q.index == placement.index)
                                        .unwrap_or(0)
                                });
                                let pose = current_pose(order);
                                let a = pose.alpha;
                                let image = lift(target, card, pose);
                                let print = lift(target.expand(matte(target)), card, pose);
                                depth::shadow(&painter, &cam, &print, a * 1.6);
                                depth::filled(&painter, &cam, &print, Color32::from_rgba_unmultiplied(250, 248, 244, (a * 255.0) as u8));
                                depth::textured(
                                    &painter,
                                    &cam,
                                    &image,
                                    texture.id(),
                                    uv_of(cover_uv(size, target.size())),
                                    Color32::from_white_alpha((a * 255.0) as u8),
                                    8,
                                );
                                if let Some(screen) = screen_box(&cam, &image) {
                                    strip_paint::live_pill(&painter, screen, now, a);
                                }
                                camera_drawn = true;
                            }
                        }
                    } else {
                        let box_ = layout::camera_rect(size[0], size[1]);
                        let panel = rect(box_, origin);
                        painter.rect_filled(panel, 8, Color32::from_rgb(10, 14, 19));
                        if let Some(texture) = &self.camera_texture {
                            let fit = layout::contain(box_.inset(2.), self.camera_aspect);
                            painter.image(
                                texture.id(),
                                rect(fit, origin),
                                if self.config.gestures {
                                    egui::Rect::from_min_max(Pos2::new(1., 0.), Pos2::new(0., 1.))
                                } else {
                                    egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.))
                                },
                                Color32::WHITE,
                            );
                            camera_drawn = true;
                            if self.config.gestures {
                                self.hand_view.boxes(&painter, rect(fit, origin));
                            }
                        } else {
                            painter.text(
                                panel.center(),
                                Align2::CENTER_CENTER,
                                "Camera unavailable",
                                FontId::proportional(14.),
                                Color32::from_rgb(126, 139, 153),
                            );
                        }
                        painter.rect_stroke(
                            panel,
                            8,
                            Stroke::new(2_f32, Color32::from_rgb(85, 106, 124)),
                            StrokeKind::Inside,
                        );
                        if self.config.gestures {
                            painter.text(
                                panel.min + Vec2::new(8., 7.),
                                Align2::LEFT_TOP,
                                "手の確認 · 鏡表示",
                                FontId::proportional(11.),
                                Color32::WHITE,
                            );
                            self.hand_view.paint_controls(
                                &painter,
                                canvas,
                                panel,
                                now,
                                self.config.gesture_controls,
                                self.navigation_notice.map(|(text, _)| text),
                            );
                        }
                    }
                }
            });
        if let Ok(mut s) = self.state.lock() {
            s.gesture_navigation_enabled = self.config.gesture_controls;
            s.strip_enabled = self.config.strip;
            s.theme_phase = self.hud.phase;
            s.theme_mood = self.hud.mood.map_or("none", |m| m.name());
            s.atlas_state = self.hud.state;
            s.strip_view = self.hud.view;
            s.navigation_pending = self.pending_navigation.is_some();
            s.navigation_requests = self.action_gate.accepted;
            s.navigation_dispatched = self.navigation_dispatched;
            s.navigation_commits = self.navigation_commits;
            s.navigation_ignored = self.action_gate.ignored;
            s.navigation_failures = self.navigation_failures;
            s.navigation_last_action = self.navigation_last_action;
            s.navigation_last_result = self.navigation_last_result;
            s.history_length = self.counts.history_len;
            s.history_position = self.counts.history_position;
            s.scene_id = self.counts.scene_id;
            s.hand_enabled = self.config.gestures;
            s.gesture_overlay_enabled = self.config.gestures;
            s.camera_mirrored = self.config.gestures;
            s.hand_api_live = self.config.gestures && self.hand_view.live();
            s.hand_count = self.hand_view.count();
            s.hand_candidate_state = self.hand_view.state().to_string();
            s.hand_guidance_state = self.hand_view.guidance_state().to_string();
            s.hand_ready_for_motion = self.hand_view.ready_for_motion();
            s.gesture_effect_direction = self.hand_view.effect_direction();
            s.gesture_effect_flashes = self.hand_view.flashes;
            s.hand_reports_received = self.hand_view.reports;
            s.hand_diagnostics = self.hand_view.diagnostics().clone();
            s.hand_event_counter = self.hand_view.counter();
            s.hand_tracking_state = self.hand_view.tracking_state().to_string();
            s.hand_search_active = self.hand_view.search_active();
            (s.hand_full_frame_detections, s.hand_roi_detections) =
                self.hand_view.detection_counts();
            s.hand_last_confirmed_age_ms = self.hand_view.confirmed_age();
            s.render_callback_frames += 1;
            s.frame_count = s.render_callback_frames;
            s.foreground_contain = true;
            s.camera_foreground_overlap = self.reserved_preview()
                && [&self.current, &self.previous]
                    .into_iter()
                    .flatten()
                    .any(|scene| {
                        scene.layout.placements.iter().any(|p| {
                            layout::intersects(
                                p.foreground,
                                layout::camera_rect(size[0], size[1]).inset(-10.0),
                            )
                        })
                    });
            if draws > 0 {
                s.image_rendering_frames += 1;
                s.photo_draw_calls += draws as u64;
            }
            if camera_drawn {
                s.camera_rendering_frames += 1;
            }
            s.last_frame = Some(Instant::now());
            s.uptime_seconds = now;
            s.viewport_points = size;
            s.pixels_per_point = ctx.pixels_per_point();
            s.playlist_generation = self.counts.generation;
            s.photos_available = self.counts.photos;
            s.photos_loaded_total = self.counts.loaded_total;
            s.skipped_videos = self.counts.skipped_videos;
            s.rejected_entries = self.counts.rejected_entries;
            s.photo_decode_failures = self.counts.decode_failures;
            s.photos_in_scene = self
                .current
                .as_ref()
                .map_or(0, |c| c.layout.placements.len());
            s.layout = self.current.as_ref().map_or("none", |c| c.layout.name);
            s.fade_active = self.previous.is_some();
            s.source_state = if self.source_error.is_some() && self.current.is_some() {
                "unavailable_retained"
            } else if self.source_error.is_some() {
                "unavailable"
            } else if self.pending {
                "loading"
            } else {
                "ready"
            };
            s.last_error = self.source_error;
            s.content_state = if self.empty {
                "empty"
            } else if self.current.is_some() {
                "photos"
            } else {
                "waiting"
            };
            s.photo_texture_count = self.current.as_ref().map_or(0, Scene::textures)
                + self.previous.as_ref().map_or(0, Scene::textures);
            s.photo_texture_bytes = self.current.as_ref().map_or(0, Scene::bytes)
                + self.previous.as_ref().map_or(0, Scene::bytes);
            s.camera_texture_count = usize::from(self.camera_texture.is_some());
            s.camera_age_seconds = self.camera_captured.map(|t| t.elapsed().as_secs_f64());
        }
        // The strip animates continuously (scanline, radar loop, globe).
        let repaint = if self.previous.is_some() || self.config.gestures || self.config.strip {
            Duration::from_millis(33)
        } else {
            Duration::from_secs(1)
        };
        let repaint = self.camera_captured.map_or(repaint, |captured| {
            repaint.min(
                worker::CAMERA_STALE
                    .saturating_sub(captured.elapsed())
                    .max(Duration::from_millis(1)),
            )
        });
        ctx.request_repaint_after(repaint);
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop.store(true, Ordering::Relaxed);
    }
    fn persist_egui_memory(&self) -> bool {
        false
    }
}
fn run(config: Config) -> Result<(), &'static str> {
    let stop = Arc::new(AtomicBool::new(false));
    let mut runtime = Runtime {
        stop: stop.clone(),
        handles: Vec::new(),
    };
    let state = Arc::new(Mutex::new(Status::new(
        8081,
        config.camera,
        config.status_port,
        config.fullscreen,
        config.scale,
    )));
    runtime.handles.push(
        status::start(config.status_port, state.clone(), stop.clone())
            .map_err(|_| "status_bind_failed")?,
    );
    let PhotoWorker { tx, rx, handle } = worker::start_photos(8081, config.backdrops, stop.clone())
        .map_err(|_| "photo_worker_failed")?;
    runtime.handles.push(handle);
    let camera_wake: worker::CameraWake = Arc::new(Mutex::new(None));
    let camera_slot = if config.camera {
        let (slot, handle) = worker::start_camera(stop.clone(), camera_wake.clone(), config.camera_port)
            .map_err(|_| "camera_worker_failed")?;
        runtime.handles.push(handle);
        Some(slot)
    } else {
        None
    };
    let hand_slot = if config.gestures {
        let (slot, handle) = hand_overlay::start(stop.clone(), camera_wake.clone())
            .map_err(|_| "hand_worker_failed")?;
        runtime.handles.push(handle);
        Some(slot)
    } else {
        None
    };
    let atlas_feed = if config.strip {
        let (slot, names, handle) = atlas::start(atlas::ATLAS_PORT, stop.clone(), camera_wake.clone())
            .map_err(|_| "atlas_worker_failed")?;
        runtime.handles.push(handle);
        Some((slot, names))
    } else {
        None
    };
    let feeds = Feeds {
        camera: camera_slot,
        hand: hand_slot,
        atlas: atlas_feed,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TSUQREA Photo Frame - Native Rust Prototype")
            .with_app_id("photo-frame-rs")
            .with_inner_size([1280., 720.])
            .with_min_inner_size([640., 360.])
            .with_fullscreen(config.fullscreen)
            .with_active(true),
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        run_and_return: true,
        ..Default::default()
    };
    let result = eframe::run_native(
        "photo-frame-rs",
        options,
        Box::new(move |cc| {
            if let Ok(mut wake) = camera_wake.lock() {
                *wake = Some(cc.egui_ctx.clone());
            }
            Ok(Box::new(App::new(cc, config, stop, state, tx, rx, feeds)))
        }),
    );
    drop(runtime);
    result.map_err(|_| "native_renderer_start_failed")
}
fn main() {
    match config().and_then(|c| c.map_or(Ok(()), run)) {
        Ok(()) => {}
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    fn fixture() -> (App, SyncSender<Outcome>, Receiver<Request>) {
        let config = Config::default();
        let (tx, commands) = mpsc::sync_channel(1);
        let (results, rx) = mpsc::sync_channel(1);
        let state = Arc::new(Mutex::new(Status::new(8081, false, 8096, false, 1.)));
        let app = App {
            config,
            stop: Arc::new(AtomicBool::new(false)),
            state,
            tx,
            rx,
            camera_slot: None,
            hand_slot: None,
            hand_view: hand_overlay::View::default(),
            camera_texture: None,
            camera_captured: None,
            camera_aspect: 1.,
            current: None,
            previous: None,
            timeline: Timeline::default(),
            started: Instant::now(),
            pending: true,
            next_refresh: 30.,
            next_retry: 0.,
            counts: Counts::default(),
            empty: false,
            source_error: None,
            focused: true,
            action_gate: navigation::Gate::default(),
            pending_navigation: None,
            scene_committed: None,
            navigation_dispatched: 0,
            navigation_commits: 0,
            navigation_failures: 0,
            navigation_last_action: None,
            navigation_last_result: "idle",
            navigation_notice: None,
            hud: Hud::default(),
        };
        (app, results, commands)
    }
    fn scene(count: usize) -> Outcome {
        let photos = (0..count)
            .map(|_| DecodedPhoto {
                image: egui::ColorImage::new([20, 20], vec![Color32::WHITE; 400]),
                backdrop: Some(egui::ColorImage::new([2, 2], vec![Color32::GRAY; 4])),
                aspect: 1.,
                name: String::new(),
            })
            .collect();
        let layout = layout::choose(&vec![1.; count], 1920., 1080., None, "").unwrap();
        Outcome::Scene {
            photos,
            layout,
            viewport: [1920., 1080.],
            counts: Counts {
                photos: 10,
                generation: 1,
                ..Default::default()
            },
        }
    }
    #[test]
    fn failed_scene_retention_and_explicit_empty_release_textures() {
        let (mut app, results, _commands) = fixture();
        let ctx = egui::Context::default();
        results.send(scene(2)).unwrap();
        app.receive(&ctx, [1920., 1080.], 1.);
        let id = app.current.as_ref().unwrap().photos[0].texture.id();
        let committed = app.timeline.committed_at;
        results
            .send(Outcome::Failed {
                reason: "bad_response",
                counts: app.counts,
            })
            .unwrap();
        app.receive(&ctx, [1920., 1080.], 4.);
        assert_eq!(app.current.as_ref().unwrap().photos[0].texture.id(), id);
        assert_eq!(app.timeline.committed_at, committed);
        assert_eq!(app.state.lock().unwrap().content_generation, 1);
        results.send(Outcome::Empty(Counts::default())).unwrap();
        app.receive(&ctx, [1920., 1080.], 5.);
        assert!(app.empty);
        assert!(app.current.is_none() && app.previous.is_none());
    }
    #[test]
    fn old_new_scene_bound_and_fade_release() {
        let (mut app, results, _commands) = fixture();
        let ctx = egui::Context::default();
        results.send(scene(4)).unwrap();
        app.receive(&ctx, [1920., 1080.], 1.);
        results.send(scene(4)).unwrap();
        app.receive(&ctx, [1920., 1080.], 13.);
        assert!(app.previous.is_some());
        let total =
            app.current.as_ref().unwrap().textures() + app.previous.as_ref().unwrap().textures();
        assert_eq!(total, 16);
        assert_eq!(app.timeline.alpha(13.), 0.);
        assert!(app.timeline.alpha(13.45) > 0.49);
        app.receive(&ctx, [1920., 1080.], 13.91);
        assert!(app.previous.is_none());
        assert_eq!(app.current.as_ref().unwrap().textures(), 8);
    }
    #[test]
    fn resize_during_worker_fetch_recomputes_result_for_current_viewport() {
        let (mut app, results, _commands) = fixture();
        let ctx = egui::Context::default();
        app.config.camera = true;
        results.send(scene(4)).unwrap();
        app.receive(&ctx, [960., 540.], 1.);
        let scene = app.current.as_ref().unwrap();
        assert_eq!(scene.size, [960., 540.]);
        let camera = layout::camera_rect(960., 540.).inset(-10.);
        for p in &scene.layout.placements {
            assert!(!layout::intersects(p.foreground, camera));
            assert!(p.foreground.x + p.foreground.w <= 960.);
            assert!(p.foreground.y + p.foreground.h <= 540. - 48.);
        }
    }
    #[test]
    fn camera_stale_expiry_clears_texture_and_verified_live_state() {
        let (mut app, _results, _commands) = fixture();
        let ctx = egui::Context::default();
        app.camera_texture = Some(ctx.load_texture(
            "camera-test",
            egui::ColorImage::new([2, 2], vec![Color32::WHITE; 4]),
            TextureOptions::LINEAR,
        ));
        app.camera_captured = Some(Instant::now() - Duration::from_secs(4));
        app.receive(&ctx, [1920., 1080.], 1.);
        assert!(app.camera_texture.is_none());
        assert!(app.camera_captured.is_none());
        assert_eq!(app.state.lock().unwrap().camera_state, "camera_stale");
    }
    #[test]
    fn hand_overlay_event_never_advances_or_replaces_photo_scene() {
        let (mut app, results, commands) = fixture();
        let ctx = egui::Context::default();
        results.send(scene(2)).unwrap();
        app.receive(&ctx, [1920., 1080.], 1.);
        let texture = app.current.as_ref().unwrap().photos[0].texture.id();
        let committed = app.timeline.committed_at;
        let slot: hand_overlay::Slot = Arc::new(Mutex::new(None));
        app.hand_slot = Some(slot.clone());
        app.config.gestures = true;
        let mut payload = serde_json::json!({"healthy":true,"controls_enabled":false,"inference_only":true,"identity_recognition":false,"open_hand":"unknown","intent":"unknown","coordinates":"original_image_pixels_unmirrored","instance_id":"test-123","gesture_profile":"display","frame_width":1920,"frame_height":1080,"source_age_ms":20,"source_headers_verified":true,"sequence":21,"detected_hands":0,"hands":[],"candidate_state":"no_hand","event_counter":0,"last_event":null});
        let frame =
            hand_overlay::parse(&serde_json::to_vec(&payload).unwrap(), Duration::ZERO).unwrap();
        *slot.lock().unwrap() = Some(hand_overlay::Notice::Frame(Box::new(frame)));
        app.receive(&ctx, [1920., 1080.], 2.);
        payload["sequence"] = serde_json::json!(22);
        payload["event_counter"] = serde_json::json!(1);
        payload["last_event"] = serde_json::json!({"counter":1,"sequence":22,"direction":"image_left","fresh_age_ms":30,"candidate_only":true});
        let frame =
            hand_overlay::parse(&serde_json::to_vec(&payload).unwrap(), Duration::ZERO).unwrap();
        *slot.lock().unwrap() = Some(hand_overlay::Notice::Frame(Box::new(frame)));
        app.receive(&ctx, [1920., 1080.], 2.1);
        assert_eq!(app.hand_view.flashes, 1);
        assert_eq!(app.hand_view.effect_direction(), Some("right"));
        assert_eq!(
            app.current.as_ref().unwrap().photos[0].texture.id(),
            texture
        );
        assert_eq!(app.timeline.committed_at, committed);
        assert!(commands.try_recv().is_err());
        assert!(!app.state.lock().unwrap().write_controls_enabled);
        *slot.lock().unwrap() = Some(hand_overlay::Notice::Unavailable);
        app.receive(&ctx, [1920., 1080.], 2.2);
        assert_eq!(app.hand_view.effect_direction(), None);
        assert_eq!(app.hand_view.count(), 0);
    }
    fn control_payload(counter: u64, raw_direction: &str) -> hand_overlay::Notice {
        let hand = serde_json::json!({"bbox":{"x1":800.,"y1":400.,"x2":860.,"y2":460.},"palm_keypoints":[{"x":810.,"y":420.},{"x":815.,"y":412.},{"x":820.,"y":410.},{"x":825.,"y":412.},{"x":830.,"y":416.},{"x":838.,"y":430.},{"x":845.,"y":440.}],"score":0.9,"geometric_quality":true});
        let event = if counter > 0 {
            serde_json::json!({"counter":counter,"sequence":100+counter,"direction":raw_direction,"fresh_age_ms":20,"candidate_only":true})
        } else {
            serde_json::Value::Null
        };
        let body = serde_json::json!({"healthy":true,"controls_enabled":false,"inference_only":true,"identity_recognition":false,"open_hand":"unknown","intent":"unknown","coordinates":"original_image_pixels_unmirrored","instance_id":"native-test-source","gesture_profile":"display","frame_width":1920,"frame_height":1080,"source_age_ms":20,"source_headers_verified":true,"sequence":100+counter,"detected_hands":1,"hands":[hand],"candidate_state":"cooldown","tracking_state":"locked","last_confirmed_age_ms":20,"event_counter":counter,"last_event":event});
        hand_overlay::Notice::Frame(Box::new(
            hand_overlay::parse(&serde_json::to_vec(&body).unwrap(), Duration::ZERO).unwrap(),
        ))
    }
    fn controlled_fixture() -> (
        App,
        SyncSender<Outcome>,
        Receiver<Request>,
        egui::Context,
        hand_overlay::Slot,
    ) {
        let (mut app, results, commands) = fixture();
        let ctx = egui::Context::default();
        app.config.gesture_controls = true;
        app.config.gestures = true;
        app.config.camera = true;
        let mut out = scene(2);
        if let Outcome::Scene { ref mut counts, .. } = out {
            counts.history_len = 3;
            counts.history_position = 1;
            counts.scene_id = 2;
        }
        results.send(out).unwrap();
        app.receive(&ctx, [1920., 1080.], 1.);
        app.scene_committed = Some(Instant::now() - Duration::from_secs(1));
        app.camera_texture = Some(ctx.load_texture(
            "synthetic-camera",
            egui::ColorImage::new([2, 2], vec![Color32::BLACK; 4]),
            TextureOptions::LINEAR,
        ));
        app.camera_captured = Some(Instant::now());
        let slot: hand_overlay::Slot =
            Arc::new(Mutex::new(Some(control_payload(0, "image_right"))));
        app.hand_slot = Some(slot.clone());
        app.receive(&ctx, [1920., 1080.], 2.);
        (app, results, commands, ctx, slot)
    }
    #[test]
    fn explicit_control_flag_implies_camera_overlay_and_can_opt_out() {
        let parse = |args: &[&str]| config_from(args.iter().map(|s| s.to_string()));
        assert!(!parse(&[]).unwrap().unwrap().gesture_controls);
        assert!(!parse(&["--gestures"]).unwrap().unwrap().gesture_controls);
        let c = parse(&["--gesture-controls"]).unwrap().unwrap();
        assert!(c.gesture_controls && c.camera && c.gestures);
        assert!(
            !parse(&["--gesture-controls", "--no-gesture-controls"])
                .unwrap()
                .unwrap()
                .gesture_controls
        );
        assert!(parse(&["--gesture-controls", "--no-camera"]).is_err());
    }
    #[test]
    fn strip_defaults_to_the_office_and_validates_position() {
        let c = config_from(Vec::<String>::new()).unwrap().unwrap();
        assert!(c.strip && (c.lat - 34.408).abs() < 1e-9 && (c.lon - 132.4547).abs() < 1e-9);
        let args = |a: &[&str]| config_from(a.iter().map(|s| s.to_string()));
        let c = args(&["--lat", "35.0", "--lon", "135.75", "--no-strip"]).unwrap().unwrap();
        assert!(!c.strip && c.lat == 35.0 && c.lon == 135.75);
        assert_eq!(args(&["--lat", "91"]).err(), Some("invalid_lat"));
        assert_eq!(args(&["--lon", "x"]).err(), Some("invalid_lon"));
    }
    #[test]
    fn live_tile_implies_camera_and_can_opt_out() {
        let parse = |args: &[&str]| config_from(args.iter().map(|s| s.to_string()));
        assert!(!parse(&[]).unwrap().unwrap().live_tile);
        let c = parse(&["--live-tile"]).unwrap().unwrap();
        assert!(c.live_tile && c.camera);
        assert!(
            !parse(&["--live-tile", "--no-live-tile"])
                .unwrap()
                .unwrap()
                .live_tile
        );
        assert!(matches!(
            parse(&["--live-tile", "--no-camera"]),
            Err("live_tile_requires_camera")
        ));
    }
    #[test]
    fn fresh_screen_left_bypasses_auto_wait_once_and_commits_only_on_success() {
        let (mut app, results, commands, ctx, slot) = controlled_fixture();
        let generation = app.state.lock().unwrap().content_generation;
        let timeline = app.timeline.committed_at;
        *slot.lock().unwrap() = Some(control_payload(1, "image_right"));
        app.receive(&ctx, [1920., 1080.], 2.1);
        let req = commands.try_recv().unwrap();
        assert!(!req.refresh);
        assert_eq!(req.navigation, Navigation::Next);
        assert!(app.pending);
        assert_eq!(app.navigation_dispatched, 1);
        assert_eq!(app.navigation_commits, 0);
        assert_eq!(app.timeline.committed_at, timeline);
        assert_eq!(app.state.lock().unwrap().content_generation, generation);
        *slot.lock().unwrap() = Some(control_payload(1, "image_right"));
        app.receive(&ctx, [1920., 1080.], 2.2);
        assert!(commands.try_recv().is_err());
        let mut next = scene(2);
        if let Outcome::Scene { ref mut counts, .. } = next {
            counts.history_len = 4;
            counts.history_position = 2;
            counts.scene_id = 3;
        }
        results.send(next).unwrap();
        app.receive(&ctx, [1920., 1080.], 2.3);
        assert_eq!(app.navigation_commits, 1);
        assert_eq!(app.counts.scene_id, 3);
        assert_eq!(app.counts.history_position, 2);
        assert_eq!(app.timeline.committed_at, Some(2.3));
        assert_eq!(app.state.lock().unwrap().content_generation, generation + 1);
    }
    #[test]
    fn screen_right_requests_previous_and_failure_retains_scene_and_timer() {
        let (mut app, results, commands, ctx, slot) = controlled_fixture();
        let texture = app.current.as_ref().unwrap().photos[0].texture.id();
        let timeline = app.timeline.committed_at;
        *slot.lock().unwrap() = Some(control_payload(1, "image_left"));
        app.receive(&ctx, [1920., 1080.], 2.1);
        assert_eq!(
            commands.try_recv().unwrap().navigation,
            Navigation::Previous
        );
        results
            .send(Outcome::Failed {
                reason: "history_photo_unavailable",
                counts: app.counts,
            })
            .unwrap();
        app.receive(&ctx, [1920., 1080.], 2.2);
        assert_eq!(
            app.current.as_ref().unwrap().photos[0].texture.id(),
            texture
        );
        assert_eq!(app.counts.history_position, 1);
        assert_eq!(app.counts.scene_id, 2);
        assert_eq!(app.timeline.committed_at, timeline);
        assert_eq!(app.navigation_commits, 0);
        assert_eq!(app.navigation_failures, 1);
        assert_eq!(app.navigation_last_result, "failed");
    }
    #[test]
    fn busy_gesture_never_becomes_a_delayed_second_navigation() {
        let (mut app, _results, commands, ctx, slot) = controlled_fixture();
        app.pending = true;
        *slot.lock().unwrap() = Some(control_payload(1, "image_right"));
        app.receive(&ctx, [1920., 1080.], 2.1);
        assert_eq!(app.navigation_last_result, "loading_busy");
        assert!(commands.try_recv().is_err());
        app.pending = false;
        *slot.lock().unwrap() = Some(control_payload(1, "image_right"));
        app.receive(&ctx, [1920., 1080.], 3.);
        assert!(commands.try_recv().is_err());
        assert_eq!(app.navigation_dispatched, 0);
    }
    #[test]
    fn display_only_and_start_of_history_do_not_send_requests() {
        for display_only in [true, false] {
            let (mut app, _results, commands, ctx, slot) = controlled_fixture();
            if display_only {
                app.config.gesture_controls = false;
            } else {
                app.counts.history_position = 0;
            }
            *slot.lock().unwrap() = Some(control_payload(1, "image_left"));
            app.receive(&ctx, [1920., 1080.], 2.1);
            assert!(commands.try_recv().is_err());
            assert_eq!(app.navigation_dispatched, 0);
            assert_eq!(
                app.navigation_last_result,
                if display_only {
                    "controls_disabled"
                } else {
                    "history_start"
                }
            );
        }
    }
    #[test]
    fn manual_navigation_wins_over_due_auto_and_refresh_in_same_update() {
        let (mut app, _results, commands, ctx, slot) = controlled_fixture();
        app.next_refresh = 0.;
        app.next_retry = 0.;
        app.timeline.committed_at = Some(-20.);
        *slot.lock().unwrap() = Some(control_payload(1, "image_right"));
        app.receive(&ctx, [1920., 1080.], 2.1);
        let req = commands.try_recv().unwrap();
        assert!(!req.refresh);
        assert_eq!(req.navigation, Navigation::Next);
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn input_error_restart_and_multihand_cannot_navigate() {
        let (mut app, _results, commands, ctx, slot) = controlled_fixture();
        *slot.lock().unwrap() = Some(hand_overlay::Notice::Unavailable);
        app.receive(&ctx, [1920., 1080.], 2.1);
        assert!(commands.try_recv().is_err());
        let hand_overlay::Notice::Frame(mut f) = control_payload(7, "image_right") else {
            unreachable!()
        };
        f.report.instance_id = "new-epoch".into();
        *slot.lock().unwrap() = Some(hand_overlay::Notice::Frame(f));
        app.receive(&ctx, [1920., 1080.], 2.2);
        assert!(commands.try_recv().is_err());
        let hand_overlay::Notice::Frame(mut f) = control_payload(8, "image_right") else {
            unreachable!()
        };
        f.report.instance_id = "new-epoch".into();
        f.report.hands.push(f.report.hands[0].clone());
        f.report.detected_hands = 2;
        f.report.tracking_state = Some("ambiguous".into());
        *slot.lock().unwrap() = Some(hand_overlay::Notice::Frame(f));
        app.receive(&ctx, [1920., 1080.], 3.1);
        assert!(commands.try_recv().is_err());
        assert_eq!(app.navigation_dispatched, 0);
    }
    #[test]
    fn synthetic_loopback_swipes_drive_real_worker_next_back_redo_without_files() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let server = thread::spawn(move || {
            let names = (0..20)
                .map(|i| format!("fixture-{i:02}.jpg"))
                .collect::<Vec<_>>();
            let list = serde_json::to_vec(&names).unwrap();
            let mut jpeg = vec![];
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
                .encode(
                    &vec![180u8; 40 * 30 * 3],
                    40,
                    30,
                    image::ExtendedColorType::Rgb8,
                )
                .unwrap();
            let started = Instant::now();
            let mut requests = 0;
            while !server_stop.load(Ordering::Relaxed)
                && started.elapsed() < Duration::from_secs(12)
                && requests < 100
            {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        requests += 1;
                        stream
                            .set_read_timeout(Some(Duration::from_millis(200)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_millis(200)))
                            .unwrap();
                        let mut request = vec![];
                        let mut buf = [0; 512];
                        while request.len() < 4096 && !request.windows(4).any(|w| w == b"\r\n\r\n")
                        {
                            let Ok(n) = stream.read(&mut buf) else {
                                break;
                            };
                            if n == 0 {
                                break;
                            }
                            request.extend_from_slice(&buf[..n]);
                        }
                        let text = String::from_utf8_lossy(&request);
                        let path = text.split_whitespace().nth(1).unwrap_or("");
                        let valid_photo = path
                            .strip_prefix("/photos/")
                            .is_some_and(|name| names.iter().any(|n| n == name));
                        let (code, kind, body) = if path == "/api/photos" {
                            (200, "application/json", list.as_slice())
                        } else if valid_photo {
                            (200, "image/jpeg", jpeg.as_slice())
                        } else {
                            (404, "text/plain", b"missing".as_slice())
                        };
                        let headers = format!(
                            "HTTP/1.1 {code} OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(headers.as_bytes());
                        let _ = stream.write_all(body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => break,
                }
            }
        });
        let (mut app, _fake_results, _fake_commands, ctx, slot) = controlled_fixture();
        let PhotoWorker { tx, rx, handle } =
            worker::start_photos(port, false, stop.clone()).unwrap();
        app.tx = tx;
        app.rx = rx;
        app.stop = stop.clone();
        app.current = None;
        app.previous = None;
        app.pending = false;
        app.pending_navigation = None;
        app.next_refresh = 0.;
        app.next_retry = 0.;
        app.counts = Counts::default();
        app.timeline = Timeline::default();
        app.scene_committed = None;
        fn wait_scene(app: &mut App, ctx: &egui::Context, now: f64, id: u64) {
            let deadline = Instant::now() + Duration::from_secs(4);
            while Instant::now() < deadline {
                app.receive(ctx, [1920., 1080.], now);
                if app.counts.scene_id == id && !app.pending {
                    return;
                }
                thread::sleep(Duration::from_millis(2));
            }
            panic!("synthetic worker result deadline");
        }
        app.receive(&ctx, [1920., 1080.], 2.);
        wait_scene(&mut app, &ctx, 2., 1);
        assert_eq!(app.counts.history_position, 0);
        let original_layout = app.current.as_ref().unwrap().layout.name;
        for (counter, raw_direction, tick, expected_id, expected_pos) in [
            (1, "image_right", 3., 2, 1),
            (2, "image_left", 5., 1, 0),
            (3, "image_right", 7., 2, 1),
            (4, "image_left", 9., 1, 0),
        ] {
            app.scene_committed = Some(Instant::now() - Duration::from_secs(1));
            app.camera_captured = Some(Instant::now());
            *slot.lock().unwrap() = Some(control_payload(counter, raw_direction));
            app.receive(&ctx, [1920., 1080.], tick);
            wait_scene(&mut app, &ctx, tick, expected_id);
            assert_eq!(app.counts.history_position, expected_pos);
            assert_eq!(app.navigation_commits, counter);
            assert_eq!(app.counts.history_len, 2);
            if expected_id == 1 {
                assert_eq!(app.current.as_ref().unwrap().layout.name, original_layout);
            }
        }
        let committed = app.timeline.committed_at;
        app.scene_committed = Some(Instant::now() - Duration::from_secs(1));
        *slot.lock().unwrap() = Some(control_payload(5, "image_left"));
        app.receive(&ctx, [1920., 1080.], 11.);
        assert_eq!(app.navigation_last_result, "history_start");
        assert_eq!(app.navigation_dispatched, 4);
        assert_eq!(app.navigation_commits, 4);
        assert_eq!(app.hand_view.diagnostics().source_event_increases, 5);
        assert_eq!(app.hand_view.diagnostics().fresh_swipes_delivered, 5);
        assert_eq!(app.hand_view.diagnostics().source_epochs, 1);
        assert_eq!(app.counts.scene_id, 1);
        assert_eq!(app.timeline.committed_at, committed);
        assert!(!app.pending);
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        server.join().unwrap();
    }
    #[test]
    fn disconnected_worker_releases_busy_without_losing_current_scene() {
        let (mut app, results, commands, ctx, slot) = controlled_fixture();
        let texture = app.current.as_ref().unwrap().photos[0].texture.id();
        let committed = app.timeline.committed_at;
        *slot.lock().unwrap() = Some(control_payload(1, "image_right"));
        app.receive(&ctx, [1920., 1080.], 2.1);
        let _ = commands.try_recv().unwrap();
        drop(results);
        app.receive(&ctx, [1920., 1080.], 2.2);
        assert!(!app.pending);
        assert_eq!(app.navigation_last_result, "worker_unavailable");
        assert_eq!(app.navigation_failures, 1);
        assert_eq!(app.navigation_commits, 0);
        assert_eq!(
            app.current.as_ref().unwrap().photos[0].texture.id(),
            texture
        );
        assert_eq!(app.timeline.committed_at, committed);
        app.receive(&ctx, [1920., 1080.], 2.3);
        assert_eq!(app.navigation_failures, 1);
        assert!(commands.try_recv().is_err());
    }
}

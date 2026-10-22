//! Hand diagnostics and explicitly opt-in fresh swipe delivery. No HTTP write/control API.
use eframe::egui::{
    self, Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2,
};
use serde::Deserialize;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const MAX_AGE: Duration = Duration::from_millis(500);
const EFFECT_SECONDS: f64 = 2.2;
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Bbox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Hand {
    pub bbox: Bbox,
    pub palm_keypoints: [Point; 7],
    pub score: f32,
    pub geometric_quality: bool,
    #[serde(default)]
    pub quality_reason: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    ImageLeft,
    ImageRight,
}
impl Direction {
    pub fn mirrored(self) -> Self {
        match self {
            Self::ImageLeft => Self::ImageRight,
            Self::ImageRight => Self::ImageLeft,
        }
    }
    pub fn sign(self) -> f32 {
        match self {
            Self::ImageLeft => -1.,
            Self::ImageRight => 1.,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::ImageLeft => "left",
            Self::ImageRight => "right",
        }
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct Event {
    pub counter: u64,
    pub sequence: u64,
    pub direction: Direction,
    pub fresh_age_ms: u64,
    pub candidate_only: bool,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Report {
    pub healthy: bool,
    pub controls_enabled: bool,
    pub inference_only: bool,
    pub identity_recognition: bool,
    pub open_hand: String,
    pub intent: String,
    pub coordinates: String,
    pub instance_id: String,
    pub gesture_profile: String,
    pub frame_width: u32,
    pub frame_height: u32,
    pub source_age_ms: u64,
    pub source_headers_verified: bool,
    pub sequence: Option<u64>,
    pub detected_hands: usize,
    pub hands: Vec<Hand>,
    pub candidate_state: String,
    pub event_counter: u64,
    pub last_event: Option<Event>,
    #[serde(default)]
    pub tracking_state: Option<String>,
    #[serde(default)]
    pub tracking_search_roi: Option<Bbox>,
    #[serde(default)]
    pub last_confirmed_age_ms: Option<u64>,
    #[serde(default)]
    pub full_frame_detections: usize,
    #[serde(default)]
    pub roi_detections: usize,
}
#[derive(Clone)]
pub struct Frame {
    pub report: Report,
    pub captured: Instant,
}
pub enum Notice {
    Frame(Box<Frame>),
    Unavailable,
}
pub type Slot = Arc<Mutex<Option<Notice>>>;
fn valid_report(r: &Report) -> bool {
    r.healthy
        && !r.controls_enabled
        && r.inference_only
        && !r.identity_recognition
        && r.open_hand == "unknown"
        && r.intent == "unknown"
        && r.coordinates == "original_image_pixels_unmirrored"
        && r.source_headers_verified
        && r.source_age_ms <= 500
        && r.sequence.is_some_and(|s| s > 0)
        && r.frame_width > 0
        && r.frame_height > 0
        && r.frame_width <= 4096
        && r.frame_height <= 3072
        && !r.instance_id.is_empty()
        && r.instance_id.len() <= 64
        && r.instance_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && matches!(r.gesture_profile.as_str(), "strict" | "display")
        && r.candidate_state.len() <= 64
        && r.detected_hands == r.hands.len()
        && r.hands.len() <= 8
        && r.full_frame_detections <= 8
        && r.roi_detections <= 8
        && r.tracking_state.as_deref().is_none_or(|v| {
            matches!(
                v,
                "untracked" | "locked" | "searching" | "lost" | "ambiguous"
            )
        })
        && r.tracking_search_roi.is_none_or(|b| {
            [b.x1, b.y1, b.x2, b.y2].iter().all(|v| v.is_finite())
                && b.x1 >= 0.
                && b.y1 >= 0.
                && b.x2 > b.x1
                && b.y2 > b.y1
                && b.x2 <= r.frame_width as f32
                && b.y2 <= r.frame_height as f32
        })
        && r.hands.iter().all(|h| {
            h.score.is_finite()
                && (0.0..=1.0).contains(&h.score)
                && [h.bbox.x1, h.bbox.y1, h.bbox.x2, h.bbox.y2]
                    .iter()
                    .all(|v| v.is_finite())
                && h.bbox.x2 > h.bbox.x1
                && h.bbox.y2 > h.bbox.y1
                && [h.bbox.x1, h.bbox.x2]
                    .iter()
                    .all(|x| (-4096.0..=8192.0).contains(x))
                && [h.bbox.y1, h.bbox.y2]
                    .iter()
                    .all(|y| (-3072.0..=6144.0).contains(y))
                && h.palm_keypoints.iter().all(|p| {
                    p.x.is_finite()
                        && p.y.is_finite()
                        && (-4096.0..=8192.0).contains(&p.x)
                        && (-3072.0..=6144.0).contains(&p.y)
                })
        })
}
pub fn parse(bytes: &[u8], elapsed: Duration) -> Result<Frame, &'static str> {
    if bytes.len() > 32768 {
        return Err("hand_json_limit");
    }
    let r: Report = serde_json::from_slice(bytes).map_err(|_| "hand_json_invalid")?;
    if !valid_report(&r) {
        return Err("hand_report_unavailable_or_invalid");
    }
    let age = Duration::from_millis(r.source_age_ms).saturating_add(elapsed);
    if age > MAX_AGE {
        return Err("hand_source_stale");
    }
    let captured = Instant::now().checked_sub(age).ok_or("hand_time_invalid")?;
    Ok(Frame {
        report: r,
        captured,
    })
}
pub fn start(
    stop: Arc<AtomicBool>,
    wake: crate::worker::CameraWake,
) -> std::io::Result<(Slot, JoinHandle<()>)> {
    let slot = Arc::new(Mutex::new(None));
    let out = slot.clone();
    let handle = thread::Builder::new()
        .name("hand-diagnostics".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                let notice = crate::http::hand_status()
                    .and_then(|r| parse(&r.body, started.elapsed()))
                    .map_or(Notice::Unavailable, |f| Notice::Frame(Box::new(f)));
                if let Ok(mut s) = out.lock() {
                    *s = Some(notice);
                }
                if let Ok(ctx) = wake.lock()
                    && let Some(ctx) = ctx.as_ref()
                {
                    ctx.request_repaint();
                }
                while started.elapsed() < Duration::from_millis(125)
                    && !stop.load(Ordering::Relaxed)
                {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            if let Ok(mut s) = out.lock() {
                *s = None;
            }
        })?;
    Ok((slot, handle))
}
/// One already-deduplicated geometric swipe, in MIRRORED screen coordinates.
/// This is not an intent/identity classification; the native action gate is separate.
#[derive(Clone, Debug)]
pub struct Swipe {
    pub direction: Direction,
    pub instance_id: String,
    pub counter: u64,
    pub occurred: Instant,
}
impl Swipe {
    pub fn fresh(&self) -> bool {
        self.occurred.elapsed() <= MAX_AGE
    }
}
#[derive(Default)]
pub struct View {
    current: Option<Frame>,
    instance: Option<String>,
    counter: u64,
    sequence: u64,
    effect: Option<(Direction, f64)>,
    pub flashes: u64,
    pub reports: u64,
    swipe: Option<Swipe>,
    diagnostics: crate::hand_diagnostics::Diagnostics,
}
impl View {
    pub fn apply(&mut self, notice: Notice, now: f64) {
        self.swipe = None;
        let Notice::Frame(frame) = notice else {
            self.diagnostics.unavailable();
            self.current = None;
            self.effect = None;
            return;
        };
        if !now.is_finite() || !valid_report(&frame.report) || frame.captured.elapsed() > MAX_AGE {
            self.diagnostics.invalid();
            self.current = None;
            self.effect = None;
            return;
        }
        let r = &frame.report;
        let changed = self.instance.as_ref() != Some(&r.instance_id);
        let unique_report = changed || r.sequence.unwrap_or(0) > self.sequence;
        if changed {
            self.diagnostics.epoch();
            self.diagnostics.observe(
                &r.candidate_state,
                r.tracking_state.as_deref().unwrap_or("untracked"),
            );
            self.instance = Some(r.instance_id.clone());
            self.counter = r.event_counter;
            self.sequence = r.sequence.unwrap_or(0);
            self.effect = None;
        } else {
            if r.event_counter < self.counter || r.sequence.unwrap_or(0) < self.sequence {
                self.diagnostics.reordered();
                self.current = None;
                self.effect = None;
                return;
            }
            if r.sequence.unwrap_or(0) > self.sequence {
                self.diagnostics.observe(
                    &r.candidate_state,
                    r.tracking_state.as_deref().unwrap_or("untracked"),
                );
            } else {
                self.diagnostics.duplicate();
            }
            if r.event_counter > self.counter {
                self.diagnostics.source_event();
                if let Some(e) = &r.last_event
                    && e.candidate_only
                    && e.counter == r.event_counter
                    && e.counter > self.counter
                    && e.sequence <= r.sequence.unwrap_or(0)
                    && e.sequence > 0
                    && e.fresh_age_ms.saturating_add(
                        (frame.captured.elapsed().as_millis() as u64)
                            .saturating_sub(r.source_age_ms),
                    ) <= 500
                {
                    self.effect = Some((e.direction.mirrored(), now));
                    self.flashes += 1;
                    let elapsed = (frame.captured.elapsed().as_millis() as u64)
                        .saturating_sub(r.source_age_ms);
                    // Multiple/ambiguous/search-only/low-quality frames can never control photos.
                    if e.sequence > self.sequence
                        && r.hands.len() == 1
                        && r.hands[0].geometric_quality
                        && r.hands[0].score >= 0.8
                        && r.tracking_state.as_deref() == Some("locked")
                        && r.last_confirmed_age_ms
                            .is_some_and(|a| a.saturating_add(elapsed) <= 250)
                    {
                        let age = Duration::from_millis(e.fresh_age_ms.saturating_add(elapsed));
                        if let Some(occurred) = Instant::now().checked_sub(age) {
                            self.diagnostics.delivered();
                            self.swipe = Some(Swipe {
                                direction: e.direction.mirrored(),
                                instance_id: r.instance_id.clone(),
                                counter: e.counter,
                                occurred,
                            });
                        }
                    }
                }
                // Expired/missing events cannot be replayed later just because a response repeats.
                self.counter = r.event_counter;
            }
            self.sequence = r.sequence.unwrap_or(self.sequence);
        }
        if unique_report {
            if r.hands.len() == 1 {
                self.diagnostics
                    .single_hand_age(frame.captured.elapsed().as_millis() as u64);
            }
            for h in &r.hands {
                if h.score < 0.8 {
                    self.diagnostics
                        .quality_rejection("score_below_control_threshold");
                } else if !h.geometric_quality {
                    self.diagnostics
                        .quality_rejection(h.quality_reason.as_deref().unwrap_or("not_reported"));
                }
            }
        }
        self.reports += 1;
        self.current = Some(*frame);
        if unique_report
            && matches!(
                self.state(),
                "armed_geometric_only" | "horizontal_motion_candidate"
            )
            && !self.single_locked_hand_fresh()
        {
            self.diagnostics.withheld_readiness();
        }
    }
    pub fn expire(&mut self, now: f64) {
        if self
            .current
            .as_ref()
            .is_some_and(|f| f.captured.elapsed() > MAX_AGE)
        {
            self.diagnostics.expire();
            self.current = None;
            self.effect = None;
            self.swipe = None;
        }
        if self.swipe.as_ref().is_some_and(|s| !s.fresh()) {
            self.swipe = None;
        }
        if self
            .effect
            .is_some_and(|(_, at)| !now.is_finite() || now < at || now - at > EFFECT_SECONDS)
        {
            self.effect = None;
        }
    }
    pub fn diagnostics(&self) -> &crate::hand_diagnostics::Summary {
        &self.diagnostics.summary
    }
    pub fn live(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|f| f.captured.elapsed() <= MAX_AGE)
    }
    pub fn count(&self) -> usize {
        if self.live() {
            self.current.as_ref().map_or(0, |f| f.report.hands.len())
        } else {
            0
        }
    }
    pub fn state(&self) -> &str {
        if self.live() {
            self.current
                .as_ref()
                .map_or("unavailable", |f| f.report.candidate_state.as_str())
        } else {
            "unavailable"
        }
    }
    // Presentation only: raw candidate state remains available for source diagnostics.
    fn single_locked_hand_fresh(&self) -> bool {
        self.live()
            && self.tracking_state() == "locked"
            && self.current.as_ref().is_some_and(|f| {
                f.report.hands.len() == 1
                    && f.report.hands[0].geometric_quality
                    && f.report.hands[0].score >= 0.8
            })
    }
    pub fn ready_for_motion(&self) -> bool {
        self.state() == "armed_geometric_only" && self.single_locked_hand_fresh()
    }
    pub fn guidance_state(&self) -> &str {
        let raw = self.state();
        if matches!(raw, "armed_geometric_only" | "horizontal_motion_candidate")
            && !self.single_locked_hand_fresh()
        {
            "tracking_not_ready"
        } else {
            raw
        }
    }
    pub fn effect_direction(&self) -> Option<&'static str> {
        self.effect.map(|(d, _)| d.label())
    }
    pub fn take_swipe(&mut self) -> Option<Swipe> {
        self.swipe.take().filter(Swipe::fresh)
    }
    pub fn instance(&self) -> Option<&str> {
        self.instance.as_deref()
    }
    pub fn counter(&self) -> u64 {
        self.counter
    }
    pub fn tracking_state(&self) -> &str {
        if !self.live() {
            return "unavailable";
        }
        let state = self
            .current
            .as_ref()
            .and_then(|f| f.report.tracking_state.as_deref())
            .unwrap_or("untracked");
        if matches!(state, "locked" | "searching") && self.confirmed_age().is_none_or(|a| a > 250) {
            "lost"
        } else {
            state
        }
    }
    pub fn search_active(&self) -> bool {
        self.current.as_ref().is_some_and(|f| {
            self.live()
                && f.report.tracking_state.as_deref() == Some("searching")
                && f.report.tracking_search_roi.is_some()
                && f.report.last_confirmed_age_ms.is_some_and(|a| {
                    a.saturating_add(
                        (f.captured.elapsed().as_millis() as u64)
                            .saturating_sub(f.report.source_age_ms),
                    ) <= 250
                })
        })
    }
    pub fn detection_counts(&self) -> (usize, usize) {
        if self.live() {
            self.current.as_ref().map_or((0, 0), |f| {
                (f.report.full_frame_detections, f.report.roi_detections)
            })
        } else {
            (0, 0)
        }
    }
    pub fn confirmed_age(&self) -> Option<u64> {
        if self.live() {
            self.current.as_ref().and_then(|f| {
                f.report.last_confirmed_age_ms.map(|a| {
                    a.saturating_add(
                        (f.captured.elapsed().as_millis() as u64)
                            .saturating_sub(f.report.source_age_ms),
                    )
                })
            })
        } else {
            None
        }
    }
    pub fn boxes(&self, painter: &Painter, fit: Rect) {
        if !self.live() {
            return;
        }
        let Some(f) = &self.current else {
            return;
        };
        for h in &f.report.hands {
            let b = project(h.bbox, f.report.frame_width, f.report.frame_height, fit);
            if let Some(b) = b {
                let color = if h.geometric_quality {
                    Color32::from_rgb(113, 242, 205)
                } else {
                    Color32::from_rgb(244, 188, 95)
                };
                let paint = painter.with_clip_rect(fit);
                paint.rect_stroke(b, 5, Stroke::new(2_f32, color), StrokeKind::Inside);
                for p in &h.palm_keypoints {
                    let px = fit.min.x + (1. - p.x / f.report.frame_width as f32) * fit.width();
                    let py = fit.min.y + p.y / f.report.frame_height as f32 * fit.height();
                    if fit.contains(Pos2::new(px, py)) {
                        paint.circle_filled(Pos2::new(px, py), 2.3, color);
                    }
                }
            }
        }
        if self.search_active()
            && let Some(b) = f
                .report
                .tracking_search_roi
                .and_then(|b| project(b, f.report.frame_width, f.report.frame_height, fit))
        {
            let paint = painter.with_clip_rect(fit);
            let color = Color32::from_rgb(106, 185, 255);
            dashed_rect(&paint, b, color);
            paint.text(
                b.left_top() + Vec2::new(4., 4.),
                Align2::LEFT_TOP,
                "再探索",
                FontId::proportional(10.),
                color,
            );
        }
    }
    pub fn paint(&self, painter: &Painter, canvas: Rect, camera: Rect, now: f64) {
        self.paint_controls(painter, canvas, camera, now, false, None);
    }
    pub fn paint_controls(
        &self,
        painter: &Painter,
        canvas: Rect,
        camera: Rect,
        now: f64,
        controls: bool,
        notice: Option<&str>,
    ) {
        let panel = panel_rect(canvas, camera);
        let p = painter.with_clip_rect(canvas);
        let c = panel.center();
        let direction = self.effect.map(|(d, _)| d);
        let base = match direction {
            Some(Direction::ImageLeft) => Color32::from_rgb(188, 167, 255),
            Some(Direction::ImageRight) => Color32::from_rgb(113, 242, 205),
            None => Color32::from_rgb(166, 205, 235),
        };
        let pulse = (now as f32 * 0.9).sin() * 0.5 + 0.5;
        for layer in (1..=7).rev() {
            let radius = 37. + layer as f32 * 7. + pulse * 5.;
            p.circle_filled(
                c + Vec2::new(0., -10.),
                radius,
                Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 3),
            );
        }
        p.rect_filled(panel, 22, Color32::from_rgba_unmultiplied(13, 22, 34, 216));
        p.rect_stroke(
            panel,
            22,
            Stroke::new(
                1_f32,
                Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 65),
            ),
            StrokeKind::Inside,
        );
        let (hand_x, title) = if let Some((d, at)) = self.effect {
            let progress = ((now - at) / EFFECT_SECONDS).clamp(0., 1.) as f32;
            let ease = (1. - (progress * std::f32::consts::PI).cos()) * 0.5;
            (
                (ease * 2. - 1.) * 75. * d.sign(),
                if d == Direction::ImageLeft {
                    "← 左スワイプを検知"
                } else {
                    "右スワイプを検知 →"
                },
            )
        } else {
            let title = match self.guidance_state() {
                "armed_geometric_only" => "手を検出 · 左右に動かしてください",
                "stabilizing" => "手を検出 · 少し止めてください",
                "horizontal_motion_candidate" => "手の動きを確認中…",
                "outside_activation_region" => "手を小窓の中央付近へ",
                "motion_quality_rejected" => "一定の速さで、まっすぐ横へ動かしてください",
                "tracking_not_ready" => "手を追跡中 · 中央で少し止めてください",
                "brief_detection_gap" | "searching_hand" => "一瞬見失い · 周辺を再検出中…",
                "searching_gap_expired" => "手を中央で止めて、もう一度",
                "ambiguous_hand_association" => "手の追跡を確認 · 一度下げてください",
                "ambiguous_multiple_hands" => "片手だけで試してください",
                "hand_quality_rejected" => "手をもう少し大きく映してください",
                "cooldown" | "awaiting_hand_withdrawal" => "一度手を下げて、もう一度",
                "unavailable" => "手の検出を確認中…",
                _ => "片手を少し止めてから、左右にスワイプ",
            };
            ((now as f32 * 1.1).sin() * 40., title)
        };
        let center = Pos2::new(
            c.x + hand_x,
            panel.min.y + 54. + (now as f32 * 1.7).sin() * 3.,
        );
        for i in (1..=4).rev() {
            hand_glyph(
                &p,
                center - Vec2::new((i * 9) as f32 * direction.map_or(1., Direction::sign), 0.),
                0.62,
                base,
                10,
            );
        }
        hand_glyph(&p, center, 0.72, base, 230);
        for dir in [-1., 1.] {
            let x = c.x + dir * (panel.width() * 0.30).min(150.);
            let active = direction.is_some_and(|d| d.sign() == dir);
            chevron(
                &p,
                Pos2::new(x, panel.min.y + 50.),
                dir,
                base,
                if active {
                    235
                } else {
                    65 + (pulse * 40.) as u8
                },
            );
        }
        p.text(
            Pos2::new(c.x, panel.min.y + 98.),
            Align2::CENTER_CENTER,
            title,
            FontId::proportional(if panel.width() > 450. { 21. } else { 14. }),
            Color32::from_rgb(229, 237, 246),
        );
        p.text(
            Pos2::new(c.x, panel.max.y - 17.),
            Align2::CENTER_CENTER,
            if controls {
                notice.unwrap_or("← 左へ：次の写真 ／ 右へ：前の写真 →")
            } else {
                "検知表示のみ · 写真はまだ操作しません"
            },
            FontId::proportional(if panel.width() > 450. { 13. } else { 10. }),
            Color32::from_rgb(151, 173, 191),
        );
    }
}
pub fn project(b: Bbox, w: u32, h: u32, fit: Rect) -> Option<Rect> {
    if w == 0
        || h == 0
        || ![b.x1, b.y1, b.x2, b.y2].iter().all(|v| v.is_finite())
        || b.x2 <= b.x1
        || b.y2 <= b.y1
    {
        return None;
    }
    let out = Rect::from_min_max(
        Pos2::new(
            fit.min.x + (1. - b.x2 / w as f32) * fit.width(),
            fit.min.y + b.y1 / h as f32 * fit.height(),
        ),
        Pos2::new(
            fit.min.x + (1. - b.x1 / w as f32) * fit.width(),
            fit.min.y + b.y2 / h as f32 * fit.height(),
        ),
    )
    .intersect(fit);
    (out.width() > 1. && out.height() > 1.).then_some(out)
}
pub fn panel_rect(canvas: Rect, camera: Rect) -> Rect {
    let left = canvas.min.x + 20.;
    let right = (camera.min.x - 16.).max(left + 100.);
    let width = (right - left).clamp(100., 620.);
    let center_x = canvas
        .center()
        .x
        .clamp(left + width / 2., right - width / 2.);
    Rect::from_center_size(
        Pos2::new(center_x, canvas.max.y - 132.),
        Vec2::new(width, 150.),
    )
}
fn dashed_rect(p: &Painter, r: Rect, color: Color32) {
    for (a, b) in [
        (r.left_top(), r.right_top()),
        (r.right_top(), r.right_bottom()),
        (r.right_bottom(), r.left_bottom()),
        (r.left_bottom(), r.left_top()),
    ] {
        let length = a.distance(b);
        if length < 1. {
            continue;
        }
        let dir = (b - a) / length;
        let mut t = 0.;
        while t < length {
            let end = (t + 5.).min(length);
            p.line_segment([a + dir * t, a + dir * end], Stroke::new(1.5_f32, color));
            t += 9.;
        }
    }
}
fn chevron(p: &Painter, c: Pos2, dir: f32, color: Color32, alpha: u8) {
    let color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
    for offset in [0., 12.] {
        let x = c.x + dir * offset;
        p.add(Shape::line(
            vec![
                Pos2::new(x - dir * 7., c.y - 8.),
                Pos2::new(x, c.y),
                Pos2::new(x - dir * 7., c.y + 8.),
            ],
            Stroke::new(3_f32, color),
        ));
    }
}
fn hand_glyph(p: &Painter, c: Pos2, scale: f32, color: Color32, alpha: u8) {
    let tint = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
    let point = |x: f32, y: f32| c + Vec2::new(x, y) * scale;
    p.rect_filled(
        Rect::from_min_max(point(-19., -8.), point(21., 30.)),
        11,
        tint,
    );
    for (x, length) in [(-17., 31.), (-6., 42.), (5., 39.), (16., 29.)] {
        p.rect_filled(
            Rect::from_min_max(point(x - 4., -length), point(x + 4., 4.)),
            4,
            tint,
        );
    }
    p.add(Shape::convex_polygon(
        vec![
            point(-17., 3.),
            point(-33., -8.),
            point(-40., -6.),
            point(-40., 2.),
            point(-20., 27.),
        ],
        tint,
        Stroke::NONE,
    ));
    p.rect_filled(
        Rect::from_min_max(point(-11., 27.), point(13., 40.)),
        3,
        tint,
    );
}
pub fn install_japanese_font(ctx: &egui::Context) -> bool {
    // The display ships Droid; the others let the same binary run on a dev machine.
    const PATHS: [&str; 3] = [
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc",
    ];
    let Some(bytes) = PATHS.iter().find_map(|path| {
        std::fs::metadata(path)
            .is_ok_and(|m| m.is_file() && m.len() <= 32 * 1024 * 1024)
            .then(|| std::fs::read(path).ok())
            .flatten()
    }) else {
        return false;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "local-japanese".into(),
        egui::FontData::from_owned(bytes).into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push("local-japanese".into());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("local-japanese".into());
    ctx.set_fonts(fonts);
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    fn notice(frame: Frame) -> Notice {
        Notice::Frame(Box::new(frame))
    }
    fn report(counter: u64) -> Frame {
        let r = Report {
            healthy: true,
            controls_enabled: false,
            inference_only: true,
            identity_recognition: false,
            open_hand: "unknown".into(),
            intent: "unknown".into(),
            coordinates: "original_image_pixels_unmirrored".into(),
            instance_id: "test-1".into(),
            gesture_profile: "display".into(),
            frame_width: 1920,
            frame_height: 1080,
            source_age_ms: 20,
            source_headers_verified: true,
            sequence: Some(20 + counter),
            detected_hands: 0,
            hands: vec![],
            candidate_state: "no_hand".into(),
            event_counter: counter,
            last_event: None,
            tracking_state: None,
            tracking_search_roi: None,
            last_confirmed_age_ms: None,
            full_frame_detections: 0,
            roi_detections: 0,
        };
        Frame {
            report: r,
            captured: Instant::now(),
        }
    }
    fn event(counter: u64, d: Direction) -> Frame {
        let mut f = report(counter);
        f.report.last_event = Some(Event {
            counter,
            sequence: 20 + counter,
            direction: d,
            fresh_age_ms: 20,
            candidate_only: true,
        });
        f
    }
    #[test]
    fn old_event_not_replayed_then_mirror_and_dedup() {
        let mut v = View::default();
        v.apply(notice(event(3, Direction::ImageLeft)), 1.);
        assert_eq!(v.flashes, 0);
        v.apply(notice(event(4, Direction::ImageLeft)), 2.);
        assert_eq!(v.effect_direction(), Some("right"));
        v.apply(notice(event(4, Direction::ImageLeft)), 2.1);
        assert_eq!(v.flashes, 1);
        v.apply(notice(event(5, Direction::ImageRight)), 2.2);
        assert_eq!(v.effect_direction(), Some("left"));
        assert_eq!(v.flashes, 2);
    }
    #[test]
    fn stale_error_and_restart_clear_effect_and_boxes() {
        let mut v = View::default();
        v.apply(notice(report(0)), 1.);
        v.apply(notice(event(1, Direction::ImageLeft)), 2.);
        v.apply(Notice::Unavailable, 2.1);
        assert!(!v.live());
        assert_eq!(v.effect_direction(), None);
        let mut f = event(8, Direction::ImageRight);
        f.report.instance_id = "test-2".into();
        v.apply(notice(f), 3.);
        assert_eq!(v.flashes, 1);
        assert_eq!(v.effect_direction(), None);
        let mut f = report(9);
        f.captured = Instant::now() - Duration::from_secs(1);
        v.apply(notice(f), 4.);
        assert!(!v.live());
    }
    #[test]
    fn expired_invalid_and_reset_events_never_flash() {
        let mut v = View::default();
        v.apply(notice(report(0)), 1.);
        let mut f = event(1, Direction::ImageLeft);
        f.report.last_event.as_mut().unwrap().fresh_age_ms = 600;
        v.apply(notice(f), 2.);
        assert_eq!(v.flashes, 0);
        v.apply(notice(event(1, Direction::ImageLeft)), 2.1);
        assert_eq!(v.flashes, 0);
        v.apply(notice(report(0)), 2.2);
        assert!(!v.live());
        let mut f = report(2);
        f.report.controls_enabled = true;
        v.apply(notice(f), 3.);
        assert!(!v.live());
    }
    #[test]
    fn effect_expires_without_navigation() {
        let mut v = View::default();
        v.apply(notice(report(0)), 1.);
        v.apply(notice(event(1, Direction::ImageRight)), 2.);
        v.expire(4.3);
        assert_eq!(v.effect_direction(), None);
        assert_eq!(v.flashes, 1);
    }
    #[test]
    fn mirrored_box_and_panel_camera_safety() {
        let fit = Rect::from_min_size(Pos2::new(100., 100.), Vec2::new(640., 360.));
        let p = project(
            Bbox {
                x1: 0.,
                y1: 0.,
                x2: 192.,
                y2: 108.,
            },
            1920,
            1080,
            fit,
        )
        .unwrap();
        assert!((p.min.x - 676.).abs() < 0.01);
        assert!((p.width() - 64.).abs() < 0.01);
        for (w, h) in [(1920., 1080.), (1280., 720.), (640., 360.)] {
            let c = Rect::from_min_size(Pos2::ZERO, Vec2::new(w, h));
            let r = crate::layout::camera_rect(w, h);
            let cam = Rect::from_min_size(Pos2::new(r.x, r.y), Vec2::new(r.w, r.h));
            let p = panel_rect(c, cam);
            assert!(!p.intersects(cam));
            assert!(c.contains_rect(p));
        }
    }
    #[test]
    fn parse_bounds_and_freshness() {
        let mut f = report(0);
        let val = serde_json::json!({"healthy":true,"controls_enabled":false,"inference_only":true,"identity_recognition":false,"open_hand":"unknown","intent":"unknown","coordinates":"original_image_pixels_unmirrored","instance_id":"test-1","gesture_profile":"display","frame_width":1920,"frame_height":1080,"source_age_ms":20,"source_headers_verified":true,"sequence":20,"detected_hands":0,"hands":[],"candidate_state":"no_hand","event_counter":0,"last_event":null});
        let bytes = serde_json::to_vec(&val).unwrap();
        assert!(parse(&bytes, Duration::from_millis(20)).is_ok());
        assert!(parse(&bytes, Duration::from_secs(1)).is_err());
        assert!(parse(&vec![0; 32769], Duration::ZERO).is_err());
        f.report.source_headers_verified = false;
        assert!(!valid_report(&f.report));
    }
    #[test]
    fn malformed_and_expired_event_cannot_overflow_or_flash() {
        let mut v = View::default();
        v.apply(notice(report(0)), 1.);
        let mut f = event(1, Direction::ImageLeft);
        f.report.last_event.as_mut().unwrap().fresh_age_ms = u64::MAX;
        v.apply(notice(f), 2.);
        assert_eq!(v.flashes, 0);
        let mut f = report(2);
        f.report.hands = vec![Hand {
            bbox: Bbox {
                x1: 0.,
                y1: 0.,
                x2: f32::MAX,
                y2: f32::MAX,
            },
            palm_keypoints: [Point { x: 0., y: 0. }; 7],
            score: 0.9,
            geometric_quality: true,
            quality_reason: None,
        }];
        f.report.detected_hands = 1;
        assert!(!valid_report(&f.report));
    }
    #[test]
    fn fluffy_hint_and_swipe_effect_emit_finite_vector_meshes() {
        let ctx = egui::Context::default();
        let mut v = View::default();
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(1920., 1080.));
        let camera = Rect::from_min_size(Pos2::new(1516., 842.), Vec2::new(380., 214.));
        for (now, event_count) in [(0., 0), (2., 0), (2.1, 1), (3., 1)] {
            if event_count == 0 {
                v.apply(notice(report(0)), now);
            } else {
                v.apply(notice(event(1, Direction::ImageLeft)), now);
            }
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(canvas),
                    ..Default::default()
                },
                |ctx| {
                    let p = ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("test-hint"),
                    ));
                    v.paint(&p, canvas, camera, now);
                },
            );
            let primitives = ctx.tessellate(output.shapes, 1.);
            let mut vertices = 0;
            for p in primitives {
                if let egui::epaint::Primitive::Mesh(mesh) = p.primitive {
                    vertices += mesh.vertices.len();
                    assert!(
                        mesh.vertices
                            .iter()
                            .all(|v| v.pos.x.is_finite() && v.pos.y.is_finite())
                    );
                }
            }
            assert!(vertices > 100);
            assert_eq!(v.flashes, event_count);
        }
        assert_eq!(v.effect_direction(), Some("right"));
    }
    #[test]
    fn searching_roi_is_not_a_detected_hand_or_a_swipe() {
        let mut v = View::default();
        let mut f = report(0);
        f.report.tracking_state = Some("searching".into());
        f.report.tracking_search_roi = Some(Bbox {
            x1: 500.,
            y1: 200.,
            x2: 900.,
            y2: 600.,
        });
        f.report.last_confirmed_age_ms = Some(50);
        v.apply(notice(f), 1.);
        assert!(v.search_active());
        assert_eq!(v.count(), 0);
        assert_eq!(v.flashes, 0);
        assert_eq!(v.effect_direction(), None);
        v.apply(Notice::Unavailable, 1.1);
        assert!(!v.search_active());
        assert_eq!(v.tracking_state(), "unavailable");
    }
    #[test]
    fn search_is_expired_even_with_a_fresh_healthy_source() {
        let mut v = View::default();
        let mut f = report(0);
        f.report.tracking_state = Some("searching".into());
        f.report.tracking_search_roi = Some(Bbox {
            x1: 500.,
            y1: 200.,
            x2: 900.,
            y2: 600.,
        });
        f.report.last_confirmed_age_ms = Some(251);
        v.apply(notice(f), 1.);
        assert!(v.live());
        assert!(!v.search_active());
        assert_eq!(v.tracking_state(), "lost");
        assert_eq!(v.count(), 0);
        let mut f = report(0);
        f.report.tracking_state = Some("searching".into());
        f.report.last_confirmed_age_ms = Some(u64::MAX);
        v.apply(notice(f), 1.1);
        assert!(!v.search_active());
    }
    #[test]
    fn invalid_search_geometry_fails_closed() {
        for b in [
            Bbox {
                x1: -1.,
                y1: 0.,
                x2: 600.,
                y2: 200.,
            },
            Bbox {
                x1: 0.,
                y1: 0.,
                x2: 1921.,
                y2: 200.,
            },
            Bbox {
                x1: 20.,
                y1: 0.,
                x2: 10.,
                y2: 100.,
            },
            Bbox {
                x1: 0.,
                y1: 0.,
                x2: f32::INFINITY,
                y2: 100.,
            },
        ] {
            let mut f = report(0);
            f.report.tracking_search_roi = Some(b);
            assert!(!valid_report(&f.report));
        }
    }
    #[test]
    fn search_dash_mesh_is_finite_and_clipped_to_preview() {
        let ctx = egui::Context::default();
        let mut v = View::default();
        let mut f = report(0);
        f.report.tracking_state = Some("searching".into());
        f.report.tracking_search_roi = Some(Bbox {
            x1: 500.,
            y1: 200.,
            x2: 900.,
            y2: 600.,
        });
        f.report.last_confirmed_age_ms = Some(10);
        v.apply(notice(f), 1.);
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(640., 360.));
        let out = ctx.run(
            egui::RawInput {
                screen_rect: Some(canvas),
                ..Default::default()
            },
            |ctx| {
                let p = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("search-test"),
                ));
                v.boxes(&p, canvas);
            },
        );
        let meshes = ctx.tessellate(out.shapes, 1.);
        let mut n = 0;
        for p in meshes {
            if let egui::epaint::Primitive::Mesh(m) = p.primitive {
                n += m.vertices.len();
                assert!(
                    m.vertices
                        .iter()
                        .all(|v| v.pos.x.is_finite() && v.pos.y.is_finite())
                );
            }
        }
        assert!(n > 40);
        assert_eq!(v.count(), 0);
        assert_eq!(v.flashes, 0);
    }
    #[test]
    fn confirmed_age_does_not_add_source_age_twice() {
        let mut v = View::default();
        let mut f = report(0);
        f.report.source_age_ms = 180;
        f.captured = Instant::now() - Duration::from_millis(180);
        f.report.tracking_state = Some("searching".into());
        f.report.tracking_search_roi = Some(Bbox {
            x1: 500.,
            y1: 200.,
            x2: 900.,
            y2: 600.,
        });
        f.report.last_confirmed_age_ms = Some(200);
        v.apply(notice(f), 1.);
        assert!(v.search_active());
        assert_eq!(v.tracking_state(), "searching");
        assert!((200..=220).contains(&v.confirmed_age().unwrap()));
        assert_eq!(v.count(), 0);
    }
    fn controllable(counter: u64, direction: Direction) -> Frame {
        let mut f = event(counter, direction);
        f.report.hands = vec![Hand {
            bbox: Bbox {
                x1: 800.,
                y1: 400.,
                x2: 860.,
                y2: 460.,
            },
            palm_keypoints: std::array::from_fn(|i| Point {
                x: 812. + i as f32 * 5.,
                y: 410. + (i % 3) as f32 * 12.,
            }),
            score: 0.9,
            geometric_quality: true,
            quality_reason: None,
        }];
        f.report.detected_hands = 1;
        f.report.tracking_state = Some("locked".into());
        f.report.last_confirmed_age_ms = Some(20);
        f
    }
    #[test]
    fn qualified_swipe_is_screen_mirrored_once_and_model_stays_disabled() {
        let mut v = View::default();
        v.apply(notice(report(0)), 1.);
        v.apply(notice(controllable(1, Direction::ImageRight)), 2.);
        let s = v.take_swipe().unwrap();
        assert_eq!(s.direction, Direction::ImageLeft);
        assert_eq!(s.counter, 1);
        assert!(s.fresh());
        assert!(v.take_swipe().is_none());
        v.apply(notice(controllable(1, Direction::ImageRight)), 2.1);
        assert!(v.take_swipe().is_none());
    }
    #[test]
    fn multiple_search_rejected_and_old_sequence_only_display_never_action() {
        for mode in 0..5 {
            let mut v = View::default();
            v.apply(notice(report(0)), 1.);
            let mut f = controllable(1, Direction::ImageRight);
            match mode {
                0 => {
                    f.report.hands.push(f.report.hands[0].clone());
                    f.report.detected_hands = 2;
                    f.report.tracking_state = Some("ambiguous".into());
                }
                1 => f.report.tracking_state = Some("searching".into()),
                2 => f.report.hands[0].geometric_quality = false,
                3 => f.report.last_confirmed_age_ms = Some(251),
                4 => f.report.last_event.as_mut().unwrap().sequence = 20,
                _ => unreachable!(),
            };
            v.apply(notice(f), 2.);
            assert!(v.take_swipe().is_none());
        }
    }
    #[test]
    fn action_age_counts_source_only_once_and_unavailable_clears_pending() {
        let mut v = View::default();
        v.apply(notice(report(0)), 1.);
        let mut f = controllable(1, Direction::ImageRight);
        f.report.source_age_ms = 220;
        f.report.last_confirmed_age_ms = Some(220);
        f.report.last_event.as_mut().unwrap().fresh_age_ms = 350;
        f.captured = Instant::now() - Duration::from_millis(220);
        v.apply(notice(f), 2.);
        assert!(v.take_swipe().is_some());
        v.apply(notice(controllable(2, Direction::ImageRight)), 3.);
        v.apply(Notice::Unavailable, 3.1);
        assert!(v.take_swipe().is_none());
    }
    #[test]
    fn action_initial_and_new_epoch_baseline_expired_missing_events_never_replay() {
        let mut v = View::default();
        v.apply(notice(controllable(5, Direction::ImageRight)), 1.);
        assert!(v.take_swipe().is_none());
        let mut f = controllable(6, Direction::ImageRight);
        f.report.last_event.as_mut().unwrap().fresh_age_ms = 501;
        v.apply(notice(f), 2.);
        assert!(v.take_swipe().is_none());
        v.apply(notice(controllable(6, Direction::ImageRight)), 2.1);
        assert!(v.take_swipe().is_none());
        let mut f = controllable(10, Direction::ImageRight);
        f.report.instance_id = "restart-epoch".into();
        v.apply(notice(f), 3.);
        assert!(v.take_swipe().is_none());
    }
    #[test]
    fn diagnostic_attempt_survives_hand_removal_and_never_creates_a_swipe() {
        let mut v = View::default();
        for (i, state) in [
            "stabilizing",
            "armed_geometric_only",
            "horizontal_motion_candidate",
            "motion_quality_rejected",
            "no_hand",
        ]
        .iter()
        .enumerate()
        {
            let mut f = report(0);
            f.report.sequence = Some(100 + i as u64);
            f.report.candidate_state = (*state).into();
            v.apply(notice(f.clone()), i as f64);
            v.apply(notice(f), i as f64 + 0.1);
            assert!(v.take_swipe().is_none());
        }
        let d = v.diagnostics();
        assert_eq!(d.unique_reports, 5);
        assert_eq!(d.duplicate_reports, 5);
        assert_eq!(d.source_event_increases, 0);
        assert_eq!(d.fresh_swipes_delivered, 0);
        assert_eq!(
            d.last_interruption.as_ref().unwrap().observed_reason,
            "motion_quality_rejected"
        );
        assert_eq!(v.flashes, 0);
        let mut f = report(0);
        f.report.sequence = Some(106);
        v.apply(notice(f), 10.);
        assert_eq!(
            v.diagnostics().last_attempt_outcome,
            Some("motion_quality_rejected")
        );
    }
    #[test]
    fn diagnostic_invalid_reordered_and_new_epoch_cannot_trigger_actions() {
        let mut v = View::default();
        let mut f = report(0);
        f.report.candidate_state = "armed_geometric_only".into();
        v.apply(notice(f.clone()), 0.);
        v.apply(Notice::Unavailable, 0.1);
        assert_eq!(
            v.diagnostics().last_attempt_outcome,
            Some("input_unavailable")
        );
        f.report.sequence = Some(19);
        v.apply(notice(f.clone()), 0.2);
        assert_eq!(v.diagnostics().reordered_reports, 1);
        f.report.sequence = Some(21);
        f.captured = Instant::now() - Duration::from_secs(1);
        v.apply(notice(f), 0.3);
        assert_eq!(v.diagnostics().invalid_or_stale_reports, 1);
        let mut f = event(8, Direction::ImageRight);
        f.report.instance_id = "new-epoch".into();
        v.apply(notice(f), 1.);
        assert_eq!(v.diagnostics().source_epochs, 2);
        assert_eq!(v.diagnostics().source_event_increases, 0);
        assert!(v.take_swipe().is_none());
        assert_eq!(v.flashes, 0);
    }
    fn armed_frame() -> Frame {
        let mut f = controllable(0, Direction::ImageLeft);
        f.report.last_event = None;
        f.report.candidate_state = "armed_geometric_only".into();
        f
    }
    #[test]
    fn readiness_guidance_follows_existing_tracking_age_without_changing_raw_state() {
        let mut v = View::default();
        v.apply(notice(armed_frame()), 1.);
        assert!(v.ready_for_motion());
        assert_eq!(v.guidance_state(), "armed_geometric_only");
        assert_eq!(v.diagnostics().readiness_withheld_reports, 0);
        assert!(v.take_swipe().is_none());
        let mut f = armed_frame();
        f.report.sequence = Some(21);
        f.report.source_age_ms = 240;
        f.report.last_confirmed_age_ms = Some(240);
        f.captured = Instant::now() - Duration::from_millis(270);
        v.apply(notice(f), 1.1);
        assert!(v.live());
        assert_eq!(v.state(), "armed_geometric_only");
        assert_eq!(v.tracking_state(), "lost");
        assert!(!v.ready_for_motion());
        assert_eq!(v.guidance_state(), "tracking_not_ready");
        assert_eq!(v.diagnostics().readiness_withheld_reports, 1);
        assert!(v.take_swipe().is_none());
        let mut f = armed_frame();
        f.report.sequence = Some(22);
        f.report.tracking_state = Some("lost".into());
        f.report.last_confirmed_age_ms = None;
        v.apply(notice(f), 1.2);
        assert_eq!(v.guidance_state(), "tracking_not_ready");
        assert_eq!(v.diagnostics().readiness_withheld_reports, 2);
    }
    #[test]
    fn readiness_expires_between_publications_even_while_source_is_healthy() {
        let mut v = View::default();
        let mut f = armed_frame();
        f.report.source_age_ms = 200;
        f.report.last_confirmed_age_ms = Some(220);
        f.captured = Instant::now() - Duration::from_millis(200);
        v.apply(notice(f), 1.);
        assert!(v.ready_for_motion());
        v.current.as_mut().unwrap().captured = Instant::now() - Duration::from_millis(240);
        assert!(v.live());
        assert_eq!(v.state(), "armed_geometric_only");
        assert!(!v.ready_for_motion());
        assert_eq!(v.guidance_state(), "tracking_not_ready");
    }
    #[test]
    fn guidance_never_advertises_ready_for_missing_multiple_rejected_or_search_only_hands() {
        for variant in 0..7 {
            let mut v = View::default();
            let mut f = armed_frame();
            match variant {
                0 => {
                    f.report.hands.clear();
                    f.report.detected_hands = 0;
                }
                1 => {
                    f.report.hands.push(f.report.hands[0].clone());
                    f.report.detected_hands = 2;
                }
                2 => f.report.hands[0].score = 0.79,
                3 => f.report.hands[0].geometric_quality = false,
                4 => f.report.tracking_state = Some("searching".into()),
                5 => f.report.last_confirmed_age_ms = None,
                _ => {
                    f.report.candidate_state = "horizontal_motion_candidate".into();
                    f.report.last_confirmed_age_ms = Some(251);
                }
            }
            v.apply(notice(f), 1.);
            assert_eq!(v.guidance_state(), "tracking_not_ready");
            assert!(!v.ready_for_motion());
            assert!(v.take_swipe().is_none());
            assert_eq!(v.diagnostics().readiness_withheld_reports, 1);
            assert_eq!(v.flashes, 0);
        }
    }
    #[test]
    fn upstream_quality_code_and_single_hand_age_survive_removal_without_geometry_history() {
        let mut v = View::default();
        let mut f = controllable(0, Direction::ImageLeft);
        f.report.last_event = None;
        f.report.candidate_state = "hand_quality_rejected".into();
        f.report.hands[0].geometric_quality = false;
        f.report.hands[0].quality_reason = Some("palm_keypoints_inconsistent".into());
        f.report.source_age_ms = 220;
        f.captured = Instant::now() - Duration::from_millis(220);
        v.apply(notice(f), 1.);
        let mut f = report(0);
        f.report.sequence = Some(21);
        v.apply(notice(f), 1.1);
        assert_eq!(
            v.diagnostics().quality_rejection_reasons["palm_keypoints_inconsistent"],
            1
        );
        assert_eq!(v.diagnostics().single_hand_reports, 1);
        assert_eq!(v.diagnostics().single_hand_input_age_ms_bins["201_250"], 1);
        assert!(v.take_swipe().is_none());
        assert_eq!(v.count(), 0);
        let mut f = controllable(0, Direction::ImageLeft);
        f.report.sequence = Some(22);
        f.report.hands[0].score = 0.79;
        v.apply(notice(f), 1.2);
        assert_eq!(
            v.diagnostics().quality_rejection_reasons["score_below_control_threshold"],
            1
        );
    }
}

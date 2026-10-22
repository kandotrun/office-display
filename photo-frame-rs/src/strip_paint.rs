//! Painting for the info strip and the HUD chrome. Geometry and timing come
//! from `strip`, `globe` and `theme`; this module only draws.
use crate::{
    atlas,
    globe,
    strip::{self, Blocks, View, ViewFrame},
    theme::{Mood, Palette, Rgb, Sun},
};
use chrono::{DateTime, Datelike, Local, NaiveDate, Timelike, Utc};
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Shape, Stroke, StrokeKind, TextureHandle, Vec2,
    epaint,
};

pub const CREDITS: &str = "© OpenStreetMap contributors · 気象庁 · NASA GIBS";

pub fn color(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}
pub fn alpha(rgb: Rgb, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], (a.clamp(0.0, 1.0) * 255.0) as u8)
}
pub fn japanese_date(date: NaiveDate) -> String {
    const DAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];
    format!(
        "{}月{}日 {}曜日",
        date.month(),
        date.day(),
        DAYS[date.weekday().num_days_from_monday() as usize]
    )
}
/// "OBS -30" / "NOW" / "FCST +20" for radar frame `i` at 10 minute steps.
pub fn frame_tag(i: usize, now_index: usize) -> String {
    let minutes = (i as i64 - now_index as i64) * 10;
    match minutes {
        0 => "NOW".into(),
        m if m < 0 => format!("OBS {m}"),
        m => format!("FCST +{m}"),
    }
}
fn local_hm(at: &str) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(at).ok()?;
    Some(t.with_timezone(&Local).format("%H:%M").to_string())
}
pub fn coords(lat: f64, lon: f64) -> String {
    format!(
        "{:.2}{} {:.2}{}",
        lat.abs(),
        if lat >= 0.0 { "N" } else { "S" },
        lon.abs(),
        if lon >= 0.0 { "E" } else { "W" }
    )
}
/// Deterministic 0..1 noise for stars and flicker.
fn hash(i: u32) -> f32 {
    let mut x = i.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}
fn r(rect: crate::layout::Rect, origin: Pos2) -> Rect {
    Rect::from_min_size(origin + Vec2::new(rect.x, rect.y), Vec2::new(rect.w, rect.h))
}
fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}
fn prop(size: f32) -> FontId {
    FontId::proportional(size)
}

/// L-shaped corner ticks, the main HUD motif.
pub fn brackets(painter: &egui::Painter, rect: Rect, len: f32, stroke: Stroke) {
    let len = len.min(rect.width() / 2.0).min(rect.height() / 2.0);
    for (corner, dx, dy) in [
        (rect.left_top(), 1.0, 1.0),
        (rect.right_top(), -1.0, 1.0),
        (rect.left_bottom(), 1.0, -1.0),
        (rect.right_bottom(), -1.0, -1.0),
    ] {
        painter.line_segment([corner, corner + Vec2::new(dx * len, 0.0)], stroke);
        painter.line_segment([corner, corner + Vec2::new(0.0, dy * len)], stroke);
    }
}
/// Faint dot grid behind everything, drifting very slowly.
pub fn backdrop(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64) {
    let step = 32.0;
    let drift = ((now * 2.0) % step as f64) as f32;
    let dot = alpha(palette.border, 0.16);
    let mut y = rect.top() + drift;
    let mut row = 0;
    while y < rect.bottom() {
        let mut x = rect.left() + if row % 2 == 0 { 0.0 } else { step / 2.0 };
        while x < rect.right() {
            painter.circle_filled(Pos2::new(x, y), 0.8, dot);
            x += step;
        }
        y += step;
        row += 1;
    }
}

pub struct MapLayer<'a> {
    pub base: &'a TextureHandle,
    pub radar: &'a [TextureHandle],
    pub frames: &'a [atlas::Frame],
    pub now_index: usize,
    pub office: [f32; 2],
    pub office_coords: (f64, f64),
}
pub struct GlobeLayer<'a> {
    pub day: &'a TextureHandle,
    pub night: &'a TextureHandle,
    pub view: (f64, f64),
    pub sun: Sun,
    pub office: (f64, f64),
    pub marker: Option<(f64, f64, &'a str)>,
}
pub struct PlaceLayer<'a> {
    pub map: &'a TextureHandle,
    pub place: &'a str,
    pub coords: (f64, f64),
}
pub enum MapCard<'a> {
    Preparing,
    Map(MapLayer<'a>),
    Globe(GlobeLayer<'a>),
    Place(PlaceLayer<'a>),
}
pub struct Telemetry<'a> {
    pub office: (f64, f64),
    pub sun: Sun,
    pub phase: &'a str,
    pub mood: Option<Mood>,
    pub atlas_state: &'a str,
    pub scene_id: u64,
    pub photos: usize,
    pub uptime: f64,
}
pub struct StripInfo<'a> {
    pub weather: Option<&'a atlas::Weather>,
    pub rain: Option<&'a atlas::Rain>,
    /// When the rain series last changed, for the bar growth animation.
    pub rain_since: f64,
    pub telemetry: Telemetry<'a>,
}
pub struct CardState<'a> {
    pub frame: Option<ViewFrame>,
    pub current: MapCard<'a>,
    pub previous: Option<MapCard<'a>>,
    pub ready: &'a [View],
}

pub fn paint_strip(
    painter: &egui::Painter,
    origin: Pos2,
    strip: crate::layout::Rect,
    blocks: &Blocks,
    palette: &Palette,
    clock: DateTime<Local>,
    now: f64,
    card: CardState<'_>,
    info: &StripInfo<'_>,
) {
    let k = blocks.scale;
    let panel = r(strip, origin);
    painter.rect_filled(panel, 6, alpha(palette.surface, 0.78));
    painter.rect_stroke(panel, 6, Stroke::new(1.0, alpha(palette.border, 0.45)), StrokeKind::Inside);
    brackets(painter, panel.shrink(3.0), 18.0 * k, Stroke::new(1.5, alpha(palette.accent, 0.8)));
    // A highlight that travels down the left edge.
    let travel = ((now / 6.0) % 1.0) as f32;
    let y = panel.top() + travel * panel.height();
    for i in 0..24 {
        let a = 0.5 * (1.0 - i as f32 / 24.0);
        let yy = y - i as f32 * 3.0;
        if yy > panel.top() {
            painter.line_segment(
                [Pos2::new(panel.left() + 0.5, yy), Pos2::new(panel.left() + 0.5, yy - 3.0)],
                Stroke::new(2.0, alpha(palette.accent, a)),
            );
        }
    }
    paint_clock(painter, r(blocks.clock, origin), palette, clock, now, k);
    paint_weather(painter, r(blocks.weather, origin), palette, info.weather, now, k);
    paint_card(painter, r(blocks.map, origin), palette, now, k, card);
    paint_telemetry(painter, r(blocks.telemetry, origin), palette, now, k, &info.telemetry);
    paint_rain(painter, r(blocks.rain, origin), palette, now, k, info.rain, info.rain_since);
    let footer = r(blocks.footer, origin);
    painter.line_segment(
        [footer.left_top(), footer.right_top()],
        Stroke::new(1.0, alpha(palette.border, 0.4)),
    );
    painter.text(
        footer.left_top() + Vec2::new(0.0, 8.0 * k),
        Align2::LEFT_TOP,
        "TSUQREA  ·  2F DISPLAY",
        mono(13.0 * k),
        color(palette.text),
    );
    painter.text(
        footer.left_bottom(),
        Align2::LEFT_BOTTOM,
        CREDITS,
        prop(10.5 * k),
        color(palette.dim),
    );
}

fn label(painter: &egui::Painter, pos: Pos2, text: &str, palette: &Palette, k: f32) {
    painter.text(pos, Align2::LEFT_TOP, text, mono(10.0 * k), alpha(palette.accent, 0.85));
}

fn paint_clock(painter: &egui::Painter, rect: Rect, palette: &Palette, clock: DateTime<Local>, now: f64, k: f32) {
    label(painter, rect.left_top(), "LOCAL TIME", palette, k);
    let hm = clock.format("%H:%M").to_string();
    let big = mono(58.0 * k);
    let galley = painter.layout_no_wrap(hm, big, color(palette.text));
    let at = rect.left_top() + Vec2::new(-2.0 * k, 12.0 * k);
    let width = galley.size().x;
    painter.galley(at, galley, color(palette.text));
    let blink = if clock.nanosecond() < 500_000_000 { 1.0 } else { 0.55 };
    painter.text(
        at + Vec2::new(width + 4.0 * k, 10.0 * k),
        Align2::LEFT_TOP,
        format!(":{:02}", clock.second()),
        mono(22.0 * k),
        alpha(palette.accent, blink),
    );
    painter.text(
        rect.right_top() + Vec2::new(0.0, 14.0 * k),
        Align2::RIGHT_TOP,
        Utc::now().format("%H:%MZ").to_string(),
        mono(13.0 * k),
        color(palette.dim),
    );
    painter.text(
        rect.right_top() + Vec2::new(0.0, 32.0 * k),
        Align2::RIGHT_TOP,
        format!("T+{:>7.0}", now),
        mono(10.0 * k),
        alpha(palette.dim, 0.7),
    );
    painter.text(
        rect.left_bottom(),
        Align2::LEFT_BOTTOM,
        format!("{}  ·  {}", japanese_date(clock.date_naive()), clock.year()),
        prop(17.0 * k),
        color(palette.dim),
    );
}

fn glyph(painter: &egui::Painter, c: Pos2, s: f32, mood: Option<Mood>, palette: &Palette, now: f64) {
    let stroke = Stroke::new(1.5, color(palette.accent));
    let cloud = |at: Pos2| {
        painter.circle_stroke(at + Vec2::new(-s * 0.35, s * 0.1), s * 0.38, stroke);
        painter.circle_stroke(at + Vec2::new(s * 0.2, -s * 0.05), s * 0.5, stroke);
    };
    match mood {
        Some(Mood::Clear) | None => {
            painter.circle_stroke(c, s * 0.42, stroke);
            for i in 0..8 {
                let a = i as f32 / 8.0 * std::f32::consts::TAU + now as f32 * 0.3;
                let d = Vec2::angled(a);
                painter.line_segment([c + d * s * 0.62, c + d * s * 0.85], stroke);
            }
        }
        Some(Mood::Cloudy) => cloud(c),
        Some(Mood::Rain) | Some(Mood::Snow) => {
            cloud(c + Vec2::new(0.0, -s * 0.25));
            for i in 0..3 {
                let x = c.x + (i as f32 - 1.0) * s * 0.4;
                let t = ((now * 1.4 + i as f64 * 0.33) % 1.0) as f32;
                let y = c.y + s * 0.35 + t * s * 0.55;
                if mood == Some(Mood::Snow) {
                    painter.circle_filled(Pos2::new(x, y), 1.6, alpha(palette.accent, 1.0 - t));
                } else {
                    painter.line_segment(
                        [Pos2::new(x, y), Pos2::new(x - 2.0, y + s * 0.18)],
                        Stroke::new(1.5, alpha(palette.accent, 1.0 - t)),
                    );
                }
            }
        }
    }
}

fn paint_weather(painter: &egui::Painter, rect: Rect, palette: &Palette, w: Option<&atlas::Weather>, now: f64, k: f32) {
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, alpha(palette.border, 0.35)),
    );
    let Some(w) = w else {
        painter.text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            "WEATHER  ·  NO FEED",
            mono(12.0 * k),
            color(palette.dim),
        );
        return;
    };
    let mood = Mood::parse(&w.mood);
    glyph(painter, rect.left_center() + Vec2::new(14.0 * k, 2.0 * k), 22.0 * k, mood, palette, now);
    painter.text(
        rect.left_center() + Vec2::new(40.0 * k, 0.0),
        Align2::LEFT_CENTER,
        w.label.as_str(),
        prop(17.0 * k),
        color(palette.text),
    );
    let temps = match (w.max, w.min) {
        (Some(hi), Some(lo)) => format!("{hi}° / {lo}°"),
        (Some(hi), None) => format!("{hi}°"),
        (None, Some(lo)) => format!("— / {lo}°"),
        (None, None) => String::new(),
    };
    painter.text(rect.right_center(), Align2::RIGHT_CENTER, temps, mono(15.0 * k), color(palette.dim));
}

/// Map card: two layers crossfading with a slight zoom, then shared chrome on top.
fn paint_card(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, card: CardState<'_>) {
    painter.rect_filled(rect, 4, color(palette.bg));
    let clip = painter.with_clip_rect(rect);
    let elapsed = card.frame.map_or(now, |f| f.elapsed);
    let blend = card.frame.map_or(1.0, |f| f.blend);
    if let Some(previous) = &card.previous
        && blend < 1.0
    {
        paint_layer(&clip, rect, palette, now, k, previous, 1.0 - blend, 1.0, elapsed + strip::DWELL_SECONDS);
    }
    let zoom = 1.04 - 0.04 * ease(blend);
    paint_layer(&clip, rect, palette, now, k, &card.current, blend.max(0.001), zoom, elapsed);
    // Grid and scanline over everything.
    let grid = alpha(palette.border, 0.10);
    for i in 1..8 {
        let x = rect.left() + rect.width() * i as f32 / 8.0;
        clip.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.0, grid));
    }
    for i in 1..6 {
        let y = rect.top() + rect.height() * i as f32 / 6.0;
        clip.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], Stroke::new(1.0, grid));
    }
    let sweep = ((now / 4.0) % 1.0) as f32;
    let sy = rect.top() + sweep * rect.height();
    for i in 0..16 {
        let a = 0.35 * (1.0 - i as f32 / 16.0).powi(2);
        let y = sy - i as f32 * 2.0;
        clip.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            Stroke::new(if i == 0 { 1.5 } else { 2.0 }, alpha(palette.accent, a)),
        );
    }
    painter.rect_stroke(rect, 4, Stroke::new(1.0, alpha(palette.border, 0.6)), StrokeKind::Inside);
    brackets(painter, rect.expand(4.0), 14.0 * k, Stroke::new(1.5, color(palette.accent)));
    // Title chip.
    let (title, code) = card
        .frame
        .map_or(("地図を準備中", "ACQUIRING"), |f| (f.view.title(), f.view.code()));
    let chip = Rect::from_min_size(rect.left_top() + Vec2::new(8.0, 8.0) * k, Vec2::new(rect.width() * 0.56, 24.0 * k));
    painter.rect_filled(chip, 3, alpha(palette.bg, 0.78));
    painter.rect_filled(
        Rect::from_min_size(chip.min, Vec2::new(3.0 * k, chip.height())),
        0,
        color(palette.accent),
    );
    painter.text(chip.left_center() + Vec2::new(10.0 * k, 0.0), Align2::LEFT_CENTER, title, prop(14.0 * k), color(palette.text));
    painter.text(
        rect.right_top() + Vec2::new(-10.0 * k, 12.0 * k),
        Align2::RIGHT_TOP,
        code,
        mono(10.5 * k),
        alpha(palette.accent, 0.9),
    );
    // Progress through the ready views.
    if let Some(f) = card.frame {
        let n = card.ready.len().max(1);
        let seg_gap = 4.0 * k;
        let total = rect.width() * 0.4;
        let seg_w = (total - seg_gap * (n as f32 - 1.0)) / n as f32;
        let y = rect.bottom() + 9.0 * k;
        let x0 = rect.center().x - total / 2.0;
        for (i, v) in card.ready.iter().enumerate() {
            let x = x0 + i as f32 * (seg_w + seg_gap);
            let back = Rect::from_min_size(Pos2::new(x, y), Vec2::new(seg_w, 2.5 * k));
            painter.rect_filled(back, 1, alpha(palette.border, 0.5));
            if *v == f.view {
                let p = (f.elapsed / f.dwell).clamp(0.0, 1.0) as f32;
                painter.rect_filled(
                    Rect::from_min_size(back.min, Vec2::new(seg_w * p, back.height())),
                    1,
                    color(palette.accent),
                );
            }
        }
    }
}
fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}
fn zoomed(rect: Rect, zoom: f32) -> Rect {
    Rect::from_center_size(rect.center(), rect.size() * zoom)
}

#[allow(clippy::too_many_arguments)]
fn paint_layer(
    painter: &egui::Painter,
    rect: Rect,
    palette: &Palette,
    now: f64,
    k: f32,
    layer: &MapCard<'_>,
    opacity: f32,
    zoom: f32,
    elapsed: f64,
) {
    let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    let target = zoomed(rect, zoom);
    let tint = |rgb: Rgb, a: f32| {
        // Neutral grey basemaps pick up the palette: lift toward the accent.
        let m = |c: u8, t: u8| ((c as f32 * 0.55 + t as f32 * 0.45) * a) as u8;
        Color32::from_rgba_premultiplied(
            m(255, rgb[0]),
            m(255, rgb[1]),
            m(255, rgb[2]),
            (a * 255.0) as u8,
        )
    };
    match layer {
        MapCard::Preparing => {
            let c = rect.center();
            let radius = rect.height() * 0.32;
            for i in 1..=3 {
                painter.circle_stroke(c, radius * i as f32 / 3.0, Stroke::new(1.0, alpha(palette.border, 0.5 * opacity)));
            }
            let a = now as f32 * 1.6;
            for i in 0..28 {
                let aa = a - i as f32 * 0.035;
                painter.line_segment(
                    [c, c + Vec2::angled(aa) * radius],
                    Stroke::new(2.0, alpha(palette.accent, 0.5 * (1.0 - i as f32 / 28.0) * opacity)),
                );
            }
            let dots = ((now * 2.0) as usize) % 4;
            painter.text(
                c + Vec2::new(0.0, radius + 16.0 * k),
                Align2::CENTER_TOP,
                format!("ACQUIRING SIGNAL{}", ".".repeat(dots)),
                mono(11.0 * k),
                alpha(palette.dim, opacity),
            );
        }
        MapCard::Map(m) => {
            painter.image(m.base.id(), target, full, tint(palette.accent, opacity));
            if let Some(f) = strip::radar_frame(elapsed, m.radar.len(), m.now_index) {
                painter.image(m.radar[f.index].id(), target, full, Color32::from_white_alpha((opacity * (1.0 - f.blend) * 255.0) as u8));
                if f.blend > 0.0 {
                    painter.image(m.radar[f.next].id(), target, full, Color32::from_white_alpha((opacity * f.blend * 255.0) as u8));
                }
                radar_timeline(painter, rect, palette, k, m, f, opacity);
            } else {
                painter.text(rect.center_bottom() - Vec2::new(0.0, 14.0 * k), Align2::CENTER_BOTTOM, "雨雲データなし", prop(13.0 * k), alpha(palette.dim, opacity));
            }
            let at = target.min + target.size() * Vec2::new(m.office[0], m.office[1]);
            marker(painter, rect, at, palette, now, k, opacity, "OFFICE", m.office_coords);
        }
        MapCard::Place(p) => {
            painter.image(p.map.id(), target, full, tint(palette.accent, opacity));
            let at = target.center();
            marker(painter, rect, at, palette, now, k, opacity, p.place, p.coords);
        }
        MapCard::Globe(g) => paint_globe(painter, rect, palette, now, k, g, opacity, zoom),
    }
}

#[allow(clippy::too_many_arguments)]
fn marker(painter: &egui::Painter, rect: Rect, at: Pos2, palette: &Palette, now: f64, k: f32, opacity: f32, name: &str, coords_: (f64, f64)) {
    let cross = Stroke::new(1.0, alpha(palette.accent, 0.28 * opacity));
    let gap = 16.0 * k;
    painter.line_segment([Pos2::new(rect.left(), at.y), Pos2::new(at.x - gap, at.y)], cross);
    painter.line_segment([Pos2::new(at.x + gap, at.y), Pos2::new(rect.right(), at.y)], cross);
    painter.line_segment([Pos2::new(at.x, rect.top()), Pos2::new(at.x, at.y - gap)], cross);
    painter.line_segment([Pos2::new(at.x, at.y + gap), Pos2::new(at.x, rect.bottom())], cross);
    for i in 0..3 {
        let p = ((now * 0.7 + i as f64 / 3.0) % 1.0) as f32;
        painter.circle_stroke(at, 4.0 * k + p * 30.0 * k, Stroke::new(1.5, alpha(palette.accent, (1.0 - p) * 0.9 * opacity)));
    }
    painter.circle_filled(at, 3.5 * k, alpha(palette.accent, opacity));
    let box_ = Rect::from_min_size(at + Vec2::new(12.0, -30.0) * k, Vec2::new(4.0, 4.0));
    painter.line_segment([at + Vec2::new(4.0, -4.0) * k, box_.min + Vec2::new(0.0, 8.0 * k)], Stroke::new(1.0, alpha(palette.accent, 0.7 * opacity)));
    painter.text(box_.min, Align2::LEFT_TOP, name, prop(13.0 * k), alpha(palette.text, opacity));
    painter.text(
        box_.min + Vec2::new(0.0, 16.0 * k),
        Align2::LEFT_TOP,
        coords(coords_.0, coords_.1),
        mono(10.0 * k),
        alpha(palette.dim, opacity),
    );
}

fn radar_timeline(painter: &egui::Painter, rect: Rect, palette: &Palette, k: f32, m: &MapLayer<'_>, f: strip::RadarFrame, opacity: f32) {
    let n = m.radar.len();
    let y = rect.bottom() - 14.0 * k;
    let x0 = rect.left() + 12.0 * k;
    let w = rect.width() * 0.5;
    let step = w / (n.max(2) - 1) as f32;
    let shown = if f.blend > 0.5 { f.next } else { f.index };
    for i in 0..n {
        let x = x0 + i as f32 * step;
        let h = if i == m.now_index { 10.0 } else { 5.0 } * k;
        let c = if i == shown {
            color(palette.accent)
        } else if i > m.now_index {
            alpha(palette.accent, 0.35 * opacity)
        } else {
            alpha(palette.text, 0.45 * opacity)
        };
        painter.line_segment([Pos2::new(x, y), Pos2::new(x, y - h)], Stroke::new(if i == shown { 2.5 } else { 1.0 }, c));
    }
    let time = m.frames.get(shown).and_then(|fr| local_hm(&fr.at)).unwrap_or_default();
    painter.text(
        Pos2::new(x0 + w + 10.0 * k, y),
        Align2::LEFT_BOTTOM,
        format!("{time}  {}", frame_tag(shown, m.now_index)),
        mono(10.5 * k),
        alpha(palette.text, opacity),
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_globe(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, g: &GlobeLayer<'_>, opacity: f32, zoom: f32) {
    let c = rect.center() + Vec2::new(0.0, 4.0 * k);
    let radius = rect.height() * 0.40 * zoom;
    // Stars.
    for i in 0..70u32 {
        let p = rect.min + rect.size() * Vec2::new(hash(i * 2), hash(i * 2 + 1));
        if (p - c).length() < radius + 6.0 {
            continue;
        }
        let tw = 0.35 + 0.65 * (0.5 + 0.5 * (now as f32 * (0.6 + hash(i + 999)) + i as f32).sin());
        painter.circle_filled(p, 0.6 + hash(i + 77) * 0.9, alpha(palette.text, 0.5 * tw * opacity));
    }
    // Atmosphere.
    for i in 0..8 {
        let a = 0.22 * (1.0 - i as f32 / 8.0) * opacity;
        painter.circle_stroke(c, radius + 1.0 + i as f32 * 1.6, Stroke::new(1.6, alpha(palette.accent, a)));
    }
    let m = globe::mesh([c.x, c.y], radius, g.view.0, g.view.1, g.sun.subsolar_lat, g.sun.subsolar_lon, 72, 36);
    let mut day = epaint::Mesh::with_texture(g.day.id());
    let mut night = epaint::Mesh::with_texture(g.night.id());
    for v in &m.vertices {
        let pos = Pos2::new(v.pos[0], v.pos[1]);
        let uv = Pos2::new(v.uv[0], v.uv[1]);
        let light = (1.0 - 0.88 * v.night) * (0.55 + 0.45 * v.facing.sqrt());
        let l = (light * opacity * 255.0) as u8;
        day.vertices.push(epaint::Vertex { pos, uv, color: Color32::from_rgba_premultiplied(l, l, l, (opacity * 255.0) as u8) });
        let n = (v.night * opacity * 230.0) as u8;
        night.vertices.push(epaint::Vertex { pos, uv, color: Color32::from_rgba_premultiplied(n, n, n, n) });
    }
    day.indices = m.indices.clone();
    night.indices = m.indices;
    painter.add(Shape::mesh(day));
    painter.add(Shape::mesh(night));
    // Graticule.
    let grat = Stroke::new(1.0, alpha(palette.accent, 0.14 * opacity));
    let to_screen = |p: [f32; 2]| Pos2::new(c.x + p[0] * radius, c.y - p[1] * radius);
    for lat in [-60.0, -30.0, 0.0, 30.0, 60.0] {
        for line in globe::graticule_line(g.view.0, g.view.1, lat, false) {
            painter.add(Shape::line(line.into_iter().map(to_screen).collect(), grat));
        }
    }
    for i in 0..12 {
        for line in globe::graticule_line(g.view.0, g.view.1, -180.0 + 30.0 * i as f64, true) {
            painter.add(Shape::line(line.into_iter().map(to_screen).collect(), grat));
        }
    }
    // Orbit ring with ticks, rotating.
    let ring = radius * 1.16;
    let spin = now as f32 * 0.12;
    for i in 0..60 {
        let a = spin + i as f32 / 60.0 * std::f32::consts::TAU;
        let len = if i % 5 == 0 { 7.0 } else { 3.0 } * k;
        let d = Vec2::angled(a);
        painter.line_segment([c + d * ring, c + d * (ring + len)], Stroke::new(1.0, alpha(palette.border, 0.6 * opacity)));
    }
    let arc: Vec<Pos2> = (0..=40)
        .map(|i| c + Vec2::angled(-spin * 2.0 + i as f32 / 40.0 * 1.2) * (ring - 4.0 * k))
        .collect();
    painter.add(Shape::line(arc, Stroke::new(2.0, alpha(palette.accent, 0.8 * opacity))));
    // Office and optional far photo place.
    if let Some(p) = globe::project(g.view.0, g.view.1, g.office.0, g.office.1) {
        let at = to_screen(p);
        for i in 0..2 {
            let ph = ((now * 0.8 + i as f64 * 0.5) % 1.0) as f32;
            painter.circle_stroke(at, 3.0 + ph * 18.0 * k, Stroke::new(1.2, alpha(palette.accent, (1.0 - ph) * opacity)));
        }
        painter.circle_filled(at, 2.5 * k, alpha(palette.accent, opacity));
    }
    if let Some((lat, lon, name)) = g.marker
        && let Some(p) = globe::project(g.view.0, g.view.1, lat, lon)
    {
        let at = to_screen(p);
        marker(painter, rect, at, palette, now, k, opacity, name, (lat, lon));
    }
    painter.text(
        rect.left_bottom() + Vec2::new(12.0, -12.0) * k,
        Align2::LEFT_BOTTOM,
        format!("SUBSOLAR {}", coords(g.sun.subsolar_lat, g.sun.subsolar_lon)),
        mono(10.0 * k),
        alpha(palette.dim, opacity),
    );
}

fn paint_telemetry(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, t: &Telemetry<'_>) {
    if rect.height() < 30.0 * k {
        return;
    }
    label(painter, rect.left_top(), "TELEMETRY", palette, k);
    let up = t.uptime as u64;
    let rows = [
        ("POSITION", coords(t.office.0, t.office.1)),
        ("SUN ELEV", format!("{:+.1}°", t.sun.elevation_deg)),
        ("PHASE", t.phase.to_ascii_uppercase()),
        ("MOOD", t.mood.map_or("—", |m| m.name()).to_ascii_uppercase()),
        ("ATLAS LINK", t.atlas_state.to_ascii_uppercase()),
        ("SCENE", format!("#{:04} · {} IMG", t.scene_id, t.photos)),
        ("UPTIME", format!("{:02}:{:02}:{:02}", up / 3600, up / 60 % 60, up % 60)),
    ];
    let line = 17.0 * k;
    let top = rect.top() + 18.0 * k;
    let fit = (((rect.height() - 18.0 * k - 40.0 * k) / line).floor().max(0.0) as usize).min(rows.len());
    let col = rect.width() * 0.42;
    for (i, (key, value)) in rows.iter().take(fit).enumerate() {
        let y = top + i as f32 * line;
        painter.text(Pos2::new(rect.left(), y), Align2::LEFT_TOP, *key, mono(11.0 * k), color(palette.dim));
        // Each row "settles" once per minute: a brief accent flash, staggered.
        let flash = ((now + i as f64 * 0.37) % 60.0) < 0.4;
        painter.text(
            Pos2::new(rect.left() + col, y),
            Align2::LEFT_TOP,
            value,
            mono(11.0 * k),
            if flash { color(palette.accent) } else { color(palette.text) },
        );
    }
    // Signal trace along the bottom of the block.
    let h = 26.0 * k;
    let base = rect.bottom() - 4.0 * k;
    if rect.height() > h + 24.0 * k {
        let n = 96;
        let pts: Vec<Pos2> = (0..n)
            .map(|i| {
                let x = i as f32 / (n - 1) as f32;
                let tt = now as f32 * 1.3 + x * 9.0;
                let v = 0.5 * (tt.sin() * 0.6 + (tt * 2.7).sin() * 0.25 + (tt * 6.1).sin() * 0.15);
                Pos2::new(rect.left() + x * rect.width(), base - h * (0.5 + v))
            })
            .collect();
        painter.line_segment(
            [Pos2::new(rect.left(), base), Pos2::new(rect.right(), base)],
            Stroke::new(1.0, alpha(palette.border, 0.4)),
        );
        painter.add(Shape::line(pts, Stroke::new(1.2, alpha(palette.accent, 0.75))));
    }
}

fn level_color(level: u8, palette: &Palette) -> Color32 {
    match level {
        0 => alpha(palette.border, 0.55),
        1 => Color32::from_rgb(110, 190, 255),
        2 => Color32::from_rgb(40, 110, 255),
        3 => Color32::from_rgb(250, 235, 40),
        4 => Color32::from_rgb(255, 160, 30),
        5 => Color32::from_rgb(255, 60, 40),
        _ => Color32::from_rgb(200, 30, 140),
    }
}

fn paint_rain(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, rain: Option<&atlas::Rain>, since: f64) {
    painter.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, alpha(palette.border, 0.35)));
    label(painter, rect.left_top() + Vec2::new(0.0, 6.0 * k), "PRECIPITATION · NEXT 60 MIN", palette, k);
    let Some(rain) = rain else {
        painter.text(rect.left_top() + Vec2::new(0.0, 26.0 * k), Align2::LEFT_TOP, "雨雲データなし", prop(18.0 * k), color(palette.dim));
        return;
    };
    let expected = rain.expected();
    painter.text(
        rect.left_top() + Vec2::new(0.0, 24.0 * k),
        Align2::LEFT_TOP,
        rain.summary.as_str(),
        prop(20.0 * k),
        if expected { color(palette.accent) } else { color(palette.text) },
    );
    let bars = Rect::from_min_max(
        Pos2::new(rect.left(), rect.bottom() - 34.0 * k),
        Pos2::new(rect.right(), rect.bottom() - 14.0 * k),
    );
    let n = rain.series.len().max(1);
    let gap = 3.0 * k;
    let bw = (bars.width() - gap * (n as f32 - 1.0)) / n as f32;
    for (i, level) in rain.series.iter().enumerate() {
        let grow = ease((((now - since) - i as f64 * 0.05) / 0.8) as f32);
        let h = (2.0 * k).max(bars.height() * (*level as f32 / 6.0).sqrt()) * grow;
        let x = bars.left() + i as f32 * (bw + gap);
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(x, bars.bottom() - h), Pos2::new(x + bw, bars.bottom())),
            1,
            level_color(*level, palette),
        );
    }
    // A cursor sweeping the hour.
    let cx = bars.left() + bars.width() * ((now / 8.0) % 1.0) as f32;
    painter.line_segment(
        [Pos2::new(cx, bars.top() - 3.0 * k), Pos2::new(cx, bars.bottom())],
        Stroke::new(1.0, alpha(palette.accent, 0.5)),
    );
    for (t, align, x) in [("NOW", Align2::LEFT_TOP, bars.left()), ("+30", Align2::CENTER_TOP, bars.center().x), ("+60", Align2::RIGHT_TOP, bars.right())] {
        painter.text(Pos2::new(x, bars.bottom() + 3.0 * k), align, t, mono(10.0 * k), color(palette.dim));
    }
}

/// HUD frame around a collage card: corner ticks and a small index tag.
pub fn card_chrome(painter: &egui::Painter, card: Rect, palette: &Palette, index: usize, opacity: f32) {
    // Brackets close in on the card as it arrives, like a target lock.
    let lock = 3.0 + 22.0 * (1.0 - ease(opacity));
    brackets(painter, card.expand(lock), 14.0, Stroke::new(1.5, alpha(palette.accent, 0.75 * opacity)));
    painter.text(
        card.left_top() + Vec2::new(10.0, 8.0),
        Align2::LEFT_TOP,
        format!("IMG-{:02}", index + 1),
        mono(10.0),
        alpha(palette.text, 0.75 * opacity),
    );
}
/// A bright vertical scan crossing the collage after a new scene commits.
pub fn sweep(painter: &egui::Painter, collage: crate::layout::Rect, origin: Pos2, palette: &Palette, progress: f32) {
    let area = r(collage, origin);
    let x = area.left() + ease(progress) * area.width();
    let fade = 1.0 - progress.powi(3);
    for i in 0..40 {
        let xx = x - i as f32 * 3.0;
        if xx < area.left() {
            break;
        }
        let a = 0.22 * (1.0 - i as f32 / 40.0).powi(2) * fade;
        painter.line_segment([Pos2::new(xx, area.top()), Pos2::new(xx, area.bottom())], Stroke::new(3.0, alpha(palette.accent, a)));
    }
    painter.line_segment([Pos2::new(x, area.top()), Pos2::new(x, area.bottom())], Stroke::new(1.5, alpha(palette.accent, 0.9 * fade)));
    for i in 0..6 {
        let y = area.top() + area.height() * (i as f32 + 0.5) / 6.0;
        painter.text(
            Pos2::new(x + 6.0, y),
            Align2::LEFT_CENTER,
            format!("{:03}", ((progress * 997.0) as u32 + i * 131) % 1000),
            mono(9.0),
            alpha(palette.accent, 0.7 * fade),
        );
    }
}
/// "◉ 京都市 · 2025.08" chip on a collage card whose photo has a place.
pub fn place_chip(painter: &egui::Painter, card: Rect, palette: &Palette, place: &atlas::Place, now: f64, opacity: f32) {
    let taken = place.taken.as_deref().map(|t| t.replace('-', ".")).unwrap_or_default();
    let text = if taken.is_empty() { place.place.clone() } else { format!("{} · {}", place.place, taken) };
    let galley = painter.layout_no_wrap(text, prop(14.0), alpha(palette.text, opacity));
    let size = galley.size() + Vec2::new(34.0, 12.0);
    let chip = Rect::from_min_size(Pos2::new(card.left() + 12.0, card.bottom() - 12.0 - size.y), size);
    painter.rect_filled(chip, 3, alpha(palette.bg, 0.78 * opacity));
    painter.rect_stroke(chip, 3, Stroke::new(1.0, alpha(palette.accent, 0.6 * opacity)), StrokeKind::Inside);
    let dot = chip.left_center() + Vec2::new(13.0, 0.0);
    let p = ((now * 0.8) % 1.0) as f32;
    painter.circle_stroke(dot, 3.0 + p * 6.0, Stroke::new(1.0, alpha(palette.accent, (1.0 - p) * opacity)));
    painter.circle_filled(dot, 3.0, alpha(palette.accent, opacity));
    painter.galley(chip.min + Vec2::new(26.0, 6.0), galley, color(palette.text));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn japanese_dates_and_tags() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        assert_eq!(japanese_date(d), "10月22日 木曜日");
        assert_eq!(japanese_date(NaiveDate::from_ymd_opt(2026, 1, 4).unwrap()), "1月4日 日曜日");
        assert_eq!(frame_tag(6, 6), "NOW");
        assert_eq!(frame_tag(3, 6), "OBS -30");
        assert_eq!(frame_tag(8, 6), "FCST +20");
        assert_eq!(coords(35.681, 139.767), "35.68N 139.77E");
        assert_eq!(coords(-33.9, -70.65), "33.90S 70.65W");
    }
    #[test]
    fn hash_is_stable_and_in_range() {
        for i in 0..1000 {
            let h = hash(i);
            assert!((0.0..=1.0).contains(&h));
            assert_eq!(h, hash(i));
        }
    }
}

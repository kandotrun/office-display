//! Painting for the info strip: a glass panel with the clock, the weather, a
//! living map card and the rain outlook. Geometry and timing come from
//! `strip`, `globe`, `space` and `theme`; this module only draws.
use crate::{
    art::{self, glow, rgba},
    atlas, depth, globe,
    space::{self, Camera, v3},
    strip::{self, Blocks, View, ViewFrame},
    theme::{Mood, Palette, Rgb, Sun, mix},
};
use chrono::{DateTime, Datelike, Local, NaiveDate, Timelike};
use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Pos2, Rect, Shape, Stroke, StrokeKind, TextureHandle, Vec2,
    epaint,
};

pub const CREDITS: &str = "© OpenStreetMap contributors · 気象庁 · NASA GIBS";
/// Thin Latin face for the big clock digits; bound in `hand_overlay::install_japanese_font`.
pub const CLOCK_FAMILY: &str = "clock";

pub fn color(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}
pub fn alpha(rgb: Rgb, a: f32) -> Color32 {
    rgba(rgb, a)
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
/// "いま" / "30分前" / "20分後" for radar frame `i` at 10 minute steps.
pub fn frame_label(i: usize, now_index: usize) -> String {
    let minutes = (i as i64 - now_index as i64) * 10;
    match minutes {
        0 => "いま".into(),
        m if m < 0 => format!("{}分前", -m),
        m => format!("{m}分後"),
    }
}
/// "2025-08" → "2025年8月".
pub fn taken_label(taken: &str) -> Option<String> {
    let (y, m) = taken.split_once('-')?;
    let m: u32 = m.parse().ok().filter(|m| (1..=12).contains(m))?;
    (y.len() == 4 && y.chars().all(|c| c.is_ascii_digit())).then(|| format!("{y}年{m}月"))
}
fn local_hm(at: &str) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(at).ok()?;
    Some(t.with_timezone(&Local).format("%H:%M").to_string())
}
fn prop(size: f32) -> FontId {
    FontId::proportional(size)
}
fn clock_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(CLOCK_FAMILY.into()))
}
fn ease(t: f32) -> f32 {
    space::ease_out(t)
}
/// Text with a soft dark halo so it reads on any sky.
fn text(painter: &egui::Painter, at: Pos2, align: Align2, s: &str, font: FontId, c: Color32) {
    let shade = Color32::from_black_alpha((c.a() as f32 * 0.35) as u8);
    painter.text(at + Vec2::new(0.0, 1.5), align, s, font.clone(), shade);
    painter.text(at, align, s, font, c);
}
/// Frosted glass: a translucent tinted fill, a hairline and a top highlight.
pub fn glass(painter: &egui::Painter, rect: Rect, palette: &Palette, radius: u8, strength: f32) {
    painter.rect_filled(rect, radius, rgba(palette.panel, 0.34 * strength));
    painter.rect_stroke(rect, radius, Stroke::new(1.0, rgba([255, 255, 255], 0.16 * strength)), StrokeKind::Inside);
    let r = radius as f32;
    painter.line_segment(
        [rect.left_top() + Vec2::new(r, 1.0), rect.right_top() + Vec2::new(-r, 1.0)],
        Stroke::new(1.0, rgba([255, 255, 255], 0.22 * strength)),
    );
}
fn pill(painter: &egui::Painter, at: Pos2, label: &str, size: f32, palette: &Palette, opacity: f32) -> Rect {
    let galley = painter.layout_no_wrap(label.to_string(), prop(size), rgba(palette.text, opacity));
    let r = Rect::from_min_size(at, galley.size() + Vec2::new(size * 1.2, size * 0.6));
    painter.rect_filled(r, (r.height() / 2.0) as u8, rgba(palette.panel, 0.55 * opacity));
    painter.galley(r.min + Vec2::new(size * 0.6, size * 0.3), galley, rgba(palette.text, opacity));
    r
}

pub struct MapLayer<'a> {
    pub base: &'a TextureHandle,
    pub radar: &'a [TextureHandle],
    pub frames: &'a [atlas::Frame],
    pub now_index: usize,
    pub office: [f32; 2],
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
}
pub enum MapCard<'a> {
    Preparing,
    Map(MapLayer<'a>),
    Globe(GlobeLayer<'a>),
    Place(PlaceLayer<'a>),
}
pub struct StripInfo<'a> {
    pub weather: Option<&'a atlas::Weather>,
    pub rain: Option<&'a atlas::Rain>,
    /// When the rain series last changed, for the growth animation.
    pub rain_since: f64,
    pub sun_elevation: f64,
}
pub struct CardState<'a> {
    pub frame: Option<ViewFrame>,
    pub current: MapCard<'a>,
    pub previous: Option<MapCard<'a>>,
    pub ready: &'a [View],
}

#[allow(clippy::too_many_arguments)]
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
    let r = |b: crate::layout::Rect| Rect::from_min_size(origin + Vec2::new(b.x, b.y), Vec2::new(b.w, b.h));
    glass(painter, r(strip), palette, (18.0 * k) as u8, 1.0);
    paint_clock(painter, r(blocks.clock), palette, clock, k);
    paint_weather(painter, r(blocks.weather), palette, info, now, k);
    paint_card(painter, r(blocks.map), palette, now, k, card);
    paint_rain(painter, r(blocks.rain), palette, now, k, info.rain, info.rain_since);
    let footer = r(blocks.footer);
    text(painter, footer.left_top(), Align2::LEFT_TOP, "T S U Q R E A", prop(14.0 * k), rgba(palette.text, 0.9));
    painter.text(footer.left_bottom(), Align2::LEFT_BOTTOM, CREDITS, prop(10.5 * k), rgba(palette.dim, 0.75));
}

fn paint_clock(painter: &egui::Painter, rect: Rect, palette: &Palette, clock: DateTime<Local>, k: f32) {
    let hm = clock.format("%H:%M").to_string();
    let galley = painter.layout_no_wrap(hm, clock_font(112.0 * k), color(palette.text));
    let at = rect.left_top() + Vec2::new(-4.0 * k, -6.0 * k);
    let size = galley.size();
    painter.galley_with_override_text_color(at + Vec2::new(0.0, 2.5 * k), galley.clone(), Color32::from_black_alpha(60));
    painter.galley(at, galley, color(palette.text));
    // Seconds as a quiet ring that fills over the minute.
    let c = Pos2::new(at.x + size.x + 26.0 * k, at.y + size.y * 0.42);
    let rr = 14.0 * k;
    painter.circle_stroke(c, rr, Stroke::new(2.0 * k, rgba(palette.text, 0.18)));
    let frac = (clock.second() as f32 + clock.nanosecond() as f32 / 1e9) / 60.0;
    let pts: Vec<Pos2> = (0..=48)
        .map(|i| c + Vec2::angled(-std::f32::consts::FRAC_PI_2 + frac * std::f32::consts::TAU * i as f32 / 48.0) * rr)
        .collect();
    painter.add(Shape::line(pts, Stroke::new(2.5 * k, color(palette.accent))));
    text(
        painter,
        rect.left_bottom() + Vec2::new(0.0, -4.0 * k),
        Align2::LEFT_BOTTOM,
        &japanese_date(clock.date_naive()),
        prop(24.0 * k),
        rgba(palette.text, 0.92),
    );
}

/// Animated weather icon: sun or moon, cloud, rain, snow.
fn icon(painter: &egui::Painter, c: Pos2, s: f32, mood: Option<Mood>, night: bool, palette: &Palette, now: f64) {
    let t = now as f32;
    let sun = |at: Pos2, r: f32| {
        if night {
            glow(painter, at, 0.0, r * 2.4, rgba([200, 210, 255], 0.25));
            painter.circle_filled(at, r, rgba([244, 240, 220], 1.0));
            painter.circle_filled(at + Vec2::new(r * 0.45, -r * 0.3), r * 0.85, rgba(palette.panel, 0.92));
        } else {
            glow(painter, at, r * 0.6, r * 2.8, rgba([255, 214, 120], 0.45));
            for i in 0..10 {
                let a = i as f32 / 10.0 * std::f32::consts::TAU + t * 0.25;
                let len = r * (0.45 + 0.12 * (t * 2.0 + i as f32).sin());
                let d = Vec2::angled(a);
                painter.line_segment([at + d * r * 1.35, at + d * (r * 1.35 + len)], Stroke::new(r * 0.16, rgba([255, 220, 120], 0.95)));
            }
            painter.circle_filled(at, r, rgba([255, 210, 90], 1.0));
        }
    };
    let cloud = |at: Pos2, r: f32, a: f32| {
        let drift = Vec2::new((t * 0.6).sin() * r * 0.08, 0.0);
        let fill = rgba([248, 250, 255], a);
        for (dx, dy, rr) in [(-0.55, 0.15, 0.48), (0.0, -0.1, 0.62), (0.55, 0.12, 0.5), (0.0, 0.25, 0.55)] {
            painter.circle_filled(at + drift + Vec2::new(dx * r, dy * r), rr * r, fill);
        }
    };
    match mood {
        Some(Mood::Clear) | None => sun(c, s * 0.42),
        Some(Mood::Cloudy) => {
            sun(c + Vec2::new(s * 0.22, -s * 0.2), s * 0.3);
            cloud(c + Vec2::new(-s * 0.05, s * 0.08), s * 0.55, 0.96);
        }
        Some(m) => {
            cloud(c + Vec2::new(0.0, -s * 0.12), s * 0.55, 0.96);
            for i in 0..4 {
                let x = c.x + (i as f32 - 1.5) * s * 0.26;
                let ph = (t * 1.3 + i as f32 * 0.27) % 1.0;
                let y = c.y + s * 0.3 + ph * s * 0.45;
                if m == Mood::Snow {
                    painter.circle_filled(Pos2::new(x, y), s * 0.06, rgba([255, 255, 255], 1.0 - ph));
                } else {
                    painter.line_segment(
                        [Pos2::new(x, y), Pos2::new(x - s * 0.05, y + s * 0.16)],
                        Stroke::new(s * 0.06, rgba([150, 200, 255], 1.0 - ph)),
                    );
                }
            }
        }
    }
}

fn paint_weather(painter: &egui::Painter, rect: Rect, palette: &Palette, info: &StripInfo<'_>, now: f64, k: f32) {
    let Some(w) = info.weather else {
        text(painter, rect.left_center(), Align2::LEFT_CENTER, "天気を取得しています", prop(18.0 * k), rgba(palette.dim, 0.9));
        return;
    };
    let night = info.sun_elevation < -3.0;
    icon(painter, rect.left_center() + Vec2::new(30.0 * k, 0.0), 60.0 * k, Mood::parse(&w.mood), night, palette, now);
    text(painter, rect.left_center() + Vec2::new(76.0 * k, 0.0), Align2::LEFT_CENTER, &w.label, prop(24.0 * k), color(palette.text));
    let right = rect.right_center();
    if let Some(lo) = w.min {
        text(painter, right, Align2::RIGHT_CENTER, &format!("{lo}°"), prop(22.0 * k), rgba(palette.dim, 0.95));
    }
    if let Some(hi) = w.max {
        let shift = if w.min.is_some() { 58.0 * k } else { 0.0 };
        text(painter, right - Vec2::new(shift, 2.0 * k), Align2::RIGHT_CENTER, &format!("{hi}°"), clock_font(44.0 * k), color(palette.text));
    }
}

/// Map card: two layers crossfading with a slight zoom, then a title and dots.
fn paint_card(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, card: CardState<'_>) {
    let radius = (16.0 * k) as u8;
    // A deep window into space behind whatever is shown.
    painter.rect_filled(rect, radius, rgba(mix(palette.panel, [0, 0, 0], 0.4), 0.85));
    let clip = painter.with_clip_rect(rect.shrink(1.0));
    art::gradient(&clip, rect, mix(palette.sky_top, palette.panel, 0.45), mix(palette.sky_bottom, palette.panel, 0.35), 0.95);
    let elapsed = card.frame.map_or(now, |f| f.elapsed);
    let blend = card.frame.map_or(1.0, |f| f.blend);
    if let Some(previous) = &card.previous
        && blend < 1.0
    {
        paint_layer(&clip, rect, palette, now, k, previous, 1.0 - blend, 1.0, elapsed + strip::DWELL_SECONDS);
    }
    let zoom = 1.06 - 0.06 * ease(blend);
    paint_layer(&clip, rect, palette, now, k, &card.current, blend.max(0.001), zoom, elapsed);
    painter.rect_stroke(rect, radius, Stroke::new(1.0, rgba([255, 255, 255], 0.18)), StrokeKind::Inside);
    let title = card.frame.map_or("地図を準備しています", |f| f.view.title());
    pill(painter, rect.left_top() + Vec2::new(14.0, 14.0) * k, title, 16.0 * k, palette, 1.0);
    if let Some(f) = card.frame {
        let n = card.ready.len();
        let y = rect.bottom() + 12.0 * k;
        let gap = 14.0 * k;
        let x0 = rect.center().x - gap * (n as f32 - 1.0) / 2.0;
        for (i, v) in card.ready.iter().enumerate() {
            let at = Pos2::new(x0 + i as f32 * gap, y);
            if *v == f.view {
                let p = (f.elapsed / f.dwell).clamp(0.0, 1.0) as f32;
                painter.circle_filled(at, 4.0 * k, rgba(palette.text, 0.35));
                painter.circle_filled(at, 4.0 * k * (0.4 + 0.6 * p), color(palette.text));
            } else {
                painter.circle_filled(at, 3.0 * k, rgba(palette.text, 0.35));
            }
        }
    }
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
    // Basemaps are neutral grey; they take on the colour of the sky.
    let tint = {
        let c = mix(palette.glow, palette.accent, 0.4);
        // Slightly see-through so the sky behind lifts the dark land.
        let a = 0.82 * opacity;
        let m = |v: u8| ((v as f32 * 0.6 + 255.0 * 0.4) * a) as u8;
        Color32::from_rgba_premultiplied(m(c[0]), m(c[1]), m(c[2]), (a * 255.0) as u8)
    };
    match layer {
        MapCard::Preparing => {
            let c = rect.center();
            let pulse = 0.5 + 0.5 * (now as f32 * 1.5).sin();
            glow(painter, c, 0.0, rect.width() * (0.25 + 0.05 * pulse), rgba(palette.glow, 0.35 * opacity));
            painter.circle_filled(c, 6.0 * k, rgba(palette.text, opacity));
        }
        MapCard::Map(m) => {
            let frame = strip::radar_frame(elapsed, m.radar.len(), m.now_index);
            let radar = frame.map(|f| (&m.radar[f.index], &m.radar[f.next], f.blend));
            let table = Table { base: m.base, radar, focus: m.office, tint };
            tilted_map(painter, rect, palette, now, k, &table, opacity, zoom, elapsed);
            if let Some(f) = frame {
                radar_progress(painter, rect, palette, k, m, f, opacity);
            } else {
                pill(painter, rect.left_bottom() + Vec2::new(14.0, -42.0) * k, "雨雲データなし", 13.0 * k, palette, opacity);
            }
        }
        MapCard::Place(p) => {
            let table = Table { base: p.map, radar: None, focus: [0.5, 0.5], tint };
            if let Some(top) = tilted_map(painter, rect, palette, now, k, &table, opacity, zoom, elapsed) {
                pill(painter, top + Vec2::new(-30.0 * k, -34.0 * k), p.place, 16.0 * k, palette, opacity);
            }
        }
        MapCard::Globe(g) => paint_globe(painter, rect, palette, now, k, g, opacity, zoom),
    }
}

struct Table<'a> {
    base: &'a TextureHandle,
    radar: Option<(&'a TextureHandle, &'a TextureHandle, f32)>,
    /// Point of interest in normalised image coordinates; the camera orbits it.
    focus: [f32; 2],
    tint: Color32,
}
/// The map as a tilted, slowly orbiting plane with a column of light rising
/// from the point of interest. Returns the screen position of the light's top.
#[allow(clippy::too_many_arguments)]
fn tilted_map(
    painter: &egui::Painter,
    rect: Rect,
    palette: &Palette,
    now: f64,
    k: f32,
    t: &Table<'_>,
    opacity: f32,
    zoom: f32,
    elapsed: f64,
) -> Option<Pos2> {
    let w = rect.width() * 1.9 * zoom;
    let h = w * 0.75;
    let focus = v3(rect.center().x, rect.center().y + rect.height() * 0.1, 0.0);
    let flat = space::rect_corners(focus.x - t.focus[0] * w, focus.y - t.focus[1] * h, w, h);
    // Settle from flat to tilted when the view arrives, then orbit slowly.
    let arrive = ease((elapsed / 1.8) as f32);
    let yaw = 0.45 * (now as f32 * 0.09).sin() + 0.3 * (1.0 - arrive);
    let pitch = 0.15 + 0.75 * arrive + 0.05 * (now as f32 * 0.21).sin();
    let corners = flat.map(|p| p.turn(focus, yaw, pitch));
    let cam = Camera::new([focus.x, focus.y], rect.width() * 1.25);
    let on = |u: f32, v: f32, lift: f32| space::on_quad(&flat, u, v).add(v3(0.0, 0.0, -lift)).turn(focus, yaw, pitch);
    let full = [[0.0, 0.0], [1.0, 1.0]];
    depth::textured(painter, &cam, &corners, t.base.id(), full, t.tint, 16);
    if let Some((a, b, blend)) = t.radar {
        depth::textured(painter, &cam, &corners, a.id(), full, Color32::from_white_alpha((opacity * (1.0 - blend) * 255.0) as u8), 16);
        if blend > 0.0 {
            depth::textured(painter, &cam, &corners, b.id(), full, Color32::from_white_alpha((opacity * blend * 255.0) as u8), 16);
        }
    }
    // Light ripples spreading over the map.
    let (fu, fv) = (t.focus[0], t.focus[1]);
    for ring in 0..3 {
        let ph = ((now * 0.45 + ring as f64 / 3.0) % 1.0) as f32;
        let radius = 0.01 + ph * 0.11;
        let pts: Vec<Pos2> = (0..=56)
            .filter_map(|i| {
                let a = i as f32 / 56.0 * std::f32::consts::TAU;
                depth::project(&cam, on(fu + radius * a.cos(), fv + radius * a.sin() * 4.0 / 3.0, 0.0))
            })
            .collect();
        painter.add(Shape::line(pts, Stroke::new(2.0 * k, rgba(palette.glow, (1.0 - ph) * 0.8 * opacity))));
    }
    // The column of light.
    let height = rect.height() * (0.42 + 0.04 * (now as f32 * 1.3).sin());
    let base = depth::project(&cam, on(fu, fv, 0.0));
    let top = depth::project(&cam, on(fu, fv, height));
    if let (Some(base), Some(top)) = (base, top) {
        glow(painter, base, 0.0, 34.0 * k, rgba(palette.glow, 0.55 * opacity));
        let steps = 16;
        for i in 0..steps {
            let (a, b) = (i as f32 / steps as f32, (i + 1) as f32 / steps as f32);
            let fade = opacity * (1.0 - a).powf(1.4);
            let (p, q) = (base.lerp(top, a), base.lerp(top, b));
            painter.line_segment([p, q], Stroke::new(14.0 * k, rgba(palette.glow, 0.10 * fade)));
            painter.line_segment([p, q], Stroke::new(5.0 * k, rgba(palette.glow, 0.25 * fade)));
            painter.line_segment([p, q], Stroke::new(1.6 * k, rgba([255, 255, 255], 0.95 * fade)));
        }
        painter.circle_filled(base, 4.5 * k, rgba([255, 255, 255], opacity));
    }
    // Soft distance haze where the map recedes.
    let haze = mix(palette.panel, [0, 0, 0], 0.45);
    for i in 0..12 {
        let y = rect.top() + rect.height() * 0.035 * i as f32;
        let band = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), rect.height() * 0.035 + 1.0));
        painter.rect_filled(band, 0, rgba(haze, 0.9 * (1.0 - i as f32 / 12.0).powf(1.5)));
    }
    top
}

fn radar_progress(painter: &egui::Painter, rect: Rect, palette: &Palette, k: f32, m: &MapLayer<'_>, f: strip::RadarFrame, opacity: f32) {
    let n = m.radar.len().max(2);
    let shown = if f.blend > 0.5 { f.next } else { f.index };
    let bar = Rect::from_min_size(
        rect.left_bottom() + Vec2::new(16.0 * k, -22.0 * k),
        Vec2::new(rect.width() - 32.0 * k, 3.0 * k),
    );
    painter.rect_filled(bar, 2, rgba(palette.text, 0.22 * opacity));
    let p = (f.index as f32 + f.blend) / (n - 1) as f32;
    painter.rect_filled(
        Rect::from_min_size(bar.min, Vec2::new(bar.width() * p.min(1.0), bar.height())),
        2,
        rgba(palette.text, 0.9 * opacity),
    );
    let nx = bar.left() + bar.width() * m.now_index as f32 / (n - 1) as f32;
    painter.circle_filled(Pos2::new(nx, bar.center().y), 4.0 * k, rgba(palette.accent, opacity));
    let time = m.frames.get(shown).and_then(|fr| local_hm(&fr.at)).unwrap_or_default();
    let label = format!("{time}  {}", frame_label(shown, m.now_index));
    text(painter, bar.left_top() - Vec2::new(0.0, 8.0 * k), Align2::LEFT_BOTTOM, &label, prop(15.0 * k), rgba(palette.text, opacity));
}

#[allow(clippy::too_many_arguments)]
fn paint_globe(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, g: &GlobeLayer<'_>, opacity: f32, zoom: f32) {
    let c = rect.center() + Vec2::new(0.0, 6.0 * k);
    let radius = rect.height().min(rect.width()) * 0.36 * zoom;
    for i in 0..60u32 {
        let h = |j: u32| {
            let mut x = (i * 7 + j).wrapping_mul(0x9E37_79B9) ^ 0x5bd1_e995;
            x ^= x >> 15;
            ((x.wrapping_mul(0x2C1B_3C6D) >> 8) & 0xFFFF) as f32 / 65535.0
        };
        let p = rect.min + rect.size() * Vec2::new(h(1), h(2));
        if (p - c).length() < radius * 1.2 {
            continue;
        }
        let tw = 0.4 + 0.6 * (0.5 + 0.5 * (now as f32 * (0.5 + h(3)) + i as f32).sin());
        painter.circle_filled(p, 0.6 + h(4), rgba([255, 255, 255], 0.6 * tw * opacity));
    }
    // Atmosphere: a wide soft halo and a bright rim.
    glow(painter, c, radius * 0.98, radius * 1.32, rgba(palette.glow, 0.45 * opacity));
    glow(painter, c, radius * 0.98, radius * 1.08, rgba([170, 210, 255], 0.55 * opacity));
    let m = globe::mesh([c.x, c.y], radius, g.view.0, g.view.1, g.sun.subsolar_lat, g.sun.subsolar_lon, 72, 36);
    let mut day = epaint::Mesh::with_texture(g.day.id());
    let mut night = epaint::Mesh::with_texture(g.night.id());
    for v in &m.vertices {
        let pos = Pos2::new(v.pos[0], v.pos[1]);
        let uv = Pos2::new(v.uv[0], v.uv[1]);
        let light = (1.0 - 0.9 * v.night) * (0.5 + 0.5 * v.facing.sqrt());
        let l = (light * opacity * 255.0) as u8;
        day.vertices.push(epaint::Vertex { pos, uv, color: Color32::from_rgba_premultiplied(l, l, l, (opacity * 255.0) as u8) });
        let n = (v.night * opacity * 255.0) as u8;
        night.vertices.push(epaint::Vertex { pos, uv, color: Color32::from_rgba_premultiplied(n, n, n, n) });
    }
    day.indices = m.indices.clone();
    night.indices = m.indices;
    painter.add(Shape::mesh(day));
    painter.add(Shape::mesh(night));
    // One slow orbit with a travelling light, hidden behind the globe.
    let to_screen = |p: space::V3| Pos2::new(c.x + p.x * radius, c.y + p.y * radius);
    let at = |theta: f32| v3(theta.cos() * 1.3, 0.0, theta.sin() * 1.3).pitch(0.5).yaw(now as f32 * 0.03);
    let hidden = |p: space::V3| p.z > 0.0 && p.x * p.x + p.y * p.y < 1.0;
    for j in 0..140 {
        let (a, b) = (at(j as f32 / 140.0 * std::f32::consts::TAU), at((j + 1) as f32 / 140.0 * std::f32::consts::TAU));
        if !hidden(a) && !hidden(b) {
            let line = if a.z <= 0.0 { 0.45 } else { 0.15 };
            painter.line_segment([to_screen(a), to_screen(b)], Stroke::new(1.2, rgba([255, 255, 255], line * opacity)));
        }
    }
    let sat = at(now as f32 * 0.4);
    if !hidden(sat) {
        glow(painter, to_screen(sat), 0.0, 12.0 * k, rgba(palette.glow, 0.8 * opacity));
        painter.circle_filled(to_screen(sat), 2.5 * k, rgba([255, 255, 255], opacity));
    }
    let disc = |lat: f64, lon: f64| globe::project(g.view.0, g.view.1, lat, lon).map(|p| Pos2::new(c.x + p[0] * radius, c.y - p[1] * radius));
    if let Some(p) = disc(g.office.0, g.office.1) {
        let ph = ((now * 0.6) % 1.0) as f32;
        glow(painter, p, 0.0, 18.0 * k, rgba(palette.glow, 0.7 * opacity));
        painter.circle_stroke(p, 4.0 * k + ph * 22.0 * k, Stroke::new(1.5, rgba([255, 255, 255], (1.0 - ph) * opacity)));
        painter.circle_filled(p, 3.5 * k, rgba([255, 255, 255], opacity));
    }
    if let Some((lat, lon, name)) = g.marker
        && let Some(p) = disc(lat, lon)
    {
        glow(painter, p, 0.0, 22.0 * k, rgba(palette.accent, 0.8 * opacity));
        painter.circle_filled(p, 4.0 * k, rgba([255, 255, 255], opacity));
        pill(painter, p + Vec2::new(10.0, -36.0) * k, name, 16.0 * k, palette, opacity);
    }
}

fn level_color(level: u8) -> Rgb {
    match level {
        0 => [255, 255, 255],
        1 => [140, 205, 255],
        2 => [70, 140, 255],
        3 => [255, 230, 90],
        4 => [255, 170, 60],
        5 => [255, 90, 70],
        _ => [230, 70, 170],
    }
}

fn paint_rain(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, rain: Option<&atlas::Rain>, since: f64) {
    let Some(rain) = rain else {
        text(painter, rect.left_top(), Align2::LEFT_TOP, "雨雲データなし", prop(22.0 * k), rgba(palette.dim, 0.9));
        return;
    };
    let expected = rain.expected();
    text(
        painter,
        rect.left_top(),
        Align2::LEFT_TOP,
        &rain.summary,
        prop(24.0 * k),
        if expected { color(palette.accent) } else { color(palette.text) },
    );
    // A smooth area over the next hour; low and calm when it stays dry.
    let chart = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + 46.0 * k), Pos2::new(rect.right(), rect.bottom() - 22.0 * k));
    let n = rain.series.len().max(2);
    let grow = ease(((now - since) / 1.2) as f32);
    let level_at = |x: f32| {
        let f = x * (n - 1) as f32;
        let i = (f.floor() as usize).min(n - 2);
        let t = f - i as f32;
        let t = t * t * (3.0 - 2.0 * t);
        let get = |j: usize| rain.series.get(j).copied().unwrap_or(0) as f32;
        get(i) * (1.0 - t) + get(i + 1) * t
    };
    let samples = 80;
    let mut mesh = epaint::Mesh::default();
    let mut top_line = Vec::with_capacity(samples + 1);
    for s in 0..=samples {
        let x = s as f32 / samples as f32;
        let lv = level_at(x);
        let wave = 0.03 * (now as f32 * 1.4 + x * 9.0).sin();
        let hgt = ((lv / 6.0).sqrt() * grow + 0.08 + wave).clamp(0.02, 1.0) * chart.height();
        let px = chart.left() + x * chart.width();
        let top = Pos2::new(px, chart.bottom() - hgt);
        let c = level_color(lv.round() as u8);
        mesh.colored_vertex(top, rgba(c, if lv > 0.05 { 0.75 } else { 0.35 }));
        mesh.colored_vertex(Pos2::new(px, chart.bottom()), rgba(c, 0.02));
        top_line.push(top);
    }
    for s in 0..samples as u32 {
        let a = 2 * s;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 2, a + 3);
    }
    painter.add(Shape::mesh(mesh));
    painter.add(Shape::line(top_line, Stroke::new(2.0 * k, rgba([255, 255, 255], 0.85))));
    for (label, align, x) in [
        ("いま", Align2::LEFT_TOP, chart.left()),
        ("30分後", Align2::CENTER_TOP, chart.center().x),
        ("60分後", Align2::RIGHT_TOP, chart.right()),
    ] {
        painter.text(Pos2::new(x, chart.bottom() + 4.0 * k), align, label, prop(13.0 * k), rgba(palette.dim, 0.9));
    }
}

/// Place chip on a photo print: "京都市 · 2025年8月".
pub fn place_chip(painter: &egui::Painter, print: Rect, palette: &Palette, place: &atlas::Place, now: f64, opacity: f32) {
    let label = match place.taken.as_deref().and_then(taken_label) {
        Some(t) => format!("{} · {}", place.place, t),
        None => place.place.clone(),
    };
    let galley = painter.layout_no_wrap(label, prop(15.0), rgba([255, 255, 255], opacity));
    let r = Rect::from_min_size(
        Pos2::new(print.left() + 14.0, print.bottom() - 26.0 - galley.size().y),
        galley.size() + Vec2::new(40.0, 12.0),
    );
    painter.rect_filled(r, (r.height() / 2.0) as u8, rgba([10, 14, 28], 0.5 * opacity));
    let dot = r.left_center() + Vec2::new(16.0, 0.0);
    let ph = ((now * 0.7) % 1.0) as f32;
    painter.circle_stroke(dot, 3.0 + ph * 7.0, Stroke::new(1.2, rgba(palette.glow, (1.0 - ph) * opacity)));
    painter.circle_filled(dot, 3.5, rgba([255, 255, 255], opacity));
    painter.galley(r.min + Vec2::new(30.0, 6.0), galley, rgba([255, 255, 255], opacity));
}
/// "LIVE" pill for the live tile.
pub fn live_pill(painter: &egui::Painter, print: Rect, now: f64, opacity: f32) {
    let r = Rect::from_min_size(print.left_bottom() + Vec2::new(14.0, -40.0), Vec2::new(64.0, 26.0));
    painter.rect_filled(r, 13, rgba([10, 14, 28], 0.55 * opacity));
    let pulse = ((now * 2.0).sin() * 0.5 + 0.5) as f32;
    painter.circle_filled(r.left_center() + Vec2::new(14.0, 0.0), 4.0, rgba([255, 90, 90], (0.6 + 0.4 * pulse) * opacity));
    painter.text(r.left_center() + Vec2::new(24.0, 0.0), Align2::LEFT_CENTER, "LIVE", prop(13.0), rgba([255, 255, 255], opacity));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn japanese_labels() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        assert_eq!(japanese_date(d), "10月22日 木曜日");
        assert_eq!(japanese_date(NaiveDate::from_ymd_opt(2026, 1, 4).unwrap()), "1月4日 日曜日");
        assert_eq!(frame_label(6, 6), "いま");
        assert_eq!(frame_label(3, 6), "30分前");
        assert_eq!(frame_label(8, 6), "20分後");
        assert_eq!(taken_label("2025-08").as_deref(), Some("2025年8月"));
        assert_eq!(taken_label("2025-13"), None);
        assert_eq!(taken_label("25-08"), None);
    }
}

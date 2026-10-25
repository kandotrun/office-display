//! Painting for the info strip: a glass panel with the clock, the
//! weather, the permanent honeypot attack map and the room-survey
//! panel. Geometry comes from `strip`; this module only draws.
use crate::{
    art::{glow, rgba},
    atlas, globe, honeypot,
    strip::{self, Blocks},
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

pub struct HoneypotLayer<'a> {
    pub status: &'a honeypot::Status,
    /// The atlas earth textures, when they have arrived; without them
    /// the globe is drawn as a dark graticule sphere.
    pub globe: Option<(&'a TextureHandle, &'a TextureHandle)>,
    pub sun: Sun,
}
pub struct StripInfo<'a> {
    pub weather: Option<&'a atlas::Weather>,
    pub rain: Option<&'a atlas::Rain>,
    pub sun_elevation: f64,
}

/// Live survey readings for the survey panel's data zone.
pub struct SurveyLayer<'a> {
    pub motion: f64,
    pub db: f64,
    pub faces: u32,
    /// (time, motion) samples for the trend sparkline.
    pub history: &'a [(f64, f32)],
}
/// Everything the survey panel draws: the MJPEG frame and, when the
/// readings feed is live, the metrics beside it.
pub struct SurveyPanel<'a> {
    pub tex: Option<&'a TextureHandle>,
    pub stats: Option<SurveyLayer<'a>>,
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
    info: &StripInfo<'_>,
    honeypot: Option<&HoneypotLayer<'_>>,
    survey: &SurveyPanel<'_>,
) {
    let k = blocks.scale;
    let r = |b: crate::layout::Rect| Rect::from_min_size(origin + Vec2::new(b.x, b.y), Vec2::new(b.w, b.h));
    glass(painter, r(strip), palette, (18.0 * k) as u8, 1.0);
    paint_clock(painter, r(blocks.clock), palette, clock, k);
    paint_weather(painter, r(blocks.weather), palette, info, now, k);
    paint_honeypot_panel(painter, r(blocks.honeypot), palette, now, k, honeypot);
    paint_survey(painter, r(blocks.survey), palette, now, k, survey);
    let footer = r(blocks.footer);
    text(painter, footer.left_top(), Align2::LEFT_TOP, "T S U Q R E A", prop(14.0 * k), rgba(palette.text, 0.9));
    painter.text(footer.left_bottom(), Align2::LEFT_BOTTOM, CREDITS, prop(10.5 * k), rgba(palette.dim, 0.75));
}

/// The attack map as a permanent panel: navy window, rounded corners,
/// the SOC content clipped inside, and its title pill.
fn paint_honeypot_panel(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, layer: Option<&HoneypotLayer<'_>>) {
    let radius = (16.0 * k) as u8;
    painter.rect_filled(rect, radius, rgba([4, 10, 22], 0.92));
    let clip = painter.with_clip_rect(rect.shrink(1.0));
    match layer {
        Some(h) => paint_honeypot(&clip, rect, palette, now, k, h, 1.0),
        None => text(
            &clip,
            rect.center(),
            Align2::CENTER_CENTER,
            "攻撃データを取得しています",
            prop(16.0 * k),
            rgba(palette.dim, 0.9),
        ),
    }
    painter.rect_stroke(rect, radius, Stroke::new(1.0, rgba([255, 255, 255], 0.18)), StrokeKind::Inside);
    pill(painter, rect.left_top() + Vec2::new(14.0, 14.0) * k, "攻撃マップ", 16.0 * k, palette, 1.0);
}

/// The largest rect of the given aspect centred inside `rect`.
fn contain(rect: Rect, aspect: f32) -> Rect {
    if aspect <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return rect;
    }
    let (mut w, mut h) = (rect.width(), rect.height());
    if w / h > aspect {
        w = h * aspect;
    } else {
        h = w / aspect;
    }
    Rect::from_center_size(rect.center(), Vec2::new(w, h))
}

/// The room survey as a permanent panel: the (square) frame on the
/// left, its live readings as data on the right — the panel is wide,
/// so the frame alone would leave most of it empty.
fn paint_survey(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, panel: &SurveyPanel<'_>) {
    let radius = (16.0 * k) as u8;
    painter.rect_filled(rect, radius, rgba(mix(palette.panel, [0, 0, 0], 0.4), 0.85));
    let clip = painter.with_clip_rect(rect.shrink(1.0));
    let draw_frame = |area: Rect| {
        if let Some(t) = panel.tex {
            let size = t.size_vec2();
            let fit = contain(area, size.x / size.y.max(1.0));
            clip.image(t.id(), fit, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
    };
    match (&panel.stats, panel.tex) {
        (Some(stats), _) => {
            let side = (rect.height() - 16.0 * k).max(0.0);
            let img = Rect::from_min_size(rect.left_top() + Vec2::splat(8.0 * k), Vec2::splat(side));
            draw_frame(img);
            paint_survey_stats(&clip, rect, palette, now, k, stats, img.right() + 18.0 * k);
        }
        (None, Some(_)) => draw_frame(rect.shrink(8.0 * k)),
        (None, None) => text(
            &clip,
            rect.center(),
            Align2::CENTER_CENTER,
            "計測図を準備しています",
            prop(16.0 * k),
            rgba(palette.dim, 0.9),
        ),
    }
    painter.rect_stroke(rect, radius, Stroke::new(1.0, rgba([255, 255, 255], 0.18)), StrokeKind::Inside);
    pill(painter, rect.left_top() + Vec2::new(14.0, 14.0) * k, "計測図", 16.0 * k, palette, 1.0);
    if panel.tex.is_some() {
        live_pill(painter, rect, now, 1.0);
    }
}

/// The metrics zone of the survey panel: three readings with bars,
/// and the motion trend underneath.
fn paint_survey_stats(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, s: &SurveyLayer<'_>, x0: f32) {
    let zone_w = (rect.right() - 16.0 * k - x0).max(0.0);
    let col_w = zone_w / 3.0;
    let cols: [(&str, String, f32, Rgb); 3] = [
        ("MOTION", format!("{:.2}", s.motion), (s.motion / 4.0).clamp(0.0, 1.0) as f32, SOC_CYAN),
        ("AUDIO dB", format!("{:.0}", s.db), ((s.db + 80.0) / 60.0).clamp(0.0, 1.0) as f32, SOC_GREEN),
        ("FACES", format!("{}", s.faces), (s.faces as f64 / 5.0).clamp(0.0, 1.0) as f32, SOC_ORANGE),
    ];
    for (i, (label, value, frac, col)) in cols.iter().enumerate() {
        let x = x0 + col_w * i as f32;
        text(painter, Pos2::new(x, rect.top() + 30.0 * k), Align2::LEFT_TOP, label, prop(13.0 * k), rgba(palette.dim, 0.95));
        text(painter, Pos2::new(x, rect.top() + 54.0 * k), Align2::LEFT_TOP, value, clock_font(40.0 * k), color(palette.text));
        let track = Rect::from_min_size(Pos2::new(x, rect.top() + 112.0 * k), Vec2::new((col_w - 18.0 * k).max(0.0), 6.0 * k));
        painter.rect_filled(track, 3, rgba(palette.dim, 0.25));
        if *frac > 0.0 {
            let fill = Rect::from_min_size(track.min, Vec2::new(track.width() * frac, track.height()));
            painter.rect_filled(fill, 3, rgba(*col, 0.9));
        }
    }
    // Motion trend over the samples the HUD kept (about ten minutes).
    text(painter, Pos2::new(x0, rect.bottom() - 66.0 * k), Align2::LEFT_TOP, "MOTION · 10 MIN", prop(11.0 * k), rgba(palette.dim, 0.9));
    let chart = Rect::from_min_size(
        Pos2::new(x0, rect.bottom() - 48.0 * k),
        Vec2::new(zone_w, 34.0 * k),
    );
    painter.line_segment([chart.left_bottom(), chart.right_bottom()], Stroke::new(1.0, rgba(palette.dim, 0.3)));
    if s.history.len() >= 2 {
        let t_end = s.history.last().map_or(now, |(t, _)| *t);
        let vmax = s.history.iter().map(|(_, v)| *v).fold(1.5f32, f32::max);
        let pts: Vec<Pos2> = s
            .history
            .iter()
            .map(|(t, v)| {
                let x = chart.right() - ((t_end - t) as f32 / 600.0) * chart.width();
                let y = chart.bottom() - (v / vmax).clamp(0.0, 1.0) * chart.height();
                Pos2::new(x.max(chart.left()), y)
            })
            .collect();
        painter.add(Shape::line(pts, Stroke::new(1.5, rgba(SOC_CYAN, 0.85))));
    }
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
    // The rain outlook as one line under the label (its chart left the
    // strip when the panels became permanent).
    if let Some(rain) = info.rain {
        let c = if rain.expected() { palette.accent } else { palette.dim };
        text(
            painter,
            rect.left_bottom() + Vec2::new(76.0 * k, -4.0 * k),
            Align2::LEFT_BOTTOM,
            &rain.summary,
            prop(16.0 * k),
            rgba(c, 0.95),
        );
    }
}

const SOC_CYAN: Rgb = [64, 220, 255];
const SOC_ORANGE: Rgb = [255, 150, 64];
const SOC_GREEN: Rgb = [80, 255, 170];
const SOC_RED: Rgb = [255, 96, 96];

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

/// SOC-style attack map: a globe with arcs flying into the target,
/// counters on the left, origin ranking on the right, the last hour
/// and the live feed along the bottom.
fn paint_honeypot(painter: &egui::Painter, rect: Rect, palette: &Palette, now: f64, k: f32, h: &HoneypotLayer<'_>, opacity: f32) {
    let s = h.status;
    painter.rect_filled(rect, 0, rgba([4, 10, 22], 0.90 * opacity));
    let grid = 26.0 * k;
    let mut gx = rect.left() + grid;
    while gx < rect.right() {
        painter.line_segment([Pos2::new(gx, rect.top()), Pos2::new(gx, rect.bottom())], Stroke::new(1.0, rgba(SOC_CYAN, 0.05 * opacity)));
        gx += grid;
    }
    let mut gy = rect.top() + grid;
    while gy < rect.bottom() {
        painter.line_segment([Pos2::new(rect.left(), gy), Pos2::new(rect.right(), gy)], Stroke::new(1.0, rgba(SOC_CYAN, 0.05 * opacity)));
        gy += grid;
    }

    let inner = rect.shrink(14.0 * k);
    let active = s.cowrie.active_sessions.len();
    let pulse = 0.6 + 0.4 * (now as f32 * 2.2).sin();
    let live_color = if active > 0 { SOC_RED } else { SOC_GREEN };
    painter.circle_filled(Pos2::new(inner.right() - 46.0 * k, inner.top() + 7.0 * k), 4.0 * k, rgba(live_color, pulse * opacity));
    text(painter, Pos2::new(inner.right() - 38.0 * k, inner.top()), Align2::LEFT_TOP, "LIVE", prop(13.0 * k), rgba(live_color, opacity));

    // Bottom band: sparkline, then the newest feed rows.
    let spark = Rect::from_min_size(
        Pos2::new(inner.left(), inner.bottom() - 96.0 * k),
        Vec2::new(inner.width(), 32.0 * k),
    );
    text(painter, spark.left_top() - Vec2::new(0.0, 4.0 * k), Align2::LEFT_BOTTOM, "LAST 60 MIN", prop(10.5 * k), rgba(SOC_CYAN, 0.75 * opacity));
    painter.rect_stroke(spark, 2, Stroke::new(1.0, rgba(SOC_CYAN, 0.25 * opacity)), StrokeKind::Inside);
    let series = &s.events_per_minute;
    if series.len() > 1 {
        let max = series.iter().max().copied().unwrap_or(1).max(1) as f32;
        let pts: Vec<Pos2> = series
            .iter()
            .enumerate()
            .map(|(i, v)| Pos2::new(
                spark.left() + spark.width() * i as f32 / (series.len() - 1) as f32,
                spark.bottom() - 2.0 - (*v as f32 / max) * (spark.height() - 5.0),
            ))
            .collect();
        painter.add(Shape::line(pts, Stroke::new(1.6 * k, rgba(SOC_ORANGE, 0.9 * opacity))));
    }
    let mut fy = inner.bottom() - 54.0 * k;
    for f in s.feed.iter().take(3) {
        let hm = local_hm(&f.ts).unwrap_or_else(|| "--:--".into());
        text(painter, Pos2::new(inner.left(), fy), Align2::LEFT_TOP, &hm, prop(12.5 * k), rgba(palette.dim, 0.95 * opacity));
        let geo = f.geo.as_ref().map_or(String::new(), |g| {
            if g.city.is_empty() { g.country.clone() } else { format!("{} {}", g.country, g.city) }
        });
        let warn = f.kind.contains('⚠');
        let line = trunc(&format!("{}  {}  {} {}", f.src, geo, f.kind, f.detail), 74);
        let c = if warn { SOC_ORANGE } else { palette.text };
        text(painter, Pos2::new(inner.left() + 46.0 * k, fy), Align2::LEFT_TOP, &line, prop(12.5 * k), rgba(c, 0.92 * opacity));
        fy += 18.0 * k;
    }

    let main_top = inner.top() + 30.0 * k;
    let main_bottom = spark.top() - 20.0 * k;
    let main_h = (main_bottom - main_top).max(0.0);

    // Left rail: counters.
    let left_w = 104.0 * k;
    let metrics: [(&str, String, Rgb); 4] = [
        ("TODAY", format!("{}", s.cowrie.sessions_today), palette.text),
        ("LAST 1H", format!("{}", s.cowrie.sessions_1h), palette.text),
        ("ACTIVE", format!("{active}"), if active > 0 { SOC_ORANGE } else { palette.text }),
        ("CANARY", format!("{}", s.canary.events_today), palette.text),
    ];
    let block_h = main_h / 4.0;
    for (i, (label, value, c)) in metrics.into_iter().enumerate() {
        let y = main_top + i as f32 * block_h;
        text(painter, Pos2::new(inner.left(), y), Align2::LEFT_TOP, label, prop(10.5 * k), rgba(SOC_CYAN, 0.8 * opacity));
        text(painter, Pos2::new(inner.left(), y + 13.0 * k), Align2::LEFT_TOP, &value, clock_font(30.0 * k), rgba(c, opacity));
        painter.line_segment(
            [Pos2::new(inner.left(), y + block_h - 6.0 * k), Pos2::new(inner.left() + left_w * 0.8, y + block_h - 6.0 * k)],
            Stroke::new(1.0, rgba(SOC_CYAN, 0.18 * opacity)),
        );
    }

    // Right rail: origin countries, then canary ports.
    let right_w = 148.0 * k;
    let right_x = inner.right() - right_w;
    text(painter, Pos2::new(right_x, main_top), Align2::LEFT_TOP, "ORIGIN TOP 5", prop(10.5 * k), rgba(SOC_CYAN, 0.8 * opacity));
    let mut countries: Vec<(String, u32)> = Vec::new();
    for a in &s.attackers {
        let label = a.country_label();
        let mut found = false;
        for e in countries.iter_mut() {
            if e.0 == label {
                e.1 += a.count;
                found = true;
                break;
            }
        }
        if !found {
            countries.push((label, a.count));
        }
    }
    countries.sort_by(|a, b| b.1.cmp(&a.1));
    let max_c = countries.first().map_or(1, |c| c.1).max(1) as f32;
    let mut ry = main_top + 20.0 * k;
    for (name, count) in countries.iter().take(5) {
        text(painter, Pos2::new(right_x, ry), Align2::LEFT_TOP, &trunc(name, 13), prop(12.0 * k), rgba(palette.text, 0.92 * opacity));
        text(painter, Pos2::new(inner.right(), ry), Align2::RIGHT_TOP, &format!("{count}"), prop(12.0 * k), rgba(SOC_ORANGE, opacity));
        let bw = right_w * (*count as f32 / max_c);
        painter.rect_filled(Rect::from_min_size(Pos2::new(right_x, ry + 15.0 * k), Vec2::new(bw, 3.5 * k)), 1, rgba(SOC_CYAN, 0.65 * opacity));
        ry += 30.0 * k;
    }
    text(painter, Pos2::new(right_x, ry + 4.0 * k), Align2::LEFT_TOP, "PORTS", prop(10.5 * k), rgba(SOC_CYAN, 0.8 * opacity));
    let mut py = ry + 22.0 * k;
    for (name, count) in s.canary.by_port.iter().take(4) {
        text(painter, Pos2::new(right_x, py), Align2::LEFT_TOP, &trunc(name, 14), prop(11.5 * k), rgba(palette.dim, 0.95 * opacity));
        text(painter, Pos2::new(inner.right(), py), Align2::RIGHT_TOP, &format!("{count}"), prop(11.5 * k), rgba(palette.text, 0.9 * opacity));
        py += 17.0 * k;
    }

    // The globe, drifting slowly around the Pacific side of Japan.
    let globe_left = inner.left() + left_w + 10.0 * k;
    let globe_right = right_x - 10.0 * k;
    let c = Pos2::new((globe_left + globe_right) / 2.0, main_top + main_h / 2.0);
    let radius = ((globe_right - globe_left).min(main_h) * 0.44).max(10.0);
    let view_lat = 30.0;
    let view_lon = 152.0 + 16.0 * (now * std::f64::consts::TAU / 150.0).sin();
    glow(painter, c, radius * 0.98, radius * 1.30, rgba(SOC_CYAN, 0.30 * opacity));
    if let Some((day, night)) = h.globe {
        let m = globe::mesh([c.x, c.y], radius, view_lat, view_lon, h.sun.subsolar_lat, h.sun.subsolar_lon, 72, 36);
        let mut day_mesh = epaint::Mesh::with_texture(day.id());
        let mut night_mesh = epaint::Mesh::with_texture(night.id());
        for v in &m.vertices {
            let pos = Pos2::new(v.pos[0], v.pos[1]);
            let uv = Pos2::new(v.uv[0], v.uv[1]);
            let light = (1.0 - 0.85 * v.night) * (0.45 + 0.55 * v.facing.sqrt());
            let l = (light * opacity).clamp(0.0, 1.0);
            day_mesh.vertices.push(epaint::Vertex {
                pos,
                uv,
                color: Color32::from_rgba_premultiplied((l * 120.0) as u8, (l * 190.0) as u8, (l * 255.0) as u8, (opacity * 255.0) as u8),
            });
            let n = (v.night * 0.55 * opacity).clamp(0.0, 1.0);
            night_mesh.vertices.push(epaint::Vertex {
                pos,
                uv,
                color: Color32::from_rgba_premultiplied((n * 30.0) as u8, (n * 60.0) as u8, (n * 120.0) as u8, (n * 255.0) as u8),
            });
        }
        day_mesh.indices = m.indices.clone();
        night_mesh.indices = m.indices;
        painter.add(Shape::mesh(day_mesh));
        painter.add(Shape::mesh(night_mesh));
    } else {
        painter.circle_filled(c, radius, rgba([8, 22, 44], 0.95 * opacity));
    }
    painter.circle_stroke(c, radius, Stroke::new(1.2, rgba(SOC_CYAN, 0.5 * opacity)));
    let disc = |lat: f64, lon: f64| globe::project(view_lat, view_lon, lat, lon).map(|p| Pos2::new(c.x + p[0] * radius, c.y - p[1] * radius));
    for fixed in [-60.0, -40.0, -20.0, 0.0, 20.0, 40.0, 60.0] {
        for seg in globe::graticule_line(view_lat, view_lon, fixed, false) {
            let pts: Vec<Pos2> = seg.iter().map(|p| Pos2::new(c.x + p[0] * radius, c.y - p[1] * radius)).collect();
            if pts.len() > 1 {
                painter.add(Shape::line(pts, Stroke::new(1.0, rgba(SOC_CYAN, 0.14 * opacity))));
            }
        }
    }
    for lon in (-180..180).step_by(20) {
        for seg in globe::graticule_line(view_lat, view_lon, lon as f64, true) {
            let pts: Vec<Pos2> = seg.iter().map(|p| Pos2::new(c.x + p[0] * radius, c.y - p[1] * radius)).collect();
            if pts.len() > 1 {
                painter.add(Shape::line(pts, Stroke::new(1.0, rgba(SOC_CYAN, 0.14 * opacity))));
            }
        }
    }

    // Arcs from every geolocated source converging on the target.
    let target = (s.target.lat, s.target.lon);
    let with_geo: Vec<&honeypot::Attacker> = s.attackers.iter().filter(|a| a.lat.is_some() && a.lon.is_some()).collect();
    for (i, a) in with_geo.iter().take(16).enumerate() {
        let from = (a.lat.unwrap(), a.lon.unwrap());
        let mut pts: Vec<Pos2> = Vec::new();
        for step in 0..=36 {
            let u = step as f64 / 36.0;
            let (la, lo) = globe::slerp_lonlat(from, target, u);
            match globe::project(view_lat, view_lon, la, lo) {
                Some(p) => {
                    let lift = 1.0 + 0.22 * ((std::f64::consts::PI * u).sin() as f32);
                    pts.push(Pos2::new(c.x + p[0] * radius * lift, c.y - p[1] * radius * lift));
                }
                None => {
                    if pts.len() > 1 {
                        painter.add(Shape::line(std::mem::take(&mut pts), Stroke::new(1.1, rgba(SOC_ORANGE, 0.30 * opacity))));
                    } else {
                        pts.clear();
                    }
                }
            }
        }
        if pts.len() > 1 {
            painter.add(Shape::line(pts, Stroke::new(1.1, rgba(SOC_ORANGE, 0.30 * opacity))));
        }
        let phase = (now * 0.16 + i as f64 * 0.37).rem_euclid(1.0);
        for trail in 0..5 {
            let u = phase - trail as f64 * 0.02;
            if u < 0.0 {
                continue;
            }
            let (la, lo) = globe::slerp_lonlat(from, target, u);
            if let Some(p) = globe::project(view_lat, view_lon, la, lo) {
                let lift = 1.0 + 0.22 * ((std::f64::consts::PI * u).sin() as f32);
                let at = Pos2::new(c.x + p[0] * radius * lift, c.y - p[1] * radius * lift);
                let r = (3.0 - trail as f32 * 0.45) * k;
                painter.circle_filled(at, r.max(0.8), rgba([255, 210, 138], (0.95 - trail as f32 * 0.18) * opacity));
            }
        }
        if let Some(p) = disc(from.0, from.1) {
            let r = (2.4 + ((a.count as f64 + 1.0).log10() as f32) * 1.7) * k;
            glow(painter, p, 0.0, r * 3.0, rgba(SOC_ORANGE, 0.45 * opacity));
            painter.circle_filled(p, r, rgba(SOC_ORANGE, 0.95 * opacity));
        }
    }
    // Labels for the loudest sources, kept clear of the target marker:
    // sources near the target go unlabelled, and labels flip to the
    // side of the point that faces away from the globe centre.
    let target_p = disc(target.0, target.1);
    for a in with_geo.iter().take(5) {
        if let Some(p) = disc(a.lat.unwrap(), a.lon.unwrap()) {
            if let Some(tp) = target_p
                && (p - tp).length() < radius * 0.30
            {
                continue;
            }
            let label = trunc(&a.place_label(), 16);
            if p.x > c.x {
                text(painter, p + Vec2::new(-9.0 * k, -9.0 * k), Align2::RIGHT_TOP, &label, prop(11.0 * k), rgba(palette.text, 0.85 * opacity));
            } else {
                text(painter, p + Vec2::new(9.0 * k, -9.0 * k), Align2::LEFT_TOP, &label, prop(11.0 * k), rgba(palette.text, 0.85 * opacity));
            }
        }
    }
    if let Some(p) = disc(target.0, target.1) {
        let ph = ((now * 0.6) % 1.0) as f32;
        glow(painter, p, 0.0, 16.0 * k, rgba(SOC_GREEN, 0.6 * opacity));
        painter.circle_stroke(p, 4.0 * k + ph * 18.0 * k, Stroke::new(1.5, rgba(SOC_GREEN, (1.0 - ph) * opacity)));
        painter.circle_filled(p, 3.6 * k, rgba(SOC_GREEN, opacity));
        pill(painter, p + Vec2::new(10.0, -34.0) * k, &format!("{} VPS", s.target.name), 13.0 * k, palette, opacity);
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
        assert_eq!(taken_label("2025-08").as_deref(), Some("2025年8月"));
        assert_eq!(taken_label("2025-13"), None);
        assert_eq!(taken_label("25-08"), None);
    }
}

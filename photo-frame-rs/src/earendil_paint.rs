//! Painting for the Earendil values band and its conversation panel.
//! The band is frosted glass like the strip: the emblem as a warm white
//! mask with a breathing halo, the English title in the clock's thin face,
//! the Japanese lines in the proportional one. The panel grows from the
//! band over the whole right column. Timing and data come from
//! `earendil`; the only heavy work here — turning the emblem outline into
//! textures — runs once, when an emblem arrives.
use crate::{
    art::{glow, rgba, smoothstep},
    earendil::{BandView, Earendil, Emblem, PanelView, State, Value},
    space::ease_out,
    strip_paint::{self, CLOCK_FAMILY},
    theme::{Palette, Rgb, mix},
};
use eframe::egui::{
    self, Color32, ColorImage, FontFamily, FontId, Galley, Pos2, Rect, Shape, Stroke, TextureHandle, TextureId,
    TextureOptions, Vec2,
    epaint::{
        self,
        text::{LayoutJob, TextFormat, TextWrapping},
    },
};
use std::{f32::consts::TAU, sync::Arc};

/// The emblem's warm white, and the warmth mixed into its light.
const EMBLEM: Rgb = [255, 248, 236];
const WARM: Rgb = [255, 224, 178];
pub const MASK_HEIGHT: usize = 512;
const MASK_MAX_WIDTH: usize = 1024;
/// Scanlines per pixel row; across a row, coverage is exact.
const SUBSAMPLES: usize = 4;
/// The glow is a blurred quarter-size copy of the mask, with this much
/// room around it (a share of its height) for the light to spread into.
const GLOW_DOWNSAMPLE: usize = 4;
const GLOW_MARGIN: f32 = 0.4;
const GLOW_RADIUS: f32 = 0.07;
/// The English title may shrink to this share of its size to stay on one line.
pub const TITLE_MIN_SCALE: f32 = 0.55;
pub const FOOTER: &str = "Earendil の Values をもとに AI が答えています";
const FULL_UV: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));

/// Texture size for an emblem of `aspect`: about 512 px tall, at most 1024 wide.
pub fn mask_size(aspect: f32) -> [usize; 2] {
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.0 };
    let w = MASK_HEIGHT as f32 * aspect;
    if w <= MASK_MAX_WIDTH as f32 {
        [(w.round() as usize).max(1), MASK_HEIGHT]
    } else {
        [MASK_MAX_WIDTH, ((MASK_MAX_WIDTH as f32 / aspect).round() as usize).max(1)]
    }
}
/// Coverage 0..1 of each pixel of a `w`×`h` raster by `rings` (view-box
/// fractions, y down), even-odd: `SUBSAMPLES` scanlines per pixel row,
/// each span's ends covered exactly.
pub fn rasterize(rings: &[Vec<[f32; 2]>], w: usize, h: usize) -> Vec<f32> {
    let mut cover = vec![0.0f32; w * h];
    let (sx, sy) = (w as f32, h as f32);
    let edges: Vec<([f32; 2], [f32; 2])> = rings
        .iter()
        .flat_map(|r| r.iter().zip(r.iter().cycle().skip(1)))
        .map(|(a, b)| ([a[0] * sx, a[1] * sy], [b[0] * sx, b[1] * sy]))
        .collect();
    let weight = 1.0 / SUBSAMPLES as f32;
    let mut xs = Vec::new();
    for (py, row) in cover.chunks_exact_mut(w.max(1)).enumerate() {
        for s in 0..SUBSAMPLES {
            let y = py as f32 + (s as f32 + 0.5) * weight;
            xs.clear();
            for (a, b) in &edges {
                if (a[1] <= y) != (b[1] <= y) {
                    xs.push(a[0] + (y - a[1]) * (b[0] - a[0]) / (b[1] - a[1]));
                }
            }
            xs.sort_by(f32::total_cmp);
            for pair in xs.chunks_exact(2) {
                span(row, pair[0], pair[1], weight);
            }
        }
    }
    for v in &mut cover {
        *v = v.clamp(0.0, 1.0);
    }
    cover
}
fn span(row: &mut [f32], a: f32, b: f32, weight: f32) {
    let n = row.len() as f32;
    let (a, b) = (a.clamp(0.0, n), b.clamp(0.0, n));
    if b <= a {
        return;
    }
    let (ia, ib) = (a as usize, b as usize);
    if ia == ib {
        row[ia] += (b - a) * weight;
        return;
    }
    row[ia] += (ia as f32 + 1.0 - a) * weight;
    for v in &mut row[ia + 1..ib] {
        *v += weight;
    }
    if ib < row.len() {
        row[ib] += (b - ib as f32) * weight;
    }
}
/// Three box passes each way: close to a Gaussian of about `radius` px.
pub fn blur(src: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let mut a = src.to_vec();
    let mut b = vec![0.0; a.len()];
    for _ in 0..3 {
        box_pass(&a, &mut b, w, h, radius, true);
        box_pass(&b, &mut a, w, h, radius, false);
    }
    a
}
fn box_pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (n, lines, step) = if horizontal { (w, h, 1) } else { (h, w, w) };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let at = |i: usize| if horizontal { line * w + i } else { line + i * step };
        let mut sum: f32 = (0..=r.min(n.saturating_sub(1))).map(|i| src[at(i)]).sum();
        for i in 0..n {
            dst[at(i)] = sum * norm;
            if i + r + 1 < n {
                sum += src[at(i + r + 1)];
            }
            if i >= r {
                sum -= src[at(i - r)];
            }
        }
    }
}
fn downsample(src: &[f32], w: usize, h: usize, f: usize) -> (Vec<f32>, usize, usize) {
    let (dw, dh) = (w.div_ceil(f), h.div_ceil(f));
    let mut out = vec![0.0; dw * dh];
    for y in 0..h {
        for x in 0..w {
            out[(y / f) * dw + x / f] += src[y * w + x];
        }
    }
    let n = (f * f) as f32;
    for v in &mut out {
        *v /= n;
    }
    (out, dw, dh)
}
/// The halo: the mask blurred at a quarter size inside a margin, its
/// brightest point 1. Returns the field, its size, and the margin as a
/// share of the emblem's height.
pub fn glow_field(cover: &[f32], w: usize, h: usize) -> (Vec<f32>, [usize; 2], f32) {
    let (small, sw, sh) = downsample(cover, w, h, GLOW_DOWNSAMPLE);
    let pad = (sh as f32 * GLOW_MARGIN).round() as usize;
    let (gw, gh) = (sw + 2 * pad, sh + 2 * pad);
    let mut field = vec![0.0; gw * gh];
    for y in 0..sh {
        field[(y + pad) * gw + pad..][..sw].copy_from_slice(&small[y * sw..][..sw]);
    }
    let mut field = blur(&field, gw, gh, ((sh as f32 * GLOW_RADIUS).round() as usize).max(1));
    let peak = field.iter().copied().fold(0.0, f32::max);
    if peak > 0.0 {
        for v in &mut field {
            *v /= peak;
        }
    }
    (field, [gw, gh], pad as f32 / sh.max(1) as f32)
}
fn alpha_image(alpha: &[f32], size: [usize; 2]) -> ColorImage {
    let pixels = alpha
        .iter()
        .map(|v| Color32::from_white_alpha((v.clamp(0.0, 1.0) * 255.0).round() as u8))
        .collect();
    ColorImage::new(size, pixels)
}

/// The emblem on the GPU: its mask, its halo and where its tip is.
pub struct EmblemArt {
    mask: TextureHandle,
    glow: TextureHandle,
    pub aspect: f32,
    tip: [f32; 2],
    margin: f32,
}
impl EmblemArt {
    pub fn new(ctx: &egui::Context, emblem: &Emblem) -> Self {
        let [w, h] = mask_size(emblem.aspect);
        let cover = rasterize(&emblem.rings, w, h);
        let (field, size, margin) = glow_field(&cover, w, h);
        // Mipmaps keep the edge smooth when the 512 px mask is drawn small.
        let crisp = TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
        Self {
            mask: ctx.load_texture("earendil-emblem", alpha_image(&cover, [w, h]), crisp),
            glow: ctx.load_texture("earendil-glow", alpha_image(&field, size), TextureOptions::LINEAR),
            aspect: emblem.aspect,
            tip: emblem.tip(),
            margin,
        }
    }
}
/// The emblem textures, rebuilt only when the emblem's revision changes;
/// our own star until the agent has one.
#[derive(Default)]
pub struct Art {
    current: Option<(Option<u64>, EmblemArt)>,
}
impl Art {
    pub fn sync(&mut self, ctx: &egui::Context, rev: u64, emblem: Option<&Emblem>) {
        let key = emblem.map(|_| rev);
        if self.current.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let art = match emblem {
            Some(e) => EmblemArt::new(ctx, e),
            None => EmblemArt::new(ctx, &Emblem::fallback()),
        };
        self.current = Some((key, art));
    }
    pub fn get(&self) -> Option<&EmblemArt> {
        self.current.as_ref().map(|(_, a)| a)
    }
    /// Whether the agent's emblem (not our star) is on screen.
    pub fn loaded(&self) -> bool {
        self.current.as_ref().is_some_and(|(k, _)| k.is_some())
    }
}

fn prop(size: f32) -> FontId {
    FontId::proportional(size)
}
fn clock(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(CLOCK_FAMILY.into()))
}
fn row_height(painter: &egui::Painter, font: &FontId) -> f32 {
    painter.fonts_mut(|f| f.row_height(font))
}
/// A laid-out text with the strip's soft halo (see `strip_paint::text`).
fn halo(painter: &egui::Painter, at: Pos2, galley: Arc<Galley>, opacity: f32) {
    let shade = Color32::from_black_alpha((255.0 * opacity.clamp(0.0, 1.0) * 0.35) as u8);
    painter.galley_with_override_text_color(at + Vec2::new(0.0, 1.5), galley.clone(), shade);
    painter.galley(at, galley, Color32::WHITE);
}
/// A paragraph wrapped to `width`, at most `rows` rows, "…" when cut.
/// egui breaks Japanese between characters by itself (and keeps 。、」
/// off the start of a line), so the text needs no spaces.
fn paragraph(painter: &egui::Painter, sections: &[(&str, FontId, Color32)], width: f32, rows: usize) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    for (text, font, color) in sections {
        job.append(text, 0.0, TextFormat::simple(font.clone(), *color));
    }
    job.wrap = TextWrapping {
        max_width: width.max(1.0),
        max_rows: rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}
/// One line, cut with "…" when it does not fit.
fn line(painter: &egui::Painter, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap = TextWrapping {
        max_width: width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}
/// The small letter-spaced labels.
fn spaced(painter: &egui::Painter, text: &str, size: f32, color: Color32) -> Arc<Galley> {
    let mut job = LayoutJob::default();
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: prop(size),
            color,
            extra_letter_spacing: size * 0.16,
            ..Default::default()
        },
    );
    painter.layout_job(job)
}
/// The size at which text `width` wide at `size` fits `avail`, but no
/// smaller than `min` × `size`.
pub fn fit_size(size: f32, width: f32, avail: f32, min: f32) -> f32 {
    if width <= avail || width <= 0.0 {
        size
    } else {
        size * (avail / width).max(min)
    }
}
/// The English title on one line in the thin clock face (Latin only, so
/// anything it cannot draw falls back to the proportional face), shrunk to
/// fit and cut with "…" past the floor.
fn title(painter: &egui::Painter, text: &str, size: f32, avail: f32, color: Color32) -> Arc<Galley> {
    let thin = painter.fonts_mut(|f| f.has_glyphs(&clock(size), text));
    let font = |s: f32| if thin { clock(s) } else { prop(s) };
    let first = painter.layout_no_wrap(text.into(), font(size), color);
    let fitted = fit_size(size, first.size().x, avail, TITLE_MIN_SCALE);
    if fitted == size {
        return first;
    }
    line(painter, text, font(fitted), color, avail)
}
/// A rounded chip with its label; returns its rect.
fn chip(painter: &egui::Painter, at: Pos2, galley: Arc<Galley>, size: f32, fill: Color32, opacity: f32) -> Rect {
    let r = Rect::from_min_size(at, galley.size() + Vec2::new(size * 1.3, size * 0.6));
    painter.rect_filled(r, (r.height() / 2.0) as u8, fill);
    halo(painter, r.min + Vec2::new(size * 0.65, size * 0.3), galley, opacity);
    r
}
fn chip_size(galley: &Galley, size: f32) -> Vec2 {
    galley.size() + Vec2::new(size * 1.3, size * 0.6)
}

/// How the emblem is lit this frame.
struct Look<'a> {
    halo: f32,
    scale: f32,
    /// The mask's brightness; dimmed while the light sweeps over it.
    dim: f32,
    sweep: Option<f32>,
    ripples: &'a [f32],
}
fn breathing(now: f64) -> Look<'static> {
    // About a 6 s breath.
    let breath = 0.5 + 0.5 * (now as f32 * TAU / 6.0).sin();
    Look {
        halo: 0.4 + 0.35 * breath,
        scale: 1.0,
        dim: 1.0,
        sweep: None,
        ripples: &[],
    }
}
fn look<'a>(state: State, level: f32, ripples: &'a [f32], now: f64) -> Look<'a> {
    let t = now as f32;
    match state {
        State::Listening => {
            let pulse = 0.5 + 0.5 * (t * TAU / 1.8).sin();
            Look {
                halo: 0.45 + 0.35 * pulse,
                scale: 1.0 + 0.025 * pulse,
                ..breathing(now)
            }
        }
        State::Thinking => Look {
            halo: 0.5,
            dim: 0.72,
            sweep: Some((t * 0.55).fract()),
            ..breathing(now)
        },
        State::Speaking => Look {
            halo: 0.5 + 0.5 * level,
            scale: 1.0 + 0.08 * level,
            ripples,
            ..breathing(now)
        },
        State::Idle | State::Answered | State::Error => breathing(now),
    }
}
fn paint_emblem(painter: &egui::Painter, rect: Rect, art: &EmblemArt, palette: &Palette, look: &Look<'_>, now: f64, alpha: f32) {
    let rect = Rect::from_center_size(rect.center(), rect.size() * look.scale);
    let light = mix(palette.glow, WARM, 0.55);
    painter.image(art.glow.id(), rect.expand(art.margin * rect.height()), FULL_UV, rgba(light, look.halo * alpha));
    for age in look.ripples {
        let radius = rect.height() * (0.36 + 0.45 * age);
        let fade = (1.0 - age).powi(2);
        let width = (rect.height() * 0.012 * (1.0 - 0.6 * age)).max(1.0);
        painter.circle_stroke(rect.center(), radius, Stroke::new(width, rgba(light, 0.55 * fade * alpha)));
    }
    painter.image(art.mask.id(), rect, FULL_UV, rgba(EMBLEM, look.dim * alpha));
    if let Some(phase) = look.sweep {
        sweep(painter, rect, art.mask.id(), phase, alpha);
    }
    let tip = rect.min + Vec2::new(art.tip[0] * rect.width(), art.tip[1] * rect.height());
    sparkle(painter, tip, rect.height() * 0.085, light, now, alpha);
}
/// A band of light running down over the emblem, through its own mask.
fn sweep(painter: &egui::Painter, rect: Rect, mask: TextureId, phase: f32, alpha: f32) {
    const N: u32 = 16;
    let centre = -0.3 + 1.6 * phase;
    let mut mesh = epaint::Mesh::with_texture(mask);
    for j in 0..=N {
        for i in 0..=N {
            let (u, v) = (i as f32 / N as f32, j as f32 / N as f32);
            let d = (0.35 * u + 0.65 * v - centre).abs() / 0.14;
            let a = (1.0 - d).max(0.0).powi(2) * alpha;
            mesh.vertices.push(epaint::Vertex {
                pos: rect.min + Vec2::new(u * rect.width(), v * rect.height()),
                uv: Pos2::new(u, v),
                color: Color32::from_white_alpha((a * 235.0) as u8),
            });
        }
    }
    for j in 0..N {
        for i in 0..N {
            let k = j * (N + 1) + i;
            mesh.add_triangle(k, k + 1, k + N + 1);
            mesh.add_triangle(k + 1, k + N + 1, k + N + 2);
        }
    }
    painter.add(Shape::mesh(mesh));
}
/// The twinkling four-point sparkle at the emblem's tip.
fn sparkle(painter: &egui::Painter, at: Pos2, size: f32, light: Rgb, now: f64, alpha: f32) {
    let t = now as f32;
    let flash = (t * 0.9).sin().max(0.0).powi(12);
    let r = size * (0.7 + 0.18 * (t * 2.6).sin() + 0.5 * flash);
    glow(painter, at, 0.0, r * 2.4, rgba(light, (0.35 + 0.4 * flash) * alpha));
    star(painter, at, r, 0.12 * (t * 0.4).sin(), rgba([255, 255, 255], alpha));
}
/// A four-point star: thin arms, the vertical pair the longer.
fn star(painter: &egui::Painter, c: Pos2, r: f32, angle: f32, color: Color32) {
    let mut mesh = epaint::Mesh::default();
    mesh.colored_vertex(c, color);
    for i in 0..8 {
        let a = angle + i as f32 * TAU / 8.0 - TAU / 4.0;
        let len = match i % 4 {
            0 => r,
            2 => r * 0.72,
            _ => r * 0.16,
        };
        mesh.colored_vertex(c + Vec2::angled(a) * len, color);
    }
    for i in 0..8u32 {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % 8);
    }
    painter.add(Shape::mesh(mesh));
}

/// Points per design unit in the band: 1 at the 1080p band height.
fn band_k(rect: Rect) -> f32 {
    (rect.height() / 230.0).clamp(0.5, 1.2)
}
struct BandLayout {
    k: f32,
    emblem: Rect,
    text: Rect,
}
fn band_layout(rect: Rect, aspect: f32) -> BandLayout {
    let k = band_k(rect);
    let inner = rect.shrink(20.0 * k);
    let h = (inner.height() * 0.84).min(inner.width() * 0.22 / aspect);
    let w = h * aspect;
    let slot = w.max(inner.height() * 0.5);
    // A little low, so the sparkle at the tip has room above.
    let centre = Pos2::new(inner.left() + slot / 2.0, inner.center().y + 0.05 * inner.height());
    BandLayout {
        k,
        emblem: Rect::from_center_size(centre, Vec2::new(w, h)),
        text: Rect::from_min_max(Pos2::new(inner.left() + slot + 28.0 * k, inner.top()), inner.max),
    }
}
/// One value: the label with its number, the title, the Japanese lines.
#[allow(clippy::too_many_arguments)]
fn value_block(painter: &egui::Painter, area: Rect, v: &Value, index: usize, total: usize, palette: &Palette, k: f32, dy: f32, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let mut y = area.top() + dy;
    let label = spaced(painter, &format!("EARENDIL · {}", v.kind.label()), 14.5 * k, rgba(palette.accent, alpha));
    let count = spaced(painter, &format!("{:02} / {:02}", index + 1, total), 14.5 * k, rgba(palette.dim, 0.9 * alpha));
    let (lw, lh) = (label.size().x, label.size().y);
    if lw + 16.0 * k + count.size().x <= area.width() {
        halo(painter, Pos2::new(area.left() + lw + 16.0 * k, y), count, alpha);
    }
    halo(painter, Pos2::new(area.left(), y), label, alpha);
    y += lh + 2.0 * k;
    let title = title(painter, &v.title, 60.0 * k, area.width(), rgba(palette.text, alpha));
    let th = title.size().y;
    halo(painter, Pos2::new(area.left() - 2.0 * k, y), title, alpha);
    y += th + 2.0 * k;
    let font = prop(21.0 * k);
    let rows = (((area.bottom() - y) / row_height(painter, &font)).floor().max(0.0) as usize).min(2);
    if rows > 0 {
        let sections = [
            (v.ja.as_str(), font.clone(), rgba(palette.text, 0.96 * alpha)),
            ("  —  ", font.clone(), rgba(palette.dim, 0.6 * alpha)),
            (v.gloss.as_str(), font, rgba(palette.dim, 0.92 * alpha)),
        ];
        halo(painter, Pos2::new(area.left(), y), paragraph(painter, &sections, area.width(), rows), alpha);
    }
}
/// The corner radius of the band's glass, and of the panel's grown from it.
fn glass_radius(band: Rect) -> u8 {
    (18.0 * band_k(band)) as u8
}
fn band_glass(painter: &egui::Painter, band: Rect, palette: &Palette) {
    strip_paint::glass(painter, band, palette, glass_radius(band), 1.0);
}
/// The permanent band's content, over its glass: one value at a time next
/// to the emblem, the hint at the bottom right. `fade` takes it out as the
/// panel opens.
fn paint_band(painter: &egui::Painter, rect: Rect, palette: &Palette, view: &BandView<'_>, art: &EmblemArt, now: f64, fade: f32) {
    if fade <= 0.0 {
        return;
    }
    let l = band_layout(rect, art.aspect);
    let k = l.k;
    let clip = painter.with_clip_rect(rect.shrink(1.0));
    paint_emblem(&clip, l.emblem, art, palette, &breathing(now), now, fade);
    let hint_font = prop(15.0 * k);
    let body = Rect::from_min_max(l.text.min, Pos2::new(l.text.right(), l.text.bottom() - row_height(painter, &hint_font) - 4.0 * k));
    let total = view.values.len();
    let shown = view.slide.and_then(|s| Some((s, view.values.get(s.index)?)));
    if let Some((slide, value)) = shown {
        // The old value slides up and fades; the new one rises in.
        let e = ease_out(slide.progress);
        let rise = 18.0 * k;
        if let Some((i, old)) = slide.previous.and_then(|i| Some((i, view.values.get(i)?))) {
            value_block(&clip, body, old, i, total, palette, k, -rise * e, (1.0 - e) * fade);
        }
        value_block(&clip, body, value, slide.index, total, palette, k, rise * (1.0 - e), e * fade);
    } else {
        let label = spaced(&clip, "EARENDIL · VALUES", 14.5 * k, rgba(palette.accent, fade));
        let lh = label.size().y;
        halo(&clip, body.min, label, fade);
        let wait = line(&clip, crate::earendil::PREPARING, prop(22.0 * k), rgba(palette.dim, 0.95 * fade), body.width());
        halo(&clip, body.min + Vec2::new(0.0, lh + 14.0 * k), wait, fade);
    }
    if !view.hint.is_empty() && shown.is_some() {
        let hint = line(&clip, view.hint, hint_font, rgba(palette.dim, 0.88 * fade), l.text.width());
        let at = l.text.right_bottom() - hint.size();
        halo(&clip, at, hint, fade);
    }
}

/// The panel's head at 1080p: the persona name, then the question.
const NAME_SIZE: f32 = 40.0;
const QUESTION_SIZE: f32 = 21.0;
const QUESTION_ROWS: usize = 2;
struct PanelLayout {
    k: f32,
    inner: Rect,
    emblem: Rect,
    /// Beside the emblem: the name at `name_at`, the question from `question_top`.
    head: Rect,
    name_at: Pos2,
    question_top: f32,
    body: Rect,
    chips_top: f32,
}
fn panel_layout(painter: &egui::Painter, column: Rect, aspect: f32) -> PanelLayout {
    let k = (column.height() / 1000.0).clamp(0.45, 1.5);
    let inner = column.shrink(34.0 * k);
    let h = (column.height() * 0.30).min(inner.width() * 0.32 / aspect);
    let w = h * aspect;
    let slot = w.max(h * 0.5);
    let emblem = Rect::from_min_size(Pos2::new(inner.left() + (slot - w) / 2.0, inner.top() + h * 0.06), Vec2::new(w, h));
    let name_at = Pos2::new(inner.left() + slot + 36.0 * k, inner.top() + 6.0 * k);
    let question_top = name_at.y + row_height(painter, &prop(NAME_SIZE * k)) + 16.0 * k;
    // As tall as the emblem, but never too short for the question: a
    // flat emblem (the contract allows 10:1) would leave it no room.
    let questions = QUESTION_ROWS as f32 * row_height(painter, &prop(QUESTION_SIZE * k));
    let head_bottom = emblem.bottom().max(question_top + questions + 2.0 * k);
    let footer_h = 18.0 * k;
    let chips_top = inner.bottom() - footer_h - 16.0 * k - 30.0 * k;
    PanelLayout {
        k,
        inner,
        emblem,
        head: Rect::from_min_max(Pos2::new(name_at.x, inner.top()), Pos2::new(inner.right(), head_bottom)),
        name_at,
        question_top,
        body: Rect::from_min_max(Pos2::new(inner.left(), head_bottom + 30.0 * k), Pos2::new(inner.right(), chips_top - 20.0 * k)),
        chips_top,
    }
}
fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect::from_min_max(a.min + (b.min - a.min) * t, a.max + (b.max - a.max) * t)
}
/// Three dots that pulse one after another.
fn dots(painter: &egui::Painter, left: Pos2, size: f32, color: Rgb, now: f64, alpha: f32) {
    for i in 0..3 {
        let phase = (now as f32 * 5.0 - i as f32 * 0.8).sin().max(0.0);
        let at = left + Vec2::new(size * (0.2 + 0.45 * i as f32), 0.0);
        painter.circle_filled(at, size * 0.09, rgba(color, (0.3 + 0.7 * phase) * alpha));
    }
}
/// The answer as large as it fits, read from across the room: 36 pt down
/// to 22 pt (at 1080p), cut with "…" only at the smallest.
fn answer(painter: &egui::Painter, text: &str, area: Rect, k: f32, color: Color32) -> Arc<Galley> {
    let mut last = None;
    for size in [36.0, 32.0, 28.0, 25.0, 22.0] {
        let font = prop(size * k);
        let rows = ((area.height() / row_height(painter, &font)).floor().max(1.0)) as usize;
        let g = paragraph(painter, &[(text, font, color)], area.width(), rows);
        if !g.elided {
            return g;
        }
        last = Some(g);
    }
    last.unwrap_or_else(|| paragraph(painter, &[], area.width(), 1))
}
/// The panel `p` of the way open: from the band (exactly, at 0) up over
/// the whole column.
fn panel_rect(band: Rect, column: Rect, p: f32) -> Rect {
    Rect::from_min_max(Pos2::new(column.left(), band.top() + (column.top() - band.top()) * p), column.max)
}
/// Behind the panel: the veil over the photos (they stay as shapes, the
/// text reads over them), and the band's glass grown to the panel.
fn panel_backdrop(painter: &egui::Painter, band: Rect, column: Rect, palette: &Palette, p: f32) {
    let radius = glass_radius(band);
    let rect = panel_rect(band, column, p);
    painter.rect_filled(column, radius, rgba(mix(palette.panel, [0, 0, 0], 0.5), 0.8 * p));
    strip_paint::glass(painter, rect, palette, radius, 1.0);
    painter.rect_filled(rect, radius, rgba(palette.panel, 0.3 * p));
}
/// The conversation, over its backdrop: the panel grows from the band over
/// the whole right column, the emblem comes forward.
fn paint_panel(painter: &egui::Painter, band: Rect, column: Rect, palette: &Palette, view: &PanelView<'_>, art: &EmblemArt, now: f64) {
    let p = view.open.clamp(0.0, 1.0);
    if p <= 0.0 {
        return;
    }
    let rect = panel_rect(band, column, p);
    let s = view.status;
    let l = panel_layout(painter, column, art.aspect);
    let k = l.k;
    // The emblem comes forward from its spot in the band; its light and
    // ripples stay inside the column.
    let from = band_layout(band, art.aspect).emblem;
    let lit = look(s.state, view.level, &view.ripples, now);
    paint_emblem(&painter.with_clip_rect(column), lerp_rect(from, l.emblem, p), art, palette, &lit, now, 1.0);
    let c = smoothstep(0.45, 1.0, p);
    if c <= 0.0 {
        return;
    }
    let clip = painter.with_clip_rect(rect.shrink(1.0));
    // Head: the name, the state chip, the question.
    let name = line(&clip, &s.persona, prop(NAME_SIZE * k), rgba(palette.text, c), l.head.width());
    let (name_size, name_at) = (name.size(), l.name_at);
    halo(&clip, name_at, name, c);
    let chip_font = 17.0 * k;
    let label = clip.layout_no_wrap(s.state.chip().into(), prop(chip_font), rgba(palette.text, c));
    let chip_at = Pos2::new(name_at.x + name_size.x + 18.0 * k, name_at.y + (name_size.y - chip_size(&label, chip_font).y) / 2.0);
    if chip_at.x + chip_size(&label, chip_font).x <= l.head.right() && !s.state.chip().is_empty() {
        chip(&clip, chip_at, label, chip_font, rgba(mix(palette.glow, WARM, 0.55), 0.26 * c), c);
    }
    if !s.question.is_empty() {
        let font = prop(QUESTION_SIZE * k);
        let rows = (((l.head.bottom() - l.question_top) / row_height(&clip, &font)).floor().max(0.0) as usize).min(QUESTION_ROWS);
        if rows > 0 {
            let q = format!("「{}」", s.question);
            let g = paragraph(&clip, &[(q.as_str(), font, rgba(palette.dim, 0.85 * c))], l.head.width(), rows);
            halo(&clip, Pos2::new(l.head.left(), l.question_top), g, c);
        }
    }
    // Body: what was heard, the answer, or the reason there is none.
    let body = l.body;
    let big = prop(30.0 * k);
    match s.state {
        State::Listening => {
            let rows = ((body.height() / row_height(&clip, &big)).floor().max(1.0)) as usize;
            let g = paragraph(&clip, &[(s.heard.as_str(), big.clone(), rgba(palette.dim, 0.8 * c))], body.width(), rows);
            let end = g.rows.last().map_or(body.left_top(), |r| body.min + r.rect().right_center().to_vec2());
            let end = if s.heard.is_empty() { body.left_top() + Vec2::new(0.0, row_height(&clip, &big) / 2.0) } else { end };
            halo(&clip, body.min, g, c);
            dots(&clip, end + Vec2::new(6.0 * k, 0.0), 30.0 * k, palette.text, now, c);
        }
        State::Error => {
            let reason = s.error.unwrap_or(crate::earendil::Failure::Other).message();
            halo(&clip, body.min, answer(&clip, reason, body, k, rgba(palette.text, c)), c);
        }
        _ if s.answer.is_empty() => {
            dots(&clip, body.left_top() + Vec2::new(0.0, row_height(&clip, &big) / 2.0), 30.0 * k, palette.text, now, c);
        }
        _ => halo(&clip, body.min, answer(&clip, &s.answer, body, k, rgba(palette.text, c)), c),
    }
    // Bottom: the values the answer stands on, then the footer.
    let mut x = l.inner.left();
    let size = 17.0 * k;
    for title in &s.values {
        let g = clip.layout_no_wrap(title.clone(), prop(size), rgba(palette.text, c));
        let w = chip_size(&g, size).x;
        if x + w > l.inner.right() {
            break;
        }
        let fill = rgba(mix(palette.glow, palette.panel, 0.35), 0.24 * c);
        x = chip(&clip, Pos2::new(x, l.chips_top), g, size, fill, c).right() + 10.0 * k;
    }
    let footer = if s.voice_credit.is_empty() { FOOTER.to_string() } else { format!("{FOOTER}  ·  {}", s.voice_credit) };
    let g = line(&clip, &footer, prop(13.0 * k), rgba(palette.dim, 0.8 * c), l.inner.width());
    halo(&clip, Pos2::new(l.inner.left(), l.inner.bottom() - g.size().y), g, c);
}
/// The band, and above it the conversation panel while there is one.
/// There is only ever one glass under the band — the panel's is the band's,
/// grown — and the band's content fades out on top of it, so nothing pops
/// in or out at either end of the animation.
pub fn paint(painter: &egui::Painter, band: Rect, column: Rect, palette: &Palette, model: &Earendil, art: &Art, now: f64) {
    let Some(art) = art.get() else { return };
    let panel = model.panel(now).filter(|p| p.open > 0.0);
    let open = panel.as_ref().map_or(0.0, |p| p.open.min(1.0));
    match &panel {
        Some(_) => panel_backdrop(painter, band, column, palette, open),
        None => band_glass(painter, band, palette),
    }
    if open < 1.0 {
        paint_band(painter, band, palette, &model.band(now), art, now, 1.0 - smoothstep(0.0, 0.3, open));
    }
    if let Some(p) = panel {
        paint_panel(painter, band, column, palette, &p, art, now);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::earendil::{self, Earendil, OPEN_SECONDS};

    fn square(a: f32, b: f32) -> Vec<[f32; 2]> {
        vec![[a, a], [b, a], [b, b], [a, b]]
    }
    fn sum(v: &[f32]) -> f32 {
        v.iter().sum()
    }

    #[test]
    fn rasterizes_with_exact_span_coverage() {
        let c = rasterize(&[square(0.25, 0.75)], 8, 8);
        assert!((sum(&c) - 16.0).abs() < 1e-3, "a quarter of the area: {}", sum(&c));
        assert!((c[3 * 8 + 3] - 1.0).abs() < 1e-5 && c[0] == 0.0 && c[7 * 8 + 7] == 0.0);
        // Edges halfway through a pixel cover it by half, left and right.
        let c = rasterize(&[vec![[0.0625, 0.0], [0.9375, 0.0], [0.9375, 1.0], [0.0625, 1.0]]], 8, 8);
        assert!((c[0] - 0.5).abs() < 1e-5 && (c[7] - 0.5).abs() < 1e-5 && (c[3] - 1.0).abs() < 1e-5);
        // Vertical coverage comes from the sub-scanlines.
        let c = rasterize(&[vec![[0.0, 0.0], [1.0, 0.0], [1.0, 0.125 * 0.5], [0.0, 0.125 * 0.5]]], 8, 8);
        assert!((c[0] - 0.5).abs() < 1e-5 && c[8] == 0.0, "{} {}", c[0], c[8]);
        // A triangle: the area, within a pixel's worth.
        let c = rasterize(&[vec![[0.0, 1.0], [1.0, 1.0], [0.5, 0.0]]], 64, 64);
        assert!((sum(&c) - 0.5 * 64.0 * 64.0).abs() < 32.0, "{}", sum(&c));
        assert!(c.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn rasterizes_holes_with_the_even_odd_rule() {
        let c = rasterize(&[square(0.0, 1.0), square(0.25, 0.75)], 8, 8);
        assert_eq!(c[3 * 8 + 3], 0.0, "the inner ring is a hole");
        assert!((c[0] - 1.0).abs() < 1e-5);
        assert!((sum(&c) - 48.0).abs() < 1e-3);
        // Overlapping rings cancel where they overlap, whichever way they wind.
        let mut reversed = square(0.25, 0.75);
        reversed.reverse();
        let c = rasterize(&[square(0.0, 1.0), reversed], 8, 8);
        assert_eq!(c[3 * 8 + 3], 0.0);
        // Degenerate rings draw nothing and do not panic.
        let c = rasterize(&[vec![[0.0, 0.5], [1.0, 0.5], [0.5, 0.5]]], 8, 8);
        assert_eq!(sum(&c), 0.0);
    }

    #[test]
    fn mask_is_about_512_tall_and_never_huge() {
        assert_eq!(mask_size(0.549), [281, 512]);
        assert_eq!(mask_size(1.0), [512, 512]);
        assert_eq!(mask_size(10.0), [1024, 102]);
        assert_eq!(mask_size(0.1), [51, 512]);
    }

    #[test]
    fn glow_is_a_padded_blurred_copy_with_a_unit_peak() {
        let (w, h) = (32, 64);
        let cover = rasterize(&[square(0.3, 0.7)], w, h);
        let (field, [gw, gh], margin) = glow_field(&cover, w, h);
        assert_eq!(field.len(), gw * gh);
        assert!(gw > w / 4 && gh > h / 4, "room around the emblem for the light");
        assert!((margin - GLOW_MARGIN).abs() < 0.07, "{margin}");
        let peak = field.iter().copied().fold(0.0, f32::max);
        assert!((peak - 1.0).abs() < 1e-5);
        let centre = field[(gh / 2) * gw + gw / 2];
        let corner = field[0];
        assert!(centre > 0.9 && corner < 0.01, "{centre} {corner}");
        // The blur spreads light past the edge but keeps its mass.
        let dot: Vec<f32> = (0..81).map(|i| if i == 40 { 1.0 } else { 0.0 }).collect();
        let b = blur(&dot, 9, 9, 1);
        assert!((sum(&b) - 1.0).abs() < 1e-4 && b[40] < 0.5 && b[40 - 9] > 0.0);
    }

    #[test]
    fn title_shrinks_to_fit_down_to_its_floor() {
        assert_eq!(fit_size(50.0, 300.0, 400.0, TITLE_MIN_SCALE), 50.0);
        assert!((fit_size(50.0, 500.0, 400.0, TITLE_MIN_SCALE) - 40.0).abs() < 1e-4);
        assert_eq!(fit_size(50.0, 2000.0, 400.0, TITLE_MIN_SCALE), 50.0 * TITLE_MIN_SCALE);
        assert_eq!(fit_size(50.0, 0.0, 0.0, TITLE_MIN_SCALE), 50.0);
    }

    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::hand_overlay::install_japanese_font(&ctx);
        ctx
    }

    #[test]
    fn japanese_wraps_between_characters_and_is_capped() {
        let ctx = context();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            let text = "人生を形づくるのは大切な人との関係。高い基準で働くけれど、ぶつかったら家族を優先する。";
            assert!(!text.contains(' '));
            let font = FontId::proportional(18.0);
            let g = paragraph(&painter, &[(text, font.clone(), Color32::WHITE)], 200.0, usize::MAX);
            assert!(g.rows.len() >= 3, "no spaces, still wraps: {} rows", g.rows.len());
            for row in &g.rows {
                assert!(row.rect().width() <= 200.5, "{}", row.rect().width());
            }
            assert!(!g.elided);
            assert!(!g.rows.iter().skip(1).any(|r| r.text().starts_with(['。', '、'])), "no line starts with 。 or 、");
            let g = paragraph(&painter, &[(text, font, Color32::WHITE)], 200.0, 2);
            assert_eq!(g.rows.len(), 2);
            assert!(g.elided && g.rows[1].text().ends_with('…'));
        });
    }

    #[test]
    fn art_is_built_once_per_emblem() {
        let ctx = context();
        let mut art = Art::default();
        assert!(art.get().is_none());
        art.sync(&ctx, 0, None);
        let fallback = art.get().unwrap().mask.id();
        art.sync(&ctx, 0, None);
        assert_eq!(art.get().unwrap().mask.id(), fallback, "no work per frame");
        assert!(!art.loaded());
        let e = Emblem { aspect: 0.5, rings: vec![square(0.1, 0.9)] };
        art.sync(&ctx, 1, Some(&e));
        let loaded = art.get().unwrap().mask.id();
        assert!(loaded != fallback && art.loaded());
        assert_eq!(art.get().unwrap().aspect, 0.5);
        art.sync(&ctx, 1, Some(&e));
        assert_eq!(art.get().unwrap().mask.id(), loaded);
        let size = art.get().unwrap().mask.size();
        assert_eq!(size, [256, 512]);
    }

    /// Fourteen values like the real list, with its longest titles and glosses.
    fn values() -> Vec<earendil::Value> {
        let entries = [
            ("We not I", "value", "「私」ではなく「私たち」", "最高のチームは個の総和を超える。ひとりでは謙虚に、チームとしては自信をもって。"),
            ("Family First", "value", "家族が第一", "人生を形づくるのは大切な人との関係。高い基準で働くけれど、ぶつかったら家族を優先する。"),
            ("Technology is the Means, not the End", "principle", "技術は手段であって目的ではない", "ソフトウェアは得意な道具にすぎない。スプレッドシートの方がうまくいくなら、迷わずそうする。"),
            ("Radical Responsibility, Generous Recognition", "principle", "徹底した責任、惜しみない称賛", "一人ひとりがオーナーとして考える。どの仕事にも明確な持ち主を。自分より先に仲間の貢献を称える。"),
            ("Be Charming", "principle", "感じよく", "追い込まれたときこそ礼儀と愛嬌を。周りを軽くして、できれば笑顔に。伝わっていると思わず、伝えすぎるくらいに。"),
        ];
        let list: Vec<_> = (0..14)
            .map(|i| {
                let (title, kind, ja, gloss) = entries[i % entries.len()];
                serde_json::json!({"title": title, "kind": kind, "ja": ja, "gloss": gloss})
            })
            .collect();
        earendil::parse_values(&serde_json::to_vec(&serde_json::json!({"values": list})).unwrap()).unwrap()
    }

    fn status(state: &str, answer: &str, cited: &[&str], error: Option<&str>) -> earendil::Status {
        let v = serde_json::json!({
            "state": state, "conversation": 3, "persona": "エンデルヒール",
            "hint": "「エンデルヒールなら？」と話しかけてみてください",
            "question": if state == "listening" { "" } else { "締め切り前に徹夜するのってどう思う？チームとしてはどう考えるべきか、エンデルヒールの意見を聞きたい" },
            "heard": if state == "listening" { "締め切り前に徹夜" } else { "" },
            "answer": answer, "values": cited,
            "voice": if state == "speaking" { serde_json::json!({"started": 100.0, "step": 0.05, "levels": vec![0.8; 100]}) } else { serde_json::Value::Null },
            "voice_credit": "VOICEVOX:青山龍星", "error": error,
        });
        earendil::parse_status(&serde_json::to_vec(&v).unwrap()).unwrap()
    }

    /// The band and the right column of a `w`×`h` screen.
    fn geometry(w: f32, h: f32) -> (Rect, Rect) {
        let s = crate::strip::split_with(w, h, true);
        let r = |b: crate::layout::Rect| Rect::from_min_size(Pos2::new(b.x, b.y), Vec2::new(b.w, b.h));
        (r(s.band.unwrap()), r(s.column()))
    }
    /// Paint one frame and return its shapes in paint order.
    fn shapes(ctx: &egui::Context, model: &Earendil, art: &Art, band: Rect, column: Rect, now: f64) -> Vec<Shape> {
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            paint(&painter, band, column, &crate::theme::DAY, model, art, now);
        });
        output.shapes.into_iter().map(|s| s.shape).collect()
    }
    fn text_of(shape: &Shape) -> Option<(String, Rect)> {
        match shape {
            Shape::Text(t) => Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()))),
            _ => None,
        }
    }
    /// Paint one frame and return every text with its screen rect.
    fn texts(ctx: &egui::Context, model: &Earendil, art: &Art, band: Rect, column: Rect, now: f64) -> Vec<(String, Rect)> {
        shapes(ctx, model, art, band, column, now).iter().filter_map(text_of).collect()
    }
    /// The filled rects over `at`, blended in paint order (premultiplied,
    /// over nothing): the glass and veils there, without the text or the
    /// emblem.
    fn backdrop_at(shapes: &[Shape], at: Pos2) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for s in shapes {
            if let Shape::Rect(r) = s
                && r.rect.contains(at)
            {
                let fill = r.fill.to_array().map(|v| v as f32 / 255.0);
                for (o, c) in out.iter_mut().zip(fill) {
                    *o = c + *o * (1.0 - fill[3]);
                }
            }
        }
        out
    }
    /// The glass outlines drawn around the whole band.
    fn outlines(shapes: &[Shape], band: Rect) -> usize {
        shapes
            .iter()
            .filter(|s| matches!(s, Shape::Rect(r) if r.stroke.width > 0.0 && r.rect.expand(0.5).contains_rect(band)))
            .count()
    }
    fn assert_tidy(texts: &[(String, Rect)], inside: Rect, what: &str) {
        for (text, r) in texts {
            assert!(inside.expand(1.0).contains_rect(*r), "{what}: {text:?} {r:?} leaves {inside:?}");
        }
        for (i, (a, ra)) in texts.iter().enumerate() {
            for (b, rb) in &texts[i + 1..] {
                if a != b {
                    assert!(!ra.shrink(1.5).intersects(rb.shrink(1.5)), "{what}: {a:?} {ra:?} overlaps {b:?} {rb:?}");
                }
            }
        }
    }

    #[test]
    fn band_and_panel_text_stays_inside_without_overlap() {
        let ctx = context();
        let mut art = Art::default();
        art.sync(&ctx, 0, None);
        // The 1080p split, and the smallest window.
        for (w, h) in [(1920.0, 1080.0), (1280.0, 720.0)] {
            let (band, column) = geometry(w, h);
            let mut model = Earendil::new(None);
            model.apply_values(values());
            let all = texts(&ctx, &model, &art, band, column, 0.0);
            assert!(all.iter().any(|(t, _)| t == earendil::PREPARING), "{all:?}");
            // Step through the first values: each tick 20 s apart moves on by one.
            for i in 0..6 {
                let at = i as f64 * 20.0;
                model.apply_status(status("idle", "", &[], None), at);
                model.tick(at, 100.0);
                model.tick(at + 1.0, 100.0);
                let band_texts = texts(&ctx, &model, &art, band, column, at + 1.0);
                assert!(band_texts.iter().any(|(t, _)| t.starts_with("EARENDIL · ")), "{band_texts:?}");
                assert!(band_texts.iter().any(|(t, _)| t.contains(" / 14")), "{band_texts:?}");
                assert!(band_texts.iter().any(|(t, _)| t.starts_with("「エンデルヒールなら")), "{band_texts:?}");
                assert_tidy(&band_texts, band, &format!("band {w}x{h} at {at}"));
            }
            // The longest answer the contract allows.
            let long: String = "エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。".chars().cycle().take(earendil::ANSWER_CHARS).collect();
            for (state, answer, cited, error) in [
                ("listening", "", vec![], None),
                ("thinking", "", vec![], None),
                ("speaking", "エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。", vec!["We not I"], None),
                ("answered", long.as_str(), vec!["We not I", "Family First", "Radical Responsibility, Generous Recognition"], None),
                ("error", "", vec![], Some("llm_failed")),
            ] {
                let t = 1000.0;
                model.apply_status(status(state, answer, &cited, error), t);
                model.tick(t, 100.5);
                model.tick(t + OPEN_SECONDS, 100.5);
                let panel_texts = texts(&ctx, &model, &art, band, column, t + OPEN_SECONDS);
                assert!(panel_texts.iter().any(|(t, _)| t == "エンデルヒール"), "{state}: {panel_texts:?}");
                assert!(panel_texts.iter().any(|(t, _)| t.starts_with(FOOTER) && t.ends_with("VOICEVOX:青山龍星")));
                let chip = earendil::parse_status(format!(r#"{{"state": "{state}"}}"#).as_bytes()).unwrap().state.chip();
                assert!(panel_texts.iter().any(|(t, _)| t == chip), "{state}: chip {chip}");
                if state == "error" {
                    assert!(panel_texts.iter().any(|(t, _)| t == earendil::Failure::LlmFailed.message()));
                }
                for title in &cited {
                    assert!(panel_texts.iter().any(|(t, _)| t == title), "{state}: chip {title}");
                }
                if state == "answered" {
                    assert!(panel_texts.iter().any(|(t, _)| *t == long), "the longest answer is drawn in full");
                }
                assert_tidy(&panel_texts, column, &format!("panel {w}x{h} {state}"));
                model.apply_status(status("idle", "", &[], None), t + 2.0);
                model.tick(t + 2.0, 100.0);
                model.tick(t + 2.0 + OPEN_SECONDS, 100.0);
            }
        }
    }

    #[test]
    fn the_band_glass_grows_into_the_panel_without_a_pop() {
        let ctx = context();
        let mut art = Art::default();
        art.sync(&ctx, 0, None);
        let (band, column) = geometry(1920.0, 1080.0);
        let mut model = Earendil::new(None);
        model.apply_values(values());
        model.apply_status(status("idle", "", &[], None), 0.0);
        model.tick(0.0, 100.0);
        // Just inside the band's top edge, clear of the panel's chips.
        let at = Pos2::new(band.center().x, band.top() + 4.0);
        let look = |model: &Earendil, now: f64| {
            let s = shapes(&ctx, model, &art, band, column, now);
            let open = model.panel(now).map_or(0.0, |p| p.open);
            assert_eq!(outlines(&s, band), 1, "one glass around the band at {open}");
            (open, backdrop_at(&s, at), s)
        };
        let same = |a: f64, b: f64, model: &Earendil, what: &str| {
            let ((oa, ga, _), (ob, gb, _)) = (look(model, a), look(model, b));
            assert!((oa - ob).abs() < 2e-3, "{what}: {oa} vs {ob}");
            assert!(ga.iter().zip(gb).all(|(x, y)| (x - y).abs() < 0.01), "{what}: the glass pops {ga:?} → {gb:?}");
        };
        model.apply_status(status("listening", "", &[], None), 10.0);
        model.tick(10.0, 100.0);
        same(10.0, 10.0 + 1e-4, &model, "opening");
        // The band's text fades out over the panel's glass, not under it.
        let (_, _, s) = look(&model, 10.0 + 1e-4);
        let text = s.iter().position(|s| text_of(s).is_some_and(|(t, _)| t.starts_with("EARENDIL · ")));
        let glass = s.iter().rposition(|s| matches!(s, Shape::Rect(r) if r.rect.contains(at)));
        assert!(glass < text, "glass at {glass:?}, the band's text at {text:?}");
        look(&model, 10.0 + OPEN_SECONDS / 2.0);
        same(10.0 + 0.9 * OPEN_SECONDS, 10.0 + OPEN_SECONDS, &model, "fully open");
        model.apply_status(status("idle", "", &[], None), 20.0);
        model.tick(20.0, 100.0);
        same(20.0, 20.0 + 1e-4, &model, "closing");
        look(&model, 20.0 + OPEN_SECONDS / 2.0);
        same(20.0 + 0.9 * OPEN_SECONDS, 20.0 + OPEN_SECONDS, &model, "closed");
    }

    #[test]
    fn a_wide_emblem_still_leaves_two_rows_for_the_question() {
        let ctx = context();
        // The contract allows any aspect from 0.1 to 10.
        for aspect in [0.1, 3.0, 10.0] {
            let mut art = Art::default();
            art.sync(&ctx, 1, Some(&Emblem { aspect, rings: vec![square(0.1, 0.9)] }));
            for (w, h) in [(1920.0, 1080.0), (1280.0, 720.0)] {
                let (band, column) = geometry(w, h);
                let mut model = Earendil::new(None);
                model.apply_values(values());
                let short = "エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。";
                model.apply_status(status("answered", short, &["We not I"], None), 0.0);
                model.tick(0.0, 100.0);
                model.tick(OPEN_SECONDS, 100.0);
                let s = shapes(&ctx, &model, &art, band, column, OPEN_SECONDS);
                let what = format!("aspect {aspect} at {w}x{h}");
                let rows = s.iter().find_map(|s| match s {
                    Shape::Text(t) if t.galley.text().starts_with("「締め切り前") => Some(t.galley.rows.len()),
                    _ => None,
                });
                assert_eq!(rows, Some(2), "{what}: the long question on two rows");
                assert_tidy(&s.iter().filter_map(text_of).collect::<Vec<_>>(), column, &what);
            }
        }
    }
}

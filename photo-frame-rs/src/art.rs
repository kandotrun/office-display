//! The living sky behind the photos: gradient, drifting colour light, stars,
//! bokeh and weather. Drawing only; colours come from `theme`.
use crate::{
    space::{Camera, v3},
    theme::{Mood, Palette, Rgb, mix},
};
use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Vec2, epaint};

pub fn rgba(rgb: Rgb, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], (a.clamp(0.0, 1.0) * 255.0) as u8)
}
fn hash(i: u32) -> f32 {
    let mut x = i.wrapping_mul(0x9E37_79B9) ^ 0x7F4A_7C15;
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    (x & 0xFFFF) as f32 / 65535.0
}
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// 0 by day, 1 at night: how much the night sky shows.
pub fn night(elevation_deg: f64) -> f32 {
    smoothstep(2.0, -10.0, elevation_deg as f32)
}

/// Soft radial light: full `color` inside `inner`, fading to nothing at `outer`.
pub fn glow(painter: &egui::Painter, c: Pos2, inner: f32, outer: f32, color: Color32) {
    let n = 48;
    let mut mesh = epaint::Mesh::default();
    let clear = Color32::from_rgba_premultiplied(0, 0, 0, 0);
    mesh.colored_vertex(c, color);
    for i in 0..n {
        let d = Vec2::angled(i as f32 / n as f32 * std::f32::consts::TAU);
        mesh.colored_vertex(c + d * inner, color);
        mesh.colored_vertex(c + d * outer, clear);
    }
    for i in 0..n as u32 {
        let (a, b) = (1 + 2 * i, 1 + 2 * ((i + 1) % n as u32));
        mesh.add_triangle(0, a, b);
        mesh.add_triangle(a, b, a + 1);
        mesh.add_triangle(b, a + 1, b + 1);
    }
    painter.add(Shape::mesh(mesh));
}

/// Vertical gradient from `top` to `bottom` with an eased middle.
pub fn gradient(painter: &egui::Painter, rect: Rect, top: Rgb, bottom: Rgb, a: f32) {
    let bands = 16;
    let mut mesh = epaint::Mesh::default();
    for i in 0..=bands {
        let t = i as f32 / bands as f32;
        let c = rgba(mix(top, bottom, smoothstep(0.0, 1.0, t)), a);
        let y = rect.top() + t * rect.height();
        mesh.colored_vertex(Pos2::new(rect.left(), y), c);
        mesh.colored_vertex(Pos2::new(rect.right(), y), c);
    }
    for i in 0..bands as u32 {
        let k = 2 * i;
        mesh.add_triangle(k, k + 1, k + 2);
        mesh.add_triangle(k + 1, k + 2, k + 3);
    }
    painter.add(Shape::mesh(mesh));
}

pub fn sky(painter: &egui::Painter, rect: Rect, palette: &Palette) {
    gradient(painter, rect, palette.sky_top, palette.sky_bottom, 1.0);
}

/// Large blooms of colour drifting across the sky, tinted by the photos on show.
pub fn aurora(painter: &egui::Painter, rect: Rect, tones: &[Rgb], palette: &Palette, now: f64) {
    let t = now as f32;
    let mut colors: Vec<Rgb> = vec![palette.glow, palette.accent];
    colors.extend(tones.iter().map(|c| vivid(*c)));
    let size = rect.width().max(rect.height());
    for (i, c) in colors.iter().take(6).enumerate() {
        let f = i as f32;
        let p = rect.min
            + rect.size()
                * Vec2::new(
                    0.5 + 0.42 * (t * (0.031 + 0.007 * f) + f * 1.9).sin(),
                    0.5 + 0.38 * (t * (0.023 + 0.005 * f) + f * 2.7).cos(),
                );
        let breathe = 0.85 + 0.15 * (t * 0.21 + f).sin();
        glow(painter, p, 0.0, size * (0.32 + 0.06 * f.min(3.0)) * breathe, rgba(*c, 0.20));
    }
}
/// Push a photo's average colour toward something luminous.
pub fn vivid(c: Rgb) -> Rgb {
    let max = *c.iter().max().unwrap_or(&1) as f32;
    let lift = if max < 1.0 { 1.0 } else { 235.0 / max };
    let lifted = c.map(|v| (v as f32 * lift).min(255.0) as u8);
    let luma = (lifted[0] as f32 + lifted[1] as f32 + lifted[2] as f32) / 3.0;
    lifted.map(|v| (luma + (v as f32 - luma) * 1.4).clamp(0.0, 255.0) as u8)
}

pub fn stars(painter: &egui::Painter, rect: Rect, amount: f32, now: f64) {
    if amount <= 0.01 {
        return;
    }
    for i in 0..160u32 {
        let p = rect.min + rect.size() * Vec2::new(hash(i * 3), hash(i * 3 + 1).powf(1.6) * 0.75);
        let tw = 0.5 + 0.5 * (now as f32 * (0.5 + hash(i * 5 + 7) * 1.5) + i as f32).sin();
        let r = 0.5 + hash(i * 7 + 3) * 1.3;
        let a = amount * (0.25 + 0.75 * tw) * (0.4 + 0.6 * hash(i + 99));
        if r > 1.4 {
            glow(painter, p, 0.0, r * 5.0, rgba([200, 215, 255], a * 0.25));
        }
        painter.circle_filled(p, r, rgba([255, 255, 255], a));
    }
}

/// Soft out-of-focus lights floating up through the scene, placed in 3D so the
/// camera sway gives them parallax.
pub fn bokeh(painter: &egui::Painter, cam: &Camera, area: Rect, palette: &Palette, tones: &[Rgb], now: f64) {
    let t = now as f32;
    for i in 0..46u32 {
        let z = -200.0 + hash(i * 13 + 1) * 2600.0;
        let rise = (t * (8.0 + 14.0 * hash(i * 17 + 3)) + hash(i * 19) * area.height() * 2.0) % (area.height() * 2.0);
        let p = v3(
            area.center().x + (hash(i * 7 + 5) - 0.5) * area.width() * 2.2 + 40.0 * (t * 0.2 + i as f32).sin(),
            area.bottom() + area.height() * 0.5 - rise,
            z,
        );
        let Some((s, k)) = cam.project(p) else { continue };
        let at = Pos2::new(s[0], s[1]);
        let color = if i % 3 == 0 {
            tones.get(i as usize % tones.len().max(1)).map_or(palette.glow, |c| vivid(*c))
        } else {
            palette.glow
        };
        let radius = (10.0 + 34.0 * hash(i * 23 + 9)) * k;
        let a = (0.10 + 0.14 * hash(i * 29)) * (0.6 + 0.4 * (t * 0.7 + i as f32).sin().abs());
        glow(painter, at, radius * 0.55, radius, rgba(color, a));
    }
}

/// Weather in the sky itself: rain streaks, snow, or slow clouds.
pub fn weather(painter: &egui::Painter, rect: Rect, mood: Option<Mood>, now: f64) {
    let t = now as f32;
    match mood {
        Some(Mood::Rain) => {
            for i in 0..170u32 {
                let speed = 900.0 + 500.0 * hash(i * 3 + 1);
                let len = 26.0 + 30.0 * hash(i * 5 + 2);
                let y = (t * speed + hash(i * 7) * rect.height() * 1.5) % (rect.height() * 1.2) - rect.height() * 0.1;
                let x = rect.left() + hash(i * 11 + 4) * rect.width() * 1.2 - y * 0.18;
                let a = 0.12 + 0.18 * hash(i * 13);
                let from = Pos2::new(x, rect.top() + y);
                painter.line_segment([from, from + Vec2::new(-len * 0.18, len)], Stroke::new(1.2, rgba([220, 232, 255], a)));
            }
        }
        Some(Mood::Snow) => {
            for i in 0..140u32 {
                let speed = 30.0 + 40.0 * hash(i * 3 + 1);
                let y = (t * speed + hash(i * 7) * rect.height()) % rect.height();
                let x = rect.left() + hash(i * 11 + 4) * rect.width() + 24.0 * (t * 0.6 + i as f32).sin();
                let r = 1.2 + 2.4 * hash(i * 13);
                painter.circle_filled(Pos2::new(x, rect.top() + y), r, rgba([255, 255, 255], 0.55 + 0.35 * hash(i)));
            }
        }
        Some(Mood::Cloudy) => clouds(painter, rect, 0.10, now),
        Some(Mood::Clear) | None => clouds(painter, rect, 0.035, now),
    }
}
fn clouds(painter: &egui::Painter, rect: Rect, a: f32, now: f64) {
    let t = now as f32;
    for i in 0..5u32 {
        let span = rect.width() * 1.6;
        let x = rect.left() - rect.width() * 0.3 + (t * (6.0 + 5.0 * hash(i)) + hash(i * 3) * span) % span;
        let y = rect.top() + rect.height() * (0.1 + 0.35 * hash(i * 5 + 1));
        let w = rect.width() * (0.18 + 0.14 * hash(i * 7));
        for j in 0..5 {
            let off = Vec2::new((j as f32 - 2.0) * w * 0.28, (hash(i * 11 + j) - 0.5) * w * 0.18);
            glow(painter, Pos2::new(x, y) + off, w * 0.12, w * 0.42, rgba([255, 255, 255], a));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn night_amount_and_vivid_colours() {
        assert_eq!(night(30.0), 0.0);
        assert_eq!(night(-20.0), 1.0);
        let dusk = night(-4.0);
        assert!(dusk > 0.0 && dusk < 1.0);
        let v = vivid([60, 40, 20]);
        assert!(v[0] > 200 && v[0] > v[1] && v[1] > v[2], "brighter, same hue order: {v:?}");
        assert_eq!(vivid([0, 0, 0]), [0, 0, 0]);
        for i in 0..500 {
            assert!((0.0..=1.0).contains(&hash(i)));
        }
    }
}

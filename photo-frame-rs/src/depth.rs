//! egui drawing for `space`: projected quads, outlines, the floor and particles.
use crate::{
    space::{self, Camera, V3, v3},
    strip_paint::alpha,
    theme::Palette,
};
use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, TextureId, epaint};

fn pos(p: [f32; 2]) -> Pos2 {
    Pos2::new(p[0], p[1])
}
/// Textured quad, subdivided so the texture follows the perspective.
#[allow(clippy::too_many_arguments)]
pub fn textured(
    painter: &egui::Painter,
    cam: &Camera,
    corners: &[V3; 4],
    texture: TextureId,
    uv: [[f32; 2]; 2],
    tint: Color32,
    n: usize,
) {
    let g = space::grid(cam, corners, uv, n);
    if g.indices.is_empty() {
        return;
    }
    let mut mesh = epaint::Mesh::with_texture(texture);
    for (p, uv) in g.points.iter().zip(&g.uv) {
        let at = p.map_or(Pos2::ZERO, |(s, _)| pos(s));
        mesh.vertices.push(epaint::Vertex {
            pos: at,
            uv: Pos2::new(uv[0], uv[1]),
            color: tint,
        });
    }
    mesh.indices = g.indices;
    painter.add(Shape::mesh(mesh));
}
pub fn filled(painter: &egui::Painter, cam: &Camera, corners: &[V3; 4], color: Color32) {
    let pts: Option<Vec<Pos2>> = corners.iter().map(|c| cam.project(*c).map(|(s, _)| pos(s))).collect();
    if let Some(pts) = pts {
        let mut mesh = epaint::Mesh::default();
        for p in pts {
            mesh.colored_vertex(p, color);
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(Shape::mesh(mesh));
    }
}
pub fn outline(painter: &egui::Painter, cam: &Camera, corners: &[V3; 4], stroke: Stroke) {
    let pts: Option<Vec<Pos2>> = corners.iter().map(|c| cam.project(*c).map(|(s, _)| pos(s))).collect();
    if let Some(pts) = pts {
        painter.add(Shape::closed_line(pts, stroke));
    }
}
pub fn segment(painter: &egui::Painter, cam: &Camera, a: V3, b: V3, stroke: Stroke) {
    if let (Some((a, _)), Some((b, _))) = (cam.project(a), cam.project(b)) {
        painter.line_segment([pos(a), pos(b)], stroke);
    }
}
/// Corner ticks on a posed quad; `len` is a fraction of each edge.
pub fn brackets(painter: &egui::Painter, cam: &Camera, c: &[V3; 4], len: f32, stroke: Stroke) {
    for i in 0..4 {
        let (p, next, prev) = (c[i], c[(i + 1) % 4], c[(i + 3) % 4]);
        segment(painter, cam, p, p.lerp(next, len), stroke);
        segment(painter, cam, p, p.lerp(prev, len), stroke);
    }
}
/// Soft drop shadow: the quad pushed back and down, in a few widening layers.
pub fn shadow(painter: &egui::Painter, cam: &Camera, c: &[V3; 4], strength: f32) {
    let centre = space::on_quad(c, 0.5, 0.5);
    for i in 0..4 {
        let grow = 1.0 + 0.02 * i as f32;
        let layer = c.map(|p| centre.add(p.sub(centre).scale(grow)).add(v3(0.0, 22.0, 60.0)));
        filled(painter, cam, &layer, Color32::from_black_alpha((strength * 40.0 / (i + 1) as f32) as u8));
    }
}
pub fn project(cam: &Camera, p: V3) -> Option<Pos2> {
    cam.project(p).map(|(s, _)| pos(s))
}

/// Receding grid floor that scrolls toward the viewer.
pub fn floor(painter: &egui::Painter, cam: &Camera, area: Rect, palette: &Palette, now: f64) {
    let y = area.bottom() + 60.0;
    let far = 5200.0;
    let step = 220.0;
    let scroll = ((now * 90.0) % step as f64) as f32;
    let span = area.width() * 2.5;
    let fade = |z: f32| (1.0 - z / far).clamp(0.0, 1.0).powi(2);
    let mut x = area.center().x - span;
    while x <= area.center().x + span {
        // Draw each rail in pieces so its alpha fades with depth.
        let mut z = 0.0;
        while z < far {
            let z2 = z + 400.0;
            segment(
                painter,
                cam,
                v3(x, y, z),
                v3(x, y, z2),
                Stroke::new(1.0, alpha(palette.accent, 0.22 * fade(z2))),
            );
            z = z2;
        }
        x += step;
    }
    let mut z = step - scroll;
    while z < far {
        segment(
            painter,
            cam,
            v3(area.center().x - span, y, z),
            v3(area.center().x + span, y, z),
            Stroke::new(1.0, alpha(palette.accent, 0.28 * fade(z))),
        );
        z += step;
    }
}
fn hash(i: u32) -> f32 {
    let mut x = i.wrapping_mul(0x2545_F491) ^ 0x9E37_79B9;
    x ^= x >> 13;
    x = x.wrapping_mul(0x5BD1_E995);
    x ^= x >> 15;
    (x & 0xFFFF) as f32 / 65535.0
}
/// Motes streaming toward the viewer; brighter and bigger as they come close.
pub fn particles(painter: &egui::Painter, cam: &Camera, area: Rect, palette: &Palette, now: f64, count: u32) {
    let depth = 4000.0;
    for i in 0..count {
        let speed = 160.0 + 260.0 * hash(i * 3 + 2);
        let z = depth - ((now as f32 * speed + hash(i * 3) * depth) % depth);
        let p = v3(
            area.center().x + (hash(i * 7 + 1) - 0.5) * area.width() * 3.0,
            area.center().y + (hash(i * 11 + 5) - 0.5) * area.height() * 2.4,
            z - 300.0,
        );
        if let Some((s, k)) = cam.project(p) {
            let at = pos(s);
            if !area.expand(40.0).contains(at) {
                continue;
            }
            let near = (1.0 - z / depth).clamp(0.0, 1.0);
            let a = 0.85 * near * near.min((z / 300.0).clamp(0.0, 1.0) + 0.2);
            // A short streak along the direction of travel.
            if let Some((s2, _)) = cam.project(p.add(v3(0.0, 0.0, 60.0 + 120.0 * near))) {
                painter.line_segment([at, pos(s2)], Stroke::new((1.4 * k).clamp(0.4, 3.0), alpha(palette.accent, a * 0.6)));
            }
            painter.circle_filled(at, (1.3 * k).clamp(0.4, 3.5), alpha(palette.text, a));
        }
    }
}

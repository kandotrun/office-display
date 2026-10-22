//! egui drawing for `space`: projected quads, outlines and shadows.
use crate::space::{self, Camera, V3, v3};
use eframe::egui::{self, Color32, Pos2, Shape, Stroke, TextureId, epaint};

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

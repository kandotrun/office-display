//! Tiny perspective camera for the 3D look. Pure math, no egui.
//!
//! World units are screen points: x right, y down, z away from the viewer. In
//! the rest pose a point on the z = 0 plane projects onto itself, so 2D layout
//! rects can be lifted into 3D without moving.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}
pub const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}
impl V3 {
    pub fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
    pub fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
    pub fn scale(self, k: f32) -> V3 {
        v3(self.x * k, self.y * k, self.z * k)
    }
    pub fn lerp(self, o: V3, t: f32) -> V3 {
        self.add(o.sub(self).scale(t))
    }
    /// Rotation about the vertical (y) axis; positive turns the right side away.
    pub fn yaw(self, a: f32) -> V3 {
        let (s, c) = a.sin_cos();
        v3(self.x * c - self.z * s, self.y, self.x * s + self.z * c)
    }
    /// Rotation about the horizontal (x) axis; positive tips the top away.
    pub fn pitch(self, a: f32) -> V3 {
        let (s, c) = a.sin_cos();
        v3(self.x, self.y * c + self.z * s, -self.y * s + self.z * c)
    }
    /// Yaw then pitch about `pivot`.
    pub fn turn(self, pivot: V3, yaw: f32, pitch: f32) -> V3 {
        self.sub(pivot).yaw(yaw).pitch(pitch).add(pivot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// World point on the optical axis at z = 0; it projects to itself.
    pub pivot: V3,
    /// Distance from the eye to the z = 0 plane, in points.
    pub focal: f32,
    pub yaw: f32,
    pub pitch: f32,
    /// Moves the whole world toward (negative) or away from the eye.
    pub dolly: f32,
}
pub const NEAR: f32 = 0.05;
impl Camera {
    pub fn new(pivot: [f32; 2], focal: f32) -> Self {
        Self {
            pivot: v3(pivot[0], pivot[1], 0.0),
            focal,
            yaw: 0.0,
            pitch: 0.0,
            dolly: 0.0,
        }
    }
    /// Screen position and scale (1 at the z = 0 plane), or None behind the eye.
    pub fn project(&self, p: V3) -> Option<([f32; 2], f32)> {
        let q = p.sub(self.pivot).yaw(self.yaw).pitch(self.pitch);
        let depth = self.focal + q.z + self.dolly;
        if depth < self.focal * NEAR {
            return None;
        }
        let k = self.focal / depth;
        Some(([self.pivot.x + q.x * k, self.pivot.y + q.y * k], k))
    }
}

/// Corners of an axis-aligned rect on z = 0, clockwise from top-left.
pub fn rect_corners(x: f32, y: f32, w: f32, h: f32) -> [V3; 4] {
    [v3(x, y, 0.0), v3(x + w, y, 0.0), v3(x + w, y + h, 0.0), v3(x, y + h, 0.0)]
}
/// Bilinear point on a quad (u along top edge, v down the left edge).
pub fn on_quad(c: &[V3; 4], u: f32, v: f32) -> V3 {
    c[0].lerp(c[1], u).lerp(c[3].lerp(c[2], u), v)
}
/// Card pose: turn its corners about the card centre, then push it back by `z`.
pub fn pose(c: [V3; 4], yaw: f32, pitch: f32, z: f32) -> [V3; 4] {
    let centre = on_quad(&c, 0.5, 0.5);
    c.map(|p| p.turn(centre, yaw, pitch).add(v3(0.0, 0.0, z)))
}
/// Subdivided, projected quad: positions, scales and uv for an n x n grid, plus
/// triangle indices. Cells with any vertex behind the eye are dropped.
pub struct Grid {
    pub points: Vec<Option<([f32; 2], f32)>>,
    pub uv: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}
pub fn grid(cam: &Camera, c: &[V3; 4], uv: [[f32; 2]; 2], n: usize) -> Grid {
    let n = n.max(1);
    let mut points = Vec::with_capacity((n + 1) * (n + 1));
    let mut uvs = Vec::with_capacity(points.capacity());
    for j in 0..=n {
        let v = j as f32 / n as f32;
        for i in 0..=n {
            let u = i as f32 / n as f32;
            points.push(cam.project(on_quad(c, u, v)));
            uvs.push([
                uv[0][0] + (uv[1][0] - uv[0][0]) * u,
                uv[0][1] + (uv[1][1] - uv[0][1]) * v,
            ]);
        }
    }
    let at = |i: usize, j: usize| j * (n + 1) + i;
    let mut indices = Vec::new();
    for j in 0..n {
        for i in 0..n {
            let q = [at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)];
            if q.iter().all(|&k| points[k].is_some()) {
                let q = q.map(|k| k as u32);
                indices.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
            }
        }
    }
    Grid {
        points,
        uv: uvs,
        indices,
    }
}
pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}
/// Slow, layered sway so the room sees the collage drift in 3D. Radians.
pub fn sway(now: f64) -> (f32, f32) {
    let t = now as f32;
    let yaw = 0.105 * (t * 0.21).sin() + 0.03 * (t * 0.53 + 1.3).sin();
    let pitch = 0.05 * (t * 0.17 + 0.7).sin() + 0.015 * (t * 0.41).sin();
    (yaw, pitch)
}
/// Card `order` flying in: (yaw, z) from the entrance progress 0..1.
pub fn fly_in(progress: f32, order: usize) -> (f32, f32) {
    let e = ease_out(progress);
    let side = if order % 2 == 0 { -1.0 } else { 1.0 };
    (side * 1.25 * (1.0 - e), 900.0 * (1.0 - e))
}
/// Outgoing card: turns away and recedes as `gone` goes 0..1.
pub fn fly_out(gone: f32, order: usize) -> (f32, f32) {
    let g = gone.clamp(0.0, 1.0);
    let g = g * g;
    let side = if order % 2 == 0 { 1.0 } else { -1.0 };
    (side * 1.1 * g, 700.0 * g)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }
    #[test]
    fn rest_pose_is_the_identity_on_the_screen_plane() {
        let cam = Camera::new([960.0, 540.0], 2000.0);
        for p in [v3(0.0, 0.0, 0.0), v3(1920.0, 1080.0, 0.0), v3(300.0, 700.0, 0.0)] {
            let (s, k) = cam.project(p).unwrap();
            assert!(close(s, [p.x, p.y]) && (k - 1.0).abs() < 1e-6);
        }
    }
    #[test]
    fn depth_shrinks_toward_the_pivot_and_behind_is_hidden() {
        let cam = Camera::new([100.0, 100.0], 1000.0);
        let (s, k) = cam.project(v3(200.0, 100.0, 1000.0)).unwrap();
        assert!(close(s, [150.0, 100.0]) && (k - 0.5).abs() < 1e-6);
        assert!(cam.project(v3(0.0, 0.0, -990.0)).is_none());
        let mut dolly = cam;
        dolly.dolly = -500.0;
        assert!(dolly.project(v3(200.0, 100.0, 0.0)).unwrap().0[0] > 200.0, "dolly in magnifies");
    }
    #[test]
    fn rotations_turn_the_expected_side_away() {
        let right = v3(1.0, 0.0, 0.0).yaw(0.5);
        assert!(right.z > 0.0 && right.x < 1.0);
        let top = v3(0.0, -1.0, 0.0).pitch(0.5);
        assert!(top.z > 0.0, "top tips away");
        let p = v3(5.0, -3.0, 2.0);
        let back = p.yaw(0.7).yaw(-0.7);
        assert!((back.x - p.x).abs() < 1e-5 && (back.z - p.z).abs() < 1e-5);
        let len = |q: V3| (q.x * q.x + q.y * q.y + q.z * q.z).sqrt();
        assert!((len(p.yaw(1.1).pitch(-0.4)) - len(p)).abs() < 1e-4);
    }
    #[test]
    fn quads_pose_and_grid() {
        let c = rect_corners(10.0, 20.0, 100.0, 50.0);
        assert_eq!(on_quad(&c, 0.5, 0.5), v3(60.0, 45.0, 0.0));
        assert_eq!(pose(c, 0.0, 0.0, 0.0), c);
        let turned = pose(c, 0.6, 0.0, 30.0);
        assert!(turned[1].z > turned[0].z, "right edge recedes");
        assert!((on_quad(&turned, 0.5, 0.5).z - 30.0).abs() < 1e-4);
        let cam = Camera::new([60.0, 45.0], 500.0);
        let g = grid(&cam, &c, [[0.0, 0.0], [1.0, 1.0]], 4);
        assert_eq!(g.points.len(), 25);
        assert_eq!(g.indices.len(), 4 * 4 * 6);
        assert_eq!(g.uv[24], [1.0, 1.0]);
        // A quad poking behind the eye loses only the cells that cross it.
        let near = [v3(0.0, 0.0, -600.0), v3(100.0, 0.0, -600.0), v3(100.0, 50.0, 0.0), v3(0.0, 50.0, 0.0)];
        let g = grid(&cam, &near, [[0.0, 0.0], [1.0, 1.0]], 4);
        assert!(!g.indices.is_empty() && g.indices.len() < 4 * 4 * 6);
    }
    #[test]
    fn sway_and_flights_stay_bounded_and_settle() {
        for i in 0..2000 {
            let (y, p) = sway(i as f64 * 0.37);
            assert!(y.abs() <= 0.14 && p.abs() <= 0.07);
        }
        assert_eq!(fly_in(1.0, 0), (0.0, 0.0));
        assert_eq!(fly_out(0.0, 3), (0.0, 0.0));
        let (y0, z0) = fly_in(0.0, 0);
        let (y1, _) = fly_in(0.0, 1);
        assert!(y0 < 0.0 && y1 > 0.0 && z0 > 800.0, "alternate sides, start far");
        assert!(fly_in(0.5, 0).1 < 900.0 * 0.2, "ease-out covers most distance early");
        assert!(fly_out(1.0, 0).1 > 600.0);
    }
}

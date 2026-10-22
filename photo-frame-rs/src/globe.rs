//! Orthographic globe. Pure geometry; the caller turns it into egui meshes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 2],
    /// Equirectangular texture coordinate (u east from -180, v south from 90).
    pub uv: [f32; 2],
    /// 0 on the day side, 1 on the night side, smooth across the terminator.
    pub night: f32,
    /// Cosine of the angle from the view centre (0 at the limb), for limb darkening.
    pub facing: f32,
}
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}
fn unit(lat: f64, lon: f64) -> [f64; 3] {
    let (la, lo) = (lat.to_radians(), lon.to_radians());
    [la.cos() * lo.cos(), la.cos() * lo.sin(), la.sin()]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
/// Unit-disc coordinates with y pointing up (north), and the facing cosine.
fn ortho(view_lat: f64, view_lon: f64, lat: f64, lon: f64) -> ([f64; 2], f64) {
    let (p0, p) = (view_lat.to_radians(), lat.to_radians());
    let dl = (lon - view_lon).to_radians();
    let x = p.cos() * dl.sin();
    let y = p0.cos() * p.sin() - p0.sin() * p.cos() * dl.cos();
    let c = p0.sin() * p.sin() + p0.cos() * p.cos() * dl.cos();
    ([x, y], c)
}
/// Visible points only. y points up.
pub fn project(view_lat: f64, view_lon: f64, lat: f64, lon: f64) -> Option<[f32; 2]> {
    let ([x, y], c) = ortho(view_lat, view_lon, lat, lon);
    (c > 0.0).then_some([x as f32, y as f32])
}
fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// Screen-space mesh of the visible hemisphere. Back-side vertices of cells that
/// straddle the limb are pushed onto the rim so the outline stays round.
#[allow(clippy::too_many_arguments)]
pub fn mesh(
    center: [f32; 2],
    radius: f32,
    view_lat: f64,
    view_lon: f64,
    sun_lat: f64,
    sun_lon: f64,
    cols: usize,
    rows: usize,
) -> Mesh {
    let (cols, rows) = (cols.max(4), rows.max(2));
    let sun = unit(sun_lat, sun_lon);
    let mut vertices = Vec::with_capacity((cols + 1) * (rows + 1));
    let mut front = Vec::with_capacity(vertices.capacity());
    for r in 0..=rows {
        let v = r as f64 / rows as f64;
        let lat = 90.0 - 180.0 * v;
        for c in 0..=cols {
            let u = c as f64 / cols as f64;
            let lon = -180.0 + 360.0 * u;
            let ([mut x, mut y], facing) = ortho(view_lat, view_lon, lat, lon);
            if facing <= 0.0 {
                let len = (x * x + y * y).sqrt().max(1e-9);
                (x, y) = (x / len, y / len);
            }
            let light = dot(unit(lat, lon), sun);
            vertices.push(Vertex {
                pos: [center[0] + radius * x as f32, center[1] - radius * y as f32],
                uv: [u as f32, v as f32],
                night: smoothstep(0.08, -0.12, light) as f32,
                facing: facing.max(0.0) as f32,
            });
            front.push(facing > 0.0);
        }
    }
    let mut indices = Vec::new();
    let at = |r: usize, c: usize| r * (cols + 1) + c;
    for r in 0..rows {
        for c in 0..cols {
            let quad = [at(r, c), at(r, c + 1), at(r + 1, c + 1), at(r + 1, c)];
            if quad.iter().any(|&i| front[i]) {
                let q = quad.map(|i| i as u32);
                indices.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
            }
        }
    }
    Mesh { vertices, indices }
}
/// Great-circle interpolation between (lat, lon) points.
pub fn slerp_lonlat(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    let (pa, pb) = (unit(a.0, a.1), unit(b.0, b.1));
    let omega = dot(pa, pb).clamp(-1.0, 1.0).acos();
    let t = t.clamp(0.0, 1.0);
    let p = if omega < 1e-9 {
        pa
    } else {
        let (sa, sb) = (((1.0 - t) * omega).sin() / omega.sin(), (t * omega).sin() / omega.sin());
        std::array::from_fn(|i| pa[i] * sa + pb[i] * sb)
    };
    let lat = p[2].clamp(-1.0, 1.0).asin().to_degrees();
    let lon = p[1].atan2(p[0]).to_degrees();
    (lat, lon)
}
/// Visible polyline of a parallel or meridian for the graticule overlay, in unit-disc
/// coordinates with y up. Splits at the limb.
pub fn graticule_line(view_lat: f64, view_lon: f64, fixed: f64, meridian: bool) -> Vec<Vec<[f32; 2]>> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let steps = 90;
    for i in 0..=steps {
        let s = i as f64 / steps as f64;
        let (lat, lon) = if meridian {
            (-90.0 + 180.0 * s, fixed)
        } else {
            (fixed, -180.0 + 360.0 * s)
        };
        match project(view_lat, view_lon, lat, lon) {
            Some(p) => current.push(p),
            None if current.len() > 1 => out.push(std::mem::take(&mut current)),
            None => current.clear(),
        }
    }
    if current.len() > 1 {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_centre_limb_and_back() {
        assert_eq!(project(35.0, 139.0, 35.0, 139.0), Some([0.0, 0.0]));
        let east = project(0.0, 0.0, 0.0, 60.0).unwrap();
        assert!((east[0] - 0.866).abs() < 0.001 && east[1].abs() < 1e-6);
        let north = project(0.0, 0.0, 45.0, 0.0).unwrap();
        assert!(north[1] > 0.7, "y points north");
        assert_eq!(project(0.0, 0.0, 0.0, 180.0), None);
        assert_eq!(project(35.0, 139.0, -35.0, -41.0), None);
    }
    #[test]
    fn mesh_covers_only_the_front_hemisphere() {
        let m = mesh([100.0, 100.0], 50.0, 35.0, 139.0, 0.0, 139.0, 72, 36);
        assert_eq!(m.vertices.len(), 73 * 37);
        assert!(!m.indices.is_empty() && m.indices.len() % 3 == 0);
        let tri = m.indices.len() / 3;
        assert!(tri < 72 * 36 * 2 * 7 / 10, "back cells culled: {tri}");
        for v in &m.vertices {
            let d = ((v.pos[0] - 100.0).powi(2) + (v.pos[1] - 100.0).powi(2)).sqrt();
            assert!(d <= 50.01, "inside the disc");
        }
        // The vertex nearest the view centre faces us and is in daylight (sun overhead).
        let centre = m
            .vertices
            .iter()
            .max_by(|a, b| a.facing.total_cmp(&b.facing))
            .unwrap();
        assert!(centre.facing > 0.99 && centre.night == 0.0);
    }
    #[test]
    fn night_follows_the_subsolar_point() {
        let m = mesh([0.0, 0.0], 1.0, 0.0, 0.0, 0.0, 90.0, 36, 18);
        let at = |lat: f64, lon: f64| {
            let c = ((lon + 180.0) / 10.0).round() as usize;
            let r = ((90.0 - lat) / 10.0).round() as usize;
            m.vertices[r * 37 + c].night
        };
        assert_eq!(at(0.0, 90.0), 0.0);
        assert_eq!(at(0.0, -90.0), 1.0);
        assert!(at(0.0, 0.0) > 0.0 && at(0.0, 0.0) < 1.0, "terminator is soft");
    }
    #[test]
    fn slerp_follows_the_great_circle() {
        let (lat, lon) = slerp_lonlat((0.0, 0.0), (0.0, 90.0), 0.5);
        assert!(lat.abs() < 1e-9 && (lon - 45.0).abs() < 1e-9);
        assert_eq!(slerp_lonlat((35.0, 139.0), (35.0, 139.0), 0.7).0.round(), 35.0);
        let (lat, _) = slerp_lonlat((35.7, 139.8), (48.9, 2.35), 0.5);
        assert!(lat > 48.9, "polar route goes north of both ends: {lat}");
        assert_eq!(slerp_lonlat((10.0, 20.0), (30.0, 40.0), 1.0).0.round(), 30.0);
    }
    #[test]
    fn graticule_splits_at_the_limb() {
        let equator = graticule_line(0.0, 0.0, 0.0, false);
        assert_eq!(equator.len(), 1);
        assert!(equator[0].iter().all(|p| p[0].abs() <= 1.0));
        assert!(graticule_line(0.0, 0.0, 180.0, true).is_empty());
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
    pub fn area(self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }
    pub fn inset(self, p: f32) -> Self {
        Self {
            x: self.x + p,
            y: self.y + p,
            w: (self.w - 2.0 * p).max(0.0),
            h: (self.h - 2.0 * p).max(0.0),
        }
    }
}
pub fn intersects(a: Rect, b: Rect) -> bool {
    a.w > 0.0
        && a.h > 0.0
        && b.w > 0.0
        && b.h > 0.0
        && a.x < b.x + b.w
        && a.x + a.w > b.x
        && a.y < b.y + b.h
        && a.y + a.h > b.y
}
pub fn contain(box_: Rect, aspect: f32) -> Rect {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.0
    };
    let h = box_.h.max(0.0).min(box_.w.max(0.0) / aspect);
    let w = h * aspect;
    Rect {
        x: box_.x + (box_.w - w) / 2.0,
        y: box_.y + (box_.h - h) / 2.0,
        w,
        h,
    }
}
pub fn camera_rect(w: f32, h: f32) -> Rect {
    let cw = (w * 0.22).clamp(180.0, 380.0).min(w * 0.4);
    let ch = cw * 9.0 / 16.0;
    Rect {
        x: w - 24.0 - cw,
        y: h - 24.0 - ch,
        w: cw,
        h: ch,
    }
}
pub fn safe_foreground(card: Rect, aspect: f32, camera: Option<Rect>) -> Rect {
    let inner = card.inset(
        (card.w * 0.012)
            .clamp(8.0, 16.0)
            .min(card.w * 0.1)
            .min(card.h * 0.1),
    );
    let fit = contain(inner, aspect);
    let Some(camera) = camera else {
        return fit;
    };
    if !intersects(fit, camera) {
        return fit;
    }
    // Four alternatives guarantee no overlap even with smaller/odd window sizes.
    let candidates = [
        Rect {
            w: inner.w.min(camera.x - 12.0 - inner.x).max(0.0),
            ..inner
        },
        Rect {
            h: inner.h.min(camera.y - 12.0 - inner.y).max(0.0),
            ..inner
        },
        Rect {
            x: (camera.x + camera.w + 12.0).max(inner.x),
            w: (inner.x + inner.w - camera.x - camera.w - 12.0).max(0.0),
            ..inner
        },
        Rect {
            y: (camera.y + camera.h + 12.0).max(inner.y),
            h: (inner.y + inner.h - camera.y - camera.h - 12.0).max(0.0),
            ..inner
        },
    ];
    candidates
        .into_iter()
        .map(|b| contain(b, aspect))
        .filter(|r| !intersects(*r, camera))
        .max_by(|a, b| a.area().total_cmp(&b.area()))
        .unwrap_or(Rect {
            w: 0.0,
            h: 0.0,
            ..inner
        })
}
#[derive(Debug, Clone)]
pub struct Placement {
    pub index: usize,
    pub card: Rect,
    pub foreground: Rect,
}
#[derive(Debug, Clone)]
pub struct Layout {
    pub name: &'static str,
    pub placements: Vec<Placement>,
    pub score: f32,
}
type Cell = [f32; 4];
const TEMPLATES: &[(&str, &[Cell])] = &[
    ("pair-columns", &[[0., 0., 0.5, 1.], [0.5, 0., 0.5, 1.]]),
    ("pair-rows", &[[0., 0., 1., 0.5], [0., 0.5, 1., 0.5]]),
    (
        "portrait-trio",
        &[
            [0., 0., 1. / 3., 1.],
            [1. / 3., 0., 1. / 3., 1.],
            [2. / 3., 0., 1. / 3., 1.],
        ],
    ),
    (
        "feature-left",
        &[
            [0., 0., 0.42, 1.],
            [0.42, 0., 0.58, 0.5],
            [0.42, 0.5, 0.58, 0.5],
        ],
    ),
    (
        "feature-right",
        &[
            [0., 0., 0.58, 0.5],
            [0., 0.5, 0.58, 0.5],
            [0.58, 0., 0.42, 1.],
        ],
    ),
    (
        "feature-top",
        &[
            [0., 0., 1., 0.52],
            [0., 0.52, 0.5, 0.48],
            [0.5, 0.52, 0.5, 0.48],
        ],
    ),
    (
        "balanced-grid",
        &[
            [0., 0., 0.5, 0.5],
            [0.5, 0., 0.5, 0.5],
            [0., 0.5, 0.5, 0.5],
            [0.5, 0.5, 0.5, 0.5],
        ],
    ),
    (
        "portrait-quartet",
        &[
            [0., 0., 0.25, 1.],
            [0.25, 0., 0.25, 1.],
            [0.5, 0., 0.25, 1.],
            [0.75, 0., 0.25, 1.],
        ],
    ),
    (
        "portrait-and-three",
        &[
            [0., 0., 0.36, 1.],
            [0.36, 0., 0.64, 0.5],
            [0.36, 0.5, 0.32, 0.5],
            [0.68, 0.5, 0.32, 0.5],
        ],
    ),
    (
        "two-portraits-two-wide",
        &[
            [0., 0., 0.28, 1.],
            [0.28, 0., 0.44, 0.5],
            [0.28, 0.5, 0.44, 0.5],
            [0.72, 0., 0.28, 1.],
        ],
    ),
    ("single-available", &[[0., 0., 1., 1.]]),
];
fn permutations(values: &mut [usize], start: usize, out: &mut Vec<Vec<usize>>) {
    if start == values.len() {
        out.push(values.to_vec());
        return;
    }
    for i in start..values.len() {
        values.swap(start, i);
        permutations(values, start + 1, out);
        values.swap(start, i);
    }
}
pub fn choose(
    aspects: &[f32],
    width: f32,
    height: f32,
    camera: Option<Rect>,
    previous: &str,
) -> Option<Layout> {
    if aspects.is_empty() || width < 240.0 || height < 180.0 {
        return None;
    }
    let available = aspects.len().min(4);
    let outer = (width * 0.008).clamp(16.0, 36.0);
    let gap = (width * 0.005).clamp(12.0, 24.0);
    let area = Rect {
        x: outer,
        y: 80.0,
        w: width - 2.0 * outer,
        h: height - 128.0,
    };
    let mut best: Option<Layout> = None;
    for &(name, cells) in TEMPLATES {
        let count = cells.len();
        if count > available || count < available.min(2) {
            continue;
        }
        let mut assignments = Vec::new();
        permutations(&mut (0..count).collect::<Vec<_>>(), 0, &mut assignments);
        for assignment in assignments {
            let placements: Vec<_> = cells
                .iter()
                .enumerate()
                .map(|(i, [x, y, w, h])| {
                    let card = Rect {
                        x: area.x + x * area.w + gap / 2.0,
                        y: area.y + y * area.h + gap / 2.0,
                        w: w * area.w - gap,
                        h: h * area.h - gap,
                    };
                    let index = assignment[i];
                    Placement {
                        index,
                        card,
                        foreground: safe_foreground(card, aspects[index], camera),
                    }
                })
                .collect();
            if placements.iter().any(|p| p.foreground.area() <= 0.0) {
                continue;
            }
            let fill = placements.iter().map(|p| p.foreground.area()).sum::<f32>() / area.area();
            let score =
                fill + 0.04 * (count as f32 - 2.0) - if name == previous { 0.012 } else { 0.0 };
            if best.as_ref().is_none_or(|b| score > b.score) {
                best = Some(Layout {
                    name,
                    placements,
                    score,
                });
            }
        }
    }
    best
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contain_preserves_complete_aspect() {
        let b = Rect {
            x: 5.,
            y: 8.,
            w: 500.,
            h: 400.,
        };
        for a in [0.1, 0.5, 1., 2., 10.] {
            let r = contain(b, a);
            assert!((r.w / r.h - a).abs() < 0.0001);
            assert!(
                r.x >= b.x
                    && r.y >= b.y
                    && r.x + r.w <= b.x + b.w + 0.01
                    && r.y + r.h <= b.y + b.h + 0.01
            );
        }
    }
    #[test]
    fn layouts_fit_camera_and_reserved_clock_footer() {
        for (w, h) in [(3840., 2160.), (1920., 1080.), (960., 540.), (640., 360.)] {
            let c = camera_rect(w, h).inset(-10.);
            for aspects in [
                vec![0.5, 0.6, 0.7, 0.75],
                vec![1.77, 1.5, 1.4, 2.],
                vec![0.6, 1.77, 0.8, 2.],
                vec![0.3],
            ] {
                let l = choose(&aspects, w, h, Some(c), "").unwrap();
                assert!(l.placements.len() >= aspects.len().min(2));
                assert!(l.placements.len() <= 4);
                for p in l.placements {
                    let r = p.foreground;
                    assert!(!intersects(r, c));
                    assert!(r.y >= 80. && r.y + r.h <= h - 48. + 0.01);
                    assert!((r.w / r.h - aspects[p.index]).abs() < 0.001);
                }
            }
        }
    }
    #[test]
    fn portrait_and_landscape_choices_differ() {
        let a = choose(&[0.5, 0.5, 0.5, 0.5], 1920., 1080., None, "").unwrap();
        let b = choose(&[1.77, 1.77, 1.77, 1.77], 1920., 1080., None, "").unwrap();
        assert_ne!(a.name, b.name);
        assert!(choose(&[], 1000., 700., None, "").is_none());
    }
}

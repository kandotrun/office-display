//! Info strip geometry and timing. Pure: rects and seconds in, rects and indices out.
use crate::layout::Rect;

/// The collage keeps using `layout::choose` unchanged: it is handed a virtual
/// viewport whose usable area is exactly `collage`, and is drawn shifted by
/// `offset`. That way the strip needs no change to the layout invariants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Split {
    pub collage: Rect,
    pub strip: Rect,
    pub virtual_viewport: [f32; 2],
    pub offset: [f32; 2],
}
pub fn outer(width: f32) -> f32 {
    (width * 0.008).clamp(16.0, 36.0)
}
pub fn gap(width: f32) -> f32 {
    (width * 0.005).clamp(12.0, 24.0)
}
pub fn split(width: f32, height: f32) -> Split {
    let outer = outer(width);
    let gap = gap(width);
    let strip_w = (width * 0.30).clamp(360.0, 640.0).min(width * 0.45);
    let strip = Rect {
        x: width - outer - strip_w,
        y: outer,
        w: strip_w,
        h: (height - 2.0 * outer).max(0.0),
    };
    let collage = Rect {
        x: outer,
        y: outer,
        w: (strip.x - gap - outer).max(0.0),
        h: strip.h,
    };
    // layout::choose reserves y 80..h-48 and x outer..w-outer.
    let virtual_width = collage.x + collage.w + outer;
    Split {
        collage,
        strip,
        virtual_viewport: [virtual_width, collage.h + 128.0],
        offset: [0.0, outer - 80.0],
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blocks {
    pub clock: Rect,
    pub weather: Rect,
    pub map: Rect,
    pub telemetry: Rect,
    pub rain: Rect,
    pub footer: Rect,
    /// Points per design unit: everything in the strip scales with this.
    pub scale: f32,
}
pub fn blocks(strip: Rect) -> Blocks {
    let k = (strip.h / 1000.0).clamp(0.45, 1.5);
    let pad = 20.0 * k;
    let gap = 12.0 * k;
    let x = strip.x + pad;
    let w = (strip.w - 2.0 * pad).max(0.0);
    let row = |y: f32, h: f32| Rect { x, y, w, h };
    let clock = row(strip.y + pad, 104.0 * k);
    let weather = row(clock.y + clock.h + gap, 36.0 * k);
    let footer_h = 46.0 * k;
    let footer = row(strip.y + strip.h - pad - footer_h, footer_h);
    let rain_h = 96.0 * k;
    let rain = row(footer.y - gap - rain_h, rain_h);
    let map_top = weather.y + weather.h + gap;
    let room = (rain.y - gap - map_top).max(0.0);
    let (map_w, map_h) = if w * 0.75 <= room {
        (w, w * 0.75)
    } else {
        (room / 0.75, room)
    };
    let map = Rect {
        x: x + (w - map_w) / 2.0,
        y: map_top,
        w: map_w,
        h: map_h,
    };
    let telemetry_top = map.y + map.h + gap;
    let telemetry = row(telemetry_top, (rain.y - gap - telemetry_top).max(0.0));
    Blocks {
        clock,
        weather,
        map,
        telemetry,
        rain,
        footer,
        scale: k,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Globe,
    Japan,
    Local,
    Photo,
}
pub const ORDER: [View; 4] = [View::Globe, View::Japan, View::Local, View::Photo];
impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Globe => "globe",
            View::Japan => "japan",
            View::Local => "local",
            View::Photo => "photo",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            View::Globe => "地球",
            View::Japan => "雨雲 · 日本",
            View::Local => "周辺 · 雨雲",
            View::Photo => "撮影地",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            View::Globe => "GLOBAL",
            View::Japan => "JP-RADAR",
            View::Local => "LOCAL",
            View::Photo => "PHOTO-LOC",
        }
    }
}
pub const DWELL_SECONDS: f64 = 15.0;
pub const RAIN_DWELL_SECONDS: f64 = 30.0;
pub const CROSSFADE_SECONDS: f64 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewFrame {
    pub view: View,
    pub previous: Option<View>,
    /// 0 → only `previous` visible, 1 → only `view`.
    pub blend: f32,
    pub elapsed: f64,
    pub dwell: f64,
}
#[derive(Default)]
pub struct Cycle {
    current: Option<(View, f64)>,
    previous: Option<View>,
}
pub fn dwell(view: View, rain_expected: bool) -> f64 {
    if view == View::Local && rain_expected {
        RAIN_DWELL_SECONDS
    } else {
        DWELL_SECONDS
    }
}
impl Cycle {
    pub fn update(&mut self, now: f64, ready: &[View], rain_expected: bool) -> Option<ViewFrame> {
        if ready.is_empty() {
            *self = Self::default();
            return None;
        }
        let (mut view, mut started) = match self.current {
            Some((v, t)) if ready.contains(&v) => (v, t),
            other => {
                self.previous = other.map(|(v, _)| v);
                let first = ORDER.into_iter().find(|v| ready.contains(v))?;
                (first, now)
            }
        };
        if now - started >= dwell(view, rain_expected) {
            let at = ORDER.iter().position(|v| *v == view).unwrap_or(0);
            let next = (1..=ORDER.len())
                .map(|i| ORDER[(at + i) % ORDER.len()])
                .find(|v| ready.contains(v))
                .unwrap_or(view);
            if next != view {
                self.previous = Some(view);
            }
            view = next;
            started = now;
        }
        self.current = Some((view, started));
        let elapsed = (now - started).max(0.0);
        let blend = (elapsed / CROSSFADE_SECONDS).clamp(0.0, 1.0) as f32;
        if blend >= 1.0 {
            self.previous = None;
        }
        Some(ViewFrame {
            view,
            previous: self.previous,
            blend,
            elapsed,
            dwell: dwell(view, rain_expected),
        })
    }
}

pub const RADAR_STEP: f64 = 0.5;
pub const RADAR_NOW_HOLD: f64 = 2.0;
pub const RADAR_END_HOLD: f64 = 1.5;
pub const RADAR_FADE: f64 = 0.25;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadarFrame {
    pub index: usize,
    pub next: usize,
    pub blend: f32,
}
pub fn radar_frame(elapsed: f64, frames: usize, now_index: usize) -> Option<RadarFrame> {
    if frames == 0 {
        return None;
    }
    if frames == 1 {
        return Some(RadarFrame {
            index: 0,
            next: 0,
            blend: 0.0,
        });
    }
    let duration = |i: usize| {
        RADAR_STEP
            + if i == now_index { RADAR_NOW_HOLD } else { 0.0 }
            + if i == frames - 1 { RADAR_END_HOLD } else { 0.0 }
    };
    let total: f64 = (0..frames).map(duration).sum();
    let mut t = elapsed.max(0.0).rem_euclid(total);
    for index in 0..frames {
        let d = duration(index);
        if t < d || index == frames - 1 {
            let next = (index + 1) % frames;
            let blend = ((t - (d - RADAR_FADE)) / RADAR_FADE).clamp(0.0, 1.0) as f32;
            return Some(RadarFrame { index, next, blend });
        }
        t -= d;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    fn inside(r: Rect, c: Rect) -> bool {
        r.w >= 0.0
            && r.h >= 0.0
            && r.x >= c.x - 0.01
            && r.y >= c.y - 0.01
            && r.x + r.w <= c.x + c.w + 0.01
            && r.y + r.h <= c.y + c.h + 0.01
    }
    const SIZES: [(f32, f32); 4] = [(3840., 2160.), (1920., 1080.), (960., 540.), (640., 360.)];
    #[test]
    fn strip_and_collage_split_the_viewport() {
        for (w, h) in SIZES {
            let s = split(w, h);
            let screen = Rect { x: 0., y: 0., w, h };
            assert!(inside(s.collage, screen) && inside(s.strip, screen));
            assert!(!layout::intersects(s.collage, s.strip));
            assert!(s.strip.w >= w * 0.29 - 0.01 || s.strip.w >= 360.0 || s.strip.w >= w * 0.45 - 0.01);
            assert!((s.strip.x + s.strip.w + outer(w) - w).abs() < 0.01, "flush right");
        }
        let s = split(1920., 1080.);
        assert_eq!(s.strip.w, 576.0);
        assert_eq!(s.collage.y, 16.0);
    }
    #[test]
    fn collage_layouts_land_inside_the_collage() {
        for (w, h) in SIZES {
            let s = split(w, h);
            for aspects in [vec![0.5, 0.6, 0.7, 0.75], vec![1.77, 1.5, 1.4, 2.], vec![0.3]] {
                let l = layout::choose(&aspects, s.virtual_viewport[0], s.virtual_viewport[1], None, "")
                    .unwrap();
                for p in l.placements {
                    let card = Rect {
                        x: p.card.x + s.offset[0],
                        y: p.card.y + s.offset[1],
                        ..p.card
                    };
                    assert!(inside(card, s.collage), "{w}x{h} {card:?} {:?}", s.collage);
                }
            }
        }
    }
    #[test]
    fn blocks_stack_without_overlap() {
        for (w, h) in SIZES {
            let s = split(w, h);
            let b = blocks(s.strip);
            let order = [b.clock, b.weather, b.map, b.telemetry, b.rain, b.footer];
            for r in order {
                assert!(inside(r, s.strip), "{w}x{h} {r:?}");
            }
            for pair in order.windows(2) {
                assert!(pair[0].y + pair[0].h <= pair[1].y + 0.01, "{w}x{h} {pair:?}");
            }
            assert!((b.map.w / b.map.h - 4.0 / 3.0).abs() < 0.01);
            assert!(b.map.w > 0.0);
        }
    }
    #[test]
    fn cycle_dwells_crossfades_and_skips_unready_views() {
        let mut c = Cycle::default();
        assert_eq!(c.update(0.0, &[], false), None);
        let all = [View::Globe, View::Japan, View::Local];
        let f = c.update(1.0, &all, false).unwrap();
        assert_eq!((f.view, f.previous, f.blend), (View::Globe, None, 0.0));
        assert_eq!(c.update(10.0, &all, false).unwrap().blend, 1.0);
        let f = c.update(16.0, &all, false).unwrap();
        assert_eq!((f.view, f.previous), (View::Japan, Some(View::Globe)));
        let f = c.update(16.4, &all, false).unwrap();
        assert!((f.blend - 0.5).abs() < 0.01 && f.previous == Some(View::Globe));
        assert_eq!(c.update(17.0, &all, false).unwrap().previous, None);
        assert_eq!(c.update(31.0, &all, true).unwrap().view, View::Local);
        // Rain keeps the neighbourhood up for 30 s.
        assert_eq!(c.update(50.0, &all, true).unwrap().view, View::Local);
        assert_eq!(c.update(61.0, &all, true).unwrap().view, View::Globe);
        // A view that stops being ready is replaced immediately.
        let f = c.update(62.0, &[View::Japan], false).unwrap();
        assert_eq!((f.view, f.previous), (View::Japan, Some(View::Globe)));
        // A single ready view stays put.
        assert_eq!(c.update(200.0, &[View::Japan], false).unwrap().view, View::Japan);
        // Photo comes after the neighbourhood when it is ready.
        let mut c = Cycle::default();
        c.update(0.0, &[View::Local, View::Photo], false);
        assert_eq!(c.update(15.0, &[View::Local, View::Photo], false).unwrap().view, View::Photo);
    }
    #[test]
    fn radar_loop_holds_on_now_and_at_the_end() {
        assert_eq!(radar_frame(3.0, 0, 0), None);
        assert_eq!(radar_frame(3.0, 1, 0).unwrap().index, 0);
        let at = |t| radar_frame(t, 13, 6).unwrap();
        assert_eq!(at(0.0).index, 0);
        assert_eq!(at(0.0).blend, 0.0);
        assert_eq!(at(0.6).index, 1);
        assert!((at(0.4).blend - 0.6).abs() < 0.01);
        // Frame 6 starts at 3.0 s and lasts 2.5 s.
        assert_eq!(at(3.1).index, 6);
        assert_eq!(at(5.0).index, 6);
        assert_eq!(at(5.0).blend, 0.0);
        assert_eq!(at(5.6).index, 7);
        // Total: 13 * 0.5 + 2 + 1.5 = 10 s; the last frame fades into the first.
        let last = at(9.9);
        assert_eq!((last.index, last.next), (12, 0));
        assert!(last.blend > 0.5);
        assert_eq!(at(10.1).index, 0);
    }
}

//! Time-of-day and weather mood. Pure: no egui, no clock reads.
pub type Rgb = [u8; 3];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub bg: Rgb,
    pub surface: Rgb,
    pub border: Rgb,
    pub text: Rgb,
    pub dim: Rgb,
    pub accent: Rgb,
}
impl Palette {
    fn channels(self) -> [Rgb; 6] {
        [self.bg, self.surface, self.border, self.text, self.dim, self.accent]
    }
    fn from_channels(c: [Rgb; 6]) -> Self {
        Self {
            bg: c[0],
            surface: c[1],
            border: c[2],
            text: c[3],
            dim: c[4],
            accent: c[5],
        }
    }
    pub fn lerp(self, other: Palette, t: f32) -> Palette {
        let (a, b) = (self.channels(), other.channels());
        Self::from_channels(std::array::from_fn(|i| mix(a[i], b[i], t)))
    }
}
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}
fn desaturate(c: Rgb, t: f32) -> Rgb {
    let luma = (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32).round() as u8;
    mix(c, [luma; 3], t)
}

pub const NIGHT: Palette = Palette {
    bg: [6, 9, 15],
    surface: [12, 18, 28],
    border: [48, 68, 92],
    text: [222, 232, 242],
    dim: [104, 122, 142],
    accent: [96, 196, 255],
};
pub const TWILIGHT: Palette = Palette {
    bg: [12, 11, 24],
    surface: [22, 20, 38],
    border: [76, 70, 112],
    text: [236, 234, 246],
    dim: [128, 122, 152],
    accent: [182, 150, 255],
};
pub const GOLDEN: Palette = Palette {
    bg: [20, 14, 12],
    surface: [32, 24, 20],
    border: [108, 84, 66],
    text: [246, 236, 224],
    dim: [152, 130, 112],
    accent: [255, 168, 84],
};
pub const DAY: Palette = Palette {
    bg: [11, 15, 21],
    surface: [20, 27, 36],
    border: [88, 108, 128],
    text: [236, 242, 247],
    dim: [132, 151, 167],
    accent: [104, 220, 255],
};
const ANCHORS: [(f64, Palette); 4] = [(-12.0, NIGHT), (-3.0, TWILIGHT), (6.0, GOLDEN), (20.0, DAY)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    Clear,
    Cloudy,
    Rain,
    Snow,
}
impl Mood {
    pub fn parse(s: &str) -> Option<Mood> {
        match s {
            "clear" => Some(Mood::Clear),
            "cloudy" => Some(Mood::Cloudy),
            "rain" => Some(Mood::Rain),
            "snow" => Some(Mood::Snow),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Mood::Clear => "clear",
            Mood::Cloudy => "cloudy",
            Mood::Rain => "rain",
            Mood::Snow => "snow",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sun {
    pub elevation_deg: f64,
    pub subsolar_lat: f64,
    pub subsolar_lon: f64,
}
/// Low-precision solar position (Astronomical Almanac), good to ~0.1 degree.
pub fn sun(unix_seconds: f64, lat: f64, lon: f64) -> Sun {
    let n = unix_seconds / 86400.0 + 2440587.5 - 2451545.0;
    let l = (280.460 + 0.9856474 * n).rem_euclid(360.0);
    let g = (357.528 + 0.9856003 * n).rem_euclid(360.0).to_radians();
    let lambda = (l + 1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
    let eps = (23.439 - 0.0000004 * n).to_radians();
    let ra = (eps.cos() * lambda.sin()).atan2(lambda.cos()).to_degrees();
    let dec = (eps.sin() * lambda.sin()).asin();
    let gmst = (280.46061837 + 360.98564736629 * n).rem_euclid(360.0);
    let subsolar_lon = (ra - gmst + 540.0).rem_euclid(360.0) - 180.0;
    let hour_angle = (gmst + lon - ra).to_radians();
    let lat_r = lat.to_radians();
    let elevation = (lat_r.sin() * dec.sin() + lat_r.cos() * dec.cos() * hour_angle.cos()).asin();
    Sun {
        elevation_deg: elevation.to_degrees(),
        subsolar_lat: dec.to_degrees(),
        subsolar_lon,
    }
}
pub fn phase(elevation_deg: f64) -> &'static str {
    if elevation_deg <= -6.0 {
        "night"
    } else if elevation_deg < 0.0 {
        "twilight"
    } else if elevation_deg < 10.0 {
        "golden"
    } else {
        "day"
    }
}
pub fn palette(elevation_deg: f64, mood: Option<Mood>) -> Palette {
    let base = if elevation_deg <= ANCHORS[0].0 {
        ANCHORS[0].1
    } else if elevation_deg >= ANCHORS[3].0 {
        ANCHORS[3].1
    } else {
        let i = ANCHORS.windows(2).position(|w| elevation_deg < w[1].0).unwrap_or(2);
        let (e0, p0) = ANCHORS[i];
        let (e1, p1) = ANCHORS[i + 1];
        p0.lerp(p1, ((elevation_deg - e0) / (e1 - e0)) as f32)
    };
    match mood {
        Some(Mood::Rain) => Palette {
            bg: mix(base.bg, [8, 14, 26], 0.3),
            surface: mix(base.surface, [10, 18, 32], 0.3),
            border: mix(base.border, [52, 82, 120], 0.3),
            accent: mix(base.accent, [90, 160, 255], 0.5),
            ..base
        },
        Some(Mood::Cloudy) => Palette::from_channels(base.channels().map(|c| desaturate(c, 0.4))),
        Some(Mood::Snow) => Palette {
            border: mix(base.border, [200, 215, 235], 0.3),
            accent: mix(base.accent, [200, 215, 235], 0.3),
            ..base
        },
        Some(Mood::Clear) | None => base,
    }
}

/// Eases the shown palette toward a target: about 95% of the way within 60 s.
pub struct Eased {
    current: [[f32; 3]; 6],
    updated: f64,
}
pub const EASE_SECONDS: f64 = 20.0;
impl Eased {
    pub fn new(p: Palette, now: f64) -> Self {
        Self {
            current: p.channels().map(|c| c.map(f32::from)),
            updated: now,
        }
    }
    pub fn step(&mut self, target: Palette, now: f64) -> Palette {
        let dt = (now - self.updated).max(0.0);
        self.updated = now;
        let k = (1.0 - (-dt / EASE_SECONDS).exp()) as f32;
        let target = target.channels();
        for (c, t) in self.current.iter_mut().zip(target) {
            for (v, tv) in c.iter_mut().zip(t) {
                *v += (tv as f32 - *v) * k;
            }
        }
        Palette::from_channels(self.current.map(|c| c.map(|v| v.round().clamp(0.0, 255.0) as u8)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unix(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> f64 {
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
            .and_utc()
            .timestamp() as f64
    }
    #[test]
    fn sun_matches_known_positions() {
        // Tokyo, solar noon on the March equinox: 90 - 35.68 ~= 54.3 degrees.
        let s = sun(unix(2026, 3, 20, 2, 48), 35.68, 139.77);
        assert!((53.6..55.0).contains(&s.elevation_deg), "{s:?}");
        assert!(s.subsolar_lat.abs() < 0.6, "{s:?}");
        assert!((s.subsolar_lon - 139.77).abs() < 2.0, "{s:?}");
        // Local midnight in June is deep night; subsolar latitude near the tropic.
        let night = sun(unix(2026, 6, 21, 15, 0), 35.68, 139.77);
        assert!(night.elevation_deg < -20.0, "{night:?}");
        assert!((night.subsolar_lat - 23.44).abs() < 0.2, "{night:?}");
        // Greenwich noon in June: subsolar longitude near 0.
        let g = sun(unix(2026, 6, 21, 12, 0), 51.48, 0.0);
        assert!(g.subsolar_lon.abs() < 1.0 && (61.0..63.0).contains(&g.elevation_deg), "{g:?}");
    }
    #[test]
    fn phases_and_anchors() {
        assert_eq!(phase(-20.0), "night");
        assert_eq!(phase(-3.0), "twilight");
        assert_eq!(phase(5.0), "golden");
        assert_eq!(phase(40.0), "day");
        assert_eq!(palette(-30.0, None), NIGHT);
        assert_eq!(palette(-3.0, None), TWILIGHT);
        assert_eq!(palette(6.0, None), GOLDEN);
        assert_eq!(palette(80.0, None), DAY);
        assert_eq!(palette(80.0, Some(Mood::Clear)), DAY);
    }
    #[test]
    fn palette_is_continuous_across_elevation() {
        let mut previous = palette(-15.0, None);
        let mut e = -15.0;
        while e < 25.0 {
            e += 0.25;
            let p = palette(e, None);
            for (a, b) in previous.channels().iter().zip(p.channels()) {
                for i in 0..3 {
                    assert!((a[i] as i32 - b[i] as i32).abs() <= 6, "jump at {e}");
                }
            }
            previous = p;
        }
    }
    #[test]
    fn moods_shift_the_palette() {
        let rain = palette(30.0, Some(Mood::Rain));
        assert!(rain.accent[2] >= rain.accent[0] && rain != DAY);
        let cloudy = palette(30.0, Some(Mood::Cloudy));
        let spread = |c: Rgb| c.iter().max().unwrap() - c.iter().min().unwrap();
        assert!(spread(cloudy.accent) < spread(DAY.accent));
        assert_eq!(Mood::parse("snow"), Some(Mood::Snow));
        assert_eq!(Mood::parse("fog"), None);
        assert_eq!(Mood::Rain.name(), "rain");
    }
    #[test]
    fn easing_reaches_target_in_a_minute_and_survives_big_steps() {
        let mut e = Eased::new(NIGHT, 0.0);
        let half = e.step(DAY, 10.0);
        assert!(half != NIGHT && half != DAY);
        let p = e.step(DAY, 60.0);
        for (a, b) in p.channels().iter().zip(DAY.channels()) {
            for i in 0..3 {
                assert!((a[i] as i32 - b[i] as i32).abs() <= 8);
            }
        }
        assert_eq!(e.step(DAY, 1e6), DAY);
        assert_eq!(e.step(DAY, 5.0), DAY, "clock going backwards is a no-op");
    }
}

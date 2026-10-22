//! Time-of-day and weather mood. Pure: no egui, no clock reads.
pub type Rgb = [u8; 3];

/// A painted sky and the colours that sit on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub sky_top: Rgb,
    pub sky_bottom: Rgb,
    /// Light that blooms through the sky and around the photos.
    pub glow: Rgb,
    /// Tint of the glass panels.
    pub panel: Rgb,
    pub text: Rgb,
    pub dim: Rgb,
    pub accent: Rgb,
}
const CHANNELS: usize = 7;
impl Palette {
    fn channels(self) -> [Rgb; CHANNELS] {
        [self.sky_top, self.sky_bottom, self.glow, self.panel, self.text, self.dim, self.accent]
    }
    fn from_channels(c: [Rgb; CHANNELS]) -> Self {
        Self {
            sky_top: c[0],
            sky_bottom: c[1],
            glow: c[2],
            panel: c[3],
            text: c[4],
            dim: c[5],
            accent: c[6],
        }
    }
    pub fn lerp(self, other: Palette, t: f32) -> Palette {
        let (a, b) = (self.channels(), other.channels());
        Self::from_channels(std::array::from_fn(|i| mix(a[i], b[i], t)))
    }
}
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}
fn desaturate(c: Rgb, t: f32) -> Rgb {
    let luma = (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32).round() as u8;
    mix(c, [luma; 3], t)
}

pub const NIGHT: Palette = Palette {
    sky_top: [8, 12, 34],
    sky_bottom: [36, 30, 78],
    glow: [110, 122, 255],
    panel: [10, 14, 38],
    text: [246, 247, 255],
    dim: [180, 188, 218],
    accent: [150, 182, 255],
};
pub const TWILIGHT: Palette = Palette {
    sky_top: [30, 36, 96],
    sky_bottom: [214, 112, 150],
    glow: [255, 150, 182],
    panel: [26, 22, 58],
    text: [255, 250, 252],
    dim: [230, 204, 218],
    accent: [255, 180, 208],
};
pub const GOLDEN: Palette = Palette {
    sky_top: [64, 92, 170],
    sky_bottom: [255, 176, 110],
    glow: [255, 214, 140],
    panel: [40, 34, 66],
    text: [255, 252, 246],
    dim: [244, 224, 204],
    accent: [255, 200, 122],
};
pub const DAY: Palette = Palette {
    sky_top: [40, 116, 212],
    sky_bottom: [156, 206, 246],
    glow: [255, 255, 255],
    panel: [16, 44, 92],
    text: [255, 255, 255],
    dim: [224, 238, 252],
    accent: [255, 234, 150],
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
            sky_top: mix(base.sky_top, [64, 78, 104], 0.5),
            sky_bottom: mix(base.sky_bottom, [118, 134, 156], 0.5),
            glow: mix(base.glow, [170, 192, 224], 0.5),
            accent: mix(base.accent, [150, 204, 255], 0.6),
            ..base
        },
        Some(Mood::Cloudy) => Palette {
            sky_top: desaturate(base.sky_top, 0.45),
            sky_bottom: desaturate(base.sky_bottom, 0.45),
            glow: desaturate(base.glow, 0.3),
            ..base
        },
        Some(Mood::Snow) => Palette {
            sky_top: mix(base.sky_top, [150, 170, 200], 0.3),
            sky_bottom: mix(base.sky_bottom, [214, 224, 240], 0.4),
            glow: mix(base.glow, [235, 242, 255], 0.4),
            ..base
        },
        Some(Mood::Clear) | None => base,
    }
}

/// Eases the shown palette toward a target: about 95% of the way within 60 s.
pub struct Eased {
    current: [[f32; 3]; CHANNELS],
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
        let spread = |c: Rgb| c.iter().max().unwrap() - c.iter().min().unwrap();
        let rain = palette(30.0, Some(Mood::Rain));
        assert!(rain != DAY && spread(rain.sky_top) < spread(DAY.sky_top), "rain greys the sky");
        let cloudy = palette(30.0, Some(Mood::Cloudy));
        assert!(spread(cloudy.sky_bottom) < spread(DAY.sky_bottom));
        assert_eq!(cloudy.text, DAY.text, "text keeps its contrast");
        let snow = palette(30.0, Some(Mood::Snow));
        assert!(snow.sky_bottom[0] > DAY.sky_bottom[0]);
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
                // About 95% of the way: within 5% of the largest possible gap.
                assert!((a[i] as i32 - b[i] as i32).abs() <= 13);
            }
        }
        assert_eq!(e.step(DAY, 1e6), DAY);
        assert_eq!(e.step(DAY, 5.0), DAY, "clock going backwards is a no-op");
    }
}

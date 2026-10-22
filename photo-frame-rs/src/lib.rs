#![forbid(unsafe_code)]

pub mod atlas;
pub mod decode;
pub mod globe;
pub mod hand_diagnostics;
pub mod hand_overlay;
pub mod http;
pub mod hud;
pub mod layout;
pub mod mjpeg;
pub mod navigation;
pub mod playlist;
pub mod status;
pub mod strip;
pub mod strip_paint;
pub mod theme;
pub mod worker;

pub const ADVANCE_SECONDS: f64 = 12.0;
pub const FADE_SECONDS: f64 = 0.9;
pub const STAGGER_SECONDS: f64 = 0.12;
pub const RISE_POINTS: f32 = 14.0;
pub const KEN_BURNS: f32 = 0.045;
pub const SWEEP_SECONDS: f64 = 1.1;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CardMotion {
    pub alpha: f32,
    pub rise: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Timeline {
    pub committed_at: Option<f64>,
}
impl Timeline {
    pub fn commit(&mut self, now: f64) {
        self.committed_at = Some(now);
    }
    pub fn alpha(&self, now: f64) -> f32 {
        self.committed_at
            .map_or(1.0, |t| ((now - t) / FADE_SECONDS).clamp(0.0, 1.0) as f32)
    }
    /// Card `order` of the committed scene: enters `STAGGER_SECONDS` after the
    /// previous one, fading in while rising `RISE_POINTS` into place.
    pub fn card(&self, now: f64, order: usize) -> CardMotion {
        let Some(t) = self.committed_at else {
            return CardMotion {
                alpha: 1.0,
                rise: 0.0,
            };
        };
        let local = ((now - t - order as f64 * STAGGER_SECONDS) / FADE_SECONDS).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - local).powi(3);
        CardMotion {
            alpha: local as f32,
            rise: RISE_POINTS * (1.0 - eased) as f32,
        }
    }
    /// Slow push-in on a shown photo: 1.0 at commit, `KEN_BURNS` by the next advance.
    pub fn push_in(&self, now: f64) -> f32 {
        self.committed_at.map_or(1.0, |t| {
            let p = ((now - t) / ADVANCE_SECONDS).clamp(0.0, 1.0);
            1.0 + KEN_BURNS * (p * (2.0 - p)) as f32
        })
    }
    /// Progress 0..1 of the scan that sweeps the collage after a commit, or None.
    pub fn sweep(&self, now: f64) -> Option<f32> {
        let t = now - self.committed_at?;
        (0.0..SWEEP_SECONDS).contains(&t).then(|| (t / SWEEP_SECONDS) as f32)
    }
    pub fn due(&self, now: f64) -> bool {
        self.committed_at
            .is_some_and(|t| now - t >= ADVANCE_SECONDS)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auto_advance_and_fade_are_commit_based() {
        let mut t = Timeline::default();
        assert!(!t.due(100.0));
        t.commit(3.0);
        assert_eq!(t.alpha(3.0), 0.0);
        assert!((t.alpha(3.45) - 0.5).abs() < 0.001);
        assert_eq!(t.alpha(4.0), 1.0);
        assert!(!t.due(14.99));
        assert!(t.due(15.0));
        // Failed fetch does not commit/reset either timer.
        assert!(t.due(17.0));
        t.commit(17.0);
        assert!(!t.due(18.0));
    }
    #[test]
    fn push_in_and_sweep_follow_the_commit() {
        let mut t = Timeline::default();
        assert_eq!((t.push_in(5.0), t.sweep(5.0)), (1.0, None));
        t.commit(10.0);
        assert_eq!(t.push_in(10.0), 1.0);
        assert!((t.push_in(10.0 + ADVANCE_SECONDS) - (1.0 + KEN_BURNS)).abs() < 1e-6);
        assert!(t.push_in(16.0) > 1.0 + KEN_BURNS / 2.0, "eases out");
        assert_eq!(t.sweep(10.0), Some(0.0));
        assert!((t.sweep(10.55).unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(t.sweep(10.0 + SWEEP_SECONDS + 0.01), None);
    }
    #[test]
    fn cards_enter_one_after_another() {
        let mut t = Timeline::default();
        assert_eq!(t.card(5.0, 3), CardMotion { alpha: 1.0, rise: 0.0 });
        t.commit(10.0);
        let first = t.card(10.0, 0);
        assert_eq!((first.alpha, first.rise), (0.0, RISE_POINTS));
        assert!(t.card(10.23, 2).alpha == 0.0 && t.card(10.3, 2).alpha > 0.0, "third card waits 0.24 s");
        let mid = t.card(10.45, 0);
        assert!((mid.alpha - 0.5).abs() < 0.01 && mid.rise < RISE_POINTS / 2.0, "ease-out rise");
        assert_eq!(t.card(10.9 + 3.0 * STAGGER_SECONDS, 3), CardMotion { alpha: 1.0, rise: 0.0 });
    }
}

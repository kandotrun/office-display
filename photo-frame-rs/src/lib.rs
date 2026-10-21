#![forbid(unsafe_code)]

pub mod decode;
pub mod hand_diagnostics;
pub mod hand_overlay;
pub mod http;
pub mod layout;
pub mod mjpeg;
pub mod navigation;
pub mod playlist;
pub mod status;
pub mod worker;

pub const ADVANCE_SECONDS: f64 = 12.0;
pub const FADE_SECONDS: f64 = 0.9;

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
}

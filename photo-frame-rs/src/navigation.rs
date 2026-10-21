//! Explicit native-only action admission. No queue, RPC/write endpoint or model mutation.
use crate::{
    hand_overlay::{Direction, Swipe},
    worker::Navigation,
};
use std::time::Instant;
pub const MIN_ACTION_SECONDS: f64 = 0.75;
#[derive(Clone, Copy)]
pub struct Availability {
    pub enabled: bool,
    pub source_live: bool,
    pub camera_live: bool,
    pub scene_ready: bool,
    pub busy: bool,
    pub has_previous: bool,
    pub scene_committed: Option<Instant>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Navigate(Navigation),
    Ignore(&'static str),
}
#[derive(Default)]
pub struct Gate {
    epoch: Option<String>,
    counter: u64,
    last_action: Option<f64>,
    pub accepted: u64,
    pub ignored: u64,
}
impl Gate {
    /// First source observation/new epoch is a high-water baseline, never an action.
    pub fn source(&mut self, epoch: Option<&str>, baseline: u64) {
        if self.epoch.as_deref() != epoch {
            self.epoch = epoch.map(str::to_owned);
            self.counter = baseline;
        }
    }
    pub fn offer(&mut self, swipe: &Swipe, now: f64, a: Availability) -> Decision {
        let reason = if self.epoch.as_deref() != Some(swipe.instance_id.as_str()) {
            Some("source_epoch_changed")
        } else if swipe.counter <= self.counter {
            Some("duplicate_event")
        } else {
            // Consume even disabled/busy/stale events: they can never execute later.
            self.counter = swipe.counter;
            if !a.enabled {
                Some("controls_disabled")
            } else if !swipe.fresh() || !now.is_finite() {
                Some("event_stale")
            } else if !a.source_live || !a.camera_live {
                Some("input_unavailable")
            } else if a.busy {
                Some("loading_busy")
            } else if !a.scene_ready {
                Some("photos_not_ready")
            } else if a.scene_committed.is_some_and(|at| swipe.occurred < at) {
                Some("scene_changed")
            } else if self
                .last_action
                .is_some_and(|at| now < at || now - at < MIN_ACTION_SECONDS)
            {
                Some("rate_limited")
            } else if swipe.direction == Direction::ImageRight && !a.has_previous {
                Some("history_start")
            } else {
                None
            }
        };
        if let Some(reason) = reason {
            self.ignored += 1;
            return Decision::Ignore(reason);
        }
        self.last_action = Some(now);
        self.accepted += 1;
        Decision::Navigate(match swipe.direction {
            Direction::ImageLeft => Navigation::Next,
            Direction::ImageRight => Navigation::Previous,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn availability() -> Availability {
        Availability {
            enabled: true,
            source_live: true,
            camera_live: true,
            scene_ready: true,
            busy: false,
            has_previous: true,
            scene_committed: Some(Instant::now() - Duration::from_secs(1)),
        }
    }
    fn swipe(counter: u64, direction: Direction) -> Swipe {
        Swipe {
            direction,
            instance_id: "epoch1".into(),
            counter,
            occurred: Instant::now(),
        }
    }
    fn gate() -> Gate {
        let mut g = Gate::default();
        g.source(Some("epoch1"), 0);
        g
    }
    #[test]
    fn screen_left_next_and_right_previous() {
        let mut g = gate();
        assert_eq!(
            g.offer(&swipe(1, Direction::ImageLeft), 1., availability()),
            Decision::Navigate(Navigation::Next)
        );
        assert_eq!(
            g.offer(&swipe(2, Direction::ImageRight), 2., availability()),
            Decision::Navigate(Navigation::Previous)
        );
    }
    #[test]
    fn duplicate_jump_is_only_one_action_not_counter_steps() {
        let mut g = gate();
        let s = swipe(9, Direction::ImageLeft);
        assert!(matches!(
            g.offer(&s, 1., availability()),
            Decision::Navigate(_)
        ));
        assert_eq!(
            g.offer(&s, 2., availability()),
            Decision::Ignore("duplicate_event")
        );
        assert_eq!(g.accepted, 1);
    }
    #[test]
    fn busy_is_consumed_and_never_queued_for_later() {
        let mut g = gate();
        let s = swipe(1, Direction::ImageLeft);
        let mut a = availability();
        a.busy = true;
        assert_eq!(g.offer(&s, 1., a), Decision::Ignore("loading_busy"));
        assert_eq!(
            g.offer(&s, 2., availability()),
            Decision::Ignore("duplicate_event")
        );
        assert_eq!(g.accepted, 0);
    }
    #[test]
    fn startup_restart_baselines_and_old_epoch_never_replay() {
        let mut g = Gate::default();
        g.source(Some("epoch1"), 7);
        assert_eq!(
            g.offer(&swipe(7, Direction::ImageLeft), 1., availability()),
            Decision::Ignore("duplicate_event")
        );
        g.source(Some("epoch2"), 5);
        assert_eq!(
            g.offer(&swipe(8, Direction::ImageLeft), 2., availability()),
            Decision::Ignore("source_epoch_changed")
        );
        let mut s = swipe(5, Direction::ImageLeft);
        s.instance_id = "epoch2".into();
        assert_eq!(
            g.offer(&s, 3., availability()),
            Decision::Ignore("duplicate_event")
        );
        s.counter = 6;
        assert!(matches!(
            g.offer(&s, 4., availability()),
            Decision::Navigate(_)
        ));
    }
    #[test]
    fn stale_disabled_missing_source_and_scene_change_cannot_act() {
        for reason in [
            "event_stale",
            "controls_disabled",
            "input_unavailable",
            "photos_not_ready",
            "scene_changed",
        ] {
            let mut g = gate();
            let mut s = swipe(1, Direction::ImageLeft);
            let mut a = availability();
            match reason {
                "event_stale" => s.occurred = Instant::now() - Duration::from_millis(501),
                "controls_disabled" => a.enabled = false,
                "input_unavailable" => a.source_live = false,
                "photos_not_ready" => a.scene_ready = false,
                "scene_changed" => {
                    a.scene_committed = Some(Instant::now() + Duration::from_millis(1))
                }
                _ => unreachable!(),
            };
            assert_eq!(g.offer(&s, 1., a), Decision::Ignore(reason));
            assert_eq!(g.accepted, 0);
        }
    }
    #[test]
    fn boundaries_and_tight_repeats_do_not_wrap_or_repeat() {
        let mut g = gate();
        let mut a = availability();
        a.has_previous = false;
        assert_eq!(
            g.offer(&swipe(1, Direction::ImageRight), 1., a),
            Decision::Ignore("history_start")
        );
        assert_eq!(
            g.offer(&swipe(2, Direction::ImageLeft), 2., availability()),
            Decision::Navigate(Navigation::Next)
        );
        assert_eq!(
            g.offer(&swipe(3, Direction::ImageLeft), 2.2, availability()),
            Decision::Ignore("rate_limited")
        );
        assert_eq!(
            g.offer(&swipe(4, Direction::ImageLeft), 2.75, availability()),
            Decision::Navigate(Navigation::Next)
        );
    }
}

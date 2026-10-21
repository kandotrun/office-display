//! Observation-only counters: no input geometry, image, trajectory, person or file history.
//! This never participates in gesture/action admission. Native restart clears everything.
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Interruption {
    pub after_stage: &'static str,
    pub observed_reason: &'static str,
}
#[derive(Clone, Default, Debug, Serialize)]
pub struct Summary {
    pub unique_reports: u64,
    pub source_epochs: u64,
    pub duplicate_reports: u64,
    pub unavailable_notices: u64,
    pub invalid_or_stale_reports: u64,
    pub reordered_reports: u64,
    pub expired_inputs: u64,
    pub state_counts: BTreeMap<&'static str, u64>,
    pub tracking_counts: BTreeMap<&'static str, u64>,
    pub source_event_increases: u64,
    pub fresh_swipes_delivered: u64,
    pub readiness_withheld_reports: u64,
    pub single_hand_reports: u64,
    pub single_hand_input_age_ms_bins: BTreeMap<&'static str, u64>,
    pub quality_rejection_reasons: BTreeMap<&'static str, u64>,
    pub last_active_stage: Option<&'static str>,
    pub last_attempt_outcome: Option<&'static str>,
    pub last_interruption: Option<Interruption>,
}
#[derive(Default)]
pub struct Diagnostics {
    pub summary: Summary,
    active: Option<&'static str>,
}
fn state(value: &str) -> &'static str {
    match value {
        "no_hand" => "no_hand",
        "stabilizing" => "stabilizing",
        "armed_geometric_only" => "armed_geometric_only",
        "horizontal_motion_candidate" => "horizontal_motion_candidate",
        "geometric_swipe_candidate" => "geometric_swipe_candidate",
        "motion_quality_rejected" => "motion_quality_rejected",
        "outside_activation_region" => "outside_activation_region",
        "brief_detection_gap" | "searching_hand" => "searching_hand",
        "searching_gap_expired" => "searching_gap_expired",
        "ambiguous_hand_association" => "ambiguous_hand_association",
        "ambiguous_multiple_hands" => "ambiguous_multiple_hands",
        "hand_quality_rejected" => "hand_quality_rejected",
        "cooldown" => "cooldown",
        "awaiting_hand_withdrawal" => "awaiting_hand_withdrawal",
        "withdrawal_rearmed" => "withdrawal_rearmed",
        "continuity_gap" => "continuity_gap",
        "reordered_or_duplicate_input" => "reordered_or_duplicate_input",
        "invalid_observation" => "invalid_observation",
        "input_unavailable" => "input_unavailable",
        _ => "other",
    }
}
fn tracking(value: &str) -> &'static str {
    match value {
        "untracked" => "untracked",
        "locked" => "locked",
        "searching" => "searching",
        "lost" => "lost",
        "ambiguous" => "ambiguous",
        _ => "other",
    }
}
fn bump(count: &mut u64) {
    *count = count.saturating_add(1);
}
impl Diagnostics {
    pub fn interrupt(&mut self, reason: &'static str) {
        if let Some(after_stage) = self.active.take() {
            self.summary.last_interruption = Some(Interruption {
                after_stage,
                observed_reason: reason,
            });
            self.summary.last_attempt_outcome = Some(reason);
        }
    }
    pub fn epoch(&mut self) {
        self.interrupt("source_restarted");
        bump(&mut self.summary.source_epochs);
    }
    pub fn unavailable(&mut self) {
        bump(&mut self.summary.unavailable_notices);
        self.interrupt("input_unavailable");
    }
    pub fn invalid(&mut self) {
        bump(&mut self.summary.invalid_or_stale_reports);
        self.interrupt("invalid_or_stale_report");
    }
    pub fn reordered(&mut self) {
        bump(&mut self.summary.reordered_reports);
        self.interrupt("reordered_report");
    }
    pub fn expire(&mut self) {
        bump(&mut self.summary.expired_inputs);
        self.interrupt("input_expired");
    }
    pub fn duplicate(&mut self) {
        bump(&mut self.summary.duplicate_reports);
    }
    pub fn source_event(&mut self) {
        bump(&mut self.summary.source_event_increases);
        self.active = None;
        self.summary.last_attempt_outcome = Some("source_event_observed");
    }
    pub fn delivered(&mut self) {
        bump(&mut self.summary.fresh_swipes_delivered);
        self.summary.last_attempt_outcome = Some("fresh_swipe_delivered");
    }
    pub fn withheld_readiness(&mut self) {
        bump(&mut self.summary.readiness_withheld_reports);
    }
    pub fn single_hand_age(&mut self, age_ms: u64) {
        bump(&mut self.summary.single_hand_reports);
        let bin = match age_ms {
            0..=100 => "0_100",
            101..=200 => "101_200",
            201..=250 => "201_250",
            251..=500 => "251_500",
            _ => "over_500",
        };
        bump(
            self.summary
                .single_hand_input_age_ms_bins
                .entry(bin)
                .or_default(),
        );
    }
    pub fn quality_rejection(&mut self, reason: &str) {
        let code = match reason {
            "invalid_palm_geometry" => "invalid_palm_geometry",
            "palm_too_small" => "palm_too_small",
            "palm_near_image_edge" => "palm_near_image_edge",
            "implausible_palm_aspect" => "implausible_palm_aspect",
            "palm_keypoints_inconsistent" => "palm_keypoints_inconsistent",
            "degenerate_palm_keypoints" => "degenerate_palm_keypoints",
            "palm_partial_crop_edge" => "palm_partial_crop_edge",
            "score_below_control_threshold" => "score_below_control_threshold",
            "not_reported" => "not_reported",
            _ => "other",
        };
        bump(
            self.summary
                .quality_rejection_reasons
                .entry(code)
                .or_default(),
        );
    }
    pub fn observe(&mut self, candidate: &str, track: &str) {
        let value = state(candidate);
        bump(&mut self.summary.unique_reports);
        bump(self.summary.state_counts.entry(value).or_default());
        bump(
            self.summary
                .tracking_counts
                .entry(tracking(track))
                .or_default(),
        );
        match value {
            "armed_geometric_only" | "horizontal_motion_candidate" => {
                let phase = if value == "armed_geometric_only" {
                    "armed"
                } else {
                    "motion"
                };
                self.active = Some(phase);
                self.summary.last_active_stage = Some(phase);
            }
            "geometric_swipe_candidate" => {
                self.active = None;
                self.summary.last_attempt_outcome = Some("source_candidate_observed");
            }
            // An actual input's search-only state does not prove an interrupted attempt yet.
            "searching_hand" => {}
            "cooldown" | "awaiting_hand_withdrawal" | "withdrawal_rearmed" => {
                self.active = None;
            }
            _ => self.interrupt(value),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn armed_motion_rejection_remains_after_empty_reports() {
        let mut d = Diagnostics::default();
        d.epoch();
        for s in [
            "stabilizing",
            "armed_geometric_only",
            "horizontal_motion_candidate",
            "motion_quality_rejected",
            "no_hand",
            "no_hand",
        ] {
            d.observe(s, "locked");
        }
        assert_eq!(d.summary.unique_reports, 6);
        assert_eq!(
            d.summary.last_interruption,
            Some(Interruption {
                after_stage: "motion",
                observed_reason: "motion_quality_rejected"
            })
        );
        assert_eq!(
            d.summary.last_attempt_outcome,
            Some("motion_quality_rejected")
        );
        assert_eq!(d.summary.state_counts["no_hand"], 2);
    }
    #[test]
    fn brief_search_is_not_a_fake_motion_or_interruption() {
        let mut d = Diagnostics::default();
        for s in [
            "armed_geometric_only",
            "horizontal_motion_candidate",
            "searching_hand",
            "horizontal_motion_candidate",
            "geometric_swipe_candidate",
        ] {
            d.observe(s, "locked");
        }
        assert_eq!(d.summary.state_counts["horizontal_motion_candidate"], 2);
        assert!(d.summary.last_interruption.is_none());
        assert_eq!(
            d.summary.last_attempt_outcome,
            Some("source_candidate_observed")
        );
    }
    #[test]
    fn loss_and_reacquisition_do_not_overwrite_last_interruption() {
        let mut d = Diagnostics::default();
        for s in ["armed_geometric_only", "no_hand", "stabilizing", "no_hand"] {
            d.observe(s, "untracked");
        }
        assert_eq!(
            d.summary.last_interruption.as_ref().unwrap().after_stage,
            "armed"
        );
        assert_eq!(d.summary.last_attempt_outcome, Some("no_hand"));
    }
    #[test]
    fn unavailable_expired_reordered_and_epoch_end_only_active_attempts() {
        let mut d = Diagnostics::default();
        d.observe("armed_geometric_only", "locked");
        d.unavailable();
        assert_eq!(d.summary.last_attempt_outcome, Some("input_unavailable"));
        d.expire();
        d.invalid();
        d.reordered();
        d.epoch();
        assert_eq!(d.summary.last_attempt_outcome, Some("input_unavailable"));
        d.observe("horizontal_motion_candidate", "locked");
        d.epoch();
        assert_eq!(d.summary.last_attempt_outcome, Some("source_restarted"));
        assert_eq!(d.summary.source_epochs, 2);
    }
    #[test]
    fn keys_and_serialization_are_bounded_and_do_not_include_geometry() {
        let mut d = Diagnostics::default();
        for n in 0..10000 {
            d.observe(&format!("untrusted-{n}"), &format!("track-{n}"));
        }
        assert_eq!(d.summary.state_counts.len(), 1);
        assert_eq!(d.summary.tracking_counts.len(), 1);
        let json = serde_json::to_string(&d.summary).unwrap();
        assert!(json.len() < 1024);
        for forbidden in [
            "untrusted-",
            "track-",
            "bbox",
            "keypoint",
            "centroid",
            "image",
            "instance_id",
        ] {
            assert!(!json.contains(forbidden));
        }
    }
    #[test]
    fn counts_saturate_and_events_remain_observations_not_actions() {
        let mut d = Diagnostics::default();
        d.summary.unique_reports = u64::MAX;
        d.summary.state_counts.insert("no_hand", u64::MAX);
        d.observe("no_hand", "untracked");
        d.duplicate();
        d.source_event();
        d.delivered();
        assert_eq!(d.summary.unique_reports, u64::MAX);
        assert_eq!(d.summary.state_counts["no_hand"], u64::MAX);
        assert_eq!(d.summary.source_event_increases, 1);
        assert_eq!(d.summary.fresh_swipes_delivered, 1);
    }
    #[test]
    fn age_bins_and_quality_codes_are_bounded_without_retaining_raw_values() {
        let mut d = Diagnostics::default();
        for n in [0, 100, 101, 200, 201, 250, 251, 500, 501, u64::MAX] {
            d.single_hand_age(n);
        }
        for n in 0..1000 {
            d.quality_rejection(&format!("raw-{n}"));
        }
        d.quality_rejection("palm_too_small");
        assert_eq!(d.summary.single_hand_input_age_ms_bins.len(), 5);
        assert_eq!(d.summary.single_hand_input_age_ms_bins["201_250"], 2);
        assert_eq!(d.summary.single_hand_reports, 10);
        assert_eq!(d.summary.quality_rejection_reasons.len(), 2);
        assert_eq!(d.summary.quality_rejection_reasons["other"], 1000);
        let text = serde_json::to_string(&d.summary).unwrap();
        assert!(!text.contains("raw-"));
        assert!(text.len() < 1400);
    }
}

//! Tune agent and reflex thresholds from outcomes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    pub agent_min_score: f64,
    pub reflex_min_score: f64,
}

/// Nudge thresholds up after success streaks, down after failures.
#[must_use]
pub fn tune(current: Thresholds, success_rate: f64) -> Thresholds {
    if success_rate >= 0.9 {
        Thresholds {
            agent_min_score: (current.agent_min_score + 0.02).min(0.95),
            reflex_min_score: (current.reflex_min_score + 0.02).min(0.95),
        }
    } else if success_rate < 0.6 {
        Thresholds {
            agent_min_score: (current.agent_min_score - 0.05).max(0.3),
            reflex_min_score: (current.reflex_min_score - 0.05).max(0.3),
        }
    } else {
        current
    }
}

#[cfg(test)]
mod zc_threshold_tuning_tests {
    use super::*;

    #[test]
    fn zc_threshold_tuning_moves_with_outcomes() {
        let base = Thresholds {
            agent_min_score: 0.5,
            reflex_min_score: 0.5,
        };
        assert!(tune(base, 0.95).agent_min_score > base.agent_min_score);
        assert!(tune(base, 0.4).agent_min_score < base.agent_min_score);
    }

    #[test]
    fn tune_keeps_thresholds_unchanged_in_neutral_band() {
        let base = Thresholds {
            agent_min_score: 0.65,
            reflex_min_score: 0.72,
        };
        assert_eq!(tune(base, 0.6), base);
        assert_eq!(tune(base, 0.75), base);
        assert_eq!(tune(base, 0.89), base);
    }

    #[test]
    fn tune_clamps_at_upper_and_lower_bounds() {
        let near_ceiling = Thresholds {
            agent_min_score: 0.94,
            reflex_min_score: 0.95,
        };
        let high = tune(near_ceiling, 0.95);
        assert!((high.agent_min_score - 0.95).abs() < 1e-9);
        assert!((high.reflex_min_score - 0.95).abs() < 1e-9);

        let near_floor = Thresholds {
            agent_min_score: 0.32,
            reflex_min_score: 0.30,
        };
        let low = tune(near_floor, 0.2);
        assert!((low.agent_min_score - 0.30).abs() < 1e-9);
        assert!((low.reflex_min_score - 0.30).abs() < 1e-9);
    }

    #[test]
    fn thresholds_serde_json_roundtrip() {
        let orig = Thresholds {
            agent_min_score: 0.75,
            reflex_min_score: 0.60,
        };
        let json = serde_json::to_string(&orig).expect("serialize");
        let back: Thresholds = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, orig);
    }
}

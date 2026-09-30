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
}

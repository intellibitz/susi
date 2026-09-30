//! Zero-config scorecard for `susi os`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualStep {
    pub description: String,
    pub closing_task: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZcScorecard {
    pub debt: u32,
    pub score: f64,
    pub delta_since_release: f64,
    pub remaining: Vec<ManualStep>,
}

/// Build a scorecard from remaining manual steps and prior release score.
#[must_use]
pub fn scorecard(remaining: Vec<ManualStep>, prior_score: f64) -> ZcScorecard {
    let debt = remaining.len() as u32;
    let score = (100.0 - f64::from(debt) * 5.0).clamp(0.0, 100.0);
    ZcScorecard {
        debt,
        score,
        delta_since_release: score - prior_score,
        remaining,
    }
}

#[cfg(test)]
mod zc_scorecard_tests {
    use super::*;

    #[test]
    fn zc_scorecard_tracks_debt_and_delta() {
        let s = scorecard(
            vec![ManualStep {
                description: "set cloud key".into(),
                closing_task: "T-CLAUDE-175".into(),
            }],
            90.0,
        );
        assert_eq!(s.debt, 1);
        assert_eq!(s.score, 95.0);
        assert!((s.delta_since_release - 5.0).abs() < f64::EPSILON);
        assert_eq!(s.remaining[0].closing_task, "T-CLAUDE-175");
    }
}

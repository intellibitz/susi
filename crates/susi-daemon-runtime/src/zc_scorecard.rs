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

    #[test]
    fn zc_scorecard_zero_manual_steps_yields_perfect_hundred_score() {
        let s = scorecard(Vec::new(), 100.0);
        assert_eq!(s.debt, 0);
        assert_eq!(s.score, 100.0);
        assert!((s.delta_since_release - 0.0).abs() < f64::EPSILON);
        assert!(s.remaining.is_empty());
    }

    #[test]
    fn zc_scorecard_clamps_large_debt_at_zero() {
        let steps: Vec<ManualStep> = (0..25)
            .map(|i| ManualStep {
                description: format!("step-{i}"),
                closing_task: format!("T-{i}"),
            })
            .collect();
        let s = scorecard(steps, 50.0);
        assert_eq!(s.debt, 25);
        assert_eq!(s.score, 0.0);
        assert!((s.delta_since_release - (-50.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn zc_scorecard_computes_negative_delta_when_score_regresses() {
        let s = scorecard(
            vec![
                ManualStep {
                    description: "step-a".into(),
                    closing_task: "T-A".into(),
                },
                ManualStep {
                    description: "step-b".into(),
                    closing_task: "T-B".into(),
                },
            ],
            95.0,
        );
        assert_eq!(s.debt, 2);
        assert_eq!(s.score, 90.0);
        assert!((s.delta_since_release - (-5.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn zc_scorecard_serde_json_roundtrip_preserves_remaining_steps() {
        let card = scorecard(
            vec![ManualStep {
                description: "configure gpu".into(),
                closing_task: "T-GPU-1".into(),
            }],
            85.0,
        );
        let json = serde_json::to_string(&card).expect("serialize ZcScorecard");
        let restored: ZcScorecard = serde_json::from_str(&json).expect("deserialize ZcScorecard");
        assert_eq!(restored, card);
    }
}

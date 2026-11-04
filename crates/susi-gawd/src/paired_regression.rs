//! Paired statistical regression decisions (VC-201-006).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegressionDecision {
    Improved,
    Regressed,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairedTrial {
    pub baseline: f64,
    pub candidate: f64,
}

/// Decide from paired deltas. Undersampled or noisy results are inconclusive.
#[must_use]
pub fn decide(trials: &[PairedTrial], min_samples: usize, confidence: f64) -> RegressionDecision {
    if trials.len() < min_samples {
        return RegressionDecision::Inconclusive;
    }
    let deltas: Vec<f64> = trials.iter().map(|t| t.candidate - t.baseline).collect();
    let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
    let var = deltas.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / deltas.len() as f64;
    let stderr = (var / deltas.len() as f64).sqrt();
    if stderr == 0.0 {
        return if mean > 0.0 {
            RegressionDecision::Improved
        } else if mean < 0.0 {
            RegressionDecision::Regressed
        } else {
            RegressionDecision::Inconclusive
        };
    }
    // Rough z-threshold from confidence (e.g. 0.95 → ~1.96).
    let z = if confidence >= 0.99 {
        2.58
    } else if confidence >= 0.95 {
        1.96
    } else {
        1.64
    };
    let margin = z * stderr;
    if mean - margin > 0.0 {
        RegressionDecision::Improved
    } else if mean + margin < 0.0 {
        RegressionDecision::Regressed
    } else {
        RegressionDecision::Inconclusive
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn decide_returns_inconclusive_when_trials_below_minimum_samples() {
        let trials = vec![
            PairedTrial {
                baseline: 10.0,
                candidate: 20.0,
            },
            PairedTrial {
                baseline: 10.0,
                candidate: 20.0,
            },
        ];
        assert_eq!(decide(&trials, 3, 0.95), RegressionDecision::Inconclusive);
        assert_eq!(decide(&[], 1, 0.95), RegressionDecision::Inconclusive);
    }

    #[test]
    fn decide_handles_zero_variance_improved_regressed_and_zero_delta() {
        let improved = vec![
            PairedTrial {
                baseline: 1.0,
                candidate: 2.0,
            },
            PairedTrial {
                baseline: 3.0,
                candidate: 4.0,
            },
        ];
        assert_eq!(decide(&improved, 2, 0.95), RegressionDecision::Improved);

        let regressed = vec![
            PairedTrial {
                baseline: 5.0,
                candidate: 3.0,
            },
            PairedTrial {
                baseline: 4.0,
                candidate: 2.0,
            },
        ];
        assert_eq!(decide(&regressed, 2, 0.95), RegressionDecision::Regressed);

        let tie = vec![
            PairedTrial {
                baseline: 2.0,
                candidate: 2.0,
            },
            PairedTrial {
                baseline: 5.0,
                candidate: 5.0,
            },
        ];
        assert_eq!(decide(&tie, 2, 0.95), RegressionDecision::Inconclusive);
    }

    #[test]
    fn decide_identifies_statistically_significant_improvement_and_regression() {
        let improved = vec![
            PairedTrial {
                baseline: 10.0,
                candidate: 15.0,
            },
            PairedTrial {
                baseline: 10.0,
                candidate: 16.0,
            },
            PairedTrial {
                baseline: 10.0,
                candidate: 14.5,
            },
            PairedTrial {
                baseline: 10.0,
                candidate: 15.5,
            },
        ];
        assert_eq!(decide(&improved, 4, 0.95), RegressionDecision::Improved);
        assert_eq!(decide(&improved, 4, 0.99), RegressionDecision::Improved);

        let regressed: Vec<PairedTrial> = improved
            .iter()
            .map(|t| PairedTrial {
                baseline: t.candidate,
                candidate: t.baseline,
            })
            .collect();
        assert_eq!(decide(&regressed, 4, 0.99), RegressionDecision::Regressed);
    }

    #[test]
    fn decide_returns_inconclusive_when_variance_exceeds_confidence_margin() {
        let noisy = vec![
            PairedTrial {
                baseline: 10.0,
                candidate: 15.0,
            },
            PairedTrial {
                baseline: 10.0,
                candidate: 5.5,
            },
            PairedTrial {
                baseline: 10.0,
                candidate: 14.0,
            },
            PairedTrial {
                baseline: 10.0,
                candidate: 6.0,
            },
        ];
        assert_eq!(decide(&noisy, 4, 0.90), RegressionDecision::Inconclusive);
        assert_eq!(decide(&noisy, 4, 0.95), RegressionDecision::Inconclusive);
    }

    #[test]
    fn paired_trial_and_regression_decision_round_trip_serde_json() {
        let trial = PairedTrial {
            baseline: 12.5,
            candidate: 14.0,
        };
        let encoded = serde_json::to_string(&trial).expect("serialize PairedTrial");
        let decoded: PairedTrial = serde_json::from_str(&encoded).expect("deserialize PairedTrial");
        assert_eq!(decoded, trial);

        for decision in [
            RegressionDecision::Improved,
            RegressionDecision::Regressed,
            RegressionDecision::Inconclusive,
        ] {
            let json = serde_json::to_string(&decision).expect("serialize RegressionDecision");
            let back: RegressionDecision =
                serde_json::from_str(&json).expect("deserialize RegressionDecision");
            assert_eq!(back, decision);
        }
    }
}

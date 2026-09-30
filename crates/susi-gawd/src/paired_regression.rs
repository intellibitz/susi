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

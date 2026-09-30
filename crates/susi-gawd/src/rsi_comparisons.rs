//! Evidence-backed RSI experiment comparisons (VC-201-010).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialOutcome {
    Success,
    Failed,
    Inconclusive,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentComparison {
    pub candidate_id: String,
    pub baseline_id: String,
    pub metric_delta: f64,
    pub receipt: String,
    pub outcome: TrialOutcome,
}

/// Machine-readable comparison lines; failed/inconclusive/rejected stay visible.
#[must_use]
pub fn publish(rows: &[ExperimentComparison]) -> String {
    let mut out = String::from("rsi_comparisons:\n");
    for r in rows {
        out.push_str(&format!(
            "- {} vs {} delta={:.4} outcome={:?} receipt={}\n",
            r.candidate_id, r.baseline_id, r.metric_delta, r.outcome, r.receipt
        ));
    }
    out
}

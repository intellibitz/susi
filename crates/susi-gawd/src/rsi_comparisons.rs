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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn publish_renders_empty_header_when_no_rows_supplied() {
        assert_eq!(publish(&[]), "rsi_comparisons:\n");
    }

    #[test]
    fn publish_includes_all_trial_outcomes_with_formatted_deltas() {
        let rows = vec![
            ExperimentComparison {
                candidate_id: "cand-1".to_string(),
                baseline_id: "base-0".to_string(),
                metric_delta: 0.125,
                receipt: "rcpt-a".to_string(),
                outcome: TrialOutcome::Success,
            },
            ExperimentComparison {
                candidate_id: "cand-2".to_string(),
                baseline_id: "base-0".to_string(),
                metric_delta: -0.05,
                receipt: "rcpt-b".to_string(),
                outcome: TrialOutcome::Rejected,
            },
        ];
        let text = publish(&rows);
        assert!(text.contains("- cand-1 vs base-0 delta=0.1250 outcome=Success receipt=rcpt-a"));
        assert!(text.contains("- cand-2 vs base-0 delta=-0.0500 outcome=Rejected receipt=rcpt-b"));
    }

    #[test]
    fn experiment_comparison_serde_roundtrip_preserves_fields() {
        let cmp = ExperimentComparison {
            candidate_id: "c1".to_string(),
            baseline_id: "b1".to_string(),
            metric_delta: 0.42,
            receipt: "r1".to_string(),
            outcome: TrialOutcome::Inconclusive,
        };
        let json = serde_json::to_string(&cmp).unwrap();
        assert!(json.contains(r#""outcome":"inconclusive""#));
        let back: ExperimentComparison = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cmp);
    }

    #[test]
    fn trial_outcome_variants_serialize_to_expected_snake_case_strings() {
        assert_eq!(
            serde_json::to_string(&TrialOutcome::Success).unwrap(),
            r#""success""#
        );
        assert_eq!(
            serde_json::to_string(&TrialOutcome::Failed).unwrap(),
            r#""failed""#
        );
        assert_eq!(
            serde_json::to_string(&TrialOutcome::Inconclusive).unwrap(),
            r#""inconclusive""#
        );
        assert_eq!(
            serde_json::to_string(&TrialOutcome::Rejected).unwrap(),
            r#""rejected""#
        );
    }
}

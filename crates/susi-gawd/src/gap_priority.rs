//! Prioritize capability gaps by observed impact (VC-201-012).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GapProposal {
    pub pattern: String,
    pub frequency: u64,
    pub impact: f64,
    pub eval_cost: f64,
    pub receipts: Vec<String>,
}

impl GapProposal {
    #[must_use]
    pub fn score(&self) -> f64 {
        if self.eval_cost <= 0.0 {
            self.frequency as f64 * self.impact
        } else {
            (self.frequency as f64 * self.impact) / self.eval_cost
        }
    }
}

/// Deduplicate by pattern; keep highest-frequency aggregate with merged receipts.
#[must_use]
pub fn prioritize(failures: &[(String, f64, String)]) -> Vec<GapProposal> {
    let mut map: BTreeMap<String, GapProposal> = BTreeMap::new();
    for (pattern, impact, receipt) in failures {
        let e = map.entry(pattern.clone()).or_insert(GapProposal {
            pattern: pattern.clone(),
            frequency: 0,
            impact: *impact,
            eval_cost: 1.0,
            receipts: Vec::new(),
        });
        e.frequency += 1;
        e.impact = e.impact.max(*impact);
        if !e.receipts.contains(receipt) {
            e.receipts.push(receipt.clone());
        }
    }
    let mut out: Vec<_> = map.into_values().collect();
    out.sort_by(|a, b| {
        b.score()
            .partial_cmp(&a.score())
            .unwrap_or(std::cmp::Ordering::Less)
    });
    out
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn gap_proposal_score_divides_by_positive_eval_cost_and_handles_non_positive_cost() {
        let normal = GapProposal {
            pattern: "p1".to_string(),
            frequency: 4,
            impact: 3.0,
            eval_cost: 2.0,
            receipts: Vec::new(),
        };
        assert!((normal.score() - 6.0).abs() < 1e-9);

        let zero_cost = GapProposal {
            eval_cost: 0.0,
            ..normal.clone()
        };
        assert!((zero_cost.score() - 12.0).abs() < 1e-9);
    }

    #[test]
    fn prioritize_deduplicates_patterns_accumulates_frequency_and_merges_unique_receipts() {
        let failures = vec![
            ("timeout".to_string(), 2.0, "r-1".to_string()),
            ("timeout".to_string(), 5.0, "r-1".to_string()),
            ("timeout".to_string(), 3.0, "r-2".to_string()),
        ];
        let ranked = prioritize(&failures);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].pattern, "timeout");
        assert_eq!(ranked[0].frequency, 3);
        assert!((ranked[0].impact - 5.0).abs() < 1e-9);
        assert_eq!(
            ranked[0].receipts,
            vec!["r-1".to_string(), "r-2".to_string()]
        );
    }

    #[test]
    fn prioritize_retains_maximum_impact_and_sorts_descending_by_score() {
        let failures = vec![
            ("low-gap".to_string(), 1.5, "r-low".to_string()),
            ("high-gap".to_string(), 4.0, "r-high-1".to_string()),
            ("high-gap".to_string(), 6.0, "r-high-2".to_string()),
        ];
        let ranked = prioritize(&failures);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].pattern, "high-gap");
        assert!((ranked[0].score() - 12.0).abs() < 1e-9);
        assert_eq!(ranked[1].pattern, "low-gap");
    }

    #[test]
    fn prioritize_on_empty_slice_returns_empty_vector() {
        let ranked = prioritize(&[]);
        assert!(ranked.is_empty());
    }
}

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

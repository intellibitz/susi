//! Benchmark regression gate for release cuts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchSample {
    pub name: String,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RegressionDecision {
    Pass,
    Fail {
        name: String,
        baseline: f64,
        candidate: f64,
    },
}

/// Fail when candidate is worse than baseline by more than `max_regression_pct`.
/// Higher-is-better metrics: candidate < baseline * (1 - pct).
#[must_use]
pub fn perf_regression_gate(
    baseline: &[BenchSample],
    candidate: &[BenchSample],
    max_regression_pct: f64,
) -> Vec<RegressionDecision> {
    let mut out = Vec::new();
    for b in baseline {
        let Some(c) = candidate.iter().find(|x| x.name == b.name) else {
            out.push(RegressionDecision::Fail {
                name: b.name.clone(),
                baseline: b.value,
                candidate: 0.0,
            });
            continue;
        };
        let floor = b.value * (1.0 - max_regression_pct / 100.0);
        if c.value + f64::EPSILON < floor {
            out.push(RegressionDecision::Fail {
                name: b.name.clone(),
                baseline: b.value,
                candidate: c.value,
            });
        } else {
            out.push(RegressionDecision::Pass);
        }
    }
    out
}

#[must_use]
pub fn gate_blocks_release(decisions: &[RegressionDecision]) -> bool {
    decisions
        .iter()
        .any(|d| matches!(d, RegressionDecision::Fail { .. }))
}

#[cfg(test)]
mod perf_regression_gate_tests {
    use super::*;

    #[test]
    fn perf_regression_gate_blocks_drop_beyond_threshold() {
        let base = vec![BenchSample {
            name: "tok_s".into(),
            value: 100.0,
        }];
        let cand = vec![BenchSample {
            name: "tok_s".into(),
            value: 80.0,
        }];
        let d = perf_regression_gate(&base, &cand, 10.0);
        assert!(gate_blocks_release(&d));
        let ok = perf_regression_gate(
            &base,
            &[BenchSample {
                name: "tok_s".into(),
                value: 95.0,
            }],
            10.0,
        );
        assert!(!gate_blocks_release(&ok));
    }
}

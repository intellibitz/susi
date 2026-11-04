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

    #[test]
    fn perf_regression_gate_empty_baseline_passes_and_missing_candidate_fails() {
        let empty = perf_regression_gate(&[], &[], 10.0);
        assert!(empty.is_empty());
        assert!(!gate_blocks_release(&empty));

        let base = vec![BenchSample {
            name: "latency_p99".into(),
            value: 50.0,
        }];
        let missing = perf_regression_gate(&base, &[], 10.0);
        assert_eq!(
            missing,
            vec![RegressionDecision::Fail {
                name: "latency_p99".into(),
                baseline: 50.0,
                candidate: 0.0,
            }]
        );
        assert!(gate_blocks_release(&missing));
    }

    #[test]
    fn perf_regression_gate_passes_when_candidate_improves_over_baseline() {
        let base = vec![BenchSample {
            name: "tok_s".into(),
            value: 100.0,
        }];
        let improved = vec![BenchSample {
            name: "tok_s".into(),
            value: 120.0,
        }];
        let decisions = perf_regression_gate(&base, &improved, 5.0);
        assert_eq!(decisions, vec![RegressionDecision::Pass]);
        assert!(!gate_blocks_release(&decisions));
    }

    #[test]
    fn perf_regression_gate_accepts_exact_threshold_boundary() {
        let base = vec![BenchSample {
            name: "tok_s".into(),
            value: 100.0,
        }];
        let boundary = vec![BenchSample {
            name: "tok_s".into(),
            value: 90.0,
        }];
        let decisions = perf_regression_gate(&base, &boundary, 10.0);
        assert_eq!(decisions, vec![RegressionDecision::Pass]);
        assert!(!gate_blocks_release(&decisions));
    }

    #[test]
    fn bench_sample_round_trips_serde_json_and_multiple_metrics_preserve_order() {
        let sample = BenchSample {
            name: "qps".into(),
            value: 250.5,
        };
        let encoded = serde_json::to_string(&sample).expect("serialize BenchSample");
        let decoded: BenchSample = serde_json::from_str(&encoded).expect("deserialize BenchSample");
        assert_eq!(decoded, sample);

        let base = vec![
            BenchSample {
                name: "m1".into(),
                value: 100.0,
            },
            BenchSample {
                name: "m2".into(),
                value: 100.0,
            },
        ];
        let cand = vec![
            BenchSample {
                name: "m1".into(),
                value: 98.0,
            },
            BenchSample {
                name: "m2".into(),
                value: 70.0,
            },
        ];
        let decisions = perf_regression_gate(&base, &cand, 5.0);
        assert_eq!(
            decisions,
            vec![
                RegressionDecision::Pass,
                RegressionDecision::Fail {
                    name: "m2".into(),
                    baseline: 100.0,
                    candidate: 70.0,
                }
            ]
        );
        assert!(gate_blocks_release(&decisions));
    }
}

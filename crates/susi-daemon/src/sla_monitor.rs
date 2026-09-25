//! SLA Monitoring (Swarm OS Bullet 75)
//!
//! Checks a `swarm_metrics::SwarmMetrics` snapshot (Bullet 30) against
//! configured latency and success-rate targets and, on a miss, builds an
//! `sla.violation` Observation pheromone — the same blackboard-alert
//! pattern `runtime_admin`'s hardware watchdog (Bullet 28) uses, so a
//! violation is actually observable by anything watching the blackboard
//! rather than just computed and discarded.

use serde::Serialize;
use susi_abi::swarm::{PheromoneKind, SwarmPheromone};

use crate::swarm_metrics::SwarmMetricsSnapshot;

#[derive(Debug, Clone, Copy, Default)]
pub struct SlaTargets {
    pub max_avg_resolution_ms: Option<u64>,
    pub min_success_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum SlaViolation {
    LatencyExceeded { target_ms: u64, actual_ms: u64 },
    SuccessRateBelowTarget { target: f64, actual: f64 },
}

/// Evaluates `snapshot` against `targets`, returning every violation
/// found. A target left `None` is never checked (no target, no miss).
pub fn check_sla(snapshot: &SwarmMetricsSnapshot, targets: &SlaTargets) -> Vec<SlaViolation> {
    let mut violations = Vec::new();

    if let Some(max_ms) = targets.max_avg_resolution_ms
        && snapshot.avg_time_to_resolution_ms > max_ms
    {
        violations.push(SlaViolation::LatencyExceeded {
            target_ms: max_ms,
            actual_ms: snapshot.avg_time_to_resolution_ms,
        });
    }

    if let Some(min_rate) = targets.min_success_rate
        && snapshot.missions_completed > 0
    {
        let actual = snapshot.missions_succeeded as f64 / snapshot.missions_completed as f64;
        if actual < min_rate {
            violations.push(SlaViolation::SuccessRateBelowTarget {
                target: min_rate,
                actual,
            });
        }
    }

    violations
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Builds an Observation pheromone summarizing `violations`, or `None`
/// when there's nothing to report — a clean SLA check deposits nothing
/// rather than a noisy "all clear" every cycle.
pub fn violations_pheromone(violations: &[SlaViolation]) -> Option<SwarmPheromone> {
    if violations.is_empty() {
        return None;
    }
    let ts = now_secs();
    Some(SwarmPheromone {
        id: format!("sla-violation-{ts}"),
        topic: "sla.violation".to_string(),
        emitter_id: "susi-sla-monitor".to_string(),
        kind: PheromoneKind::Observation,
        intensity: 1.0,
        payload: serde_json::to_value(violations).unwrap_or_else(|_| serde_json::json!({})),
        ttl_ms: 300_000,
        deposited_at: ts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(completed: usize, succeeded: usize, avg_ms: u64) -> SwarmMetricsSnapshot {
        SwarmMetricsSnapshot {
            missions_started: completed,
            missions_completed: completed,
            missions_succeeded: succeeded,
            distinct_solutions: 0,
            avg_time_to_resolution_ms: avg_ms,
        }
    }

    #[test]
    fn no_targets_configured_never_violates() {
        let snap = snapshot(10, 0, 999_999);
        assert!(check_sla(&snap, &SlaTargets::default()).is_empty());
    }

    #[test]
    fn latency_violation_is_reported_when_exceeded() {
        let snap = snapshot(10, 10, 5000);
        let targets = SlaTargets {
            max_avg_resolution_ms: Some(1000),
            min_success_rate: None,
        };
        let violations = check_sla(&snap, &targets);
        assert_eq!(
            violations,
            vec![SlaViolation::LatencyExceeded {
                target_ms: 1000,
                actual_ms: 5000
            }]
        );
    }

    #[test]
    fn success_rate_violation_is_reported_when_below_target() {
        let snap = snapshot(10, 5, 0); // 50% success
        let targets = SlaTargets {
            max_avg_resolution_ms: None,
            min_success_rate: Some(0.9),
        };
        let violations = check_sla(&snap, &targets);
        assert_eq!(
            violations,
            vec![SlaViolation::SuccessRateBelowTarget {
                target: 0.9,
                actual: 0.5
            }]
        );
    }

    #[test]
    fn meeting_both_targets_reports_nothing() {
        let snap = snapshot(10, 10, 500);
        let targets = SlaTargets {
            max_avg_resolution_ms: Some(1000),
            min_success_rate: Some(0.9),
        };
        assert!(check_sla(&snap, &targets).is_empty());
    }

    #[test]
    fn clean_snapshot_deposits_no_pheromone() {
        assert!(violations_pheromone(&[]).is_none());
    }

    #[test]
    fn violations_produce_an_observable_pheromone() {
        let violations = vec![SlaViolation::LatencyExceeded {
            target_ms: 100,
            actual_ms: 200,
        }];
        let pheromone = violations_pheromone(&violations).unwrap();
        assert_eq!(pheromone.topic, "sla.violation");
        assert_eq!(pheromone.kind, PheromoneKind::Observation);
    }
}

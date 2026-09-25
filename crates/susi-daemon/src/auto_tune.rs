//! Auto-Tune (Swarm OS Bullet 74)
//!
//! Recommends a one-step concurrency change from a swarm-metrics snapshot
//! (Bullet 30) checked against SLA targets (Bullet 75), then applies it
//! through `ElasticScheduler` (Bullet 28). The advice is derived only from
//! those measurements — no invented latency model.

use crate::elastic_scheduler::ElasticScheduler;
use crate::sla_monitor::{SlaTargets, check_sla};
use crate::swarm_metrics::SwarmMetricsSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuneDirection {
    DecreaseConcurrency,
    IncreaseConcurrency,
    Hold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TuneAdvice {
    pub direction: TuneDirection,
    pub reason: &'static str,
}

/// One-step advice. An SLA miss always decreases concurrency. A fully
/// successful completed sample with no miss increases it. Anything else
/// (no completions yet, or successes mixed with failures that still meet
/// the configured targets) holds.
pub fn advise(snapshot: &SwarmMetricsSnapshot, targets: &SlaTargets) -> TuneAdvice {
    if !check_sla(snapshot, targets).is_empty() {
        return TuneAdvice {
            direction: TuneDirection::DecreaseConcurrency,
            reason: "sla miss: reduce concurrency one step",
        };
    }
    if snapshot.missions_completed == 0 {
        return TuneAdvice {
            direction: TuneDirection::Hold,
            reason: "no completed missions",
        };
    }
    if snapshot.missions_succeeded == snapshot.missions_completed {
        return TuneAdvice {
            direction: TuneDirection::IncreaseConcurrency,
            reason: "sla held on a fully successful sample: raise concurrency one step",
        };
    }
    TuneAdvice {
        direction: TuneDirection::Hold,
        reason: "sla held, but not every mission succeeded",
    }
}

/// Applies `advise` to `scheduler` and returns the resulting concurrency
/// target. The scheduler's existing floor and ceiling still bound the step.
pub fn apply_tune(
    scheduler: &ElasticScheduler,
    snapshot: &SwarmMetricsSnapshot,
    targets: &SlaTargets,
) -> i64 {
    let advice = advise(snapshot, targets);
    let delta = match advice.direction {
        TuneDirection::DecreaseConcurrency => -1,
        TuneDirection::IncreaseConcurrency => 1,
        TuneDirection::Hold => 0,
    };
    scheduler.apply_delta(delta)
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
    fn an_sla_miss_steps_concurrency_down_and_stops_at_the_floor() {
        let scheduler = ElasticScheduler::new(1, 2);
        let targets = SlaTargets {
            max_avg_resolution_ms: Some(100),
            min_success_rate: None,
        };
        let slow = snapshot(1, 1, 500);
        assert_eq!(
            advise(&slow, &targets).direction,
            TuneDirection::DecreaseConcurrency
        );
        assert_eq!(apply_tune(&scheduler, &slow, &targets), 1);
        assert_eq!(apply_tune(&scheduler, &slow, &targets), 1);
    }

    #[test]
    fn a_clean_full_success_steps_concurrency_up_to_the_ceiling() {
        let scheduler = ElasticScheduler::new(1, 2);
        scheduler.apply_delta(-1);
        let targets = SlaTargets {
            max_avg_resolution_ms: Some(1000),
            min_success_rate: Some(1.0),
        };
        let clean = snapshot(2, 2, 10);
        assert_eq!(
            advise(&clean, &targets).direction,
            TuneDirection::IncreaseConcurrency
        );
        assert_eq!(apply_tune(&scheduler, &clean, &targets), 2);
        assert_eq!(apply_tune(&scheduler, &clean, &targets), 2);
    }

    #[test]
    fn no_completions_holds() {
        let advice = advise(&SwarmMetricsSnapshot::default(), &SlaTargets::default());
        assert_eq!(advice.direction, TuneDirection::Hold);
    }
}

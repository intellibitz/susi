//! Elastic Swarm Scheduling & Automatic Throttling (Swarm OS Bullet 28)
//!
//! Consumes the `hardware.stress` observations `runtime_admin`'s watchdog
//! deposits onto the blackboard and adjusts a shared concurrency target
//! between `min_concurrency` and `max_concurrency` — the daemon-local half
//! of "scale elastically from 1 cell to 10,000+... with automatic
//! throttling." A `gawd` process reads the target back off the blackboard
//! (`scheduler.target_concurrency`) instead of `susi-daemon` calling into
//! `susi_gawd` directly, which crate leaf order forbids.

use std::sync::atomic::{AtomicI64, Ordering};

use susi_abi::swarm::SwarmPheromone;

pub struct ElasticScheduler {
    min_concurrency: i64,
    max_concurrency: i64,
    target: AtomicI64,
}

impl Default for ElasticScheduler {
    fn default() -> Self {
        Self::new(1, 64)
    }
}

impl ElasticScheduler {
    pub fn new(min_concurrency: i64, max_concurrency: i64) -> Self {
        let target = max_concurrency.max(min_concurrency);
        Self {
            min_concurrency,
            max_concurrency,
            target: AtomicI64::new(target),
        }
    }

    pub fn target_concurrency(&self) -> i64 {
        self.target.load(Ordering::Acquire)
    }

    /// Applies one throttle/ramp step: halves the target under stress
    /// (floored at `min_concurrency`), or grows it by one step toward
    /// `max_concurrency` when clear. Returns the new target.
    pub fn apply_stress_signal(&self, under_stress: bool) -> i64 {
        let mut current = self.target.load(Ordering::Acquire);
        loop {
            let next = if under_stress {
                (current / 2).max(self.min_concurrency)
            } else {
                (current + 1).min(self.max_concurrency)
            };
            match self.target.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }

    /// Reads a `hardware.stress` pheromone's payload (as deposited by
    /// `runtime_admin::SusiRuntimeAdmin`) and applies it. `None` for any
    /// other topic.
    pub fn apply_pheromone(&self, pheromone: &SwarmPheromone) -> Option<i64> {
        if pheromone.topic != "hardware.stress" {
            return None;
        }
        let flag = |key: &str| {
            pheromone
                .payload
                .get(key)
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        };
        let under_stress = flag("thermal_stress") || flag("power_stress") || flag("load_stress");
        Some(self.apply_stress_signal(under_stress))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stress_halves_the_target_down_to_the_floor() {
        let scheduler = ElasticScheduler::new(1, 64);
        assert_eq!(scheduler.apply_stress_signal(true), 32);
        assert_eq!(scheduler.apply_stress_signal(true), 16);
        assert_eq!(scheduler.apply_stress_signal(true), 8);
        assert_eq!(scheduler.apply_stress_signal(true), 4);
        assert_eq!(scheduler.apply_stress_signal(true), 2);
        assert_eq!(scheduler.apply_stress_signal(true), 1);
        assert_eq!(scheduler.apply_stress_signal(true), 1); // floored
    }

    #[test]
    fn clear_signal_ramps_back_up_to_the_ceiling() {
        let scheduler = ElasticScheduler::new(1, 3);
        scheduler.apply_stress_signal(true); // target -> 1
        assert_eq!(scheduler.apply_stress_signal(false), 2);
        assert_eq!(scheduler.apply_stress_signal(false), 3);
        assert_eq!(scheduler.apply_stress_signal(false), 3); // ceilinged
    }

    #[test]
    fn ignores_pheromones_on_other_topics() {
        let scheduler = ElasticScheduler::default();
        let pheromone = SwarmPheromone {
            id: "x".to_string(),
            topic: "consensus.vote".to_string(),
            emitter_id: "test".to_string(),
            kind: susi_abi::swarm::PheromoneKind::Observation,
            intensity: 1.0,
            payload: serde_json::json!({}),
            ttl_ms: 1000,
            deposited_at: 0,
        };
        assert_eq!(scheduler.apply_pheromone(&pheromone), None);
    }

    #[test]
    fn applies_hardware_stress_pheromones() {
        let scheduler = ElasticScheduler::new(1, 64);
        let pheromone = SwarmPheromone {
            id: "x".to_string(),
            topic: "hardware.stress".to_string(),
            emitter_id: "susi-runtime-admin".to_string(),
            kind: susi_abi::swarm::PheromoneKind::Observation,
            intensity: 1.0,
            payload: serde_json::json!({ "load_stress": true }),
            ttl_ms: 1000,
            deposited_at: 0,
        };
        assert_eq!(scheduler.apply_pheromone(&pheromone), Some(32));
    }
}

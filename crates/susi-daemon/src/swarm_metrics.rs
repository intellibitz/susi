//! Swarm-Level Metrics (Swarm OS Bullet 30)
//!
//! Tracks per-mission lifecycle (start -> completion) to derive
//! throughput, success rate, solution diversity, and time-to-resolution —
//! the swarm-level metrics this bullet asks for, distinct from
//! `metrics_aggregator.rs`'s single global inference/token counter
//! (Bullet 97) or `cost_analyzer.rs`'s per-dimension spend breakdown
//! (Bullet 73).

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::Instant;

struct MissionStart {
    started_at: Instant,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SwarmMetricsSnapshot {
    pub missions_started: usize,
    pub missions_completed: usize,
    pub missions_succeeded: usize,
    /// Count of distinct solution fingerprints seen across completed
    /// missions — a real proxy for "diversity of solutions": how many
    /// different final answers the swarm actually produced.
    pub distinct_solutions: usize,
    pub avg_time_to_resolution_ms: u64,
}

pub struct SwarmMetrics {
    in_flight: RwLock<HashMap<String, MissionStart>>,
    started_count: RwLock<usize>,
    completed_count: RwLock<usize>,
    succeeded_count: RwLock<usize>,
    solution_fingerprints: RwLock<HashSet<String>>,
    total_resolution_ms: RwLock<u128>,
}

impl Default for SwarmMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl SwarmMetrics {
    pub fn new() -> Self {
        Self {
            in_flight: RwLock::new(HashMap::new()),
            started_count: RwLock::new(0),
            completed_count: RwLock::new(0),
            succeeded_count: RwLock::new(0),
            solution_fingerprints: RwLock::new(HashSet::new()),
            total_resolution_ms: RwLock::new(0),
        }
    }

    pub fn start_mission(&self, mission_id: &str) {
        self.in_flight
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                mission_id.to_string(),
                MissionStart {
                    started_at: Instant::now(),
                },
            );
        *self
            .started_count
            .write()
            .unwrap_or_else(|e| e.into_inner()) += 1;
    }

    /// Completes a mission, recording success/failure, solution diversity,
    /// and time-to-resolution. Returns `false` (a no-op) if the mission
    /// was never started or was already completed — a duplicate
    /// completion can't double-count.
    pub fn complete_mission(
        &self,
        mission_id: &str,
        succeeded: bool,
        solution_fingerprint: Option<&str>,
    ) -> bool {
        let Some(start) = self
            .in_flight
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(mission_id)
        else {
            return false;
        };
        let elapsed_ms = start.started_at.elapsed().as_millis();

        *self
            .completed_count
            .write()
            .unwrap_or_else(|e| e.into_inner()) += 1;
        if succeeded {
            *self
                .succeeded_count
                .write()
                .unwrap_or_else(|e| e.into_inner()) += 1;
        }
        if let Some(fingerprint) = solution_fingerprint {
            self.solution_fingerprints
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(fingerprint.to_string());
        }
        *self
            .total_resolution_ms
            .write()
            .unwrap_or_else(|e| e.into_inner()) += elapsed_ms;
        true
    }

    pub fn snapshot(&self) -> SwarmMetricsSnapshot {
        let completed = *self
            .completed_count
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let total_ms = *self
            .total_resolution_ms
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let avg_ms = if completed == 0 {
            0
        } else {
            (total_ms / completed as u128) as u64
        };

        SwarmMetricsSnapshot {
            missions_started: *self.started_count.read().unwrap_or_else(|e| e.into_inner()),
            missions_completed: completed,
            missions_succeeded: *self
                .succeeded_count
                .read()
                .unwrap_or_else(|e| e.into_inner()),
            distinct_solutions: self
                .solution_fingerprints
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            avg_time_to_resolution_ms: avg_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn completing_an_unstarted_mission_is_a_no_op() {
        let metrics = SwarmMetrics::new();
        assert!(!metrics.complete_mission("never-started", true, None));
        assert_eq!(metrics.snapshot(), SwarmMetricsSnapshot::default());
    }

    #[test]
    fn tracks_throughput_and_success_rate() {
        let metrics = SwarmMetrics::new();
        metrics.start_mission("m1");
        metrics.start_mission("m2");
        metrics.complete_mission("m1", true, None);
        metrics.complete_mission("m2", false, None);

        let snap = metrics.snapshot();
        assert_eq!(snap.missions_started, 2);
        assert_eq!(snap.missions_completed, 2);
        assert_eq!(snap.missions_succeeded, 1);
    }

    #[test]
    fn distinct_solutions_counts_unique_fingerprints_only() {
        let metrics = SwarmMetrics::new();
        metrics.start_mission("m1");
        metrics.start_mission("m2");
        metrics.start_mission("m3");
        metrics.complete_mission("m1", true, Some("answer-a"));
        metrics.complete_mission("m2", true, Some("answer-a")); // same as m1
        metrics.complete_mission("m3", true, Some("answer-b"));

        assert_eq!(metrics.snapshot().distinct_solutions, 2);
    }

    #[test]
    fn double_completion_does_not_double_count() {
        let metrics = SwarmMetrics::new();
        metrics.start_mission("m1");
        assert!(metrics.complete_mission("m1", true, None));
        assert!(!metrics.complete_mission("m1", true, None));
        assert_eq!(metrics.snapshot().missions_completed, 1);
    }

    #[test]
    fn time_to_resolution_reflects_real_elapsed_time() {
        let metrics = SwarmMetrics::new();
        metrics.start_mission("m1");
        sleep(Duration::from_millis(20));
        metrics.complete_mission("m1", true, None);

        assert!(metrics.snapshot().avg_time_to_resolution_ms >= 15);
    }
}

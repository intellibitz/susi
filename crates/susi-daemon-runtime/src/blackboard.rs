//! Swarm OS Stigmergic Blackboard & Capability Router.
//!
//! Implements the global shared memory graph, pheromone evaporation (TTL),
//! and Capability Bloom Filter routing for autonomous cells.

use crate::susi_abi::swarm::{SwarmCellManifest, SwarmPheromone};
use std::collections::HashMap;
use std::sync::Arc;
use susi_paths::sync::RwLock;
use tokio::sync::broadcast;

/// The global stigmergic blackboard and capability router.
pub struct SwarmBlackboard {
    /// Active Swarm Cells registered with the OS.
    cells: RwLock<HashMap<String, SwarmCellManifest>>,
    /// Active Pheromones deposited in the environment.
    pheromones: RwLock<HashMap<String, SwarmPheromone>>,
    /// Global event bus for pub/sub (Point 18, 21).
    bus: broadcast::Sender<SwarmPheromone>,
}

// Broadcast sender is not exhaustive in Debug; written by hand rather
// than derived so the blackboard snapshot stays readable.
impl std::fmt::Debug for SwarmBlackboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwarmBlackboard")
            .field("cells", &*self.cells.read())
            .field("pheromones", &*self.pheromones.read())
            .finish_non_exhaustive()
    }
}

impl SwarmBlackboard {
    /// Creates a new global Swarm Blackboard.
    pub fn new() -> Arc<Self> {
        let (bus, _) = broadcast::channel(10000);
        Arc::new(Self {
            cells: RwLock::default(),
            pheromones: RwLock::default(),
            bus,
        })
    }

    /// Registers a new cell in the Swarm OS (Point 12).
    pub fn register_cell(&self, manifest: SwarmCellManifest) {
        self.cells
            .write()
            .insert(manifest.cell_id.clone(), manifest);
    }

    /// Unregisters a dead or failed cell (Point 26).
    pub fn unregister_cell(&self, cell_id: &str) {
        self.cells.write().remove(cell_id);
    }

    /// Dynamically penalizes a cell's trust score for malformed syscalls (Point 41).
    pub fn penalize_cell(&self, cell_id: &str, penalty: f32) {
        if let Some(cell) = self.cells.write().get_mut(cell_id) {
            cell.trust_score = (cell.trust_score - penalty).max(0.0);
        }
    }

    /// Deposits a new pheromone into the environment (Point 25).
    pub fn deposit_pheromone(&self, pheromone: SwarmPheromone) {
        self.pheromones
            .write()
            .insert(pheromone.id.clone(), pheromone.clone());

        // Broadcast the pheromone to all subscribers.
        let _ = self.bus.send(pheromone);
    }

    /// Subscribes to the global swarm event bus (Point 18).
    pub fn subscribe(&self) -> broadcast::Receiver<SwarmPheromone> {
        self.bus.subscribe()
    }

    /// Routes an intent to capable cells using O(1) Bloom Filter matching (Point 22).
    pub fn find_capable_cells(&self, capability_hash: u64) -> Vec<SwarmCellManifest> {
        let mut capable = Vec::new();
        for cell in self.cells.read().values() {
            if cell.bloom_filter.may_contain_hash(capability_hash) {
                capable.push(cell.clone());
            }
        }
        // Sort by trust score (Point 22).
        capable.sort_by(|a, b| {
            b.trust_score
                .partial_cmp(&a.trust_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        capable
    }

    /// Checks `snapshot` against `targets` (Bullet 75) and, on any
    /// violation, deposits an `sla.violation` pheromone so it's
    /// observable by anything watching the blackboard. Returns the
    /// violations found, if any.
    pub fn check_sla_and_alert(
        &self,
        snapshot: &crate::swarm_metrics::SwarmMetricsSnapshot,
        targets: &crate::sla_monitor::SlaTargets,
    ) -> Vec<crate::sla_monitor::SlaViolation> {
        let violations = crate::sla_monitor::check_sla(snapshot, targets);
        if let Some(pheromone) = crate::sla_monitor::violations_pheromone(&violations) {
            self.deposit_pheromone(pheromone);
        }
        violations
    }

    /// Evaporates expired pheromones from the blackboard (Point 36).
    pub fn evaporate_pheromones(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.pheromones.write().retain(|_, p| {
            // TTL is in milliseconds, convert to seconds
            let ttl_secs = p.ttl_ms / 1000;
            now <= p.deposited_at + ttl_secs
        });
    }

    /// Queries the world model for a specific topic (Point 34).
    pub fn query_topic(&self, topic: &str) -> Vec<SwarmPheromone> {
        let mut results = Vec::new();
        for pheromone in self.pheromones.read().values() {
            if pheromone.topic == topic {
                results.push(pheromone.clone());
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sla_violation_deposits_an_alert_pheromone() {
        let board = SwarmBlackboard::new();
        let snapshot = crate::swarm_metrics::SwarmMetricsSnapshot {
            missions_started: 1,
            missions_completed: 1,
            missions_succeeded: 0,
            distinct_solutions: 0,
            avg_time_to_resolution_ms: 5000,
        };
        let targets = crate::sla_monitor::SlaTargets {
            max_avg_resolution_ms: Some(1000),
            min_success_rate: None,
        };

        let violations = board.check_sla_and_alert(&snapshot, &targets);
        assert_eq!(violations.len(), 1);
        assert_eq!(board.query_topic("sla.violation").len(), 1);
    }

    #[test]
    fn a_clean_snapshot_deposits_no_alert() {
        let board = SwarmBlackboard::new();
        let snapshot = crate::swarm_metrics::SwarmMetricsSnapshot::default();
        let violations =
            board.check_sla_and_alert(&snapshot, &crate::sla_monitor::SlaTargets::default());
        assert!(violations.is_empty());
        assert!(board.query_topic("sla.violation").is_empty());
    }

    #[test]
    fn deposit_pheromone_and_query_topic_filter_by_topic_and_broadcast() {
        let board = SwarmBlackboard::new();
        let mut rx = board.subscribe();
        let pheromone = SwarmPheromone {
            id: "ph-1".to_string(),
            topic: "mission.progress".to_string(),
            emitter_id: "cell-a".to_string(),
            kind: crate::susi_abi::swarm::PheromoneKind::Observation,
            intensity: 1.0,
            payload: serde_json::json!({"step": 1}),
            ttl_ms: 60_000,
            deposited_at: u64::MAX / 2,
        };
        board.deposit_pheromone(pheromone);

        let matched = board.query_topic("mission.progress");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, "ph-1");
        assert!(board.query_topic("mission.unknown").is_empty());
        let received = rx
            .try_recv()
            .expect("broadcast subscriber receives pheromone");
        assert_eq!(received.topic, "mission.progress");
    }

    #[test]
    fn min_success_rate_sla_violation_deposits_alert_pheromone() {
        let board = SwarmBlackboard::new();
        let snapshot = crate::swarm_metrics::SwarmMetricsSnapshot {
            missions_started: 4,
            missions_completed: 4,
            missions_succeeded: 1,
            distinct_solutions: 1,
            avg_time_to_resolution_ms: 200,
        };
        let targets = crate::sla_monitor::SlaTargets {
            max_avg_resolution_ms: Some(1000),
            min_success_rate: Some(0.8),
        };
        let violations = board.check_sla_and_alert(&snapshot, &targets);
        assert_eq!(violations.len(), 1);
        assert_eq!(board.query_topic("sla.violation").len(), 1);
        assert!(format!("{board:?}").contains("SwarmBlackboard"));
    }

    #[test]
    fn penalize_unregister_and_evaporate_pheromones_update_board_state() {
        let board = SwarmBlackboard::new();
        board.penalize_cell("cell-x", 0.4);
        board.unregister_cell("cell-x");
        assert!(board.find_capable_cells(42).is_empty());

        board.deposit_pheromone(SwarmPheromone {
            id: "ph-expired".to_string(),
            topic: "temp.topic".to_string(),
            emitter_id: "cell-x".to_string(),
            kind: crate::susi_abi::swarm::PheromoneKind::Observation,
            intensity: 0.5,
            payload: serde_json::json!({}),
            ttl_ms: 100,
            deposited_at: 0,
        });
        assert_eq!(board.query_topic("temp.topic").len(), 1);
        board.evaporate_pheromones();
        assert!(board.query_topic("temp.topic").is_empty());
    }
}

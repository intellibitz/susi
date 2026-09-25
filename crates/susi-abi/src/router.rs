//! Stigmergic Pheromone Router and Intent Matcher.
//!
//! Matches incoming intents against decentralized swarm cells based on capability bloom filters,
//! trust scores, and decaying pheromone gradients without centralized bottlenecks.

use super::cell::SwarmCell;
use super::swarm::{SwarmCellManifest, SwarmPheromone};
use serde::{Deserialize, Serialize};

/// Maximum age in seconds before an unrefreshed cell is considered stale/unreachable.
pub const CELL_HEARTBEAT_TIMEOUT_SECS: u64 = 60;

/// Match candidate resulting from an intent routing evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteCandidate {
    /// Cell identifier of the matched cell.
    pub cell_id: String,
    /// IPC endpoint URL to contact.
    pub endpoint: String,
    /// Routing fitness score combining trust, capability match, and latency (0.0 to 1.0).
    pub fitness: f32,
}

/// In-memory Stigmergic Pheromone Field and Swarm Cell Roster.
#[derive(Debug, Default, Clone)]
pub struct PheromoneRouter {
    /// Active registered swarm cells keyed by cell_id.
    cells: Vec<SwarmCell>,
    /// Active pheromones currently diffusing through the environment.
    pheromones: Vec<SwarmPheromone>,
}

impl PheromoneRouter {
    /// Creates a new, empty pheromone router.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            pheromones: Vec::new(),
        }
    }

    /// Registers or updates a Swarm Cell in the active roster.
    pub fn register_cell(&mut self, cell: SwarmCell) {
        if let Some(existing) = self
            .cells
            .iter_mut()
            .find(|c| c.manifest.cell_id == cell.manifest.cell_id)
        {
            *existing = cell;
        } else {
            self.cells.push(cell);
        }
    }

    /// Removes a cell from the roster by its cell_id.
    pub fn unregister_cell(&mut self, cell_id: &str) {
        self.cells.retain(|c| c.manifest.cell_id != cell_id);
    }

    /// Deposits a stigmergic pheromone into the shared environment.
    pub fn deposit_pheromone(&mut self, pheromone: SwarmPheromone) {
        self.pheromones.push(pheromone);
    }

    /// Evaporates expired pheromones based on the current timestamp.
    pub fn evaporate_expired(&mut self, now_secs: u64) {
        self.pheromones.retain(|p| {
            let age_ms = now_secs.saturating_sub(p.deposited_at).saturating_mul(1000);
            age_ms < p.ttl_ms
        });
    }

    /// Finds the best cell candidates to handle a required capability.
    ///
    /// Evaluates cells matching the capability, filters out timed-out nodes,
    /// and sorts candidates by fitness score (highest first).
    #[must_use]
    pub fn route_capability(&self, capability: &str, now_secs: u64) -> Vec<RouteCandidate> {
        let mut candidates = Vec::new();

        for cell in &self.cells {
            // Check liveness: cell must have sent a heartbeat recently
            let age = now_secs.saturating_sub(cell.manifest.last_heartbeat);
            if age > CELL_HEARTBEAT_TIMEOUT_SECS {
                continue;
            }

            // Check capability match via bloom filter + exact check
            if cell.can_handle(capability) {
                // Fitness score weights trust score heavily
                let fitness = cell.manifest.trust_score.clamp(0.0, 1.0);
                candidates.push(RouteCandidate {
                    cell_id: cell.manifest.cell_id.clone(),
                    endpoint: cell.manifest.endpoint.clone(),
                    fitness,
                });
            }
        }

        // Sort descending by fitness score
        candidates.sort_by(|a, b| {
            b.fitness
                .partial_cmp(&a.fitness)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        candidates
    }

    /// Returns active pheromones matching a specific semantic topic.
    #[must_use]
    pub fn query_topic(&self, topic: &str) -> Vec<&SwarmPheromone> {
        self.pheromones
            .iter()
            .filter(|p| p.topic == topic)
            .collect()
    }

    /// Returns all registered active cell manifests.
    #[must_use]
    pub fn list_cell_manifests(&self) -> Vec<&SwarmCellManifest> {
        self.cells.iter().map(|c| &c.manifest).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::PheromoneKind;
    use crate::swarm::SwarmRole;

    #[test]
    fn test_pheromone_routing_and_fitness_ranking() {
        let mut router = PheromoneRouter::new();

        let mut cell_a = SwarmCell::new(
            "cell-a".to_string(),
            SwarmRole::ToolDriver,
            "ipc:///tmp/cell_a.sock".to_string(),
        );
        cell_a.register_capability("git_diff");
        cell_a.set_ready(100);
        cell_a.manifest.trust_score = 0.85;

        let mut cell_b = SwarmCell::new(
            "cell-b".to_string(),
            SwarmRole::ToolDriver,
            "ipc:///tmp/cell_b.sock".to_string(),
        );
        cell_b.register_capability("git_diff");
        cell_b.set_ready(100);
        cell_b.manifest.trust_score = 0.98;

        let mut stale_cell = SwarmCell::new(
            "cell-stale".to_string(),
            SwarmRole::ToolDriver,
            "ipc:///tmp/cell_stale.sock".to_string(),
        );
        stale_cell.register_capability("git_diff");
        stale_cell.set_ready(10); // Heartbeat 10 vs now 100 (>60s stale)

        router.register_cell(cell_a);
        router.register_cell(cell_b);
        router.register_cell(stale_cell);

        let candidates = router.route_capability("git_diff", 100);
        assert_eq!(candidates.len(), 2);
        // cell-b has higher trust score (0.98), so it must be ranked first
        assert_eq!(candidates[0].cell_id, "cell-b");
        assert_eq!(candidates[1].cell_id, "cell-a");
    }

    #[test]
    fn test_pheromone_evaporation() {
        let mut router = PheromoneRouter::new();

        let p1 = SwarmPheromone {
            id: "ph-1".to_string(),
            topic: "intent.test".to_string(),
            emitter_id: "agent-1".to_string(),
            kind: PheromoneKind::Intent,
            intensity: 1.0,
            payload: serde_json::json!({"action": "check"}),
            ttl_ms: 5000,      // 5 seconds
            deposited_at: 100, // at t=100s
        };

        router.deposit_pheromone(p1);
        assert_eq!(router.query_topic("intent.test").len(), 1);

        // At t=102s, age is 2s (2000ms < 5000ms), should not evaporate
        router.evaporate_expired(102);
        assert_eq!(router.query_topic("intent.test").len(), 1);

        // At t=106s, age is 6s (6000ms >= 5000ms), should evaporate
        router.evaporate_expired(106);
        assert_eq!(router.query_topic("intent.test").len(), 0);
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::swarm::PheromoneKind;
    use proptest::prelude::*;

    fn pheromone(deposited_at: u64, ttl_ms: u64) -> SwarmPheromone {
        SwarmPheromone {
            id: "p".to_string(),
            topic: "t".to_string(),
            emitter_id: "e".to_string(),
            kind: PheromoneKind::Observation,
            intensity: 1.0,
            payload: serde_json::Value::Null,
            ttl_ms,
            deposited_at,
        }
    }

    proptest! {
        // Property: evaporate_expired's survival decision matches its own
        // age-vs-ttl formula exactly — never keeps a pheromone past its ttl,
        // never drops one still inside it.
        #[test]
        fn evaporation_matches_ttl_exactly(
            deposited_at in 0u64..1_000_000,
            ttl_ms in 1u64..600_000,
            now_secs in 0u64..1_000_000,
        ) {
            let mut router = PheromoneRouter::new();
            router.deposit_pheromone(pheromone(deposited_at, ttl_ms));
            router.evaporate_expired(now_secs);
            let age_ms = now_secs.saturating_sub(deposited_at).saturating_mul(1000);
            let expected = usize::from(age_ms < ttl_ms);
            prop_assert_eq!(router.query_topic("t").len(), expected);
        }
    }
}

//! Swarm OS Stigmergic Blackboard & Capability Router.
//!
//! Implements the global shared memory graph, pheromone evaporation (TTL),
//! and O(1) Capability Bloom Filter routing for autonomous cells.
//! This fulfills the core Swarm OS Vision (Points 5, 12, 21, 22, 25, 31).

use dashmap::DashMap;
use std::sync::Arc;
use susi_abi::swarm::{SwarmCellManifest, SwarmPheromone};
use tokio::sync::broadcast;

/// The global stigmergic blackboard and capability router.
#[derive(Debug)]
pub struct SwarmBlackboard {
    /// Active Swarm Cells registered with the OS.
    cells: DashMap<String, SwarmCellManifest>,
    /// Active Pheromones deposited in the environment.
    pheromones: DashMap<String, SwarmPheromone>,
    /// Global event bus for pub/sub (Point 18, 21).
    bus: broadcast::Sender<SwarmPheromone>,
}

impl SwarmBlackboard {
    /// Creates a new global Swarm Blackboard.
    pub fn new() -> Arc<Self> {
        let (bus, _) = broadcast::channel(10000);
        Arc::new(Self {
            cells: DashMap::new(),
            pheromones: DashMap::new(),
            bus,
        })
    }

    /// Registers a new cell in the Swarm OS (Point 12).
    pub fn register_cell(&self, manifest: SwarmCellManifest) {
        self.cells.insert(manifest.cell_id.clone(), manifest);
    }

    /// Unregisters a dead or failed cell (Point 26).
    pub fn unregister_cell(&self, cell_id: &str) {
        self.cells.remove(cell_id);
    }

    /// Dynamically penalizes a cell's trust score for malformed syscalls (Point 41).
    pub fn penalize_cell(&self, cell_id: &str, penalty: f32) {
        if let Some(mut cell) = self.cells.get_mut(cell_id) {
            cell.trust_score = (cell.trust_score - penalty).max(0.0);
        }
    }

    /// Deposits a new pheromone into the environment (Point 25).
    pub fn deposit_pheromone(&self, pheromone: SwarmPheromone) {
        self.pheromones
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
        for entry in self.cells.iter() {
            let cell = entry.value();
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

    /// Ranks currently registered cells by trust score (Bullet 79).
    pub fn leaderboard(&self) -> Vec<crate::leaderboard::LeaderboardEntry> {
        let cells: Vec<SwarmCellManifest> = self
            .cells
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        crate::leaderboard::rank_by_trust(&cells)
    }

    /// Evaporates expired pheromones from the blackboard (Point 36).
    pub fn evaporate_pheromones(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.pheromones.retain(|_, p| {
            // TTL is in milliseconds, convert to seconds
            let ttl_secs = p.ttl_ms / 1000;
            now <= p.deposited_at + ttl_secs
        });
    }

    /// Queries the world model for a specific topic (Point 34).
    pub fn query_topic(&self, topic: &str) -> Vec<SwarmPheromone> {
        let mut results = Vec::new();
        for entry in self.pheromones.iter() {
            if entry.value().topic == topic {
                results.push(entry.value().clone());
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_abi::swarm::{CapabilityBloom, SwarmRole};

    fn manifest(cell_id: &str, trust_score: f32) -> SwarmCellManifest {
        SwarmCellManifest {
            cell_id: cell_id.to_string(),
            role: SwarmRole::ReflexCell,
            capabilities: Vec::new(),
            bloom_filter: CapabilityBloom::default(),
            endpoint: "ipc:///tmp/test.sock".to_string(),
            trust_score,
            last_heartbeat: 0,
        }
    }

    #[test]
    fn leaderboard_ranks_registered_cells_by_trust() {
        let board = SwarmBlackboard::new();
        board.register_cell(manifest("low", 0.1));
        board.register_cell(manifest("high", 0.9));

        let ranking = board.leaderboard();
        assert_eq!(ranking.len(), 2);
        assert_eq!(ranking[0].cell_id, "high");
        assert_eq!(ranking[1].cell_id, "low");
    }
}

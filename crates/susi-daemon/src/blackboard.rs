//! Swarm OS Stigmergic Blackboard & Capability Router.
//!
//! Implements the global shared memory graph, pheromone evaporation (TTL),
//! and O(1) Capability Bloom Filter routing for autonomous cells.
//! This fulfills the core Swarm OS Vision (Points 5, 12, 21, 22, 25, 31).

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use susi_abi::swarm::{SwarmCellManifest, SwarmPheromone};

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

    /// Deposits a new pheromone into the environment (Point 25).
    pub fn deposit_pheromone(&self, pheromone: SwarmPheromone) {
        self.pheromones.insert(pheromone.id.clone(), pheromone.clone());
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
        capable.sort_by(|a, b| b.trust_score.partial_cmp(&a.trust_score).unwrap_or(std::cmp::Ordering::Equal));
        capable
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

//! Swarm OS Stigmergic Blackboard & Capability Router.
//!
//! Implements the global shared memory graph, pheromone evaporation (TTL),
//! and O(1) Capability Bloom Filter routing for autonomous cells.
//! This fulfills the core Swarm OS Vision (Points 5, 12, 21, 22, 25, 31).

use dashmap::DashMap;
use std::sync::{Arc, RwLock};
use susi_abi::swarm::{SwarmCellManifest, SwarmPheromone};
use tokio::sync::broadcast;

use crate::webhook_dispatcher::WebhookDispatcher;

/// The global stigmergic blackboard and capability router.
pub struct SwarmBlackboard {
    /// Active Swarm Cells registered with the OS.
    cells: DashMap<String, SwarmCellManifest>,
    /// Active Pheromones deposited in the environment.
    pheromones: DashMap<String, SwarmPheromone>,
    /// Global event bus for pub/sub (Point 18, 21).
    bus: broadcast::Sender<SwarmPheromone>,
    /// External webhook fan-out for deposited pheromones (Bullet 49).
    /// `None` until `set_webhook_dispatcher` is called — every existing
    /// caller of `deposit_pheromone` keeps its current (network-free)
    /// behavior unless a dispatcher is explicitly configured.
    webhook_dispatcher: RwLock<Option<Arc<WebhookDispatcher>>>,
}

// `ureq::Agent` (inside `WebhookDispatcher`) doesn't implement `Debug`, so
// this is written by hand rather than derived.
impl std::fmt::Debug for SwarmBlackboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwarmBlackboard")
            .field("cells", &self.cells)
            .field("pheromones", &self.pheromones)
            .finish_non_exhaustive()
    }
}

impl SwarmBlackboard {
    /// Creates a new global Swarm Blackboard.
    pub fn new() -> Arc<Self> {
        let (bus, _) = broadcast::channel(10000);
        Arc::new(Self {
            cells: DashMap::new(),
            pheromones: DashMap::new(),
            bus,
            webhook_dispatcher: RwLock::new(None),
        })
    }

    /// Configures external webhook fan-out (Bullet 49): every pheromone
    /// deposited from now on is also POSTed to any subscriber whose topic
    /// filter matches, via `dispatcher`.
    pub fn set_webhook_dispatcher(&self, dispatcher: Arc<WebhookDispatcher>) {
        *self
            .webhook_dispatcher
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(dispatcher);
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

        if let Some(dispatcher) = self
            .webhook_dispatcher
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            dispatcher.dispatch(&pheromone);
        }

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

    /// Snapshot of every currently registered cell's manifest.
    fn all_cells(&self) -> Vec<SwarmCellManifest> {
        self.cells
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Ranks currently registered cells by trust score (Bullet 79).
    pub fn leaderboard(&self) -> Vec<crate::leaderboard::LeaderboardEntry> {
        crate::leaderboard::rank_by_trust(&self.all_cells())
    }

    /// Renders every registered cell's manifest (and its spans from
    /// `spans`) as one Markdown document (Bullet 70).
    pub fn generate_docs(&self, spans: &[crate::tracing::TraceSpan]) -> String {
        crate::docs_generator::render_fleet_doc(&self.all_cells(), spans)
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

    /// Recommends up to `team_size` cells for `topic` (Bullet 78): capable
    /// cells (by `capability_hash`) ranked by trust, tiebroken by how many
    /// `Receipt` pheromones they've deposited on `topic` in the past.
    pub fn recommend_composition(
        &self,
        capability_hash: u64,
        topic: &str,
        team_size: usize,
    ) -> Vec<crate::composition_recommender::CompositionCandidate> {
        let capable = self.find_capable_cells(capability_hash);
        let history = self.query_topic(topic);
        crate::composition_recommender::recommend_composition(&capable, &history, team_size)
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
    use susi_abi::swarm::{CapabilityBloom, PheromoneKind, SwarmRole};

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
    fn generate_docs_includes_every_registered_cell() {
        let board = SwarmBlackboard::new();
        board.register_cell(manifest("cell-a", 0.5));
        board.register_cell(manifest("cell-b", 0.5));

        let doc = board.generate_docs(&[]);
        assert!(doc.contains("## cell-a"));
        assert!(doc.contains("## cell-b"));
    }

    #[test]
    fn deposit_routes_through_a_configured_webhook_dispatcher_with_no_subscribers() {
        // A dispatcher with zero subscriptions makes no network calls (its
        // dispatch loop has nothing to iterate), so this exercises the
        // real deposit_pheromone -> WebhookDispatcher::dispatch wiring
        // without depending on network access in the test environment.
        let board = SwarmBlackboard::new();
        board.set_webhook_dispatcher(Arc::new(WebhookDispatcher::new()));

        board.deposit_pheromone(SwarmPheromone {
            id: "p1".to_string(),
            topic: "hardware.stress".to_string(),
            emitter_id: "susi-runtime-admin".to_string(),
            kind: PheromoneKind::Observation,
            intensity: 1.0,
            payload: serde_json::json!({}),
            ttl_ms: 60_000,
            deposited_at: 0,
        });

        assert_eq!(board.query_topic("hardware.stress").len(), 1);
    }

    #[test]
    fn recommend_composition_filters_by_capability_and_ranks_by_history() {
        let board = SwarmBlackboard::new();
        let target_hash: u64 = 0xC0FFEE;

        let mut capable = manifest("proven", 0.5);
        capable.bloom_filter.insert_hash(target_hash);
        board.register_cell(capable);

        let mut also_capable = manifest("untested", 0.5);
        also_capable.bloom_filter.insert_hash(target_hash);
        board.register_cell(also_capable);

        // Registered but doesn't declare the target capability -- must be
        // excluded regardless of trust or history.
        board.register_cell(manifest("wrong-capability", 0.99));

        board.deposit_pheromone(SwarmPheromone {
            id: "receipt-1".to_string(),
            topic: "intent.deploy".to_string(),
            emitter_id: "proven".to_string(),
            kind: PheromoneKind::Receipt,
            intensity: 1.0,
            payload: serde_json::json!({}),
            ttl_ms: 60_000,
            deposited_at: 0,
        });

        let recommended = board.recommend_composition(target_hash, "intent.deploy", 5);
        let cell_ids: Vec<&str> = recommended.iter().map(|c| c.cell_id.as_str()).collect();
        assert_eq!(cell_ids, vec!["proven", "untested"]);
        assert_eq!(recommended[0].historical_successes, 1);
    }
}

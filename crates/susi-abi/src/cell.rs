//! Swarm Cell State Machine and Lifecycle.
//!
//! Autonomous, decoupled execution cell representing an independent agent,
//! inference engine, or device driver within the Swarm OS.

use super::swarm::{CapabilityBloom, SwarmCellManifest, SwarmRole};
use super::syscall::{SyscallRequest, SyscallResponse, SyscallStatus};
use serde::{Deserialize, Serialize};

/// Lifecycle state of an autonomous Swarm Cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellState {
    /// Cell is initializing its local weights, tools, or sandboxes.
    Initializing,
    /// Cell is active, listening for intents, and healthy.
    Ready,
    /// Cell is actively executing a mission, inference task, or tool.
    Busy,
    /// Cell is degraded or experienced a recoverable fault.
    Degraded,
    /// Cell is terminating or gracefully standing down.
    Terminated,
}

/// An autonomous Swarm Cell descriptor with local state tracking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmCell {
    /// Public manifest of this cell.
    pub manifest: SwarmCellManifest,
    /// Current lifecycle state.
    pub state: CellState,
    /// Number of completed missions or syscalls.
    pub missions_completed: u64,
    /// Number of failed missions or errors.
    pub missions_failed: u64,
    /// Monotonic sequence number for cell events.
    pub event_seq: u64,
}

impl SwarmCell {
    /// Creates a new Swarm Cell with the given role and endpoint.
    #[must_use]
    pub fn new(cell_id: String, role: SwarmRole, endpoint: String) -> Self {
        Self {
            manifest: SwarmCellManifest {
                cell_id,
                role,
                capabilities: Vec::new(),
                bloom_filter: CapabilityBloom::empty(),
                endpoint,
                trust_score: 1.0,
                last_heartbeat: 0,
            },
            state: CellState::Initializing,
            missions_completed: 0,
            missions_failed: 0,
            event_seq: 0,
        }
    }

    /// Registers a capability with this cell, automatically updating its bloom filter.
    pub fn register_capability(&mut self, capability: &str) {
        if !self.manifest.capabilities.iter().any(|c| c == capability) {
            self.manifest.capabilities.push(capability.to_string());
            let hash = compute_fnv1a_hash(capability);
            self.manifest.bloom_filter.insert_hash(hash);
        }
    }

    /// Tests whether this cell can potentially handle an intent matching a capability.
    #[must_use]
    pub fn can_handle(&self, capability: &str) -> bool {
        let hash = compute_fnv1a_hash(capability);
        self.manifest.bloom_filter.may_contain_hash(hash)
            && self.manifest.capabilities.iter().any(|c| c == capability)
    }

    /// Transitions this cell to the Ready state.
    pub fn set_ready(&mut self, now_secs: u64) {
        self.state = CellState::Ready;
        self.manifest.last_heartbeat = now_secs;
    }

    /// Records completion of a syscall or mission.
    pub fn record_success(&mut self) {
        self.missions_completed = self.missions_completed.saturating_add(1);
        self.event_seq = self.event_seq.saturating_add(1);
    }

    /// Records a failure, updating trust score and failure counters.
    pub fn record_failure(&mut self) {
        self.missions_failed = self.missions_failed.saturating_add(1);
        self.event_seq = self.event_seq.saturating_add(1);
        // Slightly decay trust score on failure
        self.manifest.trust_score = (self.manifest.trust_score * 0.95).max(0.1);
    }

    /// Formulates a baseline heartbeat syscall response for monitoring.
    #[must_use]
    pub fn heartbeat_response(&self, req: &SyscallRequest) -> SyscallResponse {
        SyscallResponse {
            id: req.id.clone(),
            status: SyscallStatus::Success,
            data: serde_json::json!({
                "cell_id": self.manifest.cell_id,
                "role": self.manifest.role,
                "state": self.state,
                "trust_score": self.manifest.trust_score,
                "missions_completed": self.missions_completed,
                "missions_failed": self.missions_failed,
            }),
            receipt: None,
            latency_us: 10,
            message: None,
        }
    }
}

/// Computes a standard 64-bit FNV-1a hash of a capability string for bloom filter indexing.
#[must_use]
pub fn compute_fnv1a_hash(s: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cell_lifecycle_and_capability_routing() {
        let mut cell = SwarmCell::new(
            "cell-infer-01".to_string(),
            SwarmRole::InferenceDriver,
            "ipc:///tmp/susi_infer.sock".to_string(),
        );

        assert_eq!(cell.state, CellState::Initializing);
        assert!(!cell.can_handle("qwen2.5-coder"));

        cell.register_capability("qwen2.5-coder");
        cell.register_capability("code_generation");

        assert!(cell.can_handle("qwen2.5-coder"));
        assert!(cell.can_handle("code_generation"));
        assert!(!cell.can_handle("web_browser"));

        cell.set_ready(1790333000);
        assert_eq!(cell.state, CellState::Ready);
        assert_eq!(cell.manifest.last_heartbeat, 1790333000);

        cell.record_success();
        assert_eq!(cell.missions_completed, 1);
        assert_eq!(cell.missions_failed, 0);

        cell.record_failure();
        assert_eq!(cell.missions_failed, 1);
        assert!(cell.manifest.trust_score < 1.0);
    }
}

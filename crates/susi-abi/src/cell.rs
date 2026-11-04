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

    /// Health snapshot carried by heartbeat replies: identity, lifecycle
    /// state, trust score, and mission counters.
    #[must_use]
    pub fn heartbeat_status(&self) -> serde_json::Value {
        serde_json::json!({
            "cell_id": self.manifest.cell_id,
            "role": self.manifest.role,
            "state": self.state,
            "trust_score": self.manifest.trust_score,
            "missions_completed": self.missions_completed,
            "missions_failed": self.missions_failed,
        })
    }

    /// Formulates a baseline heartbeat syscall response for monitoring.
    #[must_use]
    pub fn heartbeat_response(&self, req: &SyscallRequest) -> SyscallResponse {
        SyscallResponse {
            id: req.id.clone(),
            status: SyscallStatus::Success,
            data: self.heartbeat_status(),
            receipt: None,
            latency_us: 10,
            message: None,
        }
    }
}

/// Environment variable overriding a Swarm Cell's listen address.
pub const CELL_ADDR_ENV: &str = "SUSI_CELL_ADDR";

/// Default Swarm Cell ports. They sit outside the daemon's host contract
/// (9090-9094) and the leaf services (18080-18084) so a cell never
/// collides with the daemon it runs beside.
pub mod cell_ports {
    /// gmcp tool-driver cell.
    pub const GMCP: u16 = 19090;
    /// gemi inference cell.
    pub const GEMI: u16 = 19091;
    /// gawd planner cell.
    pub const GAWD: u16 = 19092;
}

/// Resolves a Swarm Cell's listen address from explicit environment inputs:
/// `cell_addr_env` when it trims to a valid [`std::net::SocketAddr`], else
/// `127.0.0.1:<default_port.saturating_add(port_offset_env)>`.
#[must_use]
pub fn cell_bind_addr_with(
    default_port: u16,
    cell_addr_env: Option<&str>,
    port_offset_env: Option<&str>,
) -> std::net::SocketAddr {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    if let Some(addr) = cell_addr_env.and_then(|v| v.trim().parse::<SocketAddr>().ok()) {
        return addr;
    }
    let offset = port_offset_env
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(0);
    SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        default_port.saturating_add(offset),
    )
}

/// Listen address for a cell: `SUSI_CELL_ADDR` when it parses, else
/// `127.0.0.1:<default_port + SUSI_PORT_OFFSET>` (the same instance offset
/// that shifts every other susi port).
#[must_use]
pub fn cell_bind_addr(default_port: u16) -> std::net::SocketAddr {
    let addr_env = std::env::var(CELL_ADDR_ENV).ok();
    let offset_env = std::env::var("SUSI_PORT_OFFSET").ok();
    cell_bind_addr_with(default_port, addr_env.as_deref(), offset_env.as_deref())
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

    #[test]
    fn cell_ports_avoid_the_host_contract_and_leaf_services() {
        for p in [cell_ports::GMCP, cell_ports::GEMI, cell_ports::GAWD] {
            assert!(
                !(9090..=9094).contains(&p),
                "{p} collides with the host contract"
            );
            assert!(
                !(18080..=18084).contains(&p),
                "{p} collides with a leaf service"
            );
        }
        // No env override in the test process: loopback + default port.
        if std::env::var(CELL_ADDR_ENV).is_err() && std::env::var("SUSI_PORT_OFFSET").is_err() {
            assert_eq!(
                cell_bind_addr(cell_ports::GAWD).to_string(),
                "127.0.0.1:19092"
            );
        }
    }

    #[test]
    fn heartbeat_status_reflects_live_counters() {
        let mut cell = SwarmCell::new(
            "cell-hb".to_string(),
            SwarmRole::ToolDriver,
            "tcp://127.0.0.1:0".to_string(),
        );
        cell.set_ready(1);
        cell.record_success();
        cell.record_failure();
        let status = cell.heartbeat_status();
        assert_eq!(status["cell_id"], "cell-hb");
        assert_eq!(status["missions_completed"], 1);
        assert_eq!(status["missions_failed"], 1);
        assert!(status["trust_score"].as_f64().is_some_and(|t| t < 1.0));
    }

    #[test]
    fn cell_bind_addr_with_and_heartbeat_response_are_deterministic() {
        assert_eq!(
            cell_bind_addr_with(cell_ports::GMCP, Some(" 127.0.0.1:29999 "), Some("250"))
                .to_string(),
            "127.0.0.1:29999"
        );
        assert_eq!(
            cell_bind_addr_with(cell_ports::GMCP, None, Some("250")).to_string(),
            "127.0.0.1:19340"
        );
        assert_eq!(
            cell_bind_addr_with(cell_ports::GMCP, Some("not-a-socket-addr"), Some("250"))
                .to_string(),
            "127.0.0.1:19340"
        );
        assert_eq!(
            cell_bind_addr_with(cell_ports::GMCP, None, Some("65535")).port(),
            u16::MAX
        );

        let mut cell = SwarmCell::new(
            "cell-hb-resp".to_string(),
            SwarmRole::ToolDriver,
            "tcp://127.0.0.1:19092".to_string(),
        );
        cell.set_ready(42);
        cell.record_success();
        let req = SyscallRequest {
            id: "req-hb-77".to_string(),
            caller_id: "daemon".to_string(),
            op: super::super::syscall::SyscallOp::Heartbeat,
            token: None,
            workspace: None,
            payload: serde_json::Value::Null,
            timestamp: 42,
        };
        let resp = cell.heartbeat_response(&req);
        assert_eq!(resp.id, "req-hb-77");
        assert_eq!(resp.status, SyscallStatus::Success);
        assert_eq!(resp.data["cell_id"], "cell-hb-resp");
        assert_eq!(resp.data["missions_completed"], 1);
    }

    #[test]
    fn register_capability_is_idempotent_and_trust_score_floors_at_point_one() {
        let mut cell = SwarmCell::new(
            "cell-cap-floor".to_string(),
            SwarmRole::ToolDriver,
            "tcp://127.0.0.1:0".to_string(),
        );
        cell.register_capability("cap-alpha");
        cell.register_capability("cap-alpha");
        assert_eq!(cell.manifest.capabilities, vec!["cap-alpha".to_string()]);

        for _ in 0..50 {
            cell.record_failure();
        }
        assert!((cell.manifest.trust_score - 0.1).abs() < 1e-6);
        assert_eq!(cell.missions_failed, 50);
    }
}

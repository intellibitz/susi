//! Vendored `susi_core` subset — canonical copies live in
//! `crates/susi-core/vendor_template/susi_core/`.
//!
//! Same public API as the `susi-core` crate, but the process-wide singletons
//! are backed by the `<cache>/bus/<pid>/` rendezvous (`plane_bus_ipc`),
//! file-backed state (`broker`, `mac_policy` grants/mode), and a receipts
//! inbox (`capture`), so independent vendored copies in one process
//! interoperate with each other and with the daemon's real `susi_core`.
//! Leaf modules (`susi_error`, `susi_paths`, `susi_config`, `susi_sandbox`,
//! `susi_native`) are referenced at the consumer's crate root — vendor them
//! first.

pub mod agent_tx;
pub mod agent_types;
pub mod broker;
pub mod bus;
pub mod capture;
pub mod commit_log;
pub mod context_graph;
pub mod evidence;
pub mod intent_bus;
pub mod mac_policy;
pub mod manifold;
pub mod mcp_client;
pub mod net_guard;
pub mod plane_bus;
pub mod plane_bus_ipc;
pub mod provider;
pub mod queue;
pub mod receipt_archive;
pub mod registry;
pub mod registry_ipc;
pub mod service_table;
pub mod task_manager;
pub mod telemetry;
pub mod truth;

// Leaf modules vendored at the consumer's crate root, re-exported so
// `susi_core::susi_error::…` / `susi_core::redact::…` call sites resolve
// unchanged against the local vendored module.
pub use crate::susi_error;
pub use crate::susi_error::redact;

// Mirror of `susi_core`'s root re-exports, restricted to modules carried by
// this vendored tree — keeps `susi_core::{NetGuard, …}` paths stable across
// consumers.
pub use agent_tx::{AgentTransaction, TxManager, TxStatus};
pub use agent_types::{
    AgentProfile, DiscoverableAsset, GawdAgent, GawdAgentInfo, HighDensityContextStore,
    MissionBlackboard, SwarmBlackboard,
};
pub use broker::{IpcBroker, IpcMessage, PermissionGrant, PermissionRequest, PermissionScope};
pub use capture::{EvidenceSession, GroundedAnswer, ReceiptCitation, ToolReceipt};
pub use context_graph::ContextGraph;
pub use evidence::{Claim, EvidenceAssessment, EvidenceRecord, EvidenceSource};
pub use intent_bus::{IntentBus, IntentKind, IntentMatch, IntentMessage};
pub use mac_policy::{CapabilityToken, MacPolicy, PrivacyMode};
pub use net_guard::{NetGuard, RateLimiter};
pub use plane_bus::agents::AgentMetaRegistry;
pub use plane_bus::{PlaneBus, PlaneHandler};
pub use provider::Provider;
pub use receipt_archive::{ArchivedReceipt, ReceiptArchive, ARCHIVE_REL, ARCHIVE_SCHEMA};
pub use registry::{AgentCapability, CapabilityRegistry, Tool};
pub use task_manager::{
    IntentTelemetryProfile, SwarmTaskManager, TaskHandle, TaskRecord, TaskStatus,
    TelemetryHistoryStore,
};
pub use telemetry::{BatteryInfo, TelemetrySnapshot, ThermalZone};
pub use truth::TruthTransformer;

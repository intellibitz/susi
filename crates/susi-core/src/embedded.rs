//! Canonical source mount for crates that embed `susi-core` without a Cargo edge.
//!
//! The modules remain inside each consumer's `susi_core` namespace, preserving
//! its independent types and process-local statics while compiling exactly one
//! checked-in implementation of every contract.

#[path = "a2a_wire.rs"]
pub mod a2a_wire;
#[path = "agent_tx.rs"]
pub mod agent_tx;
#[path = "agent_types.rs"]
pub mod agent_types;
#[path = "broker.rs"]
pub mod broker;
#[path = "bus.rs"]
pub mod bus;
#[path = "capture.rs"]
pub mod capture;
#[path = "commit_log.rs"]
pub mod commit_log;
#[path = "context_graph.rs"]
pub mod context_graph;
#[path = "evidence.rs"]
pub mod evidence;
#[path = "inference_wire.rs"]
pub mod inference_wire;
#[path = "intent_bus.rs"]
pub mod intent_bus;
#[path = "mac_policy.rs"]
pub mod mac_policy;
#[path = "manifold.rs"]
pub mod manifold;
#[path = "mcp_client.rs"]
pub mod mcp_client;
#[path = "net_guard.rs"]
pub mod net_guard;
#[path = "plane_bus.rs"]
pub mod plane_bus;
#[path = "plane_bus_ipc.rs"]
pub mod plane_bus_ipc;
#[path = "provider.rs"]
pub mod provider;
#[path = "queue.rs"]
pub mod queue;
#[path = "receipt_archive.rs"]
pub mod receipt_archive;
#[path = "registry.rs"]
pub mod registry;
#[path = "registry_ipc.rs"]
pub mod registry_ipc;
#[path = "service_table.rs"]
pub mod service_table;
#[path = "task_manager.rs"]
pub mod task_manager;
#[path = "telemetry.rs"]
pub mod telemetry;
#[path = "truth.rs"]
pub mod truth;

pub use crate::susi_error;
pub use crate::susi_error::redact;
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

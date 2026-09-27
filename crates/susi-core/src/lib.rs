#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! # susi-core
//!
//! Core types for the susi agent substrate: Evidence (2), Truth (3), and the
//! CapabilityRegistry for Pluggable (4) providers, agents, and tools.
//!
//! Public claims that hold in source today: mission finals are evidence-gated;
//! `TruthTransformer` accepts absolute sources only (live ledger citations,
//! compiled binary reads, native verified receipts — models never certify);
//! providers/agents/MCP mount behind one catalog for the backends we ship
//! (not an unbounded “any model” guarantee). Swarm consensus, HMAC audit,
//! Wasm/Docker sandbox, reflex distillation, and MCP provisioning live in
//! sibling crates (`susi-gawd`, `susi-sandbox`, `susi-gmcp`, …).

// Self-alias: `crate::susi_core::<module>` resolves inside this crate and,
// through each consumer's `pub use susi_core;`, in every crate that depends
// on it — so one module path reads the same everywhere in the workspace.
pub use self as susi_core;

pub use susi_error;

pub use susi_config;

pub use susi_abi;

pub mod abi_bridge;

pub mod a2a_wire;
pub mod agent_tx;
pub mod agent_types;
pub mod broker;
pub mod bus;
pub mod capture;
pub mod commit_log;
pub mod context_graph;
pub mod evidence;
pub mod inference_wire;
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
pub use crate::susi_error::redact;
pub mod bounded_cmd;
#[cfg(test)]
#[path = "tests/bounded_cmd.rs"]
mod bounded_cmd_tests;
pub mod bounded_io;
#[cfg(test)]
#[path = "tests/bounded_io.rs"]
mod bounded_io_tests;
pub mod registry;
pub mod registry_ipc;
pub mod service_table;
pub mod task_manager;
pub mod telemetry;
#[cfg(test)]
#[path = "tests/telemetry.rs"]
mod telemetry_tests;
pub mod truth;

// Top-level exports for the fundamental susi-core types
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
pub use receipt_archive::{
    ArchivedReceipt, ReceiptArchive, StagingHealth, ARCHIVE_REL, ARCHIVE_SCHEMA,
};
pub use registry::{AgentCapability, CapabilityRegistry, Tool};
pub use task_manager::{
    IntentTelemetryProfile, SwarmTaskManager, TaskHandle, TaskRecord, TaskStatus,
    TelemetryHistoryStore,
};
pub use telemetry::{BatteryInfo, TelemetrySnapshot, ThermalZone};
pub use truth::TruthTransformer;

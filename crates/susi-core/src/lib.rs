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

pub mod agent_tx;
pub mod broker;
pub mod bus;
pub mod capture;
pub mod context_graph;
pub mod evidence;
pub mod intent_bus;
pub mod mac_policy;
pub mod manifold;
pub mod provider;
pub mod queue;
pub mod receipt_archive;
pub mod redact;
pub mod registry;
pub mod telemetry;
pub mod truth;

// Top-level exports for the fundamental susi-core types
pub use agent_tx::{AgentTransaction, TxManager, TxStatus};
pub use broker::{IpcBroker, IpcMessage, PermissionGrant, PermissionRequest, PermissionScope};
pub use capture::{EvidenceSession, GroundedAnswer, ReceiptCitation, ToolReceipt};
pub use context_graph::ContextGraph;
pub use evidence::{Claim, EvidenceAssessment, EvidenceRecord, EvidenceSource};
pub use intent_bus::{IntentBus, IntentKind, IntentMatch, IntentMessage};
pub use mac_policy::{CapabilityToken, MacPolicy, PrivacyMode};
pub use provider::Provider;
pub use receipt_archive::{ArchivedReceipt, ReceiptArchive, ARCHIVE_REL, ARCHIVE_SCHEMA};
pub use registry::{AgentCapability, CapabilityRegistry, Tool};
pub use telemetry::{BatteryInfo, TelemetrySnapshot, ThermalZone};
pub use truth::TruthTransformer;

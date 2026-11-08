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

pub mod a2a_wire;
pub mod adapter_upgrade;
pub mod agent_tx;
pub mod agent_types;
pub mod audit_export;
pub mod broker;
pub mod bus;
pub mod capability_bus;
pub mod capture;
pub mod commit_log;
pub mod commit_log_report;
pub mod context_graph;
pub mod egress;
pub mod emergency_stop;
pub mod evidence;
pub mod intent_bus;
pub mod intent_plan;
pub mod mac_policy;
pub mod manifold;
pub mod mcp_client;
pub mod memory_provenance;
pub mod mission_trace;
pub mod model_health;
pub mod net_guard;
pub mod parallel;
pub mod peer_admin;
pub mod peer_probe;
pub mod peer_roster;
pub mod plain_http;
pub mod plane_bus;
pub mod plane_bus_ipc;
pub mod progress;
pub mod provider;
pub mod queue;
pub mod receipt_archive;
pub mod untrusted_content;
pub mod zc_egress_allowlist;
pub use crate::susi_error::redact;
pub mod bounded_cmd;
pub mod bounded_io;
pub mod registry;
pub mod registry_ipc;
pub mod self_build;
pub mod service_table;
pub mod substrate_hygiene;
pub mod task_manager;
pub mod telemetry;
pub mod truth;
pub mod verification;

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

#[cfg(test)]
#[path = "tests/audit_action_choke_point.rs"]
mod audit_action_choke_point_tests;
#[cfg(test)]
#[path = "tests/capability_bus.rs"]
mod capability_bus_tests;
#[cfg(test)]
#[path = "tests/vc_200_001.rs"]
mod vc_200_001_tests;
#[cfg(test)]
#[path = "tests/vc_201_074_mastery.rs"]
mod vc_201_074_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_080_mastery.rs"]
mod vc_201_080_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_081_mastery.rs"]
mod vc_201_081_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_089_mastery.rs"]
mod vc_201_089_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_089.rs"]
mod vc_201_089_tests;

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn receipt_archive_constants_expose_canonical_relative_path_and_schema() {
        assert!(!std::hint::black_box(ARCHIVE_REL).is_empty());
        assert!(!std::hint::black_box(ARCHIVE_SCHEMA).is_empty());
    }

    #[test]
    fn emergency_stop_running_task_and_stop_status_construct_via_crate_root() {
        let task = emergency_stop::RunningTask::new("t-1", true, false, "fleet");
        assert_eq!(task.id, "t-1");
        assert!(task.cancellable);
        let status = emergency_stop::StopStatus::default();
        assert!(status.stop.is_none());
    }

    #[test]
    fn discoverable_asset_and_agent_profile_roundtrip_json_via_crate_root() {
        let asset = DiscoverableAsset {
            tier: "core".into(),
            name: "planner".into(),
            provider: "susi".into(),
            url: "local://planner".into(),
        };
        let json = serde_json::to_string(&asset).expect("serialize");
        let decoded: DiscoverableAsset = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.name, "planner");
    }

    #[test]
    fn self_alias_susi_core_resolves_error_and_emergency_stop_types() {
        let stop = susi_core::emergency_stop::EmergencyStop {
            scope: "fleet".into(),
            reason: "drill".into(),
            active: true,
        };
        assert!(stop.active);
    }

    #[test]
    fn emergency_stop_clone_and_debug_work_via_crate_root() {
        let stop = emergency_stop::EmergencyStop {
            scope: "node-1".into(),
            reason: "maintenance".into(),
            active: false,
        };
        let cloned = stop.clone();
        assert_eq!(cloned.scope, "node-1");
        assert!(!format!("{cloned:?}").is_empty());
    }
}

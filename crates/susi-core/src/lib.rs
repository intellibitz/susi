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
//! Generic protocol contracts, authority, transport and socket machinery.
//! Mission evidence, context, truth and capability-registry implementations
//! live in the commercial runtime, which reuses these protocol types by identity.

// Self-alias: `crate::susi_core::<module>` resolves inside this crate and,
// through each consumer's `pub use susi_core;`, in every crate that depends
// on it — so one module path reads the same everywhere in the workspace.
pub use self as susi_core;

pub use susi_error;

pub use susi_config;

pub use susi_abi;

pub mod a2a_wire;
/// Compatibility path for the GAWD Agent Client Protocol.
pub use susi_gawd_acp as acp;
pub mod agent_types;
pub mod audit_export;
pub mod bus;
pub mod capability_bus;
pub mod commit_log;
pub mod delegation;
pub mod egress;
pub mod emergency_stop;
pub use susi_config::mac_policy;
pub mod mcp_client;
pub mod net_guard;
pub mod pillars;
pub use susi_config::plain_http;
pub mod plane_bus;
pub mod plane_bus_ipc;
pub use susi_config::progress;
pub mod provider;
pub mod untrusted_content;
pub mod zc_egress_allowlist;
pub use crate::susi_error::redact;
pub mod bounded_cmd;
pub mod bounded_io;
pub mod native_board;
pub mod scoped_tools;
pub mod self_build;

// Top-level exports for the fundamental susi-core types
pub use acp::{
    AcpCapabilities, AcpContentBlock, AcpDiscoveryDocument, AcpHistoryBounds, AcpInferenceMessage,
    AcpMessageRole, AcpSessionMessage, AcpSessionSummary, AcpSocketClient, AcpStopReason,
    AcpToolCallMeta, AcpTurnEvent, AcpTurnResult, ACP_PROTOCOL_VERSION, ACP_SESSION_ID_HEADER,
};
pub use agent_types::{
    AgentProfile, DiscoverableAsset, GawdAgent, GawdAgentInfo, HighDensityContextStore,
    MissionBlackboard, SwarmBlackboard,
};
pub use mac_policy::{CapabilityToken, MacPolicy, PrivacyMode};
pub use net_guard::{NetGuard, RateLimiter};
pub use plane_bus::agents::AgentMetaRegistry;
pub use plane_bus::{PlaneBus, PlaneHandler};
pub use provider::Provider;

#[cfg(test)]
#[path = "tests/audit_action_choke_point.rs"]
mod audit_action_choke_point_tests;
#[cfg(test)]
#[path = "tests/capability_bus.rs"]
mod capability_bus_tests;
#[cfg(test)]
#[path = "tests/delegation.rs"]
mod delegation_tests;
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
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

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

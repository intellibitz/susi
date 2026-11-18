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
pub mod commit_log;
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
pub use plane_bus::agents::AgentMetaRegistry;
pub use plane_bus::{PlaneBus, PlaneHandler};
pub use provider::Provider;

#[cfg(test)]
#[path = "tests/audit_action_choke_point.rs"]
mod audit_action_choke_point_tests;
#[cfg(test)]
#[path = "tests/vc_201_074_mastery.rs"]
mod vc_201_074_mastery_tests;

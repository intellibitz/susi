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

pub use self as susi_core;
pub use susi_abi;
pub use susi_config;
pub use susi_error;

/// Compatibility path for the GAWD Agent Client Protocol.
pub use susi_gawd_acp as acp;
pub mod agent_types;
pub mod bounded_cmd;
pub mod bus;
pub mod commit_log;
pub mod plane_bus;
pub mod plane_bus_ipc;
pub use crate::susi_error::redact;
pub use susi_config::progress;
pub mod scoped_tools;
pub mod self_build;
pub mod zc_egress_allowlist;

pub use acp::{
    AcpCapabilities, AcpContentBlock, AcpDiscoveryDocument, AcpHistoryBounds, AcpInferenceMessage,
    AcpMessageRole, AcpSessionMessage, AcpSessionSummary, AcpSocketClient, AcpStopReason,
    AcpToolCallMeta, AcpTurnEvent, AcpTurnResult, ACP_PROTOCOL_VERSION, ACP_SESSION_ID_HEADER,
};
pub use agent_types::{
    AgentProfile, DiscoverableAsset, GawdAgent, GawdAgentInfo, HighDensityContextStore,
    MissionBlackboard, SwarmBlackboard,
};
pub use plane_bus::{PlaneBus, PlaneHandler};

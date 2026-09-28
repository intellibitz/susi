#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod plane_handler;

pub mod registry;

/// Every test that repoints the process-wide `SUSI_HOME` holds this one
/// lock. `registry` and `plane_handler` each had their own, so a plane test
/// could register agents into the directory a registry test was counting.
#[cfg(test)]
pub(crate) static SUSI_HOME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use registry::AgentMetaRegistry;
pub use susi_core::agent_types::{
    AgentProfile, DiscoverableAsset, GawdAgent, GawdAgentInfo, HighDensityContextStore,
    MissionBlackboard, SwarmBlackboard,
};
pub use susi_vendor_agents::external;

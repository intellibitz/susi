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

//! GAWD **A2A** tier: the Agent2Agent wire protocol, served in-tree
//! ([`protocol`]) on core's std HTTP/1.1 server (`susi_http_transport::server`)
//! with its handlers on tokio.
//!
//! Depends on [`susi_gawd_agents`] (`GawdAgentFleet`), never on swarm or the
//! host `susi-gawd` crate. `susi-daemon` serves it on the A2A port; `susi-gawd`
//! does not depend on it, so the gawd chain never waits on the HTTP stack.

pub use susi_config;
pub use susi_core;
pub use susi_error;
/// GAWD Agent Client Protocol and its transports.
pub use susi_gawd_acp as acp;
pub use susi_gawd_agents::{
    accountability, admin_hooks, agents, axiom, brain, dag_hooks, external_peers, goal_shape, pkb,
    safety, scheduler, security, self_core, system_observe,
};
pub use susi_sandbox_client as susi_sandbox;

pub use agents::{GawdAgentFleet, GawdAgentInfo, HighDensityContextStore};
pub use self_core::AlphaSelf;

pub mod capabilities;
pub mod executor;
pub mod protocol;
pub mod server;
pub mod task_store;

pub use capabilities::GawdCapabilities;
pub use executor::GawdA2AExecutor;

#[cfg(test)]
#[path = "tests/acp_gawd.rs"]
mod acp_gawd_tests;
#[cfg(test)]
#[path = "tests/board_fixture.rs"]
pub(crate) mod test_board_fixture;
#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
#[cfg(test)]
mod mastery_tests {
    pub mod vc_201_087_mastery;
    pub mod vc_201_092_mastery;
}

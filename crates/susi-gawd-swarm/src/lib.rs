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

//! GAWD **swarm** tier: AMA / AMAS orchestration, MissionDag, cloud recovery.
//!
//! Depends on [`susi_gawd_agents`] only. Must not import `ra2a` or `susi-gawd`.

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod ama;
pub mod amas;
pub(crate) mod cloud_recovery;
pub mod dag;
pub mod host_hooks;
pub mod peer_registry;

pub use susi_gawd_agents::{
    accountability, admin_hooks, agents, axiom, brain, dag_hooks, external_peers, goal_shape, pkb,
    safety, scheduler, security, self_core, system_observe,
};

pub use agents::{GawdAgentFleet, GawdAgentInfo, HighDensityContextStore};
pub use axiom::AxiomSubstrate;
pub use brain::AlphaBrainContext;
pub use self_core::AlphaSelf;

pub use ama::SusiMasterAgent;
pub use dag::{MissionDag, SwarmDag};

/// Wire the MissionDag post-swarm hook into the agents leaf.
///
/// Call once from the host (`susi-gawd`) at load — idempotent first-wins.
pub fn init() {
    susi_gawd_agents::dag_hooks::init(dag::dispatch_mission_dag);
}

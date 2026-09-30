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

pub mod agent_routing;
pub mod ama;
pub mod amas;
pub mod anti_entropy;
pub mod cancel_propagate;
pub mod capability_market;
pub mod cloud_lockout;
pub(crate) mod cloud_recovery;
pub mod coordinator_fence;
pub mod dag;
pub mod deliberation;
pub mod fair_queue;
pub mod fault_matrix;
pub mod federation_compat;
pub mod host_hooks;
pub mod identity_revoke;
pub mod independent_verify;
pub mod joint_consensus;
pub mod locality_placement;
pub mod membership_model;
pub mod mission_persist;
pub mod mission_resume;
pub mod model_placement;
pub mod node_enrollment;
pub mod peer_registry;
pub mod resource_schedule;
pub mod role_select;
pub mod side_effects;
pub mod snapshot_bootstrap;
pub mod swarm_chaos;
pub mod task_lease;
pub mod wan_peers;
pub mod workspace_txn;
pub mod zc_agent_pick;
pub mod zc_lan_cluster;
pub mod zc_nat_auto;
pub mod zc_threshold_tuning;
pub mod zc_trust_store;

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

#[cfg(test)]
#[path = "tests/mission_dag_persist_wiring.rs"]
mod mission_dag_persist_wiring_tests;
#[cfg(test)]
#[path = "tests/side_effects_dispatch_wiring.rs"]
mod side_effects_dispatch_wiring_tests;
#[cfg(test)]
#[path = "tests/swarm_chaos.rs"]
mod swarm_chaos_tests;
#[cfg(test)]
#[path = "tests/task_lease_dispatch_wiring.rs"]
mod task_lease_dispatch_wiring_tests;
#[cfg(test)]
#[path = "tests/vc_201_021.rs"]
mod vc_201_021_tests;
#[cfg(test)]
#[path = "tests/vc_201_022.rs"]
mod vc_201_022_tests;
#[cfg(test)]
#[path = "tests/vc_201_023.rs"]
mod vc_201_023_tests;
#[cfg(test)]
#[path = "tests/vc_201_024.rs"]
mod vc_201_024_tests;
#[cfg(test)]
#[path = "tests/vc_201_025.rs"]
mod vc_201_025_tests;
#[cfg(test)]
#[path = "tests/vc_201_026.rs"]
mod vc_201_026_tests;
#[cfg(test)]
#[path = "tests/vc_201_027.rs"]
mod vc_201_027_tests;
#[cfg(test)]
#[path = "tests/vc_201_028.rs"]
mod vc_201_028_tests;
#[cfg(test)]
#[path = "tests/vc_201_029.rs"]
mod vc_201_029_tests;
#[cfg(test)]
#[path = "tests/vc_201_030.rs"]
mod vc_201_030_tests;
#[cfg(test)]
#[path = "tests/vc_201_031.rs"]
mod vc_201_031_tests;
#[cfg(test)]
#[path = "tests/vc_201_032.rs"]
mod vc_201_032_tests;
#[cfg(test)]
#[path = "tests/vc_201_033.rs"]
mod vc_201_033_tests;
#[cfg(test)]
#[path = "tests/vc_201_034.rs"]
mod vc_201_034_tests;
#[cfg(test)]
#[path = "tests/vc_201_035.rs"]
mod vc_201_035_tests;
#[cfg(test)]
#[path = "tests/vc_201_036.rs"]
mod vc_201_036_tests;
#[cfg(test)]
#[path = "tests/vc_201_037.rs"]
mod vc_201_037_tests;
#[cfg(test)]
#[path = "tests/vc_201_038.rs"]
mod vc_201_038_tests;
#[cfg(test)]
#[path = "tests/vc_201_039.rs"]
mod vc_201_039_tests;
#[cfg(test)]
#[path = "tests/vc_201_040.rs"]
mod vc_201_040_tests;

/// Wire the MissionDag post-swarm hook into the agents leaf.
///
/// Call once from the host (`susi-gawd`) at load — idempotent first-wins.
pub fn init() {
    susi_gawd_agents::dag_hooks::init(dag::dispatch_mission_dag);
}

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

//! GAWD **agents** tier: fleet primitives, native agents, peers, governance detectors.
//!
//! Leaf crate in the GAWD DAG — must not depend on `susi-gawd-swarm`,
//! `susi-gawd-a2a`, or `susi-gawd`. Host admin actions and MissionDag execution
//! reach this crate only through [`admin_hooks`] / [`dag_hooks`].

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

// `susi_core::susi_error` is the same `susi-error` crate this one depends
// on, so no `From` bridge is needed — `?` converts trivially.

pub mod accountability;
pub mod admin_hooks;
pub mod agents;
pub mod axiom;
pub mod brain;
pub mod cloud_intent;
pub mod dag_hooks;
pub mod external_peers;
pub mod goal_shape;
pub(crate) use susi_vendor_web::live_search;
pub mod pkb;
pub mod safety;
pub mod scheduler;
pub mod security;
pub mod self_core;
pub mod system_observe;

pub use agents::{
    AgentMetaRegistry, AgentProfile, DiscoverableAsset, GawdAgent, GawdAgentFleet, GawdAgentInfo,
    HighDensityContextStore, MissionBlackboard, SwarmBlackboard,
};
pub use axiom::AxiomSubstrate;
pub use brain::AlphaBrainContext;
pub use self_core::AlphaSelf;

/// In-memory `agents.` plane handler for unit tests. The concrete meta
/// registry lives in `susi-agents` — a crate this leaf cannot depend on — so
/// without this stub `AgentMetaRegistry` facade calls hit an unwired bus and
/// silently no-op. Seeded with the single profile the scheduler tests need;
/// `is_core` is deliberately false so the seed never auto-recruits itself
/// into `synthesize_fleet` results for unrelated goals.
#[cfg(test)]
pub(crate) mod test_plane;

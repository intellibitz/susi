//! GAWD **agents** tier: fleet primitives, native agents, peers, governance detectors.
//!
//! Leaf crate in the GAWD DAG — must not depend on `susi-gawd-swarm`,
//! `susi-gawd-a2a`, or `susi-gawd`. Host admin actions and MissionDag execution
//! reach this crate only through [`admin_hooks`] / [`dag_hooks`].

pub mod accountability;
pub mod admin_hooks;
pub mod agents;
pub mod axiom;
pub mod brain;
pub mod dag_hooks;
pub mod external_peers;
pub(crate) mod live_search;
pub mod pkb;
pub mod safety;
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

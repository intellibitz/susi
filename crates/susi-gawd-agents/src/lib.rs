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
pub mod adapter_upgrade;
pub mod admin_hooks;
pub mod agent_meta_registry;
pub mod agents;
pub mod agents_plane_handler;
pub mod axiom;
pub mod brain;
pub mod cloud_budget;
pub mod cloud_intent;
pub mod dag_hooks;
pub mod external;
pub mod external_peers;
pub mod goal_shape;
pub(crate) mod live_search;
pub mod pkb;
pub mod safety;
pub mod scheduler;
pub mod security;
pub mod self_core;
pub mod system_observe;

#[cfg(test)]
pub(crate) static SUSI_HOME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use agents::{
    AgentMetaRegistry, AgentProfile, DiscoverableAsset, GawdAgent, GawdAgentFleet, GawdAgentInfo,
    HighDensityContextStore, MissionBlackboard, SwarmBlackboard,
};
pub use axiom::AxiomSubstrate;
pub use brain::AlphaBrainContext;
pub use self_core::AlphaSelf;

/// In-memory `agents.` plane handler for unit tests.
#[cfg(test)]
pub(crate) mod test_plane;

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn discoverable_asset_preserves_tier_name_provider_and_url() {
        let asset = DiscoverableAsset {
            tier: "agent".to_string(),
            name: "Aider".to_string(),
            provider: "oss".to_string(),
            url: "https://aider.chat".to_string(),
        };
        assert_eq!(asset.name, "Aider");
        assert_eq!(asset.tier, "agent");
    }

    #[test]
    fn high_density_context_store_default_creates_clean_instance() {
        let store = HighDensityContextStore::default();
        let debug_repr = format!("{store:?}");
        assert!(!debug_repr.is_empty());
    }

    #[test]
    fn gawd_agent_info_constructs_and_preserves_provider_and_rank() {
        let info = GawdAgentInfo {
            name: "Verifier".to_string(),
            provider: "susi".to_string(),
            url: "local://verifier".to_string(),
            rank: 1.0,
        };
        assert_eq!(info.name, "Verifier");
        assert_eq!(info.provider, "susi");
        assert!((info.rank - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn susi_error_reexport_builds_governance_error() {
        let err = susi_error::EaiError::governance("policy test");
        assert!(err.to_string().contains("policy test"));
    }

    #[test]
    fn alpha_self_and_axiom_substrate_reexports_are_accessible_from_crate_root() {
        assert!(!AlphaSelf::VERSION.is_empty());
        let (rules, comps) = AxiomSubstrate::ingest_constitution(std::path::Path::new("."));
        assert!(!rules.is_empty());
        assert!(!comps.is_empty());
    }
}

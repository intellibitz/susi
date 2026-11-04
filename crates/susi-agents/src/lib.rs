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

pub mod adapter_upgrade;

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

#[cfg(test)]
mod surface_tests {
    use super::*;
    use std::any::TypeId;

    #[test]
    fn root_re_exports_are_the_defining_types() {
        assert_eq!(
            TypeId::of::<AgentMetaRegistry>(),
            TypeId::of::<registry::AgentMetaRegistry>()
        );
        assert_eq!(
            TypeId::of::<AgentProfile>(),
            TypeId::of::<susi_core::agent_types::AgentProfile>()
        );
        assert_eq!(
            TypeId::of::<GawdAgentInfo>(),
            TypeId::of::<susi_core::agent_types::GawdAgentInfo>()
        );
        assert_eq!(
            TypeId::of::<MissionBlackboard>(),
            TypeId::of::<susi_core::agent_types::MissionBlackboard>()
        );
        assert_eq!(
            TypeId::of::<SwarmBlackboard>(),
            TypeId::of::<susi_core::agent_types::SwarmBlackboard>()
        );
    }

    #[test]
    fn the_shared_susi_home_lock_is_acquirable_and_releases() {
        // One process-wide lock serializes every SUSI_HOME-repointing test;
        // a poisoned lock from an earlier failing test must not wedge it.
        {
            let _held = SUSI_HOME_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            assert!(
                SUSI_HOME_TEST_LOCK.try_lock().is_err(),
                "lock must be exclusive"
            );
        }
        let reacquired = SUSI_HOME_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        drop(reacquired);
    }

    #[test]
    fn discoverable_asset_and_high_density_context_store_re_exports_match_core_types() {
        assert_eq!(
            TypeId::of::<DiscoverableAsset>(),
            TypeId::of::<susi_core::agent_types::DiscoverableAsset>()
        );
        assert_eq!(
            TypeId::of::<HighDensityContextStore>(),
            TypeId::of::<susi_core::agent_types::HighDensityContextStore>()
        );
    }

    #[test]
    fn re_exported_crate_modules_and_vendor_external_types_resolve_through_root() {
        assert_eq!(
            TypeId::of::<external::Adapter>(),
            TypeId::of::<susi_vendor_agents::external::Adapter>()
        );
        assert_eq!(
            TypeId::of::<external::AgentManager>(),
            TypeId::of::<susi_vendor_agents::external::AgentManager>()
        );
        assert_eq!(
            TypeId::of::<crate::susi_error::EaiError>(),
            TypeId::of::<susi_error::EaiError>()
        );
        assert_eq!(
            TypeId::of::<crate::susi_config::SusiConfig>(),
            TypeId::of::<susi_config::SusiConfig>()
        );
    }

    #[test]
    fn agent_meta_registry_global_singleton_returns_stable_reference() {
        let a = AgentMetaRegistry::global() as *const AgentMetaRegistry;
        let b = AgentMetaRegistry::global() as *const AgentMetaRegistry;
        assert_eq!(a, b);
    }
}

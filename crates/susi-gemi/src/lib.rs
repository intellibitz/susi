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

//! GEMI — universal inference & model substrate.
//!
//! # Two-tier layout
//!
//! | Tier | Crate | Responsibility |
//! |------|-------|----------------|
//! | **Models** | [`susi_gemi_models`] / [`models`] | Select & provision (lifecycle, ladder, coding catalog) |
//! | **Engines** | [`engines`] | Run inference (backends, HTTP/MCP providers, router) |
//!
//! Engines depends on models. Models must not depend on this crate.
//!
//! Every module has one path: engines under [`engines`], model selection /
//! provisioning under [`models`] (the old flat aliases were migrated away).

pub use susi_abi;

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod engines;

pub use susi_gemi_models as models;

// Cross-cutting surfaces that use both tiers
pub mod plane_handler;

pub mod benchmark;
pub mod brain_explain;
pub mod brain_export;
pub mod capability_matrix;
pub mod coding_models_ext;
pub mod consensus_mode;
pub mod context_assembly;
pub mod credential_scout;
pub mod engine_benchmark;
pub mod frontier_ext;
pub mod key_arbitration;
pub mod local_tool_calls;
pub mod model_preload;
pub mod open_weight_ext;
pub mod openrouter_ext;
pub mod operator_report;
pub mod orchestration;
pub mod primary_election;
pub mod provider_contract;
pub mod pulse;
pub mod rate_limit_scheduler;
pub mod region_eligibility;
pub mod scout_probe;
pub mod scout_schedule;
pub mod seat_provider;
pub mod spend_tracker;
pub mod structured_output;
pub mod thermal_routing;
pub mod tool_call_normalisation;
pub mod usage_accounting;
pub mod worker;
pub mod zc_kv_auto;

#[cfg(test)]
#[path = "tests/availability_not_capability.rs"]
mod availability_not_capability_tests;
#[cfg(test)]
#[path = "tests/billing_mode_marginal_cost.rs"]
mod billing_mode_marginal_cost_tests;
#[cfg(test)]
#[path = "tests/brain_ranking_by_cost.rs"]
mod brain_ranking_by_cost_tests;
#[cfg(test)]
#[path = "tests/brain_scout_probe.rs"]
mod brain_scout_probe_tests;
#[cfg(test)]
#[path = "tests/brain_scout_schedule.rs"]
mod brain_scout_schedule_tests;
#[cfg(test)]
#[path = "tests/budget_ceiling.rs"]
mod budget_ceiling_tests;
#[cfg(test)]
#[path = "tests/cache_first_context.rs"]
mod cache_first_context_tests;
#[cfg(test)]
#[path = "tests/capability_floor.rs"]
mod capability_floor_tests;
#[cfg(test)]
#[path = "tests/capability_matrix_by_class.rs"]
mod capability_matrix_by_class_tests;
#[cfg(test)]
#[path = "tests/delegated_verification_by_risk.rs"]
mod delegated_verification_by_risk_tests;
pub mod eco_compat_gate;
#[cfg(test)]
#[path = "tests/engine_benchmark.rs"]
mod engine_benchmark_tests;
#[cfg(test)]
#[path = "tests/estate_operations_capability_floor.rs"]
mod estate_operations_capability_floor_tests;
#[cfg(test)]
#[path = "tests/expected_cost_with_cache.rs"]
mod expected_cost_with_cache_tests;
#[cfg(test)]
#[path = "tests/model_catalogue_drift.rs"]
mod model_catalogue_drift_tests;
#[cfg(test)]
#[path = "tests/model_health_failure_classes.rs"]
mod model_health_failure_classes_tests;
#[cfg(test)]
#[path = "tests/model_preload.rs"]
mod model_preload_tests;
#[cfg(test)]
#[path = "tests/model_pricing_cache_rates.rs"]
mod model_pricing_cache_rates_tests;
#[cfg(test)]
#[path = "tests/orchestration_topology.rs"]
mod orchestration_topology_tests;
#[cfg(test)]
#[path = "tests/primary_election.rs"]
mod primary_election_tests;
#[cfg(test)]
#[path = "tests/primary_failover_resume.rs"]
mod primary_failover_resume_tests;
#[cfg(test)]
#[path = "tests/primary_without_secondaries.rs"]
mod primary_without_secondaries_tests;
#[cfg(test)]
#[path = "tests/quota_window_accounting.rs"]
mod quota_window_accounting_tests;
#[cfg(test)]
#[path = "tests/ranking_by_marginal_cost.rs"]
mod ranking_by_marginal_cost_tests;
#[cfg(test)]
#[path = "tests/rate_limit_scheduler.rs"]
mod rate_limit_scheduler_tests;
#[cfg(test)]
#[path = "tests/routing_ladder.rs"]
mod routing_ladder_tests;
#[cfg(test)]
#[path = "tests/thermal_routing.rs"]
mod thermal_routing_tests;
// Drives the built-in Candle weight loader, which the slim default leaves out.
#[cfg(all(test, feature = "local-engine"))]
#[path = "tests/vc_201_042_native.rs"]
mod vc_201_042_native_tests;
#[cfg(test)]
#[path = "tests/vc_201_045_mastery.rs"]
mod vc_201_045_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_047_mastery.rs"]
mod vc_201_047_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_048_mastery.rs"]
mod vc_201_048_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_051.rs"]
mod vc_201_051_tests;
#[cfg(test)]
#[path = "tests/vc_201_054.rs"]
mod vc_201_054_tests;
#[cfg(test)]
#[path = "tests/vc_202_001_mastery.rs"]
mod vc_202_001_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_202_020_rate_mastery.rs"]
mod vc_202_020_rate_mastery_tests;
#[cfg(test)]
#[path = "tests/worker_may_be_an_agent.rs"]
mod worker_may_be_an_agent_tests;

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn models_tier_reexport_exposes_default_starters() {
        let starters = models::zc_first_model::default_starters();
        assert_eq!(starters.len(), 2);
    }

    #[test]
    fn engines_capability_hierarchy_is_accessible_from_gemi_root() {
        use engines::capability::ModelCapability;
        assert!(ModelCapability::Basic < ModelCapability::Coding);
        assert!(ModelCapability::Coding < ModelCapability::Reasoning);
    }

    #[test]
    fn engines_capability_of_classifies_opus_as_reasoning_via_root() {
        use engines::capability::{of, ModelCapability};
        assert_eq!(of("claude-opus-4"), ModelCapability::Reasoning);
    }

    #[test]
    fn models_intent_task_complexity_escalates_from_gemi_root() {
        use models::intent::TaskComplexity;
        assert_eq!(TaskComplexity::Trivial.escalate(), TaskComplexity::Simple);
    }
}

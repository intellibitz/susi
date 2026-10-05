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
pub mod action_executor;
pub mod placement_uses_task_class;
pub mod plane_handler;

pub mod batch_api;
pub mod benchmark;
pub mod brain_decay;
pub mod brain_experiments;
pub mod brain_explain;
pub mod brain_exploration;
pub mod brain_export;
pub mod brain_per_workspace;
pub mod coding_models_ext;
pub mod confidence_escalation;
pub mod consensus_mode;
pub mod context_length_routing;
pub mod credential_scout;
pub mod embedding_routing;
pub mod engine_benchmark;
pub mod eval;
pub mod frontier_ext;
pub mod key_arbitration;
pub mod latency_slo;
pub mod learned_classifier;
pub mod model_preload;
pub mod open_weight_ext;
pub mod openrouter_ext;
pub mod prompt_caching;
pub mod provider_contract;
pub mod pulse;
pub mod rate_limit_scheduler;
pub mod region_eligibility;
pub mod region_failover;
pub mod retry_policy;
pub mod scout_probe;
pub mod scout_schedule;
pub mod speculative_routing;
pub mod spend_tracker;
pub mod sse_streaming;
pub mod status_pages;
pub mod structured_output;
pub mod thermal_routing;
pub mod tool_call_normalisation;
pub mod tui_top;
pub mod usage_accounting;
pub mod verifier_escalation;
pub mod vision_routing;
pub mod zc_budget_derived;
pub mod zc_chat_templates;
pub mod zc_cost_from_catalog;
pub mod zc_eos_tokens;
pub mod zc_generation_defaults;
pub mod zc_gpu_asset;
pub mod zc_gpu_build_auto;
pub mod zc_idle_adaptive;
pub mod zc_kv_auto;
pub mod zc_local_first_default;
pub mod zc_preferred_auto;
pub mod zc_speculative_auto;
pub mod zc_timeouts_measured;

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
#[path = "tests/capability_floor.rs"]
mod capability_floor_tests;
pub mod eco_capability_routing;
pub mod eco_compat_gate;
pub mod eco_limits_lookup;
pub mod eco_migration_planner;
pub mod eco_protocol_negotiation;
pub mod eco_qa_cited;
#[cfg(test)]
#[path = "tests/engine_benchmark.rs"]
mod engine_benchmark_tests;
#[cfg(test)]
#[path = "tests/expected_cost_with_cache.rs"]
mod expected_cost_with_cache_tests;
#[cfg(test)]
#[path = "tests/model_catalogue_drift.rs"]
mod model_catalogue_drift_tests;
#[cfg(test)]
#[path = "tests/model_preload.rs"]
mod model_preload_tests;
#[cfg(test)]
#[path = "tests/model_pricing_cache_rates.rs"]
mod model_pricing_cache_rates_tests;
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
#[cfg(test)]
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

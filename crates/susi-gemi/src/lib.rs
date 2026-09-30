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
pub mod consensus_mode;
pub mod context_length_routing;
pub mod embedding_routing;
pub mod engine_benchmark;
pub mod eval;
pub mod frontier_ext;
pub mod latency_slo;
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
pub mod spend_tracker;
pub mod status_pages;
pub mod thermal_routing;
pub mod tui_top;
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
#[path = "tests/model_preload.rs"]
mod model_preload_tests;
#[cfg(test)]
#[path = "tests/rate_limit_scheduler.rs"]
mod rate_limit_scheduler_tests;
#[cfg(test)]
#[path = "tests/spend_tracker.rs"]
mod spend_tracker_tests;
#[cfg(test)]
#[path = "tests/thermal_routing.rs"]
mod thermal_routing_tests;
#[cfg(test)]
#[path = "tests/vc_201_051.rs"]
mod vc_201_051_tests;
#[cfg(test)]
#[path = "tests/vc_201_054.rs"]
mod vc_201_054_tests;

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

//! GEMI **models** tier: catalog, ladder, download, hardware fit, coding-model control plane.
//!
//! This crate decides *which* weights/endpoints are available. It must not depend on
//! `susi-gemi` engines (Candle forward graphs, HTTP providers, routers). Cloud
//! key/endpoint helpers live here for metadata and CLI key storage only.
//!
//! Prefer `susi_gemi::models::…` (re-exported from the engines crate) for call sites
//! that already depend on `susi-gemi`. Depend on this crate directly when only
//! selection/provisioning is needed.

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;
pub use susi_vendor_models::model_license::ModelReadWindow;

pub use susi_vendor_models::{
    cloud, cloud_eligibility, cloud_manage, eco_consistency, eco_profile, eco_query, eco_relations,
    eco_schema, eco_store, eco_taxonomy, frontier, hf_discovery, local_ecosystem, open_weight, openrouter, price_catalog, resource_inventory, runtime_lifecycle,
};
pub mod coding_models;
mod device;
pub(crate) mod download;
pub mod hardware;
pub mod intent;
mod lifecycle;
pub mod model_cache;
pub mod zc_first_model;

pub use lifecycle::*;

/// Write the selected-model override every inference path reads
/// (`selected_model_override.txt` under the config dir).
pub fn set_selected_model_override(model_name: &str) -> Result<String, String> {
    let name = model_name.trim();
    if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
        return Err(format!(
            "invalid model name {model_name:?}: expected a non-empty single-line id"
        ));
    }
    let model_file = susi_paths::SusiDirs::config_dir().join("selected_model_override.txt");
    susi_config::atomic_write_bytes(&model_file, name.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!("Selected active model override set to: '{name}'"))
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn default_starters_contains_audited_small_and_7b_entries() {
        let starters = zc_first_model::default_starters();
        assert_eq!(starters.len(), 2);
        assert!(starters[0].size_bytes < starters[1].size_bytes);
        assert_eq!(starters[0].id, "qwen2.5-0.5b-instruct-q4");
    }

    #[test]
    fn task_complexity_escalates_and_saturates_at_very_complex() {
        use intent::TaskComplexity;
        assert_eq!(TaskComplexity::Trivial.escalate(), TaskComplexity::Simple);
        assert_eq!(
            TaskComplexity::VeryComplex.escalate(),
            TaskComplexity::VeryComplex
        );
    }

    #[test]
    fn task_complexity_target_percentiles_are_monotonically_increasing() {
        use intent::TaskComplexity;
        assert!(
            TaskComplexity::Trivial.target_percentile()
                < TaskComplexity::Moderate.target_percentile()
        );
        assert!(
            TaskComplexity::Moderate.target_percentile()
                < TaskComplexity::Complex.target_percentile()
        );
    }

    #[test]
    fn starter_install_constants_expose_canonical_cli_command_and_source() {
        assert_eq!(
            std::hint::black_box(zc_first_model::STARTER_INSTALL_COMMAND),
            "susi os manage model install starter"
        );
        assert_eq!(
            std::hint::black_box(zc_first_model::STARTER_SOURCE),
            "starter"
        );
    }

    #[test]
    fn audited_starter_matches_first_default_starter_and_has_positive_size() {
        let audited = zc_first_model::audited_starter();
        let starters = zc_first_model::default_starters();
        assert_eq!(audited, starters[0]);
        assert!(audited.size_bytes > 0);
        assert!(!audited.hf_repo.is_empty());
        assert!(audited.hf_file.ends_with(".gguf"));
    }
}

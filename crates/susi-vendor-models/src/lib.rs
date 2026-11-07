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

//! Third-party model-provider integrations: cloud provider catalogs,
//! frontier-model registries, OpenRouter, open-weight registries, and
//! Hugging Face discovery. Re-exported through `susi_gemi_models` so the
//! consumer paths (`susi_gemi_models::openrouter::*`, …) are unchanged.

pub use susi_config;
pub use susi_core;
pub use susi_error;
pub use susi_sandbox_client as susi_sandbox;

pub mod accel_reserve;
pub mod cloud;
pub mod cloud_credentials;
pub mod cloud_eligibility;
pub mod cloud_manage;
pub mod cloud_quota;
pub mod cloud_reconciler;
pub mod default_models;
pub mod eco_consistency;
pub mod eco_profile;
pub mod eco_query;
pub mod eco_relations;
pub mod eco_schema;
pub mod eco_store;
pub mod eco_taxonomy;
pub mod frontier;
pub mod gguf_inspector;
pub mod hf_discovery;
pub mod infer_batch;
pub mod key_rotation;
pub mod llamacpp_launcher;
pub mod local_ecosystem;
pub mod model_deprecation;
pub mod model_license;
pub mod npu_detection;
pub mod offline_bundle;
pub mod open_weight;
pub mod openrouter;
pub mod price_catalog;
pub mod quant_recommender;
pub mod resource_inventory;
pub mod resumable_downloads;
pub mod runtime_lifecycle;
pub mod transactional_download;
pub mod zc_key_sources;

#[cfg(test)]
#[path = "tests/resource_inventory.rs"]
mod resource_inventory_tests;
#[cfg(test)]
#[path = "tests/vc_201_041_mastery.rs"]
mod vc_201_041_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_042.rs"]
mod vc_201_042_tests;
#[cfg(test)]
#[path = "tests/vc_201_043_headroom.rs"]
mod vc_201_043_headroom_tests;
#[cfg(test)]
#[path = "tests/vc_201_043_mastery.rs"]
mod vc_201_043_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_043.rs"]
mod vc_201_043_tests;
#[cfg(test)]
#[path = "tests/vc_201_044_mastery.rs"]
mod vc_201_044_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_044.rs"]
mod vc_201_044_tests;
#[cfg(test)]
#[path = "tests/vc_201_046_mastery.rs"]
mod vc_201_046_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_046.rs"]
mod vc_201_046_tests;
#[cfg(test)]
#[path = "tests/vc_201_050.rs"]
mod vc_201_050_tests;
#[cfg(test)]
#[path = "tests/vc_201_055_mastery.rs"]
mod vc_201_055_mastery_tests;
#[cfg(test)]
#[path = "tests/vc_201_055.rs"]
mod vc_201_055_tests;
#[cfg(test)]
#[path = "tests/vc_202_001_mastery.rs"]
mod vc_202_001_mastery_tests;

/// Write the selected-model override every inference path reads
/// (`selected_model_override.txt` under the config dir). Vendor registries
/// call this directly; `susi_gemi_models::ModelManager::set_selected_model`
/// delegates here so the write contract lives in exactly one place.
pub fn set_selected_model_override(model_name: &str) -> Result<String, String> {
    // An empty name would silently clear the override while reporting
    // it "set", and a control character would corrupt the one-line
    // file every inference reads.
    let name = model_name.trim();
    if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
        return Err(format!(
            "invalid model name {model_name:?}: expected a non-empty single-line id"
        ));
    }
    // Atomic: every inference reads this; an empty read means "no
    // override" and silently reroutes to the default model.
    let model_file = susi_paths::SusiDirs::config_dir().join("selected_model_override.txt");
    susi_config::atomic_write_bytes(&model_file, name.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!("Selected active model override set to: '{name}'"))
}
pub mod eco_engine_capability_map;
pub mod eco_gpu_clouds;
pub mod eco_live_probe;

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn set_selected_model_override_rejects_empty_and_whitespace_names() {
        assert!(set_selected_model_override("").is_err());
        assert!(set_selected_model_override("   \t  ").is_err());
    }

    #[test]
    fn set_selected_model_override_rejects_control_characters() {
        let err =
            set_selected_model_override("gpt-4o\ninjected").expect_err("newline must be rejected");
        assert!(err.contains("expected a non-empty single-line id"));
    }

    #[test]
    fn set_selected_model_override_rejects_overlong_names_above_512_bytes() {
        let long_name = "m".repeat(513);
        assert!(set_selected_model_override(&long_name).is_err());
    }

    #[test]
    fn price_catalog_and_model_read_window_are_accessible_from_vendor_models_root() {
        let _win = model_license::ModelReadWindow::enter();
        let cat = price_catalog::PriceCatalog::empty(1);
        assert_eq!(cat.version, 1);
        assert!(cat.entries.is_empty());
    }

    #[test]
    fn set_selected_model_override_rejects_carriage_return_and_null_bytes() {
        assert!(set_selected_model_override("model\rname").is_err());
        assert!(set_selected_model_override("model\0name").is_err());
    }
}

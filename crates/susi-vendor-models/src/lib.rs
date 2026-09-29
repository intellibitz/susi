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
pub mod cloud_manage;
pub mod eco_anthropic_messages;
pub mod eco_conflicts;
pub mod eco_consistency;
pub mod eco_coverage;
pub mod eco_matrix;
pub mod eco_openai_batch_files;
pub mod eco_openai_chat;
pub mod eco_openai_embed_media;
pub mod eco_openai_realtime;
pub mod eco_openai_responses;
pub mod eco_profile;
pub mod eco_provenance;
pub mod eco_relations;
pub mod eco_schema;
pub mod eco_signed_kb;
pub mod eco_store;
pub mod eco_taxonomy;
pub mod frontier;
pub mod hf_discovery;
pub mod local_ecosystem;
pub mod open_weight;
pub mod openrouter;
pub mod price_catalog;
pub mod resource_inventory;
pub mod signed_catalog;

#[cfg(test)]
#[path = "tests/resource_inventory.rs"]
mod resource_inventory_tests;
#[cfg(test)]
#[path = "tests/vc_201_043.rs"]
mod vc_201_043_tests;

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

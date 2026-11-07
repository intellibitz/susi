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

pub use susi_config::{cloud_eligibility, cloud_quota};

pub mod hf_discovery;
pub mod local_ecosystem;
pub mod model_license;
pub mod resumable_downloads;
pub mod transactional_download;

#[cfg(test)]
#[path = "tests/vc_201_044_mastery.rs"]
mod vc_201_044_mastery_tests;

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
    fn model_read_window_is_accessible_from_vendor_models_root() {
        let _win = model_license::ModelReadWindow::enter();
    }

    #[test]
    fn set_selected_model_override_rejects_carriage_return_and_null_bytes() {
        assert!(set_selected_model_override("model\rname").is_err());
        assert!(set_selected_model_override("model\0name").is_err());
    }
}

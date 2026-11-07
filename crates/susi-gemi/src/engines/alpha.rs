//! Tier-0 classifier surface when Candle is not linked.
//!
//! Projection and fingerprints stay real. Load, train, and predict fail
//! closed so pulse escalates and the reflex trainer restores the claim.

use std::path::Path;
use susi_error::{eai_err as anyhow, EaiResult as Result};

const LOCAL_ENGINE_ABSENT: &str =
    "built-in local model engine is not in this core; rebuild with --features local-engine";

pub(crate) use crate::engines::alpha_avail::action_available;

pub struct SusiAlphaModel;

impl SusiAlphaModel {
    pub const DIM: usize = crate::engines::alpha_pure::DIM;

    pub fn cached(_global_dir: &Path) -> Result<std::sync::Arc<Self>> {
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }

    pub fn load(_global_dir: &Path) -> Result<Self> {
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }

    pub fn train_on_staged_data(_global_dir: &Path) -> Result<String> {
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }

    pub fn train_on_staged_file(_global_dir: &Path, _staged_file: &Path) -> Result<String> {
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }

    pub fn inventory(_global_dir: &Path) -> serde_json::Value {
        serde_json::json!({
            "error": LOCAL_ENGINE_ABSENT,
            "vocabulary_capacity": Self::DIM,
        })
    }

    pub fn get_model_fingerprint(global_dir: &Path) -> String {
        let alpha_filename = crate::susi_sandbox::manager::SusiConfig::load(global_dir)
            .unwrap_or_default()
            .alpha_weights_filename();
        let bundle = global_dir
            .join("models")
            .join(alpha_filename)
            .with_extension("bundle.json");
        if let Ok(meta) = std::fs::metadata(bundle) {
            return match meta.modified() {
                Ok(modified) => format!("{modified:?}"),
                Err(_) => "unknown-mtime".to_string(),
            };
        }
        "missing".to_string()
    }

    pub fn predict_intent(&self, _raw: &str) -> Result<String> {
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }

    pub fn semantic_centroid_projection(
        prompt: &str,
        anchors: Option<&[crate::susi_core::AgentProfile]>,
    ) -> Result<Vec<f32>> {
        crate::engines::alpha_pure::semantic_centroid_projection(prompt, anchors)
    }

    pub fn reflex_features(prompt: &str) -> Vec<f32> {
        crate::engines::alpha_pure::reflex_features(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slim_core_refuses_to_load_or_train_a_reflex() {
        let dir = std::env::temp_dir();
        assert!(SusiAlphaModel::load(&dir).is_err());
        assert!(SusiAlphaModel::cached(&dir).is_err());
        assert!(SusiAlphaModel::train_on_staged_data(&dir).is_err());
        assert_eq!(SusiAlphaModel::get_model_fingerprint(&dir), "missing");
    }

    #[test]
    fn slim_core_still_projects_text() {
        let vec = SusiAlphaModel::semantic_centroid_projection("check status", None).unwrap();
        assert_eq!(vec.len(), SusiAlphaModel::DIM);
        assert!(vec.iter().any(|value| *value != 0.0));
    }
}

//! Tier-0 classifier surface served through [`crate::engines::local_plug::LocalEnginePlug`]
//! (or the built-in pure-Rust reflex engine when compiled with `--features local-engine`).
//!
//! Projection and fingerprints are always active. Load, train, and predict
//! delegate to the active [`LocalEnginePlug`] or [`PlaneBus`] commercial plug,
//! and fail closed only when neither a plug nor `--features local-engine` is active.

use crate::engines::local_plug::{
    call_bus_plug, local_engine_plug, TrainedAlphaReflex, TOPIC_LOCAL_ALPHA_INVENTORY,
    TOPIC_LOCAL_ALPHA_PREDICT, TOPIC_LOCAL_ALPHA_TRAIN,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use susi_error::{eai_err as anyhow, EaiResult as Result};

const LOCAL_ENGINE_ABSENT: &str =
    "local model engine is not attached; register a LocalEnginePlug (susi-gpu-fabric) or build with --features local-engine";

pub(crate) use crate::engines::alpha_avail::action_available;

pub struct SusiAlphaModel {
    global_dir: PathBuf,
    trained: Option<TrainedAlphaReflex>,
}

impl SusiAlphaModel {
    pub const DIM: usize = crate::engines::alpha_pure::DIM;

    pub fn cached(global_dir: &Path) -> Result<std::sync::Arc<Self>> {
        Ok(std::sync::Arc::new(Self::load(global_dir)?))
    }

    pub fn load(global_dir: &Path) -> Result<Self> {
        if let Ok(trained) = TrainedAlphaReflex::load_from_dir(global_dir) {
            return Ok(Self {
                global_dir: global_dir.to_path_buf(),
                trained: Some(trained),
            });
        }
        if local_engine_plug().is_some() {
            let trained = TrainedAlphaReflex::load_from_dir(global_dir).ok();
            if trained.is_some() {
                return Ok(Self {
                    global_dir: global_dir.to_path_buf(),
                    trained,
                });
            }
            // If an external plug is registered on the bus or in-process, allow
            // load to succeed if the plug reports a loaded vocabulary.
            let inv = Self::inventory(global_dir);
            if inv.get("error").is_none() {
                return Ok(Self {
                    global_dir: global_dir.to_path_buf(),
                    trained: None,
                });
            }
        }
        if let Some(Ok(inv)) = call_bus_plug(
            TOPIC_LOCAL_ALPHA_INVENTORY,
            json!({ "global_dir": global_dir.display().to_string() }),
        ) {
            if inv.get("error").is_none() {
                return Ok(Self {
                    global_dir: global_dir.to_path_buf(),
                    trained: None,
                });
            }
        }
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }

    pub fn train_on_staged_data(global_dir: &Path) -> Result<String> {
        let staged = global_dir.join(".susi/distillation_staged.jsonl");
        Self::train_on_staged_file(global_dir, &staged)
    }

    pub fn train_on_staged_file(global_dir: &Path, staged_file: &Path) -> Result<String> {
        if let Some(plug) = local_engine_plug() {
            return plug.alpha_train_on_staged_file(global_dir, staged_file);
        }
        if let Some(res) = call_bus_plug(
            TOPIC_LOCAL_ALPHA_TRAIN,
            json!({
                "global_dir": global_dir.display().to_string(),
                "staged_file": staged_file.display().to_string(),
            }),
        ) {
            let val = res?;
            if let Some(text) = val.get("text").and_then(serde_json::Value::as_str) {
                return Ok(text.to_string());
            }
        }
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }

    pub fn inventory(global_dir: &Path) -> serde_json::Value {
        if let Some(plug) = local_engine_plug() {
            return plug.alpha_inventory(global_dir);
        }
        if let Some(Ok(val)) = call_bus_plug(
            TOPIC_LOCAL_ALPHA_INVENTORY,
            json!({ "global_dir": global_dir.display().to_string() }),
        ) {
            return val;
        }
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

    pub fn predict_intent(&self, raw: &str) -> Result<String> {
        if let Some(trained) = &self.trained {
            return trained.predict_intent(raw);
        }
        if let Some(plug) = local_engine_plug() {
            return plug.alpha_predict_intent(&self.global_dir, raw);
        }
        if let Some(res) = call_bus_plug(
            TOPIC_LOCAL_ALPHA_PREDICT,
            json!({
                "global_dir": self.global_dir.display().to_string(),
                "prompt": raw,
            }),
        ) {
            let val = res?;
            if let Some(action) = val.get("action").and_then(serde_json::Value::as_str) {
                return Ok(action.to_string());
            }
        }
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
    use crate::engines::local_plug::{
        clear_local_engine_plug, register_local_engine_plug, BuiltinLocalEnginePlug,
    };

    #[test]
    fn slim_core_refuses_to_load_or_train_a_reflex() {
        let _guard = crate::engines::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        assert!(SusiAlphaModel::load(dir.path()).is_err());
        assert!(SusiAlphaModel::cached(dir.path()).is_err());
        assert!(SusiAlphaModel::train_on_staged_data(dir.path()).is_err());
        assert_eq!(SusiAlphaModel::get_model_fingerprint(dir.path()), "missing");
    }

    #[test]
    fn slim_core_still_projects_text() {
        let vec = SusiAlphaModel::semantic_centroid_projection("check status", None).unwrap();
        assert_eq!(vec.len(), SusiAlphaModel::DIM);
        assert!(vec.iter().any(|value| *value != 0.0));
    }

    #[test]
    fn local_engine_plug_trains_and_serves_reflex_end_to_end() {
        let _guard = crate::engines::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.jsonl");
        let lines = [
            serde_json::json!({"intent": "check system status", "action": "status", "timestamp": 1}),
            serde_json::json!({"intent": "show system health", "action": "status", "timestamp": 2}),
            serde_json::json!({"intent": "show engine version", "action": "version", "timestamp": 3}),
            serde_json::json!({"intent": "what version is running", "action": "version", "timestamp": 4}),
        ];
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&staged, body).unwrap();

        register_local_engine_plug(BuiltinLocalEnginePlug::shared());
        let summary = SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        assert!(summary.contains("Tier-0 reflex trained"), "{summary}");
        assert_ne!(SusiAlphaModel::get_model_fingerprint(dir.path()), "missing");

        let model = SusiAlphaModel::load(dir.path()).unwrap();
        assert_eq!(
            model.predict_intent("check system status").unwrap(),
            "ACTION: status"
        );
        assert_eq!(
            model.predict_intent("show engine version").unwrap(),
            "ACTION: version"
        );
        clear_local_engine_plug();
    }
}

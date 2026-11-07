//! Inference substrate served through [`crate::engines::local_plug::LocalEnginePlug`]
//! (or the built-in GGUF substrate when compiled with `--features local-engine`).

use crate::engines::local_plug::{
    call_bus_plug, local_engine_plug, TOPIC_LOCAL_LOADED, TOPIC_LOCAL_PRELOAD,
    TOPIC_LOCAL_REUSABLE, TOPIC_LOCAL_UNLOAD,
};
use serde_json::json;
use std::path::Path;

use crate::susi_error::{EaiError, EaiResult};

const LOCAL_ENGINE_ABSENT: &str =
    "local model engine is not attached; register a LocalEnginePlug (susi-gpu-fabric) or build with --features local-engine";

pub trait NeuralBackend: Send + Sync {}

pub type ModelBackend = Box<dyn NeuralBackend>;

pub struct ModelSubstrate {
    pub(crate) weights: ModelBackend,
    pub(crate) prompt_format: PromptFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptFormat {
    ChatMl,
    Llama3,
    Gemma,
    Mistral,
    Raw,
}

impl PromptFormat {
    pub(crate) fn detect(chat_template: Option<&str>) -> Self {
        let Some(tpl) = chat_template else {
            return Self::Raw;
        };
        if tpl.contains("<|im_start|>") {
            Self::ChatMl
        } else if tpl.contains("<|start_header_id|>") {
            Self::Llama3
        } else if tpl.contains("<start_of_turn>") {
            Self::Gemma
        } else if tpl.to_lowercase().contains("[inst]") {
            Self::Mistral
        } else {
            Self::Raw
        }
    }

    pub(crate) fn wrap(self, prompt: &str) -> String {
        match self {
            Self::ChatMl => format!(
                "<|im_start|>system\nYou are a helpful assistant.<|im_end|>\n<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n"
            ),
            Self::Llama3 => format!(
                "<|start_header_id|>system<|end_header_id|>\n\nYou are a helpful assistant.<|eot_id|><|start_header_id|>user<|end_header_id|>\n\n{prompt}<|eot_id|><|start_header_id|>assistant<|end_header_id|>\n\n"
            ),
            Self::Gemma => {
                format!("<start_of_turn>user\n{prompt}<end_of_turn>\n<start_of_turn>model\n")
            }
            Self::Mistral => format!("[INST] {prompt} [/INST]"),
            Self::Raw => prompt.to_string(),
        }
    }
}

pub struct InferenceHost;

impl InferenceHost {
    pub(crate) fn model_is_reusable(path: &Path) -> bool {
        if let Some(plug) = local_engine_plug() {
            return plug.model_is_reusable(path);
        }
        if let Some(Ok(val)) = call_bus_plug(
            TOPIC_LOCAL_REUSABLE,
            json!({ "path": path.display().to_string() }),
        ) {
            return val
                .get("reusable")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        }
        false
    }

    pub fn preload(model_id: &str) -> EaiResult<serde_json::Value> {
        let path = resolve_model_id(model_id)?;
        Self::preload_resolved(&path)
    }

    pub(crate) fn preload_resolved(path: &Path) -> EaiResult<serde_json::Value> {
        if let Some(plug) = local_engine_plug() {
            return plug.preload(path);
        }
        if let Some(res) = call_bus_plug(
            TOPIC_LOCAL_PRELOAD,
            json!({ "path": path.display().to_string() }),
        ) {
            return res;
        }
        Err(EaiError::inference(LOCAL_ENGINE_ABSENT))
    }

    pub fn unload(model_id: &str) -> EaiResult<serde_json::Value> {
        let path = resolve_model_id(model_id)?;
        if let Some(plug) = local_engine_plug() {
            return plug.unload(&path);
        }
        if let Some(res) = call_bus_plug(
            TOPIC_LOCAL_UNLOAD,
            json!({ "path": path.display().to_string() }),
        ) {
            return res;
        }
        Err(EaiError::inference(LOCAL_ENGINE_ABSENT))
    }

    pub(crate) fn unload_resolved(path: &Path) -> EaiResult<serde_json::Value> {
        if let Some(plug) = local_engine_plug() {
            return plug.unload(path);
        }
        if let Some(res) = call_bus_plug(
            TOPIC_LOCAL_UNLOAD,
            json!({ "path": path.display().to_string() }),
        ) {
            return res;
        }
        Ok(json!({
            "path": path,
            "unloaded": false,
            "in_flight_users": null,
            "gpu_pool": "not_applicable",
        }))
    }

    pub fn loaded_models() -> Vec<serde_json::Value> {
        if let Some(plug) = local_engine_plug() {
            return plug.loaded_models();
        }
        if let Some(Ok(val)) = call_bus_plug(TOPIC_LOCAL_LOADED, json!({})) {
            if let Some(arr) = val.get("models").and_then(|v| v.as_array()) {
                return arr.clone();
            }
        }
        Vec::new()
    }

    pub(crate) fn validate_architecture(arch: &str) -> EaiResult<()> {
        match arch {
            "llama" | "qwen2" | "qwen2moe" | "qwen3moe" => Ok(()),
            _ => Err(EaiError::inference(format!(
                "Unsupported GGUF architecture '{arch}'; supported architectures: llama, qwen2, qwen2moe, qwen3moe"
            ))),
        }
    }

    pub(crate) fn needs_qwen2_backend(arch: &str) -> bool {
        arch == "qwen2"
    }
}

fn resolve_model_id(model_id: &str) -> EaiResult<std::path::PathBuf> {
    let id = model_id.trim();
    if id.is_empty() || id.contains('/') || id.contains('\\') {
        return Err(EaiError::inference(
            "model must be a model id or file name, not a path",
        ));
    }
    let id = id.strip_suffix(".gguf").unwrap_or(id);
    crate::models::ModelManager::get_model_path(id)
        .ok_or_else(|| EaiError::inference(format!("model '{id}' not found")))
}

pub fn apply_repeat_penalty(logits: &mut [f32], penalty: f32, context: &[u32]) {
    if penalty == 1.0 || penalty <= 0.0 {
        return;
    }
    let mut seen = std::collections::HashSet::new();
    for &tok in context {
        if seen.insert(tok) {
            if let Some(logit) = logits.get_mut(tok as usize) {
                *logit = if *logit < 0.0 {
                    *logit * penalty
                } else {
                    *logit / penalty
                };
            }
        }
    }
}

pub struct ContextSummarizer;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::local_plug::{
        clear_local_engine_plug, register_local_engine_plug, BuiltinLocalEnginePlug,
    };

    #[test]
    fn preload_names_the_missing_engine_when_the_model_exists() {
        let _guard = crate::engines::env_test_lock();
        let err =
            InferenceHost::preload_resolved(std::path::Path::new("weights.gguf")).unwrap_err();
        assert!(
            err.to_string().contains("local-engine") || err.to_string().contains("weights.gguf"),
            "{err}"
        );
    }

    #[test]
    fn an_empty_model_id_is_rejected_before_the_engine_check() {
        let err = InferenceHost::preload("  ").unwrap_err();
        assert!(err.to_string().contains("not a path"));
    }

    #[test]
    fn qwen2_is_the_only_dense_qwen_backend() {
        assert!(InferenceHost::needs_qwen2_backend("qwen2"));
        assert!(!InferenceHost::needs_qwen2_backend("llama"));
        assert!(InferenceHost::validate_architecture("qwen3moe").is_ok());
        assert!(InferenceHost::validate_architecture("gpt2").is_err());
    }

    #[test]
    fn local_engine_plug_preloads_inspects_and_unloads_valid_gguf() {
        let _guard = crate::engines::env_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let gguf_path = dir.path().join("tiny.gguf");
        let mut bytes = Vec::from(&b"GGUF"[..]);
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        let key = b"general.architecture";
        bytes.extend_from_slice(&(key.len() as u64).to_le_bytes());
        bytes.extend_from_slice(key);
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&5u64.to_le_bytes());
        bytes.extend_from_slice(b"llama");
        std::fs::write(&gguf_path, &bytes).unwrap();

        register_local_engine_plug(BuiltinLocalEnginePlug::shared());
        let receipt = InferenceHost::preload_resolved(&gguf_path).unwrap();
        assert_eq!(
            receipt.get("architecture").and_then(|v| v.as_str()),
            Some("llama")
        );
        assert!(InferenceHost::model_is_reusable(&gguf_path));
        let unload = InferenceHost::unload_resolved(&gguf_path).unwrap();
        assert_eq!(unload.get("unloaded").and_then(|v| v.as_bool()), Some(true));
        clear_local_engine_plug();
    }
}

//! Inference substrate when the Candle local engine is not linked.
//!
//! Prompt formatting, architecture names, and the repeat penalty stay so the
//! protocol tests keep running. Loading weights returns a typed error.

use std::path::Path;

use crate::susi_error::{EaiError, EaiResult};

const LOCAL_ENGINE_ABSENT: &str =
    "built-in local model engine is not in this core; rebuild with --features local-engine";

pub trait NeuralBackend: Send + Sync {}

pub type ModelBackend = Box<dyn NeuralBackend>;

/// Loaded weights. The slim core never constructs one.
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
    pub(crate) fn model_is_reusable(_path: &Path) -> bool {
        false
    }

    pub fn preload(model_id: &str) -> EaiResult<serde_json::Value> {
        let _path = resolve_model_id(model_id)?;
        Err(EaiError::inference(LOCAL_ENGINE_ABSENT))
    }

    pub(crate) fn preload_resolved(path: &Path) -> EaiResult<serde_json::Value> {
        let _ = path;
        Err(EaiError::inference(LOCAL_ENGINE_ABSENT))
    }

    pub fn unload(model_id: &str) -> EaiResult<serde_json::Value> {
        let _path = resolve_model_id(model_id)?;
        Err(EaiError::inference(LOCAL_ENGINE_ABSENT))
    }

    pub(crate) fn unload_resolved(path: &Path) -> EaiResult<serde_json::Value> {
        Ok(serde_json::json!({
            "path": path,
            "unloaded": false,
            "in_flight_users": null,
            "gpu_pool": "not_applicable",
        }))
    }

    pub fn loaded_models() -> Vec<serde_json::Value> {
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

/// Dampens the logits of already-seen tokens (llama.cpp convention: divide a
/// positive logit or multiply a negative one by `penalty`).
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

    #[test]
    fn preload_names_the_missing_engine_when_the_model_exists() {
        let err =
            InferenceHost::preload_resolved(std::path::Path::new("weights.gguf")).unwrap_err();
        assert!(err.to_string().contains("local-engine"));
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
}

// GEMI: Universal AI Inference & Reasoning Bridge
// 100% Rust implementation for Native Intelligence Substrate
// Competitive Inference Racing (unrelated to the release Motion Rule, identity.json Pillar IV item 3 — this file predates that name and reused it for a different concept)

use crate::hardware::HardwareProfiler;
use crate::models::ModelManager;
use crate::susi_error::{EaiError, EaiResult};
use indicatif::{ProgressBar, ProgressStyle};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::qwen2_split as qwen2gguf;
use candle_core::quantized::gguf_file;
use candle_transformers::models::quantized_llama as llama;

/// Inference graph for explicitly supported GGUF architectures. Dense Qwen2
/// requires its bias-aware backend; Llama uses Candle's quantized Llama graph.
pub trait NeuralBackend: Send + Sync {
    fn forward(
        &mut self,
        x: &candle_core::Tensor,
        index_pos: usize,
    ) -> candle_core::Result<candle_core::Tensor>;
    fn as_qwen2_mut(&mut self) -> Option<&mut qwen2gguf::ModelWeights> {
        None
    }
}

impl NeuralBackend for llama::ModelWeights {
    fn forward(
        &mut self,
        x: &candle_core::Tensor,
        index_pos: usize,
    ) -> candle_core::Result<candle_core::Tensor> {
        self.forward(x, index_pos)
    }
}

impl NeuralBackend for qwen2gguf::ModelWeights {
    fn forward(
        &mut self,
        x: &candle_core::Tensor,
        index_pos: usize,
    ) -> candle_core::Result<candle_core::Tensor> {
        self.forward(x, index_pos)
    }
    fn as_qwen2_mut(&mut self) -> Option<&mut qwen2gguf::ModelWeights> {
        Some(self)
    }
}

pub type ModelBackend = Box<dyn NeuralBackend>;

/// Loaded neural weights (Mandate 23: Substrate Purity). See `ModelBackend`
/// for why Qwen2 needs its own graph rather than the Llama backend.
///
/// Also carries the model's own declared stop token(s) and chat-prompt
/// format, both read from the GGUF's own metadata at load time (never
/// hardcoded per model), since an instruct-tuned model only behaves
/// correctly - and only knows when to stop - within the exact turn format
/// it was fine-tuned on.
pub struct ModelSubstrate {
    pub(crate) weights: ModelBackend,
    pub(crate) eos_token_ids: Vec<u32>,
    pub(crate) prompt_format: PromptFormat,
}

/// The chat-turn wrapper a model expects, detected from its GGUF-embedded
/// `tokenizer.chat_template` Jinja string by the control-token family it
/// references. This is pattern-matching on well-known token families, not a
/// Jinja engine - it covers the common instruct-tuning conventions without
/// requiring a template interpreter, and falls back to `Raw` (feed the
/// prompt unwrapped, today's behavior) for anything unrecognized.
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

    /// Wraps a raw instruction in the turn format this model was tuned on.
    /// Only verified empirically against Qwen2.5's ChatML template on this
    /// host; Llama3/Gemma/Mistral follow the same well-documented
    /// conventions but are not independently verified here.
    pub(crate) fn wrap(self, prompt: &str) -> String {
        match self {
            Self::ChatMl => format!(
                "<|im_start|>system\nYou are a helpful assistant.<|im_end|>\n<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n"
            ),
            Self::Llama3 => format!(
                "<|start_header_id|>system<|end_header_id|>\n\nYou are a helpful assistant.<|eot_id|><|start_header_id|>user<|end_header_id|>\n\n{prompt}<|eot_id|><|start_header_id|>assistant<|end_header_id|>\n\n"
            ),
            Self::Gemma => format!("<start_of_turn>user\n{prompt}<end_of_turn>\n<start_of_turn>model\n"),
            Self::Mistral => format!("[INST] {prompt} [/INST]"),
            Self::Raw => prompt.to_string(),
        }
    }
}

pub struct InferenceHost;

impl InferenceHost {
    /// Loads a supported local GGUF, reusing weights only for the same file
    /// version, device context, and KV cache capacity.
    pub fn get_model(
        model_path: &Path,
        device: &candle_core::Device,
        task_handle: &Arc<susi_core::task_manager::TaskHandle>,
    ) -> EaiResult<Arc<RwLock<ModelSubstrate>>> {
        static CACHE: OnceLock<crate::model_cache::ModelCache<ModelSubstrate>> = OnceLock::new();
        if task_handle.is_cancelled() {
            return Err(EaiError::inference("Model loading cancelled"));
        }
        let kv_capacity = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .kv_cache_capacity_tokens();
        CACHE
            .get_or_init(Default::default)
            .get_or_load(model_path, device, kv_capacity, |path| {
                if task_handle.is_cancelled() {
                    return Err(susi_gemi_models::susi_error::EaiError::inference(
                        "Model loading cancelled",
                    ));
                }
                let model = Self::load_model(path, device, kv_capacity).map_err(|e| {
                    susi_gemi_models::susi_error::rewrap(e.kind_name(), e.to_string())
                })?;
                if task_handle.is_cancelled() {
                    return Err(susi_gemi_models::susi_error::EaiError::inference(
                        "Model loading cancelled",
                    ));
                }
                Ok(model)
            })
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))
    }

    fn load_model(
        model_path: &Path,
        device: &candle_core::Device,
        kv_cache_capacity: usize,
    ) -> EaiResult<ModelSubstrate> {
        // 2. Load Weights (Outside global cache lock to prevent substrate-wide stalls)
        println!(
            "- [Substrate Operation] Loading neural weights: {}",
            model_path.display()
        );
        let _ = std::io::stdout().flush();

        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.set_message("Loading weights...");
        pb.enable_steady_tick(std::time::Duration::from_millis(100));

        // Integrity Verification
        ModelManager::verify_model_integrity(model_path)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;

        let mut file = std::fs::File::open(model_path).map_err(|e| {
            EaiError::inference(format!(
                "Failed to open weights {}: {}",
                model_path.display(),
                e
            ))
        })?;

        let mut model_data = gguf_file::Content::read(&mut file)
            .map_err(|e| EaiError::inference(format!("GGUF Metadata Error: {}", e)))?;

        // Architectural Scout: Inspect metadata for dynamic dispatch
        let arch = model_data
            .metadata
            .get("general.architecture")
            .and_then(|v| v.to_string().ok())
            .map(|s| s.to_lowercase())
            .ok_or_else(|| EaiError::inference("GGUF is missing general.architecture"))?;
        Self::validate_architecture(&arch)?;

        // Dynamic Metadata Shimming
        if arch != "llama" {
            Self::shim_llama_compatible_metadata(&mut model_data.metadata);
        }

        // Extracted before from_gguf consumes model_data below. The model's
        // own declared EOS (e.g. Qwen2.5-Instruct's <|im_end|>, id 151645)
        // is frequently a different token than the static config-level
        // eos_token_ids list covers, and its chat_template's control-token
        // family determines whether the raw prompt needs wrapping at all -
        // an instruct model given an unwrapped prompt has no learned signal
        // for either "this is a turn" or "the turn is over".
        let eos_token_ids: Vec<u32> = model_data
            .metadata
            .get("tokenizer.ggml.eos_token_id")
            .and_then(|v| v.to_u32().ok())
            .into_iter()
            .collect();
        let prompt_format = PromptFormat::detect(
            model_data
                .metadata
                .get("tokenizer.chat_template")
                .and_then(|v| v.to_string().ok())
                .map(|s| s.as_str()),
        );

        println!(
            "- [Substrate Operation] Initializing {:?} weights on {:?}...",
            arch, device
        );
        let _ = std::io::stdout().flush();

        let weights = if Self::needs_qwen2_backend(&arch) {
            let cpu_dev = candle_core::Device::Cpu;
            let gpu_dev = device;
            let vram_budget = if device.is_cpu() {
                0
            } else {
                HardwareProfiler::gpu_vram_budget_bytes()
            };
            let split = qwen2gguf::ModelWeights::plan_gpu_layers(
                &model_data,
                vram_budget,
                kv_cache_capacity,
            );
            println!(
                "- [Inference Substrate] Executing Heterogeneous Layer Split: {} layers on GPU",
                split
            );
            qwen2gguf::ModelWeights::from_gguf_split(
                model_data,
                &mut file,
                &cpu_dev,
                gpu_dev,
                split,
                kv_cache_capacity,
            )
            .map(|m| Box::new(m) as Box<dyn NeuralBackend>)
        } else {
            llama::ModelWeights::from_gguf(model_data, &mut file, device)
                .map(|m| Box::new(m) as Box<dyn NeuralBackend>)
        }
        .map_err(|e| EaiError::inference(format!("Architecture '{}' load failure: {}", arch, e)))?;

        println!("- [Substrate Operation] Model substrate ready.");
        let _ = std::io::stdout().flush();
        pb.finish_and_clear();

        Ok(ModelSubstrate {
            weights,
            eos_token_ids,
            prompt_format,
        })
    }

    pub(crate) fn validate_architecture(arch: &str) -> EaiResult<()> {
        match arch {
            "llama" | "qwen2" => Ok(()),
            _ => Err(EaiError::inference(format!(
                "Unsupported GGUF architecture '{arch}'; supported architectures: llama, qwen2"
            ))),
        }
    }

    pub(crate) fn needs_qwen2_backend(arch: &str) -> bool {
        arch == "qwen2"
    }

    pub(crate) fn shim_llama_compatible_metadata(metadata: &mut HashMap<String, gguf_file::Value>) {
        let common_keys = [
            "attention.head_count",
            "attention.head_count_kv",
            "embedding_length",
            "feed_forward_length",
            "block_count",
            "attention.layer_norm_rms_epsilon",
            "rope.dimension_count",
        ];

        for k in common_keys {
            let llama_key = format!("llama.{}", k);
            if !metadata.contains_key(&llama_key) {
                let found_key = metadata.keys().find(|mk| mk.ends_with(k)).cloned();
                if let Some(fk) = found_key {
                    if let Some(val) = metadata.get(&fk).cloned() {
                        metadata.insert(llama_key, val);
                    }
                }
            }
        }

        // llama.cpp-produced GGUFs for several architectures (Qwen2 included)
        // omit rope.dimension_count entirely, since it's implicitly
        // embedding_length / head_count - unlike the other common_keys above,
        // there is no source key to copy for these, so candle_transformers'
        // hard `md_get("llama.rope.dimension_count")?` requirement fails
        // every load for those architectures unless we derive it ourselves
        // the same way llama.cpp does.
        if !metadata.contains_key("llama.rope.dimension_count") {
            let head_count = metadata
                .get("llama.attention.head_count")
                .and_then(|v| v.to_u32().ok());
            let embedding_length = metadata
                .get("llama.embedding_length")
                .and_then(|v| v.to_u32().ok());
            if let (Some(hc), Some(el)) = (head_count, embedding_length) {
                if hc > 0 && el % hc == 0 {
                    metadata.insert(
                        "llama.rope.dimension_count".to_string(),
                        gguf_file::Value::U32(el / hc),
                    );
                }
            }
        }
    }
}

/// Dampens the logits of already-seen tokens (llama.cpp convention: divide a
/// positive logit or multiply a negative one by `penalty`) so greedy argmax
/// decoding doesn't loop on its own recent output. A no-op for `penalty <= 0`
/// or `penalty == 1.0`. `context` is the caller's choice of window - see the
/// call site in `SusiGgufEngine::run_inference_stream` for why it must be
/// generated tokens only, never the prompt.
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

impl ContextSummarizer {}

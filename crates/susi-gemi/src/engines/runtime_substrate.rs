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
    /// `(gpu_layers, total_layers)` when the backend tracks per-layer
    /// placement; `None` means placement is uniform on the load device.
    fn gpu_layers(&self) -> Option<(usize, usize)> {
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
    fn gpu_layers(&self) -> Option<(usize, usize)> {
        Some(self.gpu_layer_count())
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
    /// Device the weights were loaded onto; `Drop` binds its context.
    // Read only by the CUDA branch of `Drop`; CPU-only builds never need it.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    device: candle_core::Device,
}

/// Weights can be dropped on any thread: an unload, the idle sweeper, a
/// request finishing after its model was evicted, or a cache replacement
/// on a fresh request thread. cudarc's `CudaSlice` drop frees without
/// binding the CUDA context and only records a failure, so a drop on a
/// thread with no current context leaks the VRAM with
/// `CUDA_ERROR_INVALID_CONTEXT`. Binding here, before the fields drop,
/// makes every drop site safe.
impl Drop for ModelSubstrate {
    fn drop(&mut self) {
        #[cfg(feature = "cuda")]
        if let candle_core::Device::Cuda(cuda) = &self.device {
            if let Err(error) = cuda.cuda_stream().context().bind_to_thread() {
                tracing::warn!(%error, "could not bind CUDA context to free model weights");
            }
        }
    }
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

/// How often the idle sweeper checks: a quarter of the timeout, clamped to
/// 5-60 seconds (a disabled timeout re-checks the config every minute).
fn sweep_interval(timeout_secs: u64) -> std::time::Duration {
    let secs = if timeout_secs == 0 {
        60
    } else {
        (timeout_secs / 4).clamp(5, 60)
    };
    std::time::Duration::from_secs(secs)
}

/// Estimated VRAM for a model: its weights plus 1/8 headroom for the KV
/// cache and activations. A heuristic — if it undershoots, the loader's
/// live-budget layer split still keeps the load from failing.
fn estimated_vram_bytes(file_len: u64) -> u64 {
    file_len.saturating_add(file_len / 8)
}

/// Which idle models (LRU order, with their weight sizes) to evict so that
/// `free + reclaimed >= need`. Empty when the model already fits; all
/// candidates when even that is not enough (evict as much as possible).
fn plan_evictions(
    free: u64,
    need: u64,
    lru: &[(std::path::PathBuf, u64)],
) -> Vec<std::path::PathBuf> {
    let mut reclaimed = free;
    lru.iter()
        .take_while(|(_, bytes)| {
            let short = reclaimed < need;
            reclaimed = reclaimed.saturating_add(*bytes);
            short
        })
        .map(|(path, _)| path.clone())
        .collect()
}

/// Free evicted weights on `device` and return the memory to the driver.
///
/// cudarc's `CudaSlice` drop calls `cuMemFreeAsync` without binding the
/// device context and only *records* a failure, so dropping weights on a
/// thread with no current context (the daemon's request threads) leaks the
/// allocation with `CUDA_ERROR_INVALID_CONTEXT`. Bind first, drop, then
/// synchronize — which surfaces any recorded free error — and trim the
/// device's memory pool so the VRAM is usable by other programs, not just
/// reserved for this process. `Ok(false)` when `device` is not CUDA.
#[cfg(feature = "cuda")]
#[allow(unsafe_code)]
pub(super) fn release_on_device<T>(
    weights: Arc<T>,
    device: &candle_core::Device,
) -> Result<bool, String> {
    use candle_core::cuda_backend::cudarc::driver::result;
    let candle_core::Device::Cuda(cuda) = device else {
        drop(weights);
        return Ok(false);
    };
    let stream = cuda.cuda_stream();
    let ctx = stream.context();
    ctx.bind_to_thread().map_err(|e| e.to_string())?;
    drop(weights);
    // Completes the async frees and reports any error recorded by a drop.
    ctx.synchronize().map_err(|e| e.to_string())?;
    // SAFETY: `cu_device` comes from a live `CudaContext` that `ctx` keeps
    // alive for this whole call, and the context is bound to this thread.
    // `cuDeviceGetMemPool` returns the driver-owned current pool, which is
    // never freed or destroyed here, and `cuMemPoolTrimTo` only releases
    // reservations that no live allocation uses.
    unsafe {
        let pool = result::device::get_mem_pool(ctx.cu_device()).map_err(|e| e.to_string())?;
        result::mem_pool::trim_to(pool, 0).map_err(|e| e.to_string())?;
    }
    Ok(true)
}

#[cfg(not(feature = "cuda"))]
#[allow(clippy::unnecessary_wraps)] // mirrors the CUDA variant's signature
pub(super) fn release_on_device<T>(
    weights: Arc<T>,
    _device: &candle_core::Device,
) -> Result<bool, String> {
    drop(weights);
    Ok(false)
}

impl InferenceHost {
    pub(super) fn cache() -> &'static crate::model_cache::ModelCache<ModelSubstrate> {
        static CACHE: OnceLock<crate::model_cache::ModelCache<ModelSubstrate>> = OnceLock::new();
        CACHE.get_or_init(|| {
            // Only processes that actually load models run the sweeper.
            let spawned = std::thread::Builder::new()
                .name("model-idle-sweeper".to_string())
                .spawn(Self::idle_sweeper);
            if let Err(error) = spawned {
                tracing::warn!(%error, "model idle sweeper did not start; idle models stay loaded");
            }
            crate::model_cache::ModelCache::default()
        })
    }

    /// Background loop: unload models idle longer than
    /// `model_idle_timeout_secs`, re-read every pass so a config change
    /// applies without a restart. Checks at a quarter of the timeout
    /// (5-60s), so a model outlives its timeout by at most that interval.
    fn idle_sweeper() {
        loop {
            let timeout = crate::susi_sandbox::manager::SusiConfig::load_global()
                .unwrap_or_default()
                .model_idle_timeout_secs();
            std::thread::sleep(sweep_interval(timeout));
            if timeout > 0 {
                Self::evict_idle(std::time::Duration::from_secs(timeout));
            }
        }
    }

    /// Unload every model unused for longer than `timeout` and not serving a
    /// request. Returns the unloaded paths.
    pub fn evict_idle(timeout: std::time::Duration) -> Vec<std::path::PathBuf> {
        let Some(cutoff) = std::time::SystemTime::now().checked_sub(timeout) else {
            return Vec::new();
        };
        let mut unloaded = Vec::new();
        for path in Self::cache().idle_since(cutoff) {
            let Ok(Some((weights, device))) = Self::cache().evict(&path) else {
                continue;
            };
            println!(
                "- [Inference Substrate] Idle for over {}s: unloading {}",
                timeout.as_secs(),
                path.display()
            );
            if let Err(error) = release_on_device(weights, &device) {
                tracing::warn!(%error, model = %path.display(), "idle unload release failed");
            }
            unloaded.push(path);
        }
        unloaded
    }

    /// LRU eviction under VRAM pressure: before a new model loads onto a
    /// CUDA device, evict idle cached models (least recently used first)
    /// until live free VRAM covers its estimated footprint, so it lands
    /// fully on the GPU instead of being split onto the CPU. Models serving
    /// a request are never evicted. Best effort: when nothing idle remains,
    /// the loader splits layers exactly as before.
    fn make_room(model_path: &Path, device: &candle_core::Device) {
        if !device.is_cuda() || Self::cache().is_loaded(model_path) {
            return;
        }
        let Ok(meta) = std::fs::metadata(model_path) else {
            return;
        };
        let need = estimated_vram_bytes(meta.len());
        let free = HardwareProfiler::gpu_vram_budget_bytes();
        let candidates = Self::cache().idle_lru(model_path, candle_core::Device::is_cuda);
        for victim in plan_evictions(free, need, &candidates) {
            // Re-measure: the estimate is by file size, the truth is live.
            if HardwareProfiler::gpu_vram_budget_bytes() >= need {
                break;
            }
            let Ok(Some((weights, victim_device))) = Self::cache().evict(&victim) else {
                continue;
            };
            println!(
                "- [Inference Substrate] VRAM pressure: evicting least-recently-used {}",
                victim.display()
            );
            if let Err(error) = release_on_device(weights, &victim_device) {
                tracing::warn!(%error, victim = %victim.display(), "LRU eviction release failed");
            }
        }
    }

    /// Resolve an operator-supplied model id (a stem or file name, never a
    /// path) to its weights, the same way inference resolves `model`.
    fn resolve_model_id(model_id: &str) -> EaiResult<std::path::PathBuf> {
        let id = model_id.trim();
        if id.is_empty() || id.contains('/') || id.contains('\\') {
            return Err(EaiError::inference(
                "model must be a model id or file name, not a path",
            ));
        }
        let id = id.strip_suffix(".gguf").unwrap_or(id);
        susi_gemi_models::ModelManager::get_model_path(id)
            .ok_or_else(|| EaiError::inference(format!("model '{id}' not found")))
    }

    /// Load a model into the cache on the device inference would pick for
    /// it, so the next request for it is served warm.
    pub fn preload(model_id: &str) -> EaiResult<serde_json::Value> {
        let path = Self::resolve_model_id(model_id)?;
        let file_size = std::fs::metadata(&path)
            .map(|m| usize::try_from(m.len()).unwrap_or(usize::MAX))
            .unwrap_or(0);
        let device = crate::hardware::HardwareProfiler::get_dynamic_device(file_size);
        let task = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("model_preload", &path.to_string_lossy());
        match Self::get_model(&path, &device, &task) {
            Ok(_) => {
                task.mark_completed("model preloaded");
                Ok(serde_json::json!({ "loaded": path }))
            }
            Err(error) => {
                task.mark_failed(&error.to_string());
                Err(error)
            }
        }
    }

    /// Evict a model from the cache.
    pub fn unload(model_id: &str) -> EaiResult<serde_json::Value> {
        let path = Self::resolve_model_id(model_id)?;
        let evicted = Self::cache()
            .evict(&path)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        let Some((weights, device)) = evicted else {
            return Ok(serde_json::json!({
                "path": path,
                "unloaded": false,
                "in_flight_users": null,
                "gpu_pool": "not_applicable",
            }));
        };
        let in_flight = Arc::strong_count(&weights).saturating_sub(1);
        // Weights a running request still holds are freed by that request's
        // thread, which has the context bound; only trim once nothing does.
        let gpu_pool = if in_flight > 0 {
            drop(weights);
            "pending_in_flight"
        } else {
            match release_on_device(weights, &device) {
                Ok(true) => "trimmed",
                Ok(false) => "not_applicable",
                Err(error) => {
                    tracing::warn!(%error, "GPU memory release failed after unload");
                    "release_failed"
                }
            }
        };
        Ok(serde_json::json!({
            "path": path,
            "unloaded": true,
            "in_flight_users": in_flight,
            "gpu_pool": gpu_pool,
        }))
    }

    /// Weights currently held by this process's inference cache: the
    /// ground truth for "which model is loaded", unlike file-mapping
    /// heuristics. Never blocks on a load or an in-flight generation.
    pub fn loaded_models() -> Vec<serde_json::Value> {
        Self::cache().snapshot(|substrate| {
            let gpu = substrate.weights.gpu_layers();
            serde_json::json!({
                "gpu_layers": gpu.map(|(on_gpu, _)| on_gpu),
                "total_layers": gpu.map(|(_, total)| total),
                "prompt_format": format!("{:?}", substrate.prompt_format),
            })
        })
    }

    /// Loads a supported local GGUF, reusing weights only for the same file
    /// version, device context, and KV cache capacity.
    pub fn get_model(
        model_path: &Path,
        device: &candle_core::Device,
        task_handle: &Arc<crate::susi_core::task_manager::TaskHandle>,
    ) -> EaiResult<Arc<RwLock<ModelSubstrate>>> {
        if task_handle.is_cancelled() {
            return Err(EaiError::inference("Model loading cancelled"));
        }
        let kv_capacity = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .kv_cache_capacity_tokens();
        Self::make_room(model_path, device);
        Self::cache()
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
            device: device.clone(),
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

#[cfg(test)]
mod eviction_tests {
    use super::*;
    use std::path::PathBuf;

    fn lru() -> Vec<(PathBuf, u64)> {
        vec![
            ("old".into(), 400),
            ("mid".into(), 300),
            ("new".into(), 200),
        ]
    }

    #[test]
    fn no_eviction_when_the_model_fits() {
        assert!(plan_evictions(1000, 900, &lru()).is_empty());
    }

    #[test]
    fn evicts_least_recently_used_until_it_fits() {
        assert_eq!(plan_evictions(300, 600, &lru()), vec![PathBuf::from("old")]);
        assert_eq!(
            plan_evictions(0, 650, &lru()),
            vec![PathBuf::from("old"), PathBuf::from("mid")]
        );
    }

    #[test]
    fn evicts_everything_idle_when_even_that_is_short() {
        assert_eq!(plan_evictions(0, 10_000, &lru()).len(), 3);
        assert!(plan_evictions(0, 10, &[]).is_empty());
    }

    #[test]
    fn sweep_interval_is_a_quarter_of_the_timeout_within_bounds() {
        use std::time::Duration;
        assert_eq!(sweep_interval(0), Duration::from_secs(60));
        assert_eq!(sweep_interval(8), Duration::from_secs(5));
        assert_eq!(sweep_interval(120), Duration::from_secs(30));
        assert_eq!(sweep_interval(1800), Duration::from_secs(60));
    }

    #[test]
    fn estimate_adds_headroom_without_overflow() {
        assert_eq!(estimated_vram_bytes(800), 900);
        assert_eq!(estimated_vram_bytes(u64::MAX), u64::MAX);
    }
}

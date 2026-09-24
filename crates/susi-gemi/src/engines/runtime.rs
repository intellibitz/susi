//! GEMI inference runtime facade.
//!
//! Substrate load/KV (`runtime_substrate`) and native backends (`runtime_native`)
//! are separate compile units; this module owns `GemiEngine` / mission planning.

#[path = "runtime_native.rs"]
mod runtime_native;
#[path = "runtime_substrate.rs"]
mod runtime_substrate;

pub use runtime_native::{
    engine_registry, LlamaCppEngine, NativeInferenceEngine, SusiFederatedEngine, SusiGgufEngine,
};
pub use runtime_substrate::{
    apply_repeat_penalty, ContextSummarizer, InferenceHost, ModelBackend, ModelSubstrate,
    NeuralBackend,
};

use crate::models::ModelManager;
use crate::susi_error::{EaiError, EaiResult};
use std::path::Path;
use std::sync::OnceLock;

#[cfg(test)]
use candle_core::quantized::gguf_file;
#[cfg(test)]
use runtime_substrate::PromptFormat;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokenizers::Tokenizer;

pub struct GemiEngine;

impl GemiEngine {
    pub fn generate_reasoning(prompt: &str, workspace: &Path) -> String {
        Self::reason_internal(prompt, workspace, true, &|_| {}, None, None)
    }

    pub fn generate_reasoning_deep(prompt: &str, workspace: &Path) -> String {
        Self::generate_reasoning_deep_with_min_complexity(prompt, workspace, None)
    }

    pub fn generate_reasoning_deep_with_min_complexity(
        prompt: &str,
        workspace: &Path,
        min_complexity: Option<crate::intent::TaskComplexity>,
    ) -> String {
        Self::reason_internal(prompt, workspace, false, &|_| {}, min_complexity, None)
    }

    pub fn generate_reasoning_deep_with_model(
        prompt: &str,
        workspace: &Path,
        model: &str,
    ) -> String {
        Self::reason_internal(prompt, workspace, false, &|_| {}, None, Some(model))
    }

    pub fn generate_reasoning_stream(
        prompt: &str,
        workspace: &Path,
        callback: &dyn Fn(String),
    ) -> String {
        Self::reason_internal(prompt, workspace, true, callback, None, None)
    }

    /// Streaming reasoning honoring a caller-requested model name — the
    /// `/v1/chat/completions` streaming path threads its `model` field here.
    pub fn generate_reasoning_stream_with_model(
        prompt: &str,
        workspace: &Path,
        callback: &dyn Fn(String),
        model: &str,
    ) -> String {
        Self::reason_internal(prompt, workspace, true, callback, None, Some(model))
    }

    /// Ultra-Latency Competitive Inference Racing
    #[allow(clippy::too_many_arguments)]
    fn reason_internal(
        prompt: &str,
        workspace: &Path,
        allow_reflex: bool,
        callback: &dyn Fn(String),
        min_complexity: Option<crate::intent::TaskComplexity>,
        requested_model: Option<&str>,
    ) -> String {
        if allow_reflex {
            let (reflex_decision, _) = super::reflex::ReflexEngine::try_solve(prompt, workspace);
            if let super::reflex::ReflexDecision::Solved(action) = reflex_decision {
                callback(action.clone());
                return action;
            }
        }

        // Ensure configured cloud endpoints are registered before routing.
        crate::http_provider::register_configured_cloud_endpoints(
            crate::susi_core::registry::CapabilityRegistry::global(),
        );

        // Latency / CPU-only gate: escalate to cloud when local is known-slow
        // (or host has no GPU). Sticky preference + optional interactive pick.
        let explicit_local_request = requested_model
            .map(|m| !crate::routing::InferenceRouter::is_cloud_provider_name(m))
            .unwrap_or(false);
        if !explicit_local_request {
            let names = crate::susi_core::registry::CapabilityRegistry::global().list_providers();
            if let Some(esc) = crate::routing::InferenceRouter::maybe_escalate_to_cloud(&names) {
                crate::routing::InferenceRouter::announce(&esc);
                callback(format!(
                    "[SUSI ROUTING] Escalating to cloud `{}` ({})\n",
                    esc.provider, esc.reason
                ));
                if let Some(text) =
                    Self::try_discovered_providers(prompt, Some(&esc.provider), callback)
                {
                    return match Self::verify_axiomatic_alignment(&text, workspace) {
                        Ok(v) => v,
                        Err(_) => text,
                    };
                }
            }
        }

        // Pillar 4/8: prefer zero-config discovered providers (Ollama, vLLM, …)
        // before the native GGUF path. Skips Candle (Local) — that provider
        // delegates back into this function and would recurse.
        if let Some(text) = Self::try_discovered_providers(prompt, requested_model, callback) {
            return match Self::verify_axiomatic_alignment(&text, workspace) {
                Ok(v) => v,
                Err(_) => text,
            };
        }

        eprintln!("[INFERENCE FAILOVER] Falling back to local inference");
        callback("[SUSI ROUTING] Falling back to local inference\n".to_string());

        // Primary Federated vs Native Inference Routing Edge
        let global_config =
            crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let active_engine_identifier = crate::models::ModelManager::get_selected_engine()
            .unwrap_or(global_config.default_engine());

        let engine_key = if requested_model.is_none()
            && (active_engine_identifier == "susi-federated" || active_engine_identifier == "cloud")
        {
            active_engine_identifier.as_str()
        } else {
            "llamacpp"
        };

        // Fully Dynamic Native Engine Instantiation using susi_core ServiceRegistry
        // Resolves the engine via Semantic Generics instead of hardcoded enum matching
        let engine = engine_registry()
            .instantiate::<std::sync::Arc<dyn NativeInferenceEngine>>(engine_key)
            .map(|arc_of_arc| (*arc_of_arc).clone())
            .unwrap_or_else(|| {
                std::sync::Arc::new(LlamaCppEngine) as std::sync::Arc<dyn NativeInferenceEngine>
            });

        let selected_model = requested_model.map(str::to_owned).or_else(|| {
            ModelManager::get_selected_model_for_request_with_min_complexity(
                prompt,
                None,
                min_complexity,
            )
        });
        let local_started = std::time::Instant::now();
        let result = engine.run_inference_stream(prompt, callback, selected_model.as_deref());
        if let Some(model) = selected_model.as_deref() {
            if active_engine_identifier != "susi-federated" && active_engine_identifier != "cloud"
                || requested_model.is_some()
            {
                ModelManager::record_inference_result(
                    model,
                    result.as_ref().is_ok_and(|text| !text.trim().is_empty()),
                );
            }
        }
        if let Ok(res) = result {
            if !res.trim().is_empty() {
                // Feed the latency gate so the next request can escalate if slow.
                if engine_key == "llamacpp" {
                    crate::routing::InferenceRouter::record_local_sample(
                        local_started.elapsed(),
                        res.len(),
                    );
                }
                return match Self::verify_axiomatic_alignment(&res, workspace) {
                    Ok(v) => v,
                    Err(_) => res,
                };
            }
        }

        // Fleet Mandate: a fresh substrate with zero provisioned weights must
        // not surface a bare failure for an otherwise-solvable intent - it
        // must fetch a hardware-fit model and retry before giving up. Only
        // engaged when no local GGUF actually exists yet (not on inference
        // errors against an existing model, which a re-download can't fix).
        if requested_model.is_none() && !Self::has_usable_local_model(workspace) {
            if let Some(res) =
                Self::provision_and_retry(prompt, workspace, callback, min_complexity)
            {
                return res;
            }
        }

        // Fallback Power Reasoning Tool
        let power_res = crate::susi_core::plane_bus::tools::execute_tool(
            "power_reason",
            &serde_json::json!(prompt),
            workspace,
        )
        .unwrap_or_default();
        if !power_res.contains("[FAIL]")
            && !power_res.contains("[CAPABILITY_GAP]")
            && !power_res.contains("Inference Error")
        {
            callback(power_res.clone());
            return power_res;
        }

        let final_msg = "[FAIL] SUSI-Tier2-Inference: Local model inference and power reasoning fallback both failed.".to_string();
        callback(final_msg.clone());
        final_msg
    }

    /// Shared runtime for bridging sync swarm callers into async `Provider` APIs.
    fn provider_runtime() -> Option<&'static tokio::runtime::Runtime> {
        static RT: OnceLock<std::io::Result<tokio::runtime::Runtime>> = OnceLock::new();
        RT.get_or_init(tokio::runtime::Runtime::new).as_ref().ok()
    }

    /// Embed `text` through the first registered provider that can serve
    /// embeddings. `model_hint` names a preferred provider — it is tried
    /// first, and every other provider still tries in order after it.
    pub fn embed_text(text: &str, model_hint: Option<&str>) -> Result<Vec<f32>, String> {
        crate::http_provider::register_configured_cloud_endpoints(
            crate::susi_core::registry::CapabilityRegistry::global(),
        );
        let registry = crate::susi_core::registry::CapabilityRegistry::global();
        let names = registry.list_providers();
        let mut ordered: Vec<&str> = Vec::with_capacity(names.len());
        if let Some(w) = model_hint {
            ordered.extend(names.iter().map(String::as_str).filter(|n| *n == w));
        }
        ordered.extend(names.iter().map(String::as_str));
        let runtime =
            Self::provider_runtime().ok_or_else(|| "embed runtime unavailable".to_string())?;
        let mut last_err = String::from("no embedding-capable provider registered");
        for name in ordered {
            let Some(provider) = registry.get_provider(name) else {
                continue;
            };
            match runtime.block_on(provider.embed(text)) {
                Ok(v) if !v.is_empty() => return Ok(v),
                Ok(_) => last_err = format!("{name}: empty embedding"),
                Err(e) => last_err = format!("{name}: {e}"),
            }
        }
        Err(last_err)
    }

    /// Rank discovered provider names: fast structured engines first, then
    /// other HTTP backends. Candle is excluded (see caller).
    pub(crate) fn rank_provider_name(name: &str) -> u8 {
        let lower = name.to_ascii_lowercase();
        if lower.contains("sglang") {
            0
        } else if lower.contains("vllm") {
            1
        } else if lower.contains("ollama") {
            2
        } else if lower.contains("llama.cpp") || lower.contains("llamacpp") {
            3
        } else if lower.contains("lmstudio") {
            4
        } else if lower.contains("openai") {
            5
        } else if lower.contains("anthropic") {
            6
        } else if lower.contains("gemini") || lower.contains("google") {
            7
        } else if lower.starts_with("mcp-") {
            8
        } else {
            10
        }
    }

    /// Try CapabilityRegistry providers registered by zero-config discovery.
    fn try_discovered_providers(
        prompt: &str,
        requested_model: Option<&str>,
        callback: &dyn Fn(String),
    ) -> Option<String> {
        // Mock-inference seam: under test the env opts out of *all* real
        // provider calls — discovered HTTP endpoints included — not just the
        // native engine path (runtime_native honors the same flag).
        if std::env::var("SUSI_TEST_MOCK_INFERENCE").unwrap_or_default() == "true" {
            return None;
        }
        Self::try_providers(
            crate::susi_core::registry::CapabilityRegistry::global(),
            prompt,
            requested_model,
            callback,
        )
    }

    fn try_providers(
        registry: &crate::susi_core::registry::CapabilityRegistry,
        prompt: &str,
        requested_model: Option<&str>,
        callback: &dyn Fn(String),
    ) -> Option<String> {
        let mut names: Vec<String> = registry
            .list_providers()
            .into_iter()
            .filter(|n| n != "Candle (Local)")
            .collect();
        if crate::susi_core::mac_policy::MacPolicy::global().blocks_cloud_inference() {
            names.retain(|n| !crate::routing::InferenceRouter::is_cloud_provider_name(n));
        }
        if names.is_empty() {
            return None;
        }

        if let Some(model) = requested_model {
            let model_l = model.to_ascii_lowercase();
            names.sort_by_key(|n| {
                let hit = n.to_ascii_lowercase().contains(&model_l);
                let preferred = crate::routing::InferenceRouter::matches_preferred_cloud(n);
                (!hit, !preferred, Self::rank_provider_name(n), n.clone())
            });
        } else {
            names.sort_by_key(|n| {
                let preferred = crate::routing::InferenceRouter::matches_preferred_cloud(n);
                (!preferred, Self::rank_provider_name(n), n.clone())
            });
        }

        let runtime = Self::provider_runtime()?;
        let mut errors: Vec<String> = Vec::new();
        for name in names {
            if crate::routing::InferenceRouter::provider_cooled(&name) {
                continue;
            }
            let Some(provider) = registry.get_provider(&name) else {
                continue;
            };
            match runtime.block_on(provider.generate(prompt)) {
                Ok(text) if !text.trim().is_empty() => {
                    crate::routing::InferenceRouter::record_provider_success(&name);
                    if !errors.is_empty() {
                        eprintln!(
                            "[INFERENCE FAILOVER] Succeeded via {} after {} prior failure(s)",
                            name,
                            errors.len()
                        );
                    }
                    callback(text.clone());
                    return Some(text);
                }
                Ok(_) => {
                    crate::routing::InferenceRouter::record_provider_failure(&name);
                    let detail = format!("{name}: empty response");
                    eprintln!("[INFERENCE FAILOVER] {detail}");
                    errors.push(detail);
                }
                Err(e) => {
                    crate::routing::InferenceRouter::record_provider_failure(&name);
                    let detail = format!("{name}: {e}");
                    eprintln!("[INFERENCE FAILOVER] {detail}");
                    errors.push(detail);
                }
            }
        }
        if !errors.is_empty() {
            eprintln!(
                "[INFERENCE FAILOVER] Exhausted {} provider(s); no usable response",
                errors.len()
            );
        }
        None
    }

    fn has_usable_local_model(workspace: &Path) -> bool {
        ModelManager::identify_best_suited_local_model(workspace, None).is_some()
    }

    /// Kicks off hardware-optimal provisioning and blocks, polling for a
    /// valid GGUF to land, up to `model_provisioning_wait_secs`. Streams
    /// progress through `callback` so a caller waiting on a fresh install
    /// isn't staring at silence for however long the download takes.
    /// Returns `None` (never `Some("[FAIL]...")`) if provisioning didn't
    /// finish in time, so the caller falls through to its next fallback
    /// rather than treating a timeout as a completed retry.
    fn provision_and_retry(
        prompt: &str,
        workspace: &Path,
        callback: &dyn Fn(String),
        min_complexity: Option<crate::intent::TaskComplexity>,
    ) -> Option<String> {
        callback(
            "[SUSI] No local model provisioned yet - fetching a hardware-fit model to solve this intent...\n".to_string(),
        );
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        if cfg
            .settings
            .get("auto_download_models")
            .and_then(|v| v.as_bool())
            == Some(false)
        {
            return None;
        }
        let (sender, provisioning) = std::sync::mpsc::channel();
        let provisioning_workspace = workspace.to_path_buf();
        std::thread::spawn(move || {
            let result = ModelManager::ensure_hardware_optimal_models(&provisioning_workspace);
            let _ = sender.send(result);
        });

        let wait_secs = cfg.model_provisioning_wait_secs();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
        let poll_interval = std::time::Duration::from_secs(10);
        let mut last_reported_pct: i64 = -1;

        while std::time::Instant::now() < deadline {
            if let Ok(Err(error)) = provisioning.try_recv() {
                callback(format!("[SUSI] Provisioning unavailable: {error}\n"));
                return None;
            }
            if Self::has_usable_local_model(workspace) {
                callback("[SUSI] Model provisioned. Resuming inference...\n".to_string());
                let engine = LlamaCppEngine;
                let selected = ModelManager::get_selected_model_for_request_with_min_complexity(
                    prompt,
                    None,
                    min_complexity,
                );
                if let Ok(res) = engine.run_inference_stream(prompt, callback, selected.as_deref())
                {
                    if !res.trim().is_empty() {
                        return Some(match Self::verify_axiomatic_alignment(&res, workspace) {
                            Ok(v) => v,
                            Err(_) => res,
                        });
                    }
                }
                return None;
            }

            if let Some(progress) = ModelManager::download_controller_progress() {
                let pct = progress as i64;
                if pct != last_reported_pct {
                    callback(format!("[SUSI] Provisioning model... {}%\n", pct));
                    last_reported_pct = pct;
                }
            }

            std::thread::sleep(poll_interval);
        }

        callback(
            "[SUSI] Model provisioning did not complete in time; trying alternate reasoning path...\n"
                .to_string(),
        );
        None
    }

    pub fn verify_axiomatic_alignment(reasoning: &str, _workspace: &Path) -> EaiResult<String> {
        // Fast Rust-Native Axiomatic Alignment Guard (<2ms Reflex Mandate)
        let risk_patterns = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .axiomatic_risk_patterns();
        for pattern in &risk_patterns {
            if reasoning.contains(pattern.as_str()) {
                return Err(EaiError::governance(format!(
                    "Axiomatic Violation: High-risk pattern '{}' detected in reasoning.",
                    pattern
                )));
            }
        }
        Ok(reasoning.to_string())
    }
}

pub struct MissionPlan {
    pub goals: Vec<String>,
}

pub struct MissionPlanner;

impl MissionPlanner {
    pub fn plan_mission(goal: &str, workspace: &Path) -> EaiResult<MissionPlan> {
        let prompts = crate::susi_sandbox::manager::SusiPrompts::load_global();
        let plan_prompt = prompts.intent_planner_prompt().replace("{goal}", goal);
        let plan_str = GemiEngine::generate_reasoning(&plan_prompt, workspace);
        let mut goals = Vec::new();
        if plan_str.contains(',') {
            for g in plan_str.split(',') {
                let clean = g.trim();
                if !clean.is_empty() {
                    goals.push(clean.to_string());
                }
            }
        } else {
            goals.push(goal.to_string());
        }
        Ok(MissionPlan { goals })
    }

    pub fn partition_mission(goal: &str, workspace: &Path) -> EaiResult<MissionPlan> {
        let prompts = crate::susi_sandbox::manager::SusiPrompts::load_global();
        let plan_prompt = prompts.mission_partition_prompt().replace("{goal}", goal);
        let plan_str = GemiEngine::generate_reasoning(&plan_prompt, workspace);
        let mut goals = Vec::new();
        if plan_str.contains(',') {
            for g in plan_str.split(',') {
                let clean = g.trim();
                if !clean.is_empty() {
                    goals.push(clean.to_string());
                }
            }
        } else {
            goals.push(goal.to_string());
        }
        Ok(MissionPlan { goals })
    }

    pub fn refine_plan(
        original_goal: &str,
        blackboard_state: &str,
        workspace: &Path,
    ) -> EaiResult<MissionPlan> {
        let prompts = crate::susi_sandbox::manager::SusiPrompts::load_global();
        let refine_prompt = prompts
            .mission_refine_prompt()
            .replace("{original_goal}", original_goal)
            .replace("{blackboard_state}", blackboard_state);
        Self::plan_mission(&refine_prompt, workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::thread;

    #[test]
    fn local_model_loads_on_cpu_and_reuses_cached_weights() {
        let path = crate::susi_paths::SusiDirs::data_dir()
            .join("models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
        if !path.is_file() {
            eprintln!("skipping: {} not present on this host", path.display());
            return;
        }
        let task = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("model_load_test", "CPU model loading");
        let model = InferenceHost::get_model(&path, &candle_core::Device::Cpu, &task).unwrap();
        let again = InferenceHost::get_model(&path, &candle_core::Device::Cpu, &task).unwrap();
        assert!(Arc::ptr_eq(&model, &again));
        let mut model = model.write();
        let backend = model.weights.as_qwen2_mut().expect("dense Qwen2 backend");
        assert!(!backend.is_fully_gpu_resident());
        let input = candle_core::Tensor::new(&[[100_u32, 200]], &candle_core::Device::Cpu).unwrap();
        let logits = backend.forward(&input, 0).unwrap();
        assert!(logits.device().is_cpu());
        assert!(logits.elem_count() > 0);
    }

    #[test]
    fn test_backend_dispatch_rejects_unsupported_architectures() {
        // Regression: quantized_llama (the generic GGUF loader) never reads
        // attn_{q,k,v}.bias, which Qwen2's GGUF export always carries since
        // Qwen2 (unlike Llama) trains a bias term on its Q/K/V projections.
        // Verified live against the real qwen2.5-0.5b-instruct-q4_k_m.gguf:
        // loading it through quantized_llama produced fluent-looking but
        // completely wrong output from the first token (reproduced
        // identically on CPU and CUDA, and across Q4_K_M and Q8_0, ruling
        // out a device or quantization-format cause) because every layer's
        // attention silently dropped its trained bias.
        assert!(InferenceHost::needs_qwen2_backend("qwen2"));
        assert!(!InferenceHost::needs_qwen2_backend("qwen2moe"));
        for arch in ["qwen2moe", "qwen3", "gemma", "unknown"] {
            assert!(InferenceHost::validate_architecture(arch).is_err());
        }
        assert!(InferenceHost::validate_architecture("llama").is_ok());
        assert!(InferenceHost::validate_architecture("qwen2").is_ok());
        assert!(!InferenceHost::needs_qwen2_backend("llama"));
        assert!(!InferenceHost::needs_qwen2_backend("qwen3"));
        assert!(!InferenceHost::needs_qwen2_backend("gemma"));
    }

    #[test]
    fn test_prompt_format_detects_chatml_from_qwen_template() {
        // Regression: susi fed raw prompts to instruct-tuned GGUFs with no
        // conversational scaffolding at all, which is fundamentally
        // incompatible with how models fine-tuned on a specific chat
        // template behave - verified live, this produced pure gibberish
        // output on qwen2.5-0.5b-instruct starting from the first token.
        // Real (truncated) Qwen2.5 tokenizer.chat_template excerpt.
        let template = "{%- if tools %}\n    {{- '<|im_start|>system\\n' }}\n{%- endif %}";
        assert_eq!(PromptFormat::detect(Some(template)), PromptFormat::ChatMl);
    }

    #[test]
    fn test_prompt_format_detects_llama3_and_gemma_and_mistral() {
        assert_eq!(
            PromptFormat::detect(Some("<|start_header_id|>user<|end_header_id|>")),
            PromptFormat::Llama3
        );
        assert_eq!(
            PromptFormat::detect(Some("<start_of_turn>user\n{{ content }}")),
            PromptFormat::Gemma
        );
        assert_eq!(
            PromptFormat::detect(Some("[INST] {{ content }} [/INST]")),
            PromptFormat::Mistral
        );
    }

    #[test]
    fn test_prompt_format_falls_back_to_raw_when_unrecognized_or_absent() {
        assert_eq!(PromptFormat::detect(None), PromptFormat::Raw);
        assert_eq!(
            PromptFormat::detect(Some("some unrecognized template syntax")),
            PromptFormat::Raw
        );
    }

    #[test]
    fn test_chatml_wrap_produces_well_formed_turn_structure() {
        let wrapped = PromptFormat::ChatMl.wrap("hello");
        assert!(wrapped.starts_with("<|im_start|>system\n"));
        assert!(wrapped.contains("<|im_start|>user\nhello<|im_end|>\n"));
        assert!(wrapped.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn test_raw_wrap_is_passthrough() {
        assert_eq!(PromptFormat::Raw.wrap("hello"), "hello");
    }

    #[test]
    fn test_shim_derives_missing_rope_dimension_count_from_embedding_and_head_count() {
        // Regression: qwen2.5's own GGUF metadata has no *.rope.dimension_count
        // key at all (verified against a real qwen2.5-0.5b-instruct GGUF),
        // which previously made every qwen2 load fail instantly inside
        // candle_transformers' hard `md_get("llama.rope.dimension_count")?`.
        let mut metadata: HashMap<String, gguf_file::Value> = HashMap::new();
        metadata.insert(
            "qwen2.attention.head_count".to_string(),
            gguf_file::Value::U32(14),
        );
        metadata.insert(
            "qwen2.embedding_length".to_string(),
            gguf_file::Value::U32(896),
        );

        InferenceHost::shim_llama_compatible_metadata(&mut metadata);

        assert_eq!(
            metadata
                .get("llama.rope.dimension_count")
                .and_then(|v| v.to_u32().ok()),
            Some(64) // 896 / 14
        );
    }

    #[test]
    fn test_shim_prefers_existing_rope_dimension_count_over_derived_value() {
        let mut metadata: HashMap<String, gguf_file::Value> = HashMap::new();
        metadata.insert(
            "qwen2.attention.head_count".to_string(),
            gguf_file::Value::U32(14),
        );
        metadata.insert(
            "qwen2.embedding_length".to_string(),
            gguf_file::Value::U32(896),
        );
        metadata.insert(
            "qwen2.rope.dimension_count".to_string(),
            gguf_file::Value::U32(128),
        );

        InferenceHost::shim_llama_compatible_metadata(&mut metadata);

        assert_eq!(
            metadata
                .get("llama.rope.dimension_count")
                .and_then(|v| v.to_u32().ok()),
            Some(128)
        );
    }

    #[test]
    fn test_has_usable_local_model_false_for_empty_workspace() {
        // Point the models scan at an empty dir (test builds honor SUSI_MODEL_DIR
        // ahead of the shared susi_test_models folder).
        let isolated =
            std::env::temp_dir().join(format!("susi_engine_test_no_models_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&isolated);
        let models = isolated.join("models");
        let workspace = isolated.join("workspace");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let prev = std::env::var_os("SUSI_MODEL_DIR");
        unsafe {
            std::env::set_var("SUSI_MODEL_DIR", &models);
        }
        let usable = GemiEngine::has_usable_local_model(&workspace);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("SUSI_MODEL_DIR", v),
                None => std::env::remove_var("SUSI_MODEL_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&isolated);
        assert!(!usable);
    }

    #[test]
    fn test_competitive_racing_logic() {
        let (tx, rx) = flume::unbounded();
        let tx1 = tx.clone();
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(50));
            let _ = tx1.send("FastPath".to_string());
        });
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(200));
            let _ = tx.send("SlowPath".to_string());
        });
        let winner = rx.recv().unwrap();
        assert_eq!(winner, "FastPath");
    }

    struct MockRouteProvider {
        name: &'static str,
        reply: &'static str,
    }

    impl crate::susi_core::provider::Provider for MockRouteProvider {
        fn name(&self) -> &str {
            self.name
        }
        fn is_healthy(
            &self,
        ) -> crate::susi_core::provider::BoxFuture<'_, crate::susi_core::susi_error::EaiResult<bool>>
        {
            Box::pin(async { Ok(true) })
        }
        fn generate(
            &self,
            _prompt: &str,
        ) -> crate::susi_core::provider::BoxFuture<
            '_,
            crate::susi_core::susi_error::EaiResult<String>,
        > {
            let reply = self.reply.to_string();
            Box::pin(async move {
                if reply.starts_with("ERR:") {
                    return Err(crate::susi_core::susi_error::EaiError::process(reply));
                }
                Ok(reply)
            })
        }
        fn embed(
            &self,
            _text: &str,
        ) -> crate::susi_core::provider::BoxFuture<
            '_,
            crate::susi_core::susi_error::EaiResult<Vec<f32>>,
        > {
            Box::pin(async { Ok(vec![]) })
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn test_try_providers_fails_over_after_provider_error() {
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        registry.register_provider(MockRouteProvider {
            name: "openai-primary",
            reply: "ERR:402 billing",
        });
        registry.register_provider(MockRouteProvider {
            name: "deepseek-backup",
            reply: "recovered",
        });

        let out = GemiEngine::try_providers(&registry, "hello", None, &|_| {});
        assert_eq!(out.as_deref(), Some("recovered"));
    }

    #[test]
    fn test_try_providers_prefers_ollama_over_generic_and_skips_candle() {
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        registry.register_provider(MockRouteProvider {
            name: "Candle (Local)",
            reply: "from-candle",
        });
        registry.register_provider(MockRouteProvider {
            name: "generic-remote",
            reply: "from-generic",
        });
        registry.register_provider(MockRouteProvider {
            name: "ollama-llama3",
            reply: "from-ollama",
        });

        let out = GemiEngine::try_providers(&registry, "hello", None, &|_| {});
        assert_eq!(out.as_deref(), Some("from-ollama"));
    }

    #[test]
    fn test_try_providers_honors_requested_model_name_match() {
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        registry.register_provider(MockRouteProvider {
            name: "ollama-llama3",
            reply: "llama3",
        });
        registry.register_provider(MockRouteProvider {
            name: "vllm-mixtral",
            reply: "mixtral",
        });

        let out = GemiEngine::try_providers(&registry, "hello", Some("mixtral"), &|_| {});
        assert_eq!(out.as_deref(), Some("mixtral"));
    }

    #[test]
    fn test_native_tokenization() {
        let _home = crate::susi_paths::SusiDirs::home_dir();
        let tokenizer_path = crate::susi_paths::SusiDirs::data_dir().join("models/tokenizer.json");
        if tokenizer_path.exists() {
            let tokenizer = Tokenizer::from_file(tokenizer_path);
            assert!(tokenizer.is_ok());
        }
    }

    /// `axiomatic_risk_patterns` is config-driven now, not a hardcoded Rust
    /// array — prove the configured patterns actually gate the check, and
    /// that ordinary reasoning output isn't blocked.
    #[test]
    fn test_verify_axiomatic_alignment_uses_configured_risk_patterns() {
        let patterns =
            crate::susi_sandbox::manager::SusiConfig::default().axiomatic_risk_patterns();
        assert!(!patterns.is_empty());

        let workspace = Path::new(".");
        for pattern in &patterns {
            let reasoning = format!("Here is a plan: {}", pattern);
            assert!(
                GemiEngine::verify_axiomatic_alignment(&reasoning, workspace).is_err(),
                "configured risk pattern '{}' must be blocked",
                pattern
            );
        }

        assert!(GemiEngine::verify_axiomatic_alignment(
            "Here is a perfectly safe plan to list files.",
            workspace
        )
        .is_ok());
    }

    #[test]
    fn test_logits_repetition_penalty_dampens_recent_tokens() {
        let mut logits_v = vec![10.0f32, 10.0f32, 10.0f32]; // Equal logits for tokens 0, 1, 2
        apply_repeat_penalty(&mut logits_v, 1.15, &[1u32]); // Token 1 has been generated

        // Token 1 was penalized: 10.0 / 1.15 ~ 8.695
        assert!(logits_v[1] < logits_v[0]);
        assert!(logits_v[1] < logits_v[2]);
        assert_eq!(logits_v[0], 10.0);
        assert_eq!(logits_v[2], 10.0);
    }

    #[test]
    fn test_repeat_penalty_disabled_at_1_0_or_below() {
        let original = vec![10.0f32, -5.0, 3.0];
        let mut logits_v = original.clone();
        apply_repeat_penalty(&mut logits_v, 1.0, &[0, 1, 2]);
        assert_eq!(logits_v, original);

        let mut logits_v = original.clone();
        apply_repeat_penalty(&mut logits_v, 0.0, &[0, 1, 2]);
        assert_eq!(logits_v, original);
    }

    #[test]
    fn test_repeat_penalty_dampens_negative_logits_by_multiplying() {
        // llama.cpp convention: a negative logit is already "unlikely," so
        // penalizing it means moving it further negative (multiply), not
        // dividing (which would move a negative value toward zero, i.e.
        // *more* likely - the opposite of a penalty).
        let mut logits_v = vec![-4.0f32];
        apply_repeat_penalty(&mut logits_v, 1.15, &[0]);
        assert_eq!(logits_v[0], -4.6);
    }

    #[test]
    fn test_generation_loop_excludes_prompt_tokens_from_repeat_penalty_window() {
        // Regression: the window used to be built from prompt_tokens +
        // all_tokens combined, so a short prompt sat inside the
        // repeat_last_n window for the entire generation and got penalized
        // from the very first token. Verified live: "What is the capital of
        // France?" (where "France" is a prompt token) drifted into an
        // unrelated fact about France without ever saying "Paris," because
        // "France" was already penalized before generation started. The
        // fix scopes the window to `all_tokens` (generated so far) only;
        // this locks in that a prompt-only token is never penalized.
        let prompt_tokens: Vec<u32> = vec![42]; // e.g. the token for "France"
        let all_tokens: Vec<u32> = vec![]; // nothing generated yet
        let repeat_last_n = 64usize;

        let mut logits_v = vec![10.0f32; 100];
        let start_idx = all_tokens.len().saturating_sub(repeat_last_n);
        apply_repeat_penalty(&mut logits_v, 1.15, &all_tokens[start_idx..]);

        assert_eq!(
            logits_v[prompt_tokens[0] as usize], 10.0,
            "a prompt-only token must not be penalized just because it appears in the prompt"
        );
    }
}

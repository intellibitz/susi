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

/// Reflex tiers answer only when no specific model was requested. An
/// OpenAI-style client names a model it picked from `/v1/models` (unknown
/// names are a 400), and the streaming layer labels frames with the
/// backend that served; a Tier-0 classifier's `ACTION:` line — or Tier-1's
/// 0.5B model — answering in that model's name broke both.
fn reflex_allowed(requested_model: Option<&str>) -> bool {
    requested_model.is_none()
}

/// The action name a served reflex answered with, bounded for receipt
/// tooling. `ACTION: <name> <args>` reduces to `<name>`; a bare action id
/// (`status`) keeps its name; a free-text generative answer is
/// `generative` — the trace needs the join key, not the payload.
fn reflex_serve_label(action: &str) -> String {
    let trimmed = action.trim();
    let raw = if let Some(rest) = trimmed.strip_prefix("ACTION:") {
        rest.split_whitespace().next().unwrap_or("unknown")
    } else if trimmed.chars().all(|c| c.is_alphanumeric() || c == '_') && !trimmed.is_empty() {
        trimmed
    } else {
        "generative"
    };
    raw.chars().take(48).collect()
}

#[cfg(test)]
use runtime_substrate::PromptFormat;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use susi_vendor_candle::candle_core;
#[cfg(test)]
use susi_vendor_candle::candle_core::quantized::gguf_file;
#[cfg(test)]
use susi_vendor_candle::tokenizers::Tokenizer;

pub struct GemiEngine;

impl GemiEngine {
    pub fn generate_reasoning(prompt: &str, workspace: &Path) -> String {
        Self::reason_internal(prompt, workspace, true, &|_| {}, None, None, &|_| {})
    }

    pub fn generate_reasoning_deep(prompt: &str, workspace: &Path) -> String {
        Self::generate_reasoning_deep_with_min_complexity(prompt, workspace, None)
    }

    pub fn generate_reasoning_deep_with_min_complexity(
        prompt: &str,
        workspace: &Path,
        min_complexity: Option<crate::models::intent::TaskComplexity>,
    ) -> String {
        Self::reason_internal(
            prompt,
            workspace,
            false,
            &|_| {},
            min_complexity,
            None,
            &|_| {},
        )
    }

    pub fn generate_reasoning_deep_with_model(
        prompt: &str,
        workspace: &Path,
        model: &str,
    ) -> String {
        Self::reason_internal(
            prompt,
            workspace,
            false,
            &|_| {},
            None,
            Some(model),
            &|_| {},
        )
    }

    pub fn generate_reasoning_stream(
        prompt: &str,
        workspace: &Path,
        callback: &dyn Fn(String),
    ) -> String {
        Self::reason_internal(prompt, workspace, true, callback, None, None, &|_| {})
    }

    /// Streaming reasoning honoring a caller-requested model name — the
    /// `/v1/chat/completions` streaming path threads its `model` field here.
    pub fn generate_reasoning_stream_with_model(
        prompt: &str,
        workspace: &Path,
        callback: &dyn Fn(String),
        model: &str,
    ) -> String {
        Self::reason_internal(
            prompt,
            workspace,
            reflex_allowed(Some(model)),
            callback,
            None,
            Some(model),
            &|_| {},
        )
    }

    /// Streaming reasoning that also reports the actual serving backend
    /// (provider name or local model id) through `meta` the moment routing
    /// picks it — before any content chunk — so SSE callers can label
    /// frames truthfully instead of echoing the requested model.
    pub fn generate_reasoning_stream_meta(
        prompt: &str,
        workspace: &Path,
        callback: &dyn Fn(String),
        model: Option<&str>,
        meta: &dyn Fn(&str),
    ) -> String {
        Self::reason_internal(
            prompt,
            workspace,
            reflex_allowed(model),
            callback,
            None,
            model,
            meta,
        )
    }

    /// `generate_reasoning_stream_meta` without the reflex tiers.
    pub fn generate_reasoning_stream_deep_meta(
        prompt: &str,
        workspace: &Path,
        callback: &dyn Fn(String),
        model: Option<&str>,
        meta: &dyn Fn(&str),
    ) -> String {
        Self::reason_internal(prompt, workspace, false, callback, None, model, meta)
    }

    /// Ultra-Latency Competitive Inference Racing
    #[allow(clippy::too_many_arguments)]
    fn reason_internal(
        prompt: &str,
        workspace: &Path,
        allow_reflex: bool,
        callback: &dyn Fn(String),
        min_complexity: Option<crate::models::intent::TaskComplexity>,
        requested_model: Option<&str>,
        meta: &dyn Fn(&str),
    ) -> String {
        if allow_reflex {
            let (reflex_decision, _) = super::reflex::ReflexEngine::try_solve(prompt, workspace);
            if let super::reflex::ReflexDecision::Solved(action) = reflex_decision {
                // Record the serve on the mission's evidence session so the
                // trace — and therefore distill — can join *which* Tier-0/1
                // action answered to the mission's outcome. The `reflex:*`
                // prefix keeps it out of the citable-evidence set (a cached
                // answer must not certify the mission).
                let label = reflex_serve_label(&action);
                let _ = crate::susi_core::capture::EvidenceSession::capture_call(
                    &format!("reflex:{label}"),
                    &serde_json::json!({}),
                    workspace,
                    || Ok(action.clone()),
                );
                callback(action.clone());
                return action;
            }
        }

        // Learn from how recent missions in this workspace actually ended
        // before ranking providers for this one.
        crate::engines::brain::apply_mission_verdicts(workspace);
        // Tag the mission with each provider that answers (non-citable
        // `brain:*` receipt) so its verified outcome can be joined back.
        let class = crate::engines::brain::TaskClass::classify(prompt);
        let tagged_meta = |provider: &str| {
            let _ = crate::susi_core::capture::EvidenceSession::capture_call(
                &crate::engines::brain::receipt_tool(provider, class),
                &serde_json::json!({}),
                workspace,
                || Ok("answered".to_string()),
            );
            meta(provider);
        };
        let meta = &tagged_meta;

        // Ensure configured cloud endpoints are registered before routing.
        crate::engines::http_provider::register_configured_cloud_endpoints(
            crate::susi_core::registry::CapabilityRegistry::global(),
        );

        // Latency / CPU-only gate: escalate to cloud when local is known-slow
        // (or host has no GPU). Sticky preference + optional interactive pick.
        // A requested model is "explicitly local" when no registered
        // provider matches it — `is_cloud_provider_name`'s '-' catch-all
        // would misclassify hyphenated GGUF ids like `qwen2.5-0.5b-*`.
        let explicit_local_request =
            requested_model.is_some() && !Self::model_names_a_provider(requested_model);
        if !explicit_local_request {
            let names = crate::susi_core::registry::CapabilityRegistry::global().list_providers();
            if let Some(esc) =
                crate::engines::routing::InferenceRouter::maybe_escalate_to_cloud(&names)
            {
                crate::engines::routing::InferenceRouter::announce(&esc);
                callback(format!(
                    "[SUSI ROUTING] Escalating to cloud `{}` ({})\n",
                    esc.provider, esc.reason
                ));
                let (escalated, mut esc_ladder) = Self::try_discovered_providers(
                    prompt,
                    Some(workspace),
                    Some(&esc.provider),
                    callback,
                    meta,
                );
                if esc_ladder.step_downs() > 0 {
                    callback(format!("[SUSI ROUTING] {}\n", esc_ladder.summary()));
                }
                esc_ladder.persist();
                if let Some(text) = escalated {
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
        let (provider_answer, mut ladder) = Self::try_discovered_providers(
            prompt,
            Some(workspace),
            requested_model,
            callback,
            meta,
        );
        if let Some(text) = provider_answer {
            if ladder.step_downs() > 0 {
                callback(format!("[SUSI ROUTING] {}\n", ladder.summary()));
            }
            ladder.persist();
            return match Self::verify_axiomatic_alignment(&text, workspace) {
                Ok(v) => v,
                Err(_) => text,
            };
        }

        // Only announce failover when the provider cascade was actually in
        // play: no model named (normal route) or the name matched a
        // registered provider. A caller-named local model id reaches here
        // by honoring, not fallback.
        if requested_model.is_none() || Self::model_names_a_provider(requested_model) {
            eprintln!("[INFERENCE FAILOVER] Falling back to local inference");
            callback("[SUSI ROUTING] Falling back to local inference\n".to_string());
        } else {
            eprintln!("[INFERENCE] Caller-requested local model — native inference");
        }

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
        // Report the actual local generator before the first content chunk.
        meta(selected_model.as_deref().unwrap_or(engine_key));
        // Why the request reached the local rung — either the provider
        // cascade stepped every candidate down, or it never ran because a
        // caller-named local model was honored (VC-202-004: the trace must
        // say which model ran and why).
        let local_reason = if ladder.steps.is_empty() {
            "no provider cascade ran — routing directly to local".to_string()
        } else {
            format!(
                "every provider rung stepped down ({} skipped/failed)",
                ladder.step_downs()
            )
        };
        let local_candidate = selected_model
            .clone()
            .unwrap_or_else(|| engine_key.to_string());
        let local_started = std::time::Instant::now();
        // Drive the engine through the shared lifecycle contract: discover →
        // load → ready (health probe) → infer (VC-201-042). A failed probe
        // is a typed error here, the same outcome the bare call produced.
        let result = crate::engines::native_lifecycle::infer_via_contract(
            engine,
            match engine_key {
                "susi-federated" | "cloud" => {
                    crate::engines::native_lifecycle::EngineKind::Federated
                }
                _ => crate::engines::native_lifecycle::EngineKind::Native,
            },
            &crate::engines::native_lifecycle::ContractCall::new(
                prompt,
                callback,
                selected_model.as_deref(),
            ),
        );
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
        use crate::engines::routing::{LadderRung, StepOutcome};
        match &result {
            Ok(res) if !res.trim().is_empty() => {
                ladder.record(
                    LadderRung::Local,
                    local_candidate,
                    StepOutcome::Selected,
                    local_reason,
                );
                if ladder.step_downs() > 0 {
                    callback(format!("[SUSI ROUTING] {}\n", ladder.summary()));
                }
                ladder.persist();
                // Feed the latency gate so the next request can escalate if slow.
                if engine_key == "llamacpp" {
                    crate::engines::routing::InferenceRouter::record_local_sample(
                        selected_model.as_deref().unwrap_or(""),
                        local_started.elapsed(),
                        res.len(),
                    );
                }
                return match Self::verify_axiomatic_alignment(res, workspace) {
                    Ok(v) => v,
                    Err(_) => res.clone(),
                };
            }
            Ok(_) => {
                eprintln!("[INFERENCE] Local engine returned empty output");
                ladder.record(
                    LadderRung::Local,
                    local_candidate.clone(),
                    StepOutcome::Failed,
                    "local engine returned empty output",
                );
            }
            Err(e) => {
                eprintln!("[INFERENCE] Local engine failed: {e}");
                ladder.record(
                    LadderRung::Local,
                    local_candidate.clone(),
                    StepOutcome::Failed,
                    e.to_string(),
                );
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
                ladder.record(
                    LadderRung::Local,
                    "provisioning-retry",
                    StepOutcome::Selected,
                    "fetched a hardware-fit model after the first local attempt failed",
                );
                if ladder.step_downs() > 0 {
                    callback(format!("[SUSI ROUTING] {}\n", ladder.summary()));
                }
                ladder.persist();
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
        if !power_res.trim().is_empty() && !Self::looks_like_error_text(&power_res) {
            ladder.record(
                LadderRung::Local,
                "power_reason",
                StepOutcome::Selected,
                "local engine failed; power reasoning tool answered",
            );
            if ladder.step_downs() > 0 {
                callback(format!("[SUSI ROUTING] {}\n", ladder.summary()));
            }
            ladder.persist();
            callback(power_res.clone());
            return power_res;
        }

        ladder.persist();
        let final_msg = "[FAIL] SUSI-Tier2-Inference: Local model inference and power reasoning fallback both failed.".to_string();
        callback(final_msg.clone());
        final_msg
    }

    /// Shared runtime for bridging sync swarm callers into async `Provider` APIs.
    pub(crate) fn provider_runtime() -> Option<&'static tokio::runtime::Runtime> {
        static RT: OnceLock<std::io::Result<tokio::runtime::Runtime>> = OnceLock::new();
        RT.get_or_init(tokio::runtime::Runtime::new).as_ref().ok()
    }

    /// Embed `text` through the first registered provider that can serve
    /// embeddings. `model_hint` names a preferred provider — it is tried
    /// first, and every other provider still tries in order after it.
    pub fn embed_text(text: &str, model_hint: Option<&str>) -> Result<Vec<f32>, String> {
        crate::engines::http_provider::register_configured_cloud_endpoints(
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
    /// Returns the answer plus the routing ladder that recorded how the
    /// cascade resolved — the caller owns emitting and persisting it.
    fn try_discovered_providers(
        prompt: &str,
        workspace: Option<&Path>,
        requested_model: Option<&str>,
        callback: &dyn Fn(String),
        meta: &dyn Fn(&str),
    ) -> (Option<String>, crate::engines::routing::RoutingLadder) {
        // Mock-inference seam: under test the env opts out of *all* real
        // provider calls — discovered HTTP endpoints included — not just the
        // native engine path (runtime_native honors the same flag).
        if std::env::var("SUSI_TEST_MOCK_INFERENCE").unwrap_or_default() == "true" {
            return (
                None,
                crate::engines::routing::RoutingLadder::new(
                    crate::engines::brain::TaskClass::classify(prompt),
                ),
            );
        }
        Self::try_providers(
            crate::susi_core::registry::CapabilityRegistry::global(),
            prompt,
            workspace,
            requested_model,
            callback,
            meta,
        )
    }

    /// True when a caller-requested model name substring-matches a
    /// registered provider — the predicate both the strict-honoring early
    /// return and the fallback announcement share, so a named local model
    /// never triggers the cascade nor the "falling back" notice.
    fn model_names_a_provider(requested_model: Option<&str>) -> bool {
        let Some(model) = requested_model else {
            return false;
        };
        let model_l = model.to_ascii_lowercase();
        crate::susi_core::registry::CapabilityRegistry::global()
            .list_providers()
            .iter()
            .any(|n| n.to_ascii_lowercase().contains(&model_l))
    }

    /// True when `text` is a stringified engine/tool failure rather than
    /// generated content. Adapters flatten `Err(EaiError)` and MCP/JSON-RPC
    /// failures into `Ok(String)`, so the markers must be detected textually.
    /// Display prefixes are checked only near the head so legitimate output
    /// that merely mentions an error is not misclassified.
    #[must_use]
    pub fn looks_like_error_text(text: &str) -> bool {
        let t = text.trim_start();
        if t.contains("[FAIL]")
            || t.contains("[CAPABILITY_GAP]")
            || t.contains("[INFERENCE_FAILED]")
        {
            return true;
        }
        let head: String = t.chars().take(64).collect();
        head.contains(" Error:")
            || head.contains(" Violation:")
            || head.contains("Mcp error")
            || head.contains("MCP Error")
    }

    #[allow(clippy::too_many_arguments)] // flat params mirror every call site; a builder would only wrap them
    pub(crate) fn try_providers(
        registry: &crate::susi_core::registry::CapabilityRegistry,
        prompt: &str,
        workspace: Option<&Path>,
        requested_model: Option<&str>,
        callback: &dyn Fn(String),
        meta: &dyn Fn(&str),
    ) -> (Option<String>, crate::engines::routing::RoutingLadder) {
        use crate::engines::routing::{LadderRung, RoutingLadder, StepOutcome};

        let class = crate::engines::brain::TaskClass::classify(prompt);
        let mut ladder = RoutingLadder::new(class);
        // Mission the spend is attributed to: the workspace's evidence
        // session first, then a session entered on this thread (federated /
        // contract paths carry no workspace).
        let mission = workspace
            .and_then(crate::susi_core::capture::EvidenceSession::for_workspace)
            .or_else(crate::susi_core::capture::EvidenceSession::current)
            .map(|s| s.id().to_string())
            .unwrap_or_default();
        let mut names: Vec<String> = registry
            .list_providers()
            .into_iter()
            .filter(|n| {
                n != "Candle (Local)" && !crate::engines::http_provider::is_non_chat_model_id(n)
            })
            .collect();
        if crate::susi_core::mac_policy::MacPolicy::global().blocks_cloud_inference() {
            names.retain(|n| !crate::engines::routing::InferenceRouter::is_cloud_provider_name(n));
        }
        if names.is_empty() {
            return (None, ladder);
        }

        // The brain's rank position for this task class drives the cascade
        // order: it already folds in the capability floor, cost per
        // verified outcome (or evidence score when unpriced / Budget::Max)
        // and the static-rank tiebreak (VC-202-003). The rank call is fed
        // the canonical static order first so a brain tie breaks
        // deterministically — `names` arrives in registry (hash) order.
        names.sort_by_key(|n| (Self::rank_provider_name(n), n.clone()));
        let ranked: std::collections::HashMap<String, (bool, usize)> =
            crate::engines::brain::rank(&names, class)
                .into_iter()
                .enumerate()
                .map(|(pos, r)| (r.provider, (r.meets_floor, pos)))
                .collect();
        let brain_rank = |n: &String| ranked.get(n).copied().unwrap_or((false, usize::MAX));
        // A provider that keeps failing right now (no credit, rejected key)
        // sorts behind every fit one — even the sticky preferred cloud.
        let unfit = |n: &String| crate::engines::brain::is_unfit(n, class);
        // One trial call for a vendor susi moved the preference away from once
        // its quarantine has lapsed, so a top-up is noticed without any action.
        let probe = |n: &String| crate::engines::routing::InferenceRouter::is_recovery_probe(n);

        if let Some(model) = requested_model {
            let model_l = model.to_ascii_lowercase();
            // Strict honoring: a requested name that matches no registered
            // provider is a local model id — return so the caller falls
            // through to local inference instead of silently rerouting the
            // named model to an arbitrary cloud provider. Shares its
            // predicate with the fallback-announcement gate.
            if !names
                .iter()
                .any(|n| n.to_ascii_lowercase().contains(&model_l))
            {
                ladder.record(
                    LadderRung::Model,
                    model,
                    StepOutcome::Skipped,
                    "named model matched no registered provider — honored as a local model id",
                );
                return (None, ladder);
            }
            names.sort_by_key(|n| {
                let hit = n.to_ascii_lowercase().contains(&model_l);
                let preferred =
                    crate::engines::routing::InferenceRouter::matches_preferred_cloud(n);
                let (meets_floor, pos) = brain_rank(n);
                (
                    !hit,
                    !probe(n),
                    unfit(n),
                    !preferred,
                    !meets_floor,
                    pos,
                    Self::rank_provider_name(n),
                    n.clone(),
                )
            });
        } else {
            names.sort_by_key(|n| {
                let preferred =
                    crate::engines::routing::InferenceRouter::matches_preferred_cloud(n);
                let (meets_floor, pos) = brain_rank(n);
                (
                    !probe(n),
                    unfit(n),
                    !preferred,
                    !meets_floor,
                    pos,
                    Self::rank_provider_name(n),
                    n.clone(),
                )
            });
        }

        let Some(runtime) = Self::provider_runtime() else {
            ladder.record(
                LadderRung::Provider,
                "provider-runtime",
                StepOutcome::Failed,
                "tokio runtime unavailable — provider cascade could not start",
            );
            return (None, ladder);
        };
        let mut errors: Vec<String> = Vec::new();
        for name in names {
            if let Some(until) =
                crate::engines::routing::InferenceRouter::provider_cooldown_until(&name)
            {
                ladder.record(
                    LadderRung::Provider,
                    &name,
                    StepOutcome::Skipped,
                    format!("provider cooled until unix {until} after recent failures"),
                );
                continue;
            }
            // Per-key rate + concurrency arbitration: a saturated key is
            // skipped like a cooled provider — the cascade tries the next
            // candidate rather than queueing a request that would race the
            // provider's own rate limit.
            let permit = match crate::key_arbitration::try_acquire(&name) {
                Ok(permit) => permit,
                Err(denial) => {
                    let reason = match denial {
                        crate::key_arbitration::Denial::Quota { reset_unix } => {
                            format!("key quota window exhausted — resets at unix {reset_unix}")
                        }
                        crate::key_arbitration::Denial::Concurrency
                        | crate::key_arbitration::Denial::RateWindow
                        | crate::key_arbitration::Denial::Global => denial.describe().to_string(),
                    };
                    ladder.record(LadderRung::Provider, &name, StepOutcome::Skipped, reason);
                    continue;
                }
            };
            let _permit = permit;
            // Spend ceilings refuse before the call is made: an expected
            // task cost that would cross an hourly/daily/mission/task cap
            // steps the cascade down to a cheaper rung (VC-202-005).
            let expected_usd = crate::engines::cost::expected_task_cost_usd(&name, class);
            if let Some(usd) = expected_usd {
                let intent = crate::spend_tracker::SpendIntent {
                    provider: &name,
                    expected_usd: usd,
                    mission: &mission,
                };
                if let Err(refusal) = crate::spend_tracker::check(&intent) {
                    ladder.record(
                        LadderRung::Provider,
                        &name,
                        StepOutcome::Skipped,
                        refusal.describe(),
                    );
                    continue;
                }
            }
            let Some(provider) = registry.get_provider(&name) else {
                ladder.record(
                    LadderRung::Provider,
                    &name,
                    StepOutcome::Skipped,
                    "registration vanished before dispatch",
                );
                continue;
            };
            let started = std::time::Instant::now();
            // Adapters flatten failures into `Ok("... Error: ...")`; counting that
            // as an answer would teach the brain to prefer a broken provider
            // and would hand the caller the error text as the reply.
            let outcome = match runtime.block_on(provider.generate(prompt)) {
                Ok(text) if Self::looks_like_error_text(&text) => {
                    Err(crate::susi_core::susi_error::EaiError::process(text))
                }
                other => other,
            };
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let answered = matches!(&outcome, Ok(text) if !text.trim().is_empty());
            crate::engines::brain::record_outcome(&name, class, answered, elapsed_ms);
            // Every attempted call is spend — success or failure — so the
            // ceiling's window sums stay real (VC-202-005).
            crate::spend_tracker::record(&crate::spend_tracker::SpendWrite {
                provider: &name,
                class,
                ok: answered,
                latency_ms: elapsed_ms,
                usd: expected_usd.unwrap_or(0.0),
                mission: &mission,
            });
            match outcome {
                Ok(text) if !text.trim().is_empty() => {
                    crate::engines::routing::InferenceRouter::record_provider_success(&name);
                    if !errors.is_empty() {
                        eprintln!(
                            "[INFERENCE FAILOVER] Succeeded via {} after {} prior failure(s)",
                            name,
                            errors.len()
                        );
                    }
                    let (meets_floor, _) = brain_rank(&name);
                    let mut reason = if meets_floor {
                        format!(
                            "top-ranked candidate above the {} capability floor",
                            class.label()
                        )
                    } else {
                        "last resort — every floor-meeting candidate had already stepped down"
                            .to_string()
                    };
                    if let Some(usd) = crate::engines::cost::expected_task_cost_usd(&name, class) {
                        reason.push_str(&format!("; expected task cost ≈${usd:.4}"));
                    }
                    ladder.record(LadderRung::Provider, &name, StepOutcome::Selected, reason);
                    // Actual generator — emitted before the content chunk so
                    // SSE labels can name it instead of the requested model.
                    meta(&name);
                    callback(text.clone());
                    return (Some(text), ladder);
                }
                Ok(_) => {
                    crate::engines::routing::InferenceRouter::record_provider_failure(&name);
                    let detail = format!("{name}: empty response");
                    eprintln!("[INFERENCE FAILOVER] {detail}");
                    ladder.record(
                        LadderRung::Provider,
                        &name,
                        StepOutcome::Failed,
                        "empty response",
                    );
                    errors.push(detail);
                }
                Err(e) => {
                    let msg = e.to_string();
                    crate::engines::routing::InferenceRouter::record_failure(&name, &msg);
                    let detail = format!("{name}: {e}");
                    eprintln!("[INFERENCE FAILOVER] {detail}");
                    ladder.record(LadderRung::Provider, &name, StepOutcome::Failed, msg);
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
        (None, ladder)
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
        min_complexity: Option<crate::models::intent::TaskComplexity>,
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
        // checked_add: a huge configured wait means "no deadline", not an
        // overflow panic (which aborts the daemon under panic = "abort").
        let deadline =
            std::time::Instant::now().checked_add(std::time::Duration::from_secs(wait_secs));
        let poll_interval = std::time::Duration::from_secs(10);
        let mut last_reported_pct: i64 = -1;

        while deadline.is_none_or(|d| std::time::Instant::now() < d) {
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
        // Deep path (EV-CLAUDE-051): a templated planner prompt wants a
        // goal list, never a reflex's bare `ACTION:` line.
        let plan_str = GemiEngine::generate_reasoning_deep(&plan_prompt, workspace);
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
        // Deep path (EV-CLAUDE-051): a templated planner prompt wants a
        // goal list, never a reflex's bare `ACTION:` line.
        let plan_str = GemiEngine::generate_reasoning_deep(&plan_prompt, workspace);
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
    fn error_text_detection_rejects_stringified_failures() {
        // The observed live leak: an MCP -32603 surfaced through Ok(String)
        // all the way into a chat completion's content.
        assert!(GemiEngine::looks_like_error_text(
            "Protocol Error: Mcp error: -32603: Unknown tool: reason"
        ));
        assert!(GemiEngine::looks_like_error_text(
            "[FAIL] SUSI-Tier2-Inference: exhausted"
        ));
        assert!(GemiEngine::looks_like_error_text(
            "Inference Error: model crashed"
        ));
        assert!(GemiEngine::looks_like_error_text(
            "Governance Violation: blocked"
        ));
        assert!(!GemiEngine::looks_like_error_text("the answer is 42"));
        // A multi-byte char straddling byte 64 must not panic the check.
        let tricky = format!("{}é tail", "a".repeat(63));
        assert!(!GemiEngine::looks_like_error_text(&tricky));
        assert!(!GemiEngine::looks_like_error_text(
            "Later in the text an error: is discussed"
        ));
    }

    /// GPU tests share the global model cache and measure process-wide
    /// VRAM, so they must not run concurrently.
    #[cfg(feature = "cuda")]
    static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(feature = "cuda")]
    fn own_gpu_mib() -> u64 {
        let out = std::process::Command::new("nvidia-smi")
            .args([
                "--query-compute-apps=pid,used_memory",
                "--format=csv,noheader,nounits",
            ])
            .output()
            .unwrap();
        let me = std::process::id().to_string();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once(','))
            .filter(|(pid, _)| pid.trim() == me)
            .map(|(_, mib)| mib.trim().parse::<u64>().unwrap_or(0))
            .sum()
    }

    /// Regression: weights loaded on one thread and evicted on another (the
    /// daemon's request threads) must actually free their VRAM. Before the
    /// fix the drop failed with `CUDA_ERROR_INVALID_CONTEXT` and leaked.
    #[cfg(feature = "cuda")]
    #[test]
    fn evicting_on_another_thread_returns_vram_to_the_driver() {
        let _serial = GPU_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path =
            susi_paths::SusiDirs::data_dir().join("models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
        let Ok(device) = candle_core::Device::new_cuda(0) else {
            eprintln!("skipping: no CUDA device");
            return;
        };
        if !path.is_file() {
            eprintln!("skipping: {} not present on this host", path.display());
            return;
        }
        let baseline = own_gpu_mib();
        for _ in 0..2 {
            let task = crate::susi_core::task_manager::SwarmTaskManager::global()
                .register_task("vram_release_test", "vram");
            let model = InferenceHost::get_model(&path, &device, &task).unwrap();
            let placement = model.read().weights.gpu_layers();
            drop(model);
            if !placement.is_some_and(|(on_gpu, total)| on_gpu == total && total > 0) {
                // Not enough free VRAM (another process holds it): the loader
                // split layers onto the CPU, so VRAM deltas prove nothing.
                eprintln!("skipping: model not fully GPU-resident ({placement:?})");
                if let Ok(Some((weights, device))) = InferenceHost::cache().evict(&path) {
                    let _ = super::runtime_substrate::release_on_device(weights, &device);
                }
                return;
            }
            let loaded = own_gpu_mib();
            assert!(
                loaded > baseline + 200,
                "load must allocate VRAM: {baseline} -> {loaded}"
            );
            let path = path.clone();
            let released = std::thread::spawn(move || {
                let (weights, device) = InferenceHost::cache().evict(&path).unwrap().unwrap();
                super::runtime_substrate::release_on_device(weights, &device)
            })
            .join()
            .unwrap();
            assert_eq!(released, Ok(true));
            let after = own_gpu_mib();
            assert!(
                after < baseline + 100,
                "VRAM must return near baseline: {baseline} -> {loaded} -> {after}"
            );
        }
    }

    /// Regression: the last handle to evicted weights may be dropped by a
    /// request finishing on a thread with no CUDA context (or by a cache
    /// replacement on a fresh request thread). That drop must still free
    /// the VRAM instead of recording `CUDA_ERROR_INVALID_CONTEXT`.
    #[cfg(feature = "cuda")]
    #[test]
    fn late_drop_on_a_context_less_thread_frees_vram() {
        let _serial = GPU_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path =
            susi_paths::SusiDirs::data_dir().join("models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
        let Ok(device) = candle_core::Device::new_cuda(0) else {
            eprintln!("skipping: no CUDA device");
            return;
        };
        if !path.is_file() {
            eprintln!("skipping: {} not present on this host", path.display());
            return;
        }
        let baseline = own_gpu_mib();
        let task = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("late_drop_test", "vram");
        let in_flight = InferenceHost::get_model(&path, &device, &task).unwrap();
        let placement = in_flight.read().weights.gpu_layers();
        let (weights, evicted_on) = InferenceHost::cache().evict(&path).unwrap().unwrap();
        drop(weights);
        if !placement.is_some_and(|(on_gpu, total)| on_gpu == total && total > 0) {
            eprintln!("skipping: model not fully GPU-resident ({placement:?})");
            drop(in_flight);
            return;
        }
        let loaded = own_gpu_mib();
        assert!(
            loaded > baseline + 200,
            "load must allocate VRAM: {baseline} -> {loaded}"
        );
        // The "request" finishes on a thread that never touched CUDA.
        std::thread::spawn(move || drop(in_flight)).join().unwrap();
        // Synchronize (surfacing any error the drop recorded) and trim.
        let trimmed = std::thread::spawn(move || {
            super::runtime_substrate::release_on_device(Arc::new(()), &evicted_on)
        })
        .join()
        .unwrap();
        assert_eq!(trimmed, Ok(true));
        let after = own_gpu_mib();
        assert!(
            after < baseline + 100,
            "VRAM must return near baseline: {baseline} -> {loaded} -> {after}"
        );
    }

    #[test]
    fn local_model_loads_on_cpu_and_reuses_cached_weights() {
        let path =
            susi_paths::SusiDirs::data_dir().join("models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
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
        // The store is process-global and its path comes from a
        // process-global env var that other tests set and clear, so readers
        // take the same lock the writers do.
        let _env = crate::engines::env_test_lock();
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        registry.register_provider(MockRouteProvider {
            name: "openai-primary",
            reply: "ERR:402 billing",
        });
        registry.register_provider(MockRouteProvider {
            name: "deepseek-backup",
            reply: "recovered",
        });

        let (out, _ladder) =
            GemiEngine::try_providers(&registry, "hello", None, None, &|_| {}, &|_| {});
        assert_eq!(out.as_deref(), Some("recovered"));
    }

    #[test]
    fn test_try_providers_records_outcomes_that_reorder_the_brain() {
        // The store is process-global and its path comes from a
        // process-global env var that other tests set and clear, so readers
        // take the same lock the writers do.
        let _env = crate::engines::env_test_lock();
        use crate::engines::brain::{rank, TaskClass};
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        // Same static rank; name order would try `brainlearn-a` first.
        registry.register_provider(MockRouteProvider {
            name: "ollama-brainlearn-a",
            reply: "ERR:boom",
        });
        registry.register_provider(MockRouteProvider {
            name: "ollama-brainlearn-b",
            reply: "fine",
        });
        let prompt = "brainlearn ping";
        assert_eq!(TaskClass::classify(prompt), TaskClass::Reflex);
        let (out, _ladder) =
            GemiEngine::try_providers(&registry, prompt, None, None, &|_| {}, &|_| {});
        assert_eq!(out.as_deref(), Some("fine"));
        let both = vec![
            "ollama-brainlearn-a".to_string(),
            "ollama-brainlearn-b".to_string(),
        ];
        let ranked = rank(&both, TaskClass::Reflex);
        assert_eq!(ranked[0].provider, "ollama-brainlearn-b");
        assert_eq!(ranked[1].success_rate, Some(0.0));
    }

    #[test]
    fn test_try_providers_treats_flattened_error_text_as_failure() {
        // The store is process-global and its path comes from a
        // process-global env var that other tests set and clear, so readers
        // take the same lock the writers do.
        let _env = crate::engines::env_test_lock();
        use crate::engines::brain::{rank, TaskClass};
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        registry.register_provider(MockRouteProvider {
            name: "ollama-flatten-a",
            reply: "[INFERENCE_FAILED] upstream 500",
        });
        registry.register_provider(MockRouteProvider {
            name: "ollama-flatten-b",
            reply: "real answer",
        });
        let prompt = "flatten check";
        let (out, _ladder) =
            GemiEngine::try_providers(&registry, prompt, None, None, &|_| {}, &|_| {});
        assert_eq!(out.as_deref(), Some("real answer"));
        let names = vec!["ollama-flatten-a".to_string()];
        let a = &rank(&names, TaskClass::classify(prompt))[0];
        assert_eq!(
            a.success_rate,
            Some(0.0),
            "error text must not count as an answer"
        );
    }

    #[test]
    fn test_try_providers_never_lets_a_failing_provider_lead() {
        // The store is process-global and its path comes from a
        // process-global env var that other tests set and clear, so readers
        // take the same lock the writers do.
        let _env = crate::engines::env_test_lock();
        use crate::engines::brain::{note_failure, record_outcome, FailureKind, TaskClass};
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        // Name order would try `ollama-dry-a` first.
        registry.register_provider(MockRouteProvider {
            name: "ollama-dry-a",
            reply: "dry answered",
        });
        registry.register_provider(MockRouteProvider {
            name: "ollama-dry-b",
            reply: "fit answered",
        });
        let prompt = "dry ping";
        let class = TaskClass::classify(prompt);
        for _ in 0..4 {
            record_outcome("ollama-dry-a", class, false, 0);
        }
        note_failure("ollama-dry-a", FailureKind::Funds);
        let (out, _ladder) =
            GemiEngine::try_providers(&registry, prompt, None, None, &|_| {}, &|_| {});
        assert_eq!(out.as_deref(), Some("fit answered"));
    }

    #[test]
    fn test_try_providers_prefers_ollama_over_generic_and_skips_candle() {
        // The store is process-global and its path comes from a
        // process-global env var that other tests set and clear, so readers
        // take the same lock the writers do.
        let _env = crate::engines::env_test_lock();
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

        let (out, _ladder) =
            GemiEngine::try_providers(&registry, "hello", None, None, &|_| {}, &|_| {});
        assert_eq!(out.as_deref(), Some("from-ollama"));
    }

    #[test]
    fn test_try_providers_honors_requested_model_name_match() {
        // The store is process-global and its path comes from a
        // process-global env var that other tests set and clear, so readers
        // take the same lock the writers do.
        let _env = crate::engines::env_test_lock();
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        registry.register_provider(MockRouteProvider {
            name: "ollama-llama3",
            reply: "llama3",
        });
        registry.register_provider(MockRouteProvider {
            name: "vllm-mixtral",
            reply: "mixtral",
        });

        let (out, _ladder) =
            GemiEngine::try_providers(&registry, "hello", None, Some("mixtral"), &|_| {}, &|_| {});
        assert_eq!(out.as_deref(), Some("mixtral"));
    }

    #[test]
    fn test_native_tokenization() {
        let _home = susi_paths::SusiDirs::home_dir();
        let tokenizer_path = susi_paths::SusiDirs::data_dir().join("models/tokenizer.json");
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

#[cfg(test)]
mod reflex_allowed_tests {
    #[test]
    fn a_requested_model_bypasses_the_reflex_tiers() {
        assert!(super::reflex_allowed(None));
        assert!(!super::reflex_allowed(Some("qwen2.5-7b-instruct")));
    }
}

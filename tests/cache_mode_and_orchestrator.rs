//! Integration tests for Full Cache Mode and Tiered Orchestration.
//!
//! Validates:
//! 1. Full Cache Mode across all 3 providers (DeepSeek, Gemini, Claude):
//!    - Ephemeral cache breakpoint insertion (Anthropic)
//!    - Prefix caching request structures (DeepSeek)
//!    - System instruction & cached content (Gemini)
//!    - Correct token usage extraction including cached tokens
//!    - Cost estimation honoring provider-specific cache discounts
//! 2. Tiered Orchestrator routing:
//!    - Routine tasks -> Worker Tier (DeepSeek Flash / Gemini Flash)
//!    - Tough problems -> Pro Tier (Claude / DeepSeek Pro / Gemini Pro)
//!    - Full telemetry aggregation & Prometheus metrics export

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

use serde_json::json;
use susi_core::inference_wire::{
    anthropic_cache_payload, deepseek_cache_payload, estimate_cost_micros, extract_token_usage,
    gemini_cache_payload, InferenceProtocol,
};
use susi_gawd::orchestrator_tier::{
    CacheMode, ModelTier, ProblemDifficulty, TieredModelCatalog, TieredOrchestrator,
};
use susi_gawd::telemetry::InferenceTelemetry;

#[test]
fn test_claude_full_cache_mode_payload_and_extraction() {
    let tools = Some(json!([
        {
            "name": "read_file",
            "description": "Read file contents",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
        }
    ]));

    let payload = anthropic_cache_payload(
        "claude-3-5-sonnet-20241022",
        "You are an expert system orchestrator with vast codebase context.",
        "Refactor the core event loop.",
        tools,
        8192,
    );

    // Verify cache_control breakpoints exist in system prompt
    let system = payload.get("system").and_then(|v| v.as_array()).unwrap();
    assert_eq!(system.len(), 1);
    assert_eq!(
        system[0]["cache_control"]["type"], "ephemeral",
        "Claude system prompt must declare ephemeral cache control in Full Cache Mode"
    );

    // Verify cache_control breakpoints exist in tools
    let tools_out = payload.get("tools").and_then(|v| v.as_array()).unwrap();
    assert_eq!(
        tools_out[0]["cache_control"]["type"], "ephemeral",
        "Claude tool definitions must declare ephemeral cache control in Full Cache Mode"
    );

    // Verify extraction of cached tokens from Anthropic response
    let raw_response = json!({
        "id": "msg_01X9",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Plan formulated."}],
        "usage": {
            "input_tokens": 500,
            "cache_read_input_tokens": 4500,
            "cache_creation_input_tokens": 0,
            "output_tokens": 120
        }
    });

    let usage = extract_token_usage(InferenceProtocol::Anthropic, &raw_response);
    let cost = estimate_cost_micros("anthropic/claude-3-5-sonnet", &usage);

    assert_eq!(usage.prompt_tokens, 5000);
    assert_eq!(usage.cached_tokens, 4500);
    assert_eq!(usage.completion_tokens, 120);

    // Cached tokens on Claude 3.5 Sonnet receive 90% discount ($0.30 vs $3.00/M)
    // 500 base * 3 + 4500 cached * 0.3 + 120 comp * 15 = 1500 + 1350 + 1800 = 4650 micros
    assert_eq!(cost, 4650);
}

#[test]
fn test_gemini_full_cache_mode_payload_and_extraction() {
    let payload = gemini_cache_payload(
        "System invariants and security policies.",
        "Verify architectural boundaries.",
        Some("cachedContents/susi-workspace-ctx-v1"),
    );

    // Verify system instruction and cached content reference
    assert_eq!(
        payload["systemInstruction"]["parts"][0]["text"],
        "System invariants and security policies."
    );
    assert_eq!(
        payload["cachedContent"], "cachedContents/susi-workspace-ctx-v1",
        "Gemini payload must reference cachedContent in Full Cache Mode"
    );

    // Verify extraction of cached tokens from Gemini response
    let raw_response = json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{"text": "Verification passed."}]
            }
        }],
        "usageMetadata": {
            "promptTokenCount": 8000,
            "cachedContentTokenCount": 7000,
            "candidatesTokenCount": 250,
            "totalTokenCount": 8250
        }
    });

    let usage = extract_token_usage(InferenceProtocol::Gemini, &raw_response);
    let cost = estimate_cost_micros("google/gemini-2.5-pro", &usage);

    assert_eq!(usage.prompt_tokens, 8000);
    assert_eq!(usage.cached_tokens, 7000);
    assert_eq!(usage.completion_tokens, 250);

    // 1000 base * 1.25 + 7000 cached * (1.25 * 0.25 = 0.3125) + 250 comp * 5.0
    // = 1250 + 2187 + 1250 = 4687 micros
    assert_eq!(cost, 4687);
}

#[test]
fn test_deepseek_full_cache_mode_payload_and_extraction() {
    let payload = deepseek_cache_payload(
        "deepseek-chat",
        "Workspace prompt with immutable guidelines.",
        "Generate parsing routine.",
        4096,
    );

    assert_eq!(payload["model"], "deepseek-chat");
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");

    // Verify extraction of prefix cache hits from DeepSeek response
    let raw_response = json!({
        "id": "chatcmpl-99",
        "object": "chat.completion",
        "choices": [{
            "message": {"role": "assistant", "content": "Done."}
        }],
        "usage": {
            "prompt_tokens": 4000,
            "prompt_cache_hit_tokens": 3600,
            "prompt_cache_miss_tokens": 400,
            "completion_tokens": 80
        }
    });

    let usage = extract_token_usage(InferenceProtocol::OpenAiChat, &raw_response);
    let cost = estimate_cost_micros("deepseek/deepseek-chat", &usage);

    assert_eq!(usage.prompt_tokens, 4000);
    assert_eq!(usage.cached_tokens, 3600);
    assert_eq!(usage.completion_tokens, 80);

    // DeepSeek cache hit costs 10% ($0.014 vs $0.14/M)
    // 400 miss * 0.14 + 3600 hit * 0.014 + 80 comp * 0.28
    // = 56 + 50 + 22 = 128 micros
    assert_eq!(cost, 128);
}

#[test]
fn test_tiered_orchestrator_worker_and_pro_routing() {
    let orch = TieredOrchestrator::new("master-orchestrator");
    assert_eq!(orch.cache_mode(), CacheMode::Full);

    // 1. Difficulty classification
    assert_eq!(
        orch.classify_difficulty("write unit tests for url parser"),
        ProblemDifficulty::Routine
    );
    assert_eq!(
        orch.classify_difficulty("format and clean lint warnings"),
        ProblemDifficulty::Routine
    );
    assert_eq!(
        orch.classify_difficulty("prove formal state invariants under concurrency"),
        ProblemDifficulty::Tough
    );
    assert_eq!(
        orch.classify_difficulty("refactor core distributed consensus algorithm"),
        ProblemDifficulty::Tough
    );

    // 2. Model allocation by tier
    let worker_default = orch.select_model(ModelTier::Worker, None);
    assert_eq!(worker_default, TieredModelCatalog::DEFAULT_WORKER);
    assert!(
        worker_default.contains("deepseek") || worker_default.contains("flash"),
        "Worker tier should default to DeepSeek or Flash"
    );

    let worker_gemini = orch.select_model(ModelTier::Worker, Some("gemini"));
    assert_eq!(worker_gemini, "google/gemini-2.5-flash");

    let pro_default = orch.select_model(ModelTier::Pro, None);
    assert_eq!(pro_default, TieredModelCatalog::DEFAULT_PRO);

    let pro_deepseek = orch.select_model(ModelTier::Pro, Some("deepseek"));
    assert_eq!(pro_deepseek, "deepseek/deepseek-reasoner");

    let pro_gemini = orch.select_model(ModelTier::Pro, Some("gemini"));
    assert_eq!(pro_gemini, "google/gemini-2.5-pro");

    // 3. Mission execution with subtask decomposition
    let subtasks = [
        "generate mock responses for unit test suite",
        "format yaml manifest files",
        "prove formal memory safety invariants in unsafe ffi blocks",
    ];

    let report = orch.orchestrate_mission(
        "Secure FFI Migration",
        &subtasks,
        "System Architecture Mandates 48-56",
    );

    assert_eq!(report.subtasks_executed, 3);
    assert_eq!(
        report.worker_dispatches, 2,
        "2 routine tasks routed to Workers"
    );
    assert_eq!(
        report.pro_dispatches, 2,
        "1 tough task + 1 synthesis routed to Pro tier"
    );

    assert!(report.total_prompt_tokens > 0);
    assert!(report.total_cached_tokens > 0);

    let cache_hit_ratio = report.total_cached_tokens as f64 / report.total_prompt_tokens as f64;
    assert!(
        cache_hit_ratio >= 0.70,
        "Full cache mode must deliver >= 70% cache hit ratio across mission: {cache_hit_ratio:.2}"
    );

    // 4. Verify telemetry recording and Prometheus export
    let telemetry = InferenceTelemetry::global();
    let prom = telemetry.render_prometheus();
    assert!(
        prom.contains("susi_inference_tokens_total{kind=\"cached\"}"),
        "Prometheus metrics must export cached token gauge"
    );
}

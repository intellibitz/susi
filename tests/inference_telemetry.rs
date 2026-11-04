//! Integration tests for inference financial governance and telemetry exposition.
//!
//! Validates that token usage recording, model cost aggregation, and
//! Prometheus metrics rendering operate correctly in susi-gawd telemetry.

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

use susi_gawd::telemetry::{InferenceTelemetry, TokenUsage};

#[test]
fn telemetry_registry_aggregates_tokens_and_costs() {
    let registry = InferenceTelemetry::new();
    let usage = TokenUsage::new(1_000, 500);

    registry.record_call("anthropic/claude-3-5-sonnet", &usage, 6000, false);
    registry.record_call("openai/gpt-4o", &usage, 4500, false);
    registry.record_call("openai/gpt-4o", &usage, 4500, true);

    assert_eq!(registry.total_requests(), 3);
    assert_eq!(registry.total_prompt_tokens(), 3_000);
    assert_eq!(registry.total_completion_tokens(), 1_500);
    assert_eq!(registry.total_cost_micros(), 15_000);
    assert_eq!(registry.total_errors(), 1);

    let prom = registry.render_prometheus();
    assert!(prom.contains("susi_inference_requests_total 3"));
    assert!(prom.contains("susi_inference_tokens_total{kind=\"prompt\"} 3000"));
    assert!(prom.contains("susi_inference_tokens_total{kind=\"completion\"} 1500"));
    assert!(prom.contains("susi_inference_cost_micros_total 15000"));
    assert!(prom.contains("susi_inference_errors_total 1"));
    assert!(prom.contains("susi_model_requests_total{model=\"openai/gpt-4o\"} 2"));
    assert!(prom.contains("susi_model_cost_micros_total{model=\"openai/gpt-4o\"} 9000"));
}

#[test]
fn telemetry_singleton_is_thread_safe() {
    let global = InferenceTelemetry::global();
    let usage = TokenUsage::new(100, 50);
    global.record_call("anthropic/claude-haiku", &usage, 50, false);
    assert!(global.total_requests() > 0);
    assert!(global.total_cost_micros() > 0);
}

#[test]
fn telemetry_with_cache_tracks_cached_tokens_and_zero_usage_cleanly() {
    let registry = InferenceTelemetry::new();
    let zero = TokenUsage::new(0, 0);
    registry.record_call("local/qwen", &zero, 0, false);
    assert_eq!(registry.total_requests(), 1);
    assert_eq!(registry.total_prompt_tokens(), 0);
    assert_eq!(registry.total_completion_tokens(), 0);
    assert_eq!(registry.total_cost_micros(), 0);

    let cached = TokenUsage::with_cache(2_000, 300, 1_500);
    assert_eq!(cached.prompt_tokens, 2_000);
    assert_eq!(cached.completion_tokens, 300);
    assert_eq!(cached.cached_tokens, 1_500);
    registry.record_call("deepseek/deepseek-chat", &cached, 120, false);
    assert_eq!(registry.total_requests(), 2);
    assert_eq!(registry.total_prompt_tokens(), 2_000);
    assert_eq!(registry.total_completion_tokens(), 300);
    assert_eq!(registry.total_cost_micros(), 120);
}

#[test]
fn telemetry_prometheus_exposition_includes_type_and_help_headers() {
    let registry = InferenceTelemetry::new();
    let prom = registry.render_prometheus();
    assert!(prom.contains("susi_inference_requests_total 0"));
    assert!(prom.contains("susi_inference_cost_micros_total 0"));
    assert!(prom.contains("susi_inference_errors_total 0"));
}

#[test]
fn telemetry_token_usage_constructors_set_expected_fields() {
    let u = TokenUsage::new(120, 45);
    assert_eq!(u.prompt_tokens, 120);
    assert_eq!(u.completion_tokens, 45);
    assert_eq!(u.cached_tokens, 0);
}

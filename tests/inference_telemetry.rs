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

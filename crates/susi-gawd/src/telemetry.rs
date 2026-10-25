#![forbid(unsafe_code)]
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

//! Prometheus and operational telemetry metrics for inference and financial governance.
//!
//! Provides global thread-safe aggregation of token consumption, dollar costs (micros),
//! request latency, and failure rates, and exports them in standard Prometheus exposition
//! format for scraping by enterprise observability stacks (Prometheus, Datadog, Grafana).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Token count usage for a single inference call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Construct a new TokenUsage with automatic sum.
    #[must_use]
    pub const fn new(prompt_tokens: u64, completion_tokens: u64) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    }
}

/// Cumulative metrics for a single model identifier.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ModelTelemetryStats {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_cost_micros: u64,
    pub errors: u64,
}

/// Global registry for inference telemetry and financial governance.
#[derive(Debug, Default)]
pub struct InferenceTelemetry {
    total_requests: AtomicU64,
    total_prompt_tokens: AtomicU64,
    total_completion_tokens: AtomicU64,
    total_cost_micros: AtomicU64,
    total_errors: AtomicU64,
    models: Mutex<BTreeMap<String, ModelTelemetryStats>>,
}

impl InferenceTelemetry {
    /// Create a fresh telemetry registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the singleton global telemetry registry.
    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<InferenceTelemetry> = OnceLock::new();
        REGISTRY.get_or_init(Self::new)
    }

    /// Record an inference dispatch with its token usage and cost.
    pub fn record_call(&self, model: &str, usage: &TokenUsage, cost_micros: u64, is_error: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.total_prompt_tokens
            .fetch_add(usage.prompt_tokens, Ordering::Relaxed);
        self.total_completion_tokens
            .fetch_add(usage.completion_tokens, Ordering::Relaxed);
        self.total_cost_micros
            .fetch_add(cost_micros, Ordering::Relaxed);
        if is_error {
            self.total_errors.fetch_add(1, Ordering::Relaxed);
        }

        let mut lock = self.models.lock().unwrap_or_else(|e| e.into_inner());
        let entry = lock.entry(model.to_string()).or_default();
        entry.requests = entry.requests.saturating_add(1);
        entry.prompt_tokens = entry.prompt_tokens.saturating_add(usage.prompt_tokens);
        entry.completion_tokens = entry
            .completion_tokens
            .saturating_add(usage.completion_tokens);
        entry.total_cost_micros = entry.total_cost_micros.saturating_add(cost_micros);
        if is_error {
            entry.errors = entry.errors.saturating_add(1);
        }
    }

    /// Snapshot of total requests across all models.
    #[must_use]
    pub fn total_requests(&self) -> u64 {
        self.total_requests.load(Ordering::Relaxed)
    }

    /// Snapshot of total prompt tokens processed.
    #[must_use]
    pub fn total_prompt_tokens(&self) -> u64 {
        self.total_prompt_tokens.load(Ordering::Relaxed)
    }

    /// Snapshot of total completion tokens generated.
    #[must_use]
    pub fn total_completion_tokens(&self) -> u64 {
        self.total_completion_tokens.load(Ordering::Relaxed)
    }

    /// Snapshot of cumulative cost across all models in micro-dollars (micros).
    #[must_use]
    pub fn total_cost_micros(&self) -> u64 {
        self.total_cost_micros.load(Ordering::Relaxed)
    }

    /// Snapshot of total inference errors encountered.
    #[must_use]
    pub fn total_errors(&self) -> u64 {
        self.total_errors.load(Ordering::Relaxed)
    }

    /// Render Prometheus exposition format (`/metrics`).
    #[must_use]
    pub fn render_prometheus(&self) -> String {
        let mut out = String::with_capacity(1024);

        out.push_str(
            "# HELP susi_inference_requests_total Total LLM inference calls dispatched.\n",
        );
        out.push_str("# TYPE susi_inference_requests_total counter\n");
        out.push_str(&format!(
            "susi_inference_requests_total {}\n\n",
            self.total_requests()
        ));

        out.push_str("# HELP susi_inference_tokens_total Total tokens processed by inference.\n");
        out.push_str("# TYPE susi_inference_tokens_total counter\n");
        out.push_str(&format!(
            "susi_inference_tokens_total{{kind=\"prompt\"}} {}\n",
            self.total_prompt_tokens()
        ));
        out.push_str(&format!(
            "susi_inference_tokens_total{{kind=\"completion\"}} {}\n\n",
            self.total_completion_tokens()
        ));

        out.push_str("# HELP susi_inference_cost_micros_total Cumulative inference expenditure in micro-dollars.\n");
        out.push_str("# TYPE susi_inference_cost_micros_total counter\n");
        out.push_str(&format!(
            "susi_inference_cost_micros_total {}\n\n",
            self.total_cost_micros()
        ));

        out.push_str("# HELP susi_inference_errors_total Total failed inference dispatches.\n");
        out.push_str("# TYPE susi_inference_errors_total counter\n");
        out.push_str(&format!(
            "susi_inference_errors_total {}\n\n",
            self.total_errors()
        ));

        let lock = self.models.lock().unwrap_or_else(|e| e.into_inner());
        if !lock.is_empty() {
            out.push_str("# HELP susi_model_requests_total Requests per model identifier.\n");
            out.push_str("# TYPE susi_model_requests_total counter\n");
            for (model, stats) in lock.iter() {
                out.push_str(&format!(
                    "susi_model_requests_total{{model=\"{model}\"}} {}\n",
                    stats.requests
                ));
            }

            out.push_str("\n# HELP susi_model_cost_micros_total Cumulative cost per model in micro-dollars.\n");
            out.push_str("# TYPE susi_model_cost_micros_total counter\n");
            for (model, stats) in lock.iter() {
                out.push_str(&format!(
                    "susi_model_cost_micros_total{{model=\"{model}\"}} {}\n",
                    stats.total_cost_micros
                ));
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_telemetry_records_and_aggregates_metrics() {
        let registry = InferenceTelemetry::new();
        assert_eq!(registry.total_requests(), 0);
        assert_eq!(registry.total_cost_micros(), 0);

        let usage = TokenUsage::new(100, 50);
        registry.record_call("anthropic/claude-3-5-sonnet", &usage, 1050, false);
        registry.record_call("anthropic/claude-3-5-sonnet", &usage, 1050, false);
        registry.record_call("openai/gpt-4o", &usage, 750, true);

        assert_eq!(registry.total_requests(), 3);
        assert_eq!(registry.total_prompt_tokens(), 300);
        assert_eq!(registry.total_completion_tokens(), 150);
        assert_eq!(registry.total_cost_micros(), 2850);
        assert_eq!(registry.total_errors(), 1);

        let prom = registry.render_prometheus();
        assert!(prom.contains("susi_inference_requests_total 3"));
        assert!(prom.contains("susi_inference_tokens_total{kind=\"prompt\"} 300"));
        assert!(prom.contains("susi_inference_tokens_total{kind=\"completion\"} 150"));
        assert!(prom.contains("susi_inference_cost_micros_total 2850"));
        assert!(prom.contains("susi_inference_errors_total 1"));
        assert!(prom.contains("susi_model_requests_total{model=\"anthropic/claude-3-5-sonnet\"} 2"));
        assert!(prom
            .contains("susi_model_cost_micros_total{model=\"anthropic/claude-3-5-sonnet\"} 2100"));
    }
}

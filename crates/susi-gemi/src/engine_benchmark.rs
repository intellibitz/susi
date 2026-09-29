//! Benchmark harness: tokens/sec per engine and model (VC-201-045 / T-CLAUDE-23).
//!
//! Deterministic against a fake provider in tests (`engine_benchmark_*`).

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineBenchSample {
    pub engine: String,
    pub model: String,
    pub prompt: String,
    pub tokens_per_sec: f32,
    pub first_token_latency_ms: u128,
    pub success: bool,
    pub observed_unix: u64,
}

/// Abstraction over a completion provider for the harness.
pub trait BenchProvider {
    fn engine_name(&self) -> &str;
    fn model_name(&self) -> &str;
    /// Returns (completion text, optional first-token latency override).
    fn complete(&self, prompt: &str) -> Result<(String, Duration), String>;
}

/// Fake provider: fixed output and latency for hermetic tests.
#[derive(Debug, Clone)]
pub struct FakeProvider {
    pub engine: String,
    pub model: String,
    pub reply: String,
    pub latency: Duration,
    pub fail: bool,
}

impl BenchProvider for FakeProvider {
    fn engine_name(&self) -> &str {
        &self.engine
    }
    fn model_name(&self) -> &str {
        &self.model
    }
    fn complete(&self, _prompt: &str) -> Result<(String, Duration), String> {
        if self.fail {
            return Err("fake failure".into());
        }
        Ok((self.reply.clone(), self.latency))
    }
}

/// Rough token estimate: whitespace-separated words (deterministic, not a real tokenizer).
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| !w.is_empty())
        .count()
        .max(1)
}

pub fn run_engine_benchmark(
    provider: &dyn BenchProvider,
    prompts: &[&str],
) -> Vec<EngineBenchSample> {
    let mut out = Vec::new();
    for prompt in prompts {
        let start = Instant::now();
        match provider.complete(prompt) {
            Ok((text, first_tok)) => {
                let elapsed = start.elapsed().max(Duration::from_millis(1));
                let tokens = estimate_tokens(&text) as f32;
                let tps = tokens / elapsed.as_secs_f32().max(0.001);
                out.push(EngineBenchSample {
                    engine: provider.engine_name().to_string(),
                    model: provider.model_name().to_string(),
                    prompt: (*prompt).to_string(),
                    tokens_per_sec: tps,
                    first_token_latency_ms: first_tok.as_millis(),
                    success: true,
                    observed_unix: now_unix(),
                });
            }
            Err(_) => out.push(EngineBenchSample {
                engine: provider.engine_name().to_string(),
                model: provider.model_name().to_string(),
                prompt: (*prompt).to_string(),
                tokens_per_sec: 0.0,
                first_token_latency_ms: 0,
                success: false,
                observed_unix: now_unix(),
            }),
        }
    }
    out
}

/// Persist samples as brain-style evidence JSON lines.
pub fn write_bench_evidence(
    path: &std::path::Path,
    samples: &[EngineBenchSample],
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut lines = String::new();
    for s in samples {
        let line = serde_json::to_string(s).map_err(|e| e.to_string())?;
        lines.push_str(&line);
        lines.push('\n');
    }
    std::fs::write(path, lines).map_err(|e| e.to_string())
}

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

/// A model serialization format in the placement benchmark matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFormat {
    Gguf,
    Safetensors,
    Exl2,
    Awq,
}

impl ModelFormat {
    /// Every format the matrix benchmarks.
    pub const ALL: [ModelFormat; 4] = [
        ModelFormat::Gguf,
        ModelFormat::Safetensors,
        ModelFormat::Exl2,
        ModelFormat::Awq,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ModelFormat::Gguf => "GGUF",
            ModelFormat::Safetensors => "Safetensors",
            ModelFormat::Exl2 => "EXL2",
            ModelFormat::Awq => "AWQ",
        }
    }
}

/// A quantization level in the placement benchmark matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantization {
    Q4,
    Q8,
    Fp16,
}

impl Quantization {
    /// Every quantization the matrix benchmarks.
    pub const ALL: [Quantization; 3] = [Quantization::Q4, Quantization::Q8, Quantization::Fp16];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Quantization::Q4 => "Q4",
            Quantization::Q8 => "Q8",
            Quantization::Fp16 => "FP16",
        }
    }
}

/// One (format, quantization) candidate for a model on an engine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateSpec {
    pub engine: String,
    pub model: String,
    pub format: ModelFormat,
    pub quantization: Quantization,
}

/// The full format × quantization placement matrix for one model on one engine.
#[must_use]
pub fn format_quantization_matrix(engine: &str, model: &str) -> Vec<CandidateSpec> {
    let mut matrix = Vec::new();
    for format in ModelFormat::ALL {
        for quantization in Quantization::ALL {
            matrix.push(CandidateSpec {
                engine: engine.to_string(),
                model: model.to_string(),
                format,
                quantization,
            });
        }
    }
    matrix
}

/// One benchmarked matrix candidate: throughput, memory and correctness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateBenchmark {
    pub spec: CandidateSpec,
    /// The engine accepted the format/quantization combination.
    pub supported: bool,
    /// Every prompt produced non-empty output (a real correctness check, not
    /// merely "the call returned").
    pub correct: bool,
    pub memory_mb: f32,
    pub tokens_per_sec: f32,
    pub first_token_latency_ms: u128,
}

/// Benchmarks one matrix candidate through the same [`BenchProvider`]
/// abstraction the harness uses, recording memory, throughput and a
/// non-empty-output correctness check. An unsupported format/backend
/// combination yields `supported == false` and is never recommended.
#[must_use]
pub fn run_candidate_benchmark(
    provider: &dyn BenchProvider,
    spec: CandidateSpec,
    memory_mb: f32,
    prompts: &[&str],
) -> CandidateBenchmark {
    let mut total_tps = 0.0f32;
    let mut first_token_latency_ms = 0u128;
    let mut ran = 0usize;
    let mut all_succeeded = !prompts.is_empty();
    let mut correct = !prompts.is_empty();

    for prompt in prompts {
        let start = Instant::now();
        match provider.complete(prompt) {
            Ok((text, first_tok)) => {
                let elapsed = start.elapsed().max(Duration::from_millis(1));
                total_tps += estimate_tokens(&text) as f32 / elapsed.as_secs_f32().max(0.001);
                if ran == 0 {
                    first_token_latency_ms = first_tok.as_millis();
                }
                if text.trim().is_empty() {
                    correct = false;
                }
                ran += 1;
            }
            Err(_) => {
                all_succeeded = false;
                break;
            }
        }
    }

    let supported = all_succeeded && ran == prompts.len();
    let correct = supported && correct;
    let tokens_per_sec = if ran > 0 { total_tps / ran as f32 } else { 0.0 };

    CandidateBenchmark {
        spec,
        supported,
        correct,
        memory_mb,
        tokens_per_sec,
        first_token_latency_ms,
    }
}

/// Ranks benchmarked candidates into the placement recommendation: supported,
/// correct candidates first, ordered by throughput descending.
#[must_use]
pub fn recommend_placement(results: &[CandidateBenchmark]) -> Vec<CandidateSpec> {
    let mut ranked: Vec<&CandidateBenchmark> = results
        .iter()
        .filter(|r| r.supported && r.correct)
        .collect();
    ranked.sort_by(|a, b| {
        b.tokens_per_sec
            .partial_cmp(&a.tokens_per_sec)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.into_iter().map(|r| r.spec.clone()).collect()
}

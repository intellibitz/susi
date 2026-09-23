// SUSI Local-vs-Cloud Inference Benchmark
//
// Exists to make an efficiency claim ("susi runs local models as efficient
// as some leading cloud models") checkable against real numbers instead of
// asserted. Backfills `ModelBenchmarkResult` (gemi/models.rs), which
// previously had zero constructors and zero consumers anywhere in the
// codebase — the measurement shape existed, nothing ever measured anything.
//
// Every number this module reports is either a real wall-clock measurement
// or the API's own authoritative token count. Missing counts are labeled
// unavailable; words are never substituted for tokens.

use crate::engine::{LlamaCppEngine, NativeInferenceEngine};
use crate::models::ModelBenchmarkResult;
use crate::susi_error::{EaiError, EaiResult};
use std::path::Path;
use std::time::Instant;
use sysinfo::System;

/// Small, fixed prompt set so repeated runs are comparable to each other.
/// Deliberately short — this benchmarks latency/throughput characteristics,
/// not model quality, and a long generation on CPU-bound local inference
/// would make the local leg impractically slow to run.
pub const BENCHMARK_PROMPTS: &[&str] = &[
    "Explain the difference between TCP and UDP in two sentences.",
    "Write a Rust function that reverses a string.",
    "List three benefits of test-driven development.",
];

pub struct BenchmarkRunner;

impl BenchmarkRunner {
    /// Runs `prompt` through the real local inference engine and measures
    /// actual wall-clock latency, plus a genuine generated-token count: the
    /// returned text re-tokenized with the same model's own tokenizer (the
    /// generation loop itself doesn't currently surface a live token count,
    /// so this is a real post-hoc count, not an estimate — re-tokenizing the
    /// actual output is the standard technique when the generator's internal
    /// ids aren't exposed).
    pub fn benchmark_local(prompt: &str, _workspace: &Path) -> EaiResult<ModelBenchmarkResult> {
        let model_id = crate::models::ModelManager::get_selected_model(Some(
            crate::intent::IntentClassifier::classify(prompt),
        ))
        .ok_or_else(|| EaiError::inference("No local reasoning model selected."))?;

        let mut sys = System::new_all();
        sys.refresh_all();
        let pid = sysinfo::get_current_pid()
            .map_err(|e| EaiError::inference(format!("Could not get PID: {}", e)))?;
        let mem_before = sys.process(pid).map(|p| p.memory()).unwrap_or(0);

        let start = Instant::now();
        // Measure exactly the model named in the report. Reasoning orchestration
        // can fall back to tools or another model and is not a kernel benchmark.
        let output = LlamaCppEngine.run_inference_stream(prompt, &|_| {}, Some(&model_id))?;
        let elapsed = start.elapsed();

        sys.refresh_all();
        let mem_after = sys.process(pid).map(|p| p.memory()).unwrap_or(0);
        let peak_memory_mb = (mem_after as f32) / (1024.0 * 1024.0);
        let memory_used_mb = ((mem_after.saturating_sub(mem_before)) as f32) / (1024.0 * 1024.0);

        let token_count = crate::models::ModelManager::get_tokenizer_path(&model_id)
            .and_then(|p| tokenizers::Tokenizer::from_file(p).ok())
            .and_then(|t| t.encode(output.as_str(), false).ok())
            .map(|enc| enc.get_ids().len())
            .unwrap_or(0);

        Ok(ModelBenchmarkResult {
            model_id,
            name: "Local".to_string(),
            is_local: true,
            latency_ms: elapsed.as_millis(),
            tokens_per_sec: Self::tokens_per_sec(token_count, elapsed),
            status: if output.trim().is_empty() {
                "EMPTY_OUTPUT".to_string()
            } else if token_count == 0 {
                "TOKEN_COUNT_UNAVAILABLE".to_string()
            } else {
                "OK (output re-tokenized)".to_string()
            },
            memory_used_mb,
            peak_memory_mb,
        })
    }

    /// Runs `prompt` through a configured OpenAI-chat-completions-compatible
    /// cloud endpoint and measures the same real wall-clock latency. Uses
    /// the API's own reported `usage.completion_tokens` when present
    /// (authoritative, server-side count) rather than re-tokenizing locally
    /// with a tokenizer that likely doesn't match the remote model's real
    /// vocabulary. Missing usage is labeled unavailable, with zero throughput
    /// rather than an invented token count.
    ///
    /// Returns `Ok(None)`, not `Err`, when no cloud endpoint is configured:
    /// this leg is optional, opt-in comparison, not a required part of the
    /// benchmark. Configured via `SUSI_BENCH_CLOUD_API_BASE` (required to
    /// opt in), `SUSI_BENCH_CLOUD_API_KEY`, `SUSI_BENCH_CLOUD_MODEL`
    /// (defaults to "gpt-4o-mini") — kept separate from the existing
    /// `inference_endpoints` config (`sandbox/manager.rs`), which has no
    /// `api_key` field and is designed for unauthenticated LAN inference
    /// servers (vLLM/SGLang/etc.), not gated cloud APIs.
    pub fn benchmark_cloud(prompt: &str) -> EaiResult<Option<ModelBenchmarkResult>> {
        let api_base = match std::env::var("SUSI_BENCH_CLOUD_API_BASE") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => return Ok(None),
        };
        let api_key = std::env::var("SUSI_BENCH_CLOUD_API_KEY").unwrap_or_default();
        let model =
            std::env::var("SUSI_BENCH_CLOUD_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

        let payload = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": 1024
        });

        let mut sys = System::new_all();
        sys.refresh_all();
        let pid = sysinfo::get_current_pid()
            .map_err(|e| EaiError::inference(format!("Could not get PID: {}", e)))?;
        let mem_before = sys.process(pid).map(|p| p.memory()).unwrap_or(0);

        let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));
        let start = Instant::now();
        let mut req = susi_sandbox::manager::http_agent()
            .post(&url)
            .header("Content-Type", "application/json");
        if !api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", api_key));
        }
        let resp = req.send_json(payload).map_err(|e| {
            EaiError::inference(format!(
                "Cloud benchmark endpoint '{}' unreachable: {}",
                url, e
            ))
        })?;
        let body: serde_json::Value = resp
            .into_body()
            .read_json()
            .map_err(|e| EaiError::inference(format!("Invalid benchmark response: {e}")))?;
        // The response headers can arrive well before generation/body transfer
        // finishes. Include reading the full body in end-to-end latency.
        let elapsed = start.elapsed();
        let (token_count, status) = Self::completion_count(&body)?;

        sys.refresh_all();
        let mem_after = sys.process(pid).map(|p| p.memory()).unwrap_or(0);
        let peak_memory_mb = (mem_after as f32) / (1024.0 * 1024.0);
        let memory_used_mb = ((mem_after.saturating_sub(mem_before)) as f32) / (1024.0 * 1024.0);

        Ok(Some(ModelBenchmarkResult {
            model_id: model,
            name: "Cloud".to_string(),
            is_local: false,
            latency_ms: elapsed.as_millis(),
            tokens_per_sec: Self::tokens_per_sec(token_count, elapsed),
            status,
            memory_used_mb,
            peak_memory_mb,
        }))
    }

    fn completion_count(body: &serde_json::Value) -> EaiResult<(usize, String)> {
        if body.get("error").is_some() {
            return Err(EaiError::inference("Benchmark endpoint returned an error"));
        }
        let text = body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| EaiError::inference("Benchmark response has no completion text"))?;
        if text.trim().is_empty() {
            return Err(EaiError::inference(
                "Benchmark endpoint returned empty output",
            ));
        }
        match body["usage"]["completion_tokens"].as_u64() {
            Some(n) if n > 0 => Ok((
                usize::try_from(n).map_err(|_| EaiError::inference("Token count overflow"))?,
                "OK".into(),
            )),
            _ => Ok((
                0,
                "TOKEN_COUNT_UNAVAILABLE (throughput not measured)".into(),
            )),
        }
    }

    fn tokens_per_sec(token_count: usize, elapsed: std::time::Duration) -> f32 {
        let secs = elapsed.as_secs_f32().max(0.001);
        token_count as f32 / secs
    }

    /// Runs the full `BENCHMARK_PROMPTS` set through both legs and renders a
    /// plain-text comparison — real numbers only, no verdict asserted beyond
    /// what the numbers in this specific run actually show.
    pub fn run_and_render(workspace: &Path) -> String {
        let mut out = String::from("# SUSI Local vs Cloud Inference Benchmark\n\n");
        let mut local_results = Vec::new();
        let mut cloud_results = Vec::new();

        for (i, prompt) in BENCHMARK_PROMPTS.iter().enumerate() {
            out.push_str(&format!("## Prompt {}: {}\n\n", i + 1, prompt));

            match Self::benchmark_local(prompt, workspace) {
                Ok(r) => {
                    out.push_str(&format!(
                        "- [LOCAL] model={} latency={}ms tokens/sec={:.2} client_rss_after={:.2}MB mem_used={:.2}MB status={}\n",
                        r.model_id, r.latency_ms, r.tokens_per_sec, r.peak_memory_mb, r.memory_used_mb, r.status
                    ));
                    if r.status.starts_with("OK") {
                        local_results.push(r);
                    }
                }
                Err(e) => out.push_str(&format!("- [LOCAL] FAILED: {}\n", e)),
            }

            match Self::benchmark_cloud(prompt) {
                Ok(Some(r)) => {
                    out.push_str(&format!(
                        "- [CLOUD] model={} latency={}ms tokens/sec={:.2} client_rss_after={:.2}MB mem_used={:.2}MB status={}\n",
                        r.model_id, r.latency_ms, r.tokens_per_sec, r.peak_memory_mb, r.memory_used_mb, r.status
                    ));
                    if r.status == "OK" {
                        cloud_results.push(r);
                    }
                }
                Ok(None) => out.push_str(
                    "- [CLOUD] skipped: SUSI_BENCH_CLOUD_API_BASE not set (opt-in comparison)\n",
                ),
                Err(e) => out.push_str(&format!("- [CLOUD] FAILED: {}\n", e)),
            }
            out.push('\n');
        }

        out.push_str("## Summary\n\n");
        if local_results.is_empty() {
            out.push_str(
                "No local runs with measured token counts — no local average to report.\n",
            );
        } else {
            out.push_str(&format!(
                "Local avg: {:.2} tokens/sec, {}ms latency, {:.2}MB client RSS after request (n={})\n",
                local_results.iter().map(|r| r.tokens_per_sec).sum::<f32>()
                    / local_results.len() as f32,
                local_results.iter().map(|r| r.latency_ms).sum::<u128>()
                    / local_results.len() as u128,
                local_results.iter().map(|r| r.peak_memory_mb).sum::<f32>()
                    / local_results.len() as f32,
                local_results.len()
            ));
        }
        if cloud_results.is_empty() {
            out.push_str(
                "No remote runs with measured token counts (set SUSI_BENCH_CLOUD_API_BASE to enable comparison).\n",
            );
        } else {
            out.push_str(&format!(
                "Cloud avg: {:.2} tokens/sec, {}ms latency, {:.2}MB client RSS after request (n={})\n",
                cloud_results.iter().map(|r| r.tokens_per_sec).sum::<f32>()
                    / cloud_results.len() as f32,
                cloud_results.iter().map(|r| r.latency_ms).sum::<u128>()
                    / cloud_results.len() as u128,
                cloud_results.iter().map(|r| r.peak_memory_mb).sum::<f32>()
                    / cloud_results.len() as f32,
                cloud_results.len()
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokens_per_sec_computes_real_ratio() {
        let elapsed = std::time::Duration::from_secs(2);
        assert_eq!(BenchmarkRunner::tokens_per_sec(20, elapsed), 10.0);
    }

    #[test]
    fn test_tokens_per_sec_floors_elapsed_to_avoid_division_by_zero() {
        let elapsed = std::time::Duration::from_nanos(1);
        let result = BenchmarkRunner::tokens_per_sec(5, elapsed);
        assert!(result.is_finite());
        assert!(result > 0.0);
    }

    #[test]
    fn test_benchmark_cloud_skips_when_unconfigured() {
        unsafe {
            std::env::remove_var("SUSI_BENCH_CLOUD_API_BASE");
        }
        let result = BenchmarkRunner::benchmark_cloud("test prompt").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_benchmark_prompts_are_non_empty_and_fixed() {
        assert!(!BENCHMARK_PROMPTS.is_empty());
        assert!(BENCHMARK_PROMPTS.iter().all(|p| !p.trim().is_empty()));
    }
    #[test]
    fn benchmark_rejects_error_empty_and_malformed_responses() {
        for body in [
            serde_json::json!({"error": {"message": "failed"}}),
            serde_json::json!({}),
            serde_json::json!({"choices": [{"message": {"content": " "}}], "usage": {"completion_tokens": 20}}),
        ] {
            assert!(BenchmarkRunner::completion_count(&body).is_err());
        }
    }

    #[test]
    fn benchmark_never_calls_word_counts_tokens() {
        let mut body =
            serde_json::json!({"choices": [{"message": {"content": "Some generated text"}}]});
        let (count, status) = BenchmarkRunner::completion_count(&body).unwrap();
        assert_eq!(count, 0);
        assert!(status.contains("UNAVAILABLE"));
        body["usage"] = serde_json::json!({"completion_tokens": 7});
        assert_eq!(
            BenchmarkRunner::completion_count(&body).unwrap(),
            (7, "OK".into())
        );
    }
}

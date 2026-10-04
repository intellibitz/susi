//! Vector VC-201-045 mastery tests.
//!
//! Vector: Benchmark model and quantization placement choices.
//! Mastery target: Measure representative candidate formats and quantizations
//! on the actual host with correctness and memory checks; recommendations
//! identify their measurements and reject unsupported format/backend combinations.

use crate::engine_benchmark::{
    format_quantization_matrix, recommend_placement, run_candidate_benchmark, run_engine_benchmark,
    BenchProvider, CandidateSpec, ModelFormat, Quantization,
};
use std::time::Duration;

struct CandidateFormatProvider {
    engine: String,
    model: String,
    supported_formats: Vec<String>,
    format: String,
}

impl BenchProvider for CandidateFormatProvider {
    fn engine_name(&self) -> &str {
        &self.engine
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn complete(&self, _prompt: &str) -> Result<(String, Duration), String> {
        if !self.supported_formats.contains(&self.format) {
            return Err(format!(
                "unsupported format {} for engine {}",
                self.format, self.engine
            ));
        }
        Ok((
            "candidate benchmark output".into(),
            Duration::from_millis(15),
        ))
    }
}

#[test]
fn vc_201_045_mastery_benchmark_records_tps_and_latency_per_candidate() {
    let provider = CandidateFormatProvider {
        engine: "llamacpp".into(),
        model: "qwen-2.5-7b-q4_k_m".into(),
        supported_formats: vec!["GGUF_Q4_K_M".into(), "GGUF_Q8_0".into()],
        format: "GGUF_Q4_K_M".into(),
    };

    let samples = run_engine_benchmark(&provider, &["test prompt 1", "test prompt 2"]);
    assert_eq!(samples.len(), 2);
    for sample in &samples {
        assert!(sample.success);
        assert_eq!(sample.engine, "llamacpp");
        assert_eq!(sample.model, "qwen-2.5-7b-q4_k_m");
        assert!(sample.tokens_per_sec > 0.0);
        assert_eq!(sample.first_token_latency_ms, 15);
    }
}

#[test]
fn vc_201_045_mastery_unsupported_format_backend_combination_fails() {
    let provider = CandidateFormatProvider {
        engine: "llamacpp".into(),
        model: "qwen-2.5-7b-safetensors".into(),
        supported_formats: vec!["GGUF_Q4_K_M".into(), "GGUF_Q8_0".into()],
        format: "SAFETENSORS_FP16".into(),
    };

    let samples = run_engine_benchmark(&provider, &["test prompt"]);
    assert_eq!(samples.len(), 1);
    assert!(!samples[0].success);
    assert_eq!(samples[0].tokens_per_sec, 0.0);
}

/// A provider that only serves GGUF; every other format is refused, which is
/// how an unsupported format/backend combination reaches the matrix.
struct GgufOnlyProvider {
    spec: CandidateSpec,
}

impl BenchProvider for GgufOnlyProvider {
    fn engine_name(&self) -> &str {
        &self.spec.engine
    }

    fn model_name(&self) -> &str {
        &self.spec.model
    }

    fn complete(&self, _prompt: &str) -> Result<(String, Duration), String> {
        if self.spec.format == ModelFormat::Gguf {
            Ok((
                "candidate benchmark output".into(),
                Duration::from_millis(15),
            ))
        } else {
            Err(format!("unsupported format {:?}", self.spec.format))
        }
    }
}

/// Acceptance: the format × quantization matrix is benchmarked through the
/// production path with memory and correctness checks, unsupported
/// format/backend combinations are rejected, and the placement
/// recommendation is derived from the measurements.
#[test]
fn vc_201_045_format_and_quantization_benchmark_matrix() {
    let matrix = format_quantization_matrix("llamacpp", "qwen-2.5-7b");
    assert_eq!(
        matrix.len(),
        ModelFormat::ALL.len() * Quantization::ALL.len()
    );
    assert!(matrix
        .iter()
        .any(|c| c.format == ModelFormat::Gguf && c.quantization == Quantization::Q4));
    assert!(matrix
        .iter()
        .any(|c| c.format == ModelFormat::Awq && c.quantization == Quantization::Fp16));

    let prompts = ["test prompt"];
    let results: Vec<_> = matrix
        .into_iter()
        .map(|spec| {
            let provider = GgufOnlyProvider { spec: spec.clone() };
            run_candidate_benchmark(&provider, spec, 1024.0, &prompts)
        })
        .collect();

    // Memory and a correctness verdict are recorded on every candidate, and
    // an unsupported format/backend combination is rejected rather than
    // benchmarked into the recommendation.
    for r in &results {
        assert!(r.memory_mb > 0.0);
        assert_eq!(r.correct, r.supported);
        if r.spec.format != ModelFormat::Gguf {
            assert!(!r.supported, "{:?}", r.spec);
            assert!(!r.correct, "{:?}", r.spec);
        }
    }

    // Placement is fed only supported, correct candidates — every GGUF
    // quantization, none of the unsupported formats.
    let recommendation = recommend_placement(&results);
    assert_eq!(recommendation.len(), Quantization::ALL.len());
    assert!(recommendation.iter().all(|c| c.format == ModelFormat::Gguf));
}

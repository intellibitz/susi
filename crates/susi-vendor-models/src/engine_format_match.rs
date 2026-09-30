//! Pick the best local engine for a model's format.
//!
//! Model weights ship in several container formats; each serving engine
//! supports a subset. This module maps formats to engines and chooses
//! among installed/running engines, preferring the one with the best
//! recorded benchmark.

use serde::{Deserialize, Serialize};
use susi_error::EaiResult;

/// A model-weight container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelFormat {
    Gguf,
    Safetensors,
    Mlx,
    Awq,
    Gptq,
    Onnx,
    Unknown,
}

/// A local serving engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    LlamaCpp,
    Ollama,
    Vllm,
    Sglang,
    MlxLm,
    Candle,
    TensorRt,
}

/// One engine's observed state on this host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineState {
    pub engine: Engine,
    /// Higher is better (tokens/sec or a composite score); `None` when
    /// unmeasured — such engines sort below any measured one.
    pub benchmark: Option<f64>,
    /// A running engine is preferred over an installed-but-stopped one.
    pub running: bool,
}

/// A chosen engine with the reason it won.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnginePick {
    pub engine: Engine,
    pub reason: &'static str,
}

/// Detect the weight format from a repo/file name or path. GGUF files end
/// `.gguf`; safetensors repos contain `.safetensors`; `mlx-*`/`MLX` repos
/// are Apple MLX ports; `awq`/`gptq` in the name mark quantised HF repos.
#[must_use]
pub fn detect_format(name_or_path: &str) -> ModelFormat {
    let n = name_or_path.to_ascii_lowercase();
    if n.ends_with(".gguf") || n.contains("-gguf") || n.contains("gguf-") {
        ModelFormat::Gguf
    } else if n.contains("mlx") {
        ModelFormat::Mlx
    } else if n.contains("awq") {
        ModelFormat::Awq
    } else if n.contains("gptq") {
        ModelFormat::Gptq
    } else if n.ends_with(".onnx") || n.contains("onnx") {
        ModelFormat::Onnx
    } else if n.contains("safetensors") || n.ends_with(".safetensors") {
        ModelFormat::Safetensors
    } else {
        ModelFormat::Unknown
    }
}

/// Every engine able to serve `format`, unordered.
#[must_use]
pub fn engines_for(format: ModelFormat) -> Vec<Engine> {
    match format {
        ModelFormat::Gguf => vec![Engine::LlamaCpp, Engine::Ollama, Engine::Candle],
        ModelFormat::Safetensors => vec![Engine::Vllm, Engine::Sglang, Engine::Candle],
        ModelFormat::Mlx => vec![Engine::MlxLm],
        ModelFormat::Awq | ModelFormat::Gptq => {
            vec![Engine::Vllm, Engine::Sglang, Engine::TensorRt]
        }
        ModelFormat::Onnx => vec![Engine::TensorRt],
        ModelFormat::Unknown => Vec::new(),
    }
}

/// Choose the best engine for `format` among `installed`. Preference:
/// running > measured benchmark (highest) > canonical priority order.
/// Returns `None` when no installed engine supports the format.
#[must_use]
pub fn choose_engine(format: ModelFormat, installed: &[EngineState]) -> Option<EnginePick> {
    let mut candidates: Vec<&EngineState> = installed
        .iter()
        .filter(|s| engines_for(format).contains(&s.engine))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // Stable canonical priority within equal score: list order above.
    let priority = |e: Engine| -> usize {
        engines_for(format)
            .iter()
            .position(|c| *c == e)
            .unwrap_or(usize::MAX)
    };
    candidates.sort_by(|a, b| {
        b.running
            .cmp(&a.running)
            .then(
                b.benchmark
                    .partial_cmp(&a.benchmark)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(priority(a.engine).cmp(&priority(b.engine)))
    });
    let best = candidates.first()?;
    let reason = if best.running && best.benchmark.is_some() {
        "running engine with the best benchmark"
    } else if best.running {
        "running engine"
    } else if best.benchmark.is_some() {
        "best recorded benchmark"
    } else {
        "highest-priority engine for the format"
    };
    Some(EnginePick {
        engine: best.engine,
        reason,
    })
}

/// Convenience: pick an engine for a model name/path.
pub fn pick_for_model(name_or_path: &str, installed: &[EngineState]) -> EaiResult<EnginePick> {
    let format = detect_format(name_or_path);
    choose_engine(format, installed).ok_or_else(|| {
        susi_error::EaiError::config(format!(
            "no installed engine serves format {format:?} for {name_or_path}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(engine: Engine, benchmark: Option<f64>, running: bool) -> EngineState {
        EngineState {
            engine,
            benchmark,
            running,
        }
    }

    #[test]
    fn engine_format_match_detects_containers() {
        assert_eq!(detect_format("qwen2.5-7b.Q4_K_M.gguf"), ModelFormat::Gguf);
        assert_eq!(detect_format("meta-llama-3-8b-GGUF"), ModelFormat::Gguf);
        assert_eq!(detect_format("mlx-community/qwen-4bit"), ModelFormat::Mlx);
        assert_eq!(detect_format("thebloke/llama-AWQ"), ModelFormat::Awq);
        assert_eq!(detect_format("model.safetensors"), ModelFormat::Safetensors);
        assert_eq!(detect_format("some-model"), ModelFormat::Unknown);
    }

    #[test]
    fn engine_format_match_maps_engines() {
        assert!(engines_for(ModelFormat::Gguf).contains(&Engine::LlamaCpp));
        assert!(engines_for(ModelFormat::Safetensors).contains(&Engine::Vllm));
        assert!(engines_for(ModelFormat::Awq).contains(&Engine::Vllm));
        assert_eq!(engines_for(ModelFormat::Mlx), vec![Engine::MlxLm]);
        assert!(engines_for(ModelFormat::Unknown).is_empty());
    }

    #[test]
    fn engine_format_match_prefers_best_benchmark() {
        let installed = vec![
            st(Engine::Ollama, Some(20.0), false),
            st(Engine::LlamaCpp, Some(45.0), false),
        ];
        let pick = choose_engine(ModelFormat::Gguf, &installed);
        assert_eq!(pick.map(|p| p.engine), Some(Engine::LlamaCpp));
    }

    #[test]
    fn engine_format_match_running_beats_faster_stopped() {
        let installed = vec![
            st(Engine::LlamaCpp, Some(50.0), false),
            st(Engine::Ollama, Some(10.0), true),
        ];
        let pick = choose_engine(ModelFormat::Gguf, &installed);
        assert_eq!(pick.map(|p| p.engine), Some(Engine::Ollama));
    }

    #[test]
    fn engine_format_match_canonical_priority_unmeasured() {
        let installed = vec![
            st(Engine::Sglang, None, false),
            st(Engine::Vllm, None, false),
        ];
        let pick = choose_engine(ModelFormat::Safetensors, &installed);
        assert_eq!(pick.map(|p| p.engine), Some(Engine::Vllm));
    }

    #[test]
    fn engine_format_match_none_when_unsupported() {
        let installed = vec![st(Engine::LlamaCpp, Some(40.0), true)];
        assert!(choose_engine(ModelFormat::Safetensors, &installed).is_none());
        assert!(pick_for_model("model.safetensors", &installed).is_err());
    }
}

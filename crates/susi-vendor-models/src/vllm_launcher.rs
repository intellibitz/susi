//! Launch vLLM/SGLang with tensor-parallel sizing from detected GPUs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VllmLaunch {
    pub engine: String,
    pub tensor_parallel: u32,
    pub argv: Vec<String>,
}

/// Size tensor parallelism to the number of detected GPUs (clamped).
#[must_use]
pub fn plan_vllm(model: &str, gpu_count: u32, prefer_sglang: bool) -> VllmLaunch {
    let engine = if prefer_sglang { "sglang" } else { "vllm" };
    let tp = gpu_count.clamp(1, 8);
    VllmLaunch {
        engine: engine.into(),
        tensor_parallel: tp,
        argv: vec![
            engine.into(),
            "serve".into(),
            model.into(),
            "--tensor-parallel-size".into(),
            tp.to_string(),
        ],
    }
}

#[cfg(test)]
mod vllm_launcher_tests {
    use super::*;

    #[test]
    fn vllm_launcher_sizes_tensor_parallel() {
        let p = plan_vllm("mistral", 4, false);
        assert_eq!(p.engine, "vllm");
        assert_eq!(p.tensor_parallel, 4);
        assert!(plan_vllm("m", 0, true).tensor_parallel >= 1);
    }
}

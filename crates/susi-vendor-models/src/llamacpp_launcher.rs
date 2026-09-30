//! Launch llama-server with model + GPU-layer plan.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlamaLaunch {
    pub argv: Vec<String>,
    pub n_gpu_layers: u32,
}

/// Plan a llama-server launch from model path and free VRAM estimate.
#[must_use]
pub fn plan_llama(model: &str, free_vram_mb: u32) -> LlamaLaunch {
    let n_gpu_layers = if free_vram_mb >= 16_000 {
        99
    } else if free_vram_mb >= 8_000 {
        40
    } else {
        0
    };
    LlamaLaunch {
        argv: vec![
            "llama-server".into(),
            "-m".into(),
            model.into(),
            "-ngl".into(),
            n_gpu_layers.to_string(),
        ],
        n_gpu_layers,
    }
}

#[cfg(test)]
mod llamacpp_launcher_tests {
    use super::*;

    #[test]
    fn llamacpp_launcher_sets_gpu_layers_from_vram() {
        let p = plan_llama("/models/x.gguf", 24_000);
        assert_eq!(p.n_gpu_layers, 99);
        assert!(p.argv.contains(&"-ngl".into()));
        assert_eq!(plan_llama("/models/x.gguf", 100).n_gpu_layers, 0);
    }
}

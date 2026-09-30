//! `vllm serve` / `sglang serve` argv planner — derives
//! `--tensor-parallel-size` from the GPU roster, `--max-model-len` and
//! `--gpu-memory-utilization` from VRAM vs model size, and refuses when the
//! model cannot fit at all. Pure planning; tests exercise argv only.

use crate::multi_gpu_plan::Gpu;
use susi_error::{EaiError, EaiResult};

/// Model facts the planner needs (from GGUF header or HF config).
#[derive(Debug, Clone, Copy)]
pub struct ServeModel {
    /// Weight bytes at the intended quantisation/dtype.
    pub weight_bytes: u64,
    /// Native context length (`max_position_embeddings` / GGUF ctx).
    pub context_length: u64,
    /// HF-style model id or local path used as the serve target.
    pub name: &'static str,
}

/// One serving host: a set of GPUs (free VRAM each) and an upper bound on
/// context we are willing to allocate KV for.
#[derive(Debug, Clone)]
pub struct ServeHost {
    pub gpus: Vec<Gpu>,
    /// Hard cap on `--max-model-len` (operator budget).
    pub max_context: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServePlan {
    pub argv: Vec<String>,
    pub tensor_parallel: u32,
    pub max_model_len: u64,
    /// `--gpu-memory-utilization` value (0.5–0.95).
    pub gpu_memory_utilization: f64,
    pub why: String,
}

/// Plan a `vllm serve` invocation. Returns `Err` when the weight footprint
/// exceeds 95% of aggregate free VRAM (vLLM cannot run it — CPU offload is
/// out of scope for this engine).
pub fn vllm(model: &ServeModel, host: &ServeHost) -> EaiResult<ServePlan> {
    let free: u64 = host.gpus.iter().map(|g| g.free_vram).sum();
    if free == 0 {
        return Err(EaiError::config("vllm requires at least one GPU"));
    }
    // tensor parallel across cards that can each hold an even shard.
    let tp = tp_size(model.weight_bytes, &host.gpus);
    if tp == 0 {
        return Err(EaiError::config(format!(
            "{} ({:.1} GiB) cannot be sharded across the GPU roster — per-card free VRAM too small for an even tensor-parallel split",
            model.name,
            gib(model.weight_bytes)
        )));
    }
    let util = memory_utilization(model.weight_bytes, tp, &host.gpus)?;
    let len = model.context_length.min(host.max_context).max(1024);
    let argv = vec![
        "vllm".into(),
        "serve".into(),
        model.name.into(),
        "--tensor-parallel-size".into(),
        tp.to_string(),
        "--max-model-len".into(),
        len.to_string(),
        "--gpu-memory-utilization".into(),
        format!("{util:.2}"),
    ];
    Ok(ServePlan {
        argv,
        tensor_parallel: tp,
        max_model_len: len,
        gpu_memory_utilization: util,
        why: format!(
            "TP={tp} across {} GPU(s); {:.1} GiB weights at utilisation {util:.2}",
            host.gpus.len(),
            gib(model.weight_bytes)
        ),
    })
}

/// Same planning for `sglang serve` (`sglang.launch_server`): identical
/// sizing maths, different flag spellings.
pub fn sglang(model: &ServeModel, host: &ServeHost) -> EaiResult<ServePlan> {
    let mut p = vllm(model, host)?;
    p.argv = vec![
        "sglang".into(),
        "serve".into(),
        model.name.into(),
        "--tp".into(),
        p.tensor_parallel.to_string(),
        "--context-length".into(),
        p.max_model_len.to_string(),
        "--mem-fraction-static".into(),
        format!("{:.2}", p.gpu_memory_utilization),
    ];
    Ok(p)
}

/// Largest TP size (≤ GPU count, power-of-two preferred for interconnect
/// efficiency) such that every participating card can hold an equal shard
/// at 90% utilisation.
fn tp_size(weights: u64, gpus: &[Gpu]) -> u32 {
    let mut sorted: Vec<u64> = gpus.iter().map(|g| g.free_vram).collect();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let mut best = 0u32;
    for tp in [8u32, 4, 2, 1] {
        let t = tp as usize;
        if sorted.len() < t {
            continue;
        }
        let smallest = sorted[t - 1];
        let shard = weights / tp as u64;
        if shard <= (smallest as f64 * 0.90) as u64 {
            best = tp;
            break;
        }
    }
    best
}

/// Utilisation that leaves just enough headroom for KV + runtime: target the
/// weight share plus margin, floored at 0.5, capped at 0.95.
fn memory_utilization(weights: u64, tp: u32, gpus: &[Gpu]) -> EaiResult<f64> {
    let mut sorted: Vec<u64> = gpus.iter().map(|g| g.free_vram).collect();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let per_card = sorted
        .get(tp as usize - 1)
        .copied()
        .ok_or_else(|| EaiError::config("not enough GPUs"))?;
    let need = weights / tp as u64;
    let frac = need as f64 / per_card as f64;
    Ok((frac + 0.10).clamp(0.5, 0.95))
}

fn gib(b: u64) -> f64 {
    b as f64 / (1 << 30) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn model(weights_gib: u64, ctx: u64) -> ServeModel {
        ServeModel {
            weight_bytes: weights_gib * GIB,
            context_length: ctx,
            name: "org/model",
        }
    }

    fn host(free: &[u64], max_ctx: u64) -> ServeHost {
        ServeHost {
            gpus: free
                .iter()
                .enumerate()
                .map(|(i, v)| Gpu {
                    index: i as u32,
                    free_vram: v * GIB,
                })
                .collect(),
            max_context: max_ctx,
        }
    }

    #[test]
    fn vllm_launcher_single_gpu_full_fit() {
        let p = vllm(&model(14, 8192), &host(&[24], 8192)).unwrap();
        assert_eq!(p.tensor_parallel, 1);
        assert_eq!(p.max_model_len, 8192);
        assert!(p.argv.join(" ").contains("vllm serve org/model"));
        assert!(p.argv.join(" ").contains("--tensor-parallel-size 1"));
    }

    #[test]
    fn vllm_launcher_tensor_parallel_when_model_exceeds_one_card() {
        // 40 GiB weights on 2×24 GiB → TP=2.
        let p = vllm(&model(40, 8192), &host(&[24, 24], 8192)).unwrap();
        assert_eq!(p.tensor_parallel, 2);
        assert!(p.argv.join(" ").contains("--tensor-parallel-size 2"));
    }

    #[test]
    fn vllm_launcher_uneven_cards_use_min_shard() {
        // 40 GiB over 24+12 GiB: TP=2 needs 20 GiB per shard — the 12 GiB
        // card cannot hold it, so no TP plan exists; TP=1 also fails (40 >
        // 24×0.9) → refuse.
        assert!(vllm(&model(40, 8192), &host(&[24, 12], 8192)).is_err());
    }

    #[test]
    fn vllm_launcher_refuses_when_no_gpu() {
        assert!(vllm(&model(8, 4096), &host(&[], 4096)).is_err());
    }

    #[test]
    fn vllm_launcher_refuses_oversized_model() {
        assert!(vllm(
            &model(180, 4096),
            &host(&[24, 24, 24, 24, 24, 24, 24, 24], 4096)
        )
        .is_err());
    }

    #[test]
    fn vllm_launcher_max_model_len_respects_host_cap() {
        let p = vllm(&model(8, 131_072), &host(&[24], 32_768)).unwrap();
        assert_eq!(p.max_model_len, 32_768);
        assert!(p.argv.join(" ").contains("--max-model-len 32768"));
    }

    #[test]
    fn vllm_launcher_utilization_scales_with_size() {
        // 20 GiB on a 24 GiB card → frac .83 + .10 = .93
        let p = vllm(&model(20, 4096), &host(&[24], 4096)).unwrap();
        assert!((p.gpu_memory_utilization - 0.93).abs() < 0.02);
        assert!(p.argv.join(" ").contains("--gpu-memory-utilization 0.9"));
    }

    #[test]
    fn vllm_launcher_sglang_flags() {
        let p = sglang(&model(40, 8192), &host(&[24, 24], 8192)).unwrap();
        let j = p.argv.join(" ");
        assert!(j.contains("sglang serve org/model"));
        assert!(j.contains("--tp 2"));
        assert!(j.contains("--context-length 8192"));
        assert!(j.contains("--mem-fraction-static"));
    }
}

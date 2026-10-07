// Quantized GGUF MoE loader: CPU-capable expert dispatch for the sparse
// families candle only executes through CUDA kernels. `moe_gemm_gguf` and
// `indexed_moe_forward` bail on non-CUDA backends, so `qwen3moe`,
// `qwen2moe` and `llama`-arch Mixtral GGUFs get their expert GEMMs through
// per-expert QMatMul here instead — the llama.cpp stacked `ffn_*_exps`
//! tensors are contiguous per expert, so slicing is a byte copy, not a
//! dequantize, and the matmul stays quantized on CPU, CUDA and Metal.

use candle_core::quantized::{gguf_file, QStorage, QTensor};
use candle_core::{DType, Device, Result, Tensor, D};
use candle_nn::kv_cache::ConcatKvCache;
use candle_nn::{ops, Embedding, Linear, Module};
use candle_transformers::models::quantized_qwen3::{Gguf, RotaryEmbedding};
use candle_transformers::models::with_tracing::QMatMul;
use candle_transformers::quantized_nn::RmsNorm;
use candle_transformers::utils::repeat_kv;
use std::borrow::Cow;
use std::io::{Read, Seek};
use std::sync::Arc;

#[derive(Debug)]
struct Mlp {
    feed_forward_w1: QMatMul,
    feed_forward_w2: QMatMul,
    feed_forward_w3: QMatMul,
}

impl Module for Mlp {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let w1 = self.feed_forward_w1.forward(xs)?;
        let w3 = self.feed_forward_w3.forward(xs)?;
        self.feed_forward_w2.forward(&(ops::silu(&w1)? * w3)?)
    }
}

/// Splits a stacked `ffn_*_exps.weight` tensor `(num_experts, n, k)` into
/// one quantized matmul per expert. llama.cpp lays experts out contiguously
/// in the leading dimension, so each expert is a raw byte range of the
/// shared block buffer — no dequantize, and the copy is paid once at load.
fn split_experts(qt: &QTensor, device: &Device) -> Result<Vec<QMatMul>> {
    let (num_experts, n, k) = qt.shape().dims3()?;
    if num_experts == 0 {
        candle_core::bail!("expert tensor with zero experts");
    }
    let raw = qt.data()?;
    if raw.len() % num_experts != 0 {
        candle_core::bail!(
            "expert tensor {:?} has {} bytes, not divisible by {} experts",
            qt.shape(),
            raw.len(),
            num_experts
        );
    }
    let stride = raw.len() / num_experts;
    let mut out = Vec::with_capacity(num_experts);
    for e in 0..num_experts {
        let bytes = raw[e * stride..(e + 1) * stride].to_vec();
        let storage = QStorage::from_data(Cow::Owned(bytes), device, qt.dtype())?;
        let expert = QTensor::new(storage, (n, k))?;
        out.push(QMatMul::from_weights(Arc::new(expert))?);
    }
    Ok(out)
}

/// Sparse expert bank: router logits + per-expert quantized GEMMs + the
/// optional gated shared expert Qwen2-MoE carries. The forward is the
/// token-gather loop candle's non-quantized `qwen2_moe` uses — correct on
/// every backend, where `FusedMoeGGUF` is CUDA-only.
struct ExpertBank {
    gate: Linear,
    gate_experts: Vec<QMatMul>,
    up_experts: Vec<QMatMul>,
    down_experts: Vec<QMatMul>,
    num_experts_per_tok: usize,
    /// `{arch}.expert_weights_norm` — renormalize top-k weights to sum 1.
    norm_topk_prob: bool,
    /// `{arch}.expert_weights_scale`; 1.0 when absent (Mixtral/Qwen2-MoE).
    routed_scale: f32,
    shared_expert: Option<Mlp>,
    shared_expert_gate: Option<Linear>,
}

impl ExpertBank {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let (b_size, seq_len, hidden_dim) = xs.dims3()?;
        let xs = xs.reshape(((), hidden_dim))?;
        // Router logits in f32 — the gate is a dequantized (E, H) Linear and
        // the softmax must not run in a reduced dtype.
        let router_logits = self.gate.forward(&xs.to_dtype(DType::F32)?)?;
        let routing_weights = ops::softmax_last_dim(&router_logits)?;
        let experts_per_tok = routing_weights
            .arg_sort_last_dim(false)?
            .narrow(D::Minus1, 0, self.num_experts_per_tok)?
            .contiguous()?;
        let routing_weights = routing_weights.gather(&experts_per_tok, D::Minus1)?;
        let routing_weights = routing_weights.to_vec2::<f32>()?;
        let experts_per_tok = experts_per_tok.to_vec2::<u32>()?;

        let n_experts = self.gate_experts.len();
        let mut top_x: Vec<Vec<u32>> = vec![Vec::new(); n_experts];
        let mut selected_weights: Vec<Vec<f32>> = vec![Vec::new(); n_experts];
        for (row_idx, (rw, expert_idxs)) in routing_weights
            .iter()
            .zip(experts_per_tok.iter())
            .enumerate()
        {
            let sum_rw: f32 = rw.iter().sum();
            for (&rw, &expert_idx) in rw.iter().zip(expert_idxs.iter()) {
                let expert_idx = expert_idx as usize;
                if expert_idx >= n_experts {
                    candle_core::bail!("router selected expert {expert_idx} of {n_experts}");
                }
                top_x[expert_idx].push(row_idx as u32);
                let w = if self.norm_topk_prob && sum_rw > 0.0 {
                    rw / sum_rw
                } else {
                    rw
                };
                selected_weights[expert_idx].push(w * self.routed_scale);
            }
        }

        let mut ys = xs.zeros_like()?;
        for (expert_idx, top_x) in top_x.iter().enumerate() {
            if top_x.is_empty() {
                continue;
            }
            let top_x = Tensor::new(top_x.as_slice(), xs.device())?;
            let weights = Tensor::new(selected_weights[expert_idx].as_slice(), xs.device())?
                .reshape(((), 1))?
                .to_dtype(xs.dtype())?;
            let current = xs.index_select(&top_x, 0)?;
            let gate = self.gate_experts[expert_idx].forward(&current)?;
            let up = self.up_experts[expert_idx].forward(&current)?;
            let out = self.down_experts[expert_idx].forward(&(ops::silu(&gate)? * up)?)?;
            ys = ys.index_add(&top_x, &out.broadcast_mul(&weights)?, 0)?;
        }

        if let Some(shared) = &self.shared_expert {
            let shared_out = shared.forward(&xs)?;
            let shared_out = match &self.shared_expert_gate {
                Some(g) => shared_out.broadcast_mul(&ops::sigmoid(&g.forward(&xs)?)?)?,
                None => shared_out,
            };
            ys = (ys + shared_out)?;
        }
        ys.reshape((b_size, seq_len, hidden_dim))
    }
}

enum MoeOrMlp {
    Moe(ExpertBank),
    Mlp(Mlp),
}

impl MoeOrMlp {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        match self {
            Self::Moe(m) => m.forward(xs),
            Self::Mlp(m) => m.forward(xs),
        }
    }
}

/// Attention identical across all three MoE families; the optional pieces
/// (qkv bias on `qwen2moe`, per-head q/k rms-norm on `qwen3moe`) are
/// try-loaded so one implementation serves every arch.
struct Attention {
    attention_wq: QMatMul,
    attention_wk: QMatMul,
    attention_wv: QMatMul,
    attention_bq: Option<Tensor>,
    attention_bk: Option<Tensor>,
    attention_bv: Option<Tensor>,
    attention_wo: QMatMul,
    q_norm: Option<RmsNorm>,
    k_norm: Option<RmsNorm>,
    n_head: usize,
    n_kv_head: usize,
    head_dim: usize,
    num_kv_groups: usize,
    rotary_emb: Arc<RotaryEmbedding>,
    dtype: DType,
    kv_cache: ConcatKvCache,
}

impl Attention {
    // Flat arg list mirrors candle's QuantizedAttention::new signature —
    // grouping these into a struct would add a second shape for no gain.
    #[allow(clippy::too_many_arguments)]
    fn new<R: Read + Seek>(
        gg: &mut Gguf<R>,
        prefix: &str,
        dtype: DType,
        num_heads: usize,
        num_kv_heads: usize,
        head_dim: usize,
        rms_norm_eps: f64,
        device: &Device,
        rotary_emb: Arc<RotaryEmbedding>,
    ) -> Result<Self> {
        let attention_wq = gg.qmatmul(&format!("{prefix}.attn_q.weight"))?;
        let attention_wk = gg.qmatmul(&format!("{prefix}.attn_k.weight"))?;
        let attention_wv = gg.qmatmul(&format!("{prefix}.attn_v.weight"))?;
        let attention_wo = gg.qmatmul(&format!("{prefix}.attn_output.weight"))?;
        // Optional per arch: qkv bias (qwen2moe), per-head q/k rms-norm
        // (qwen3moe). A failed name lookup leaves the reader positionally
        // clean — Content::tensor seeks by name.
        let opt_f32 = |gg: &mut Gguf<R>, name: &str| {
            gg.tensor(name)
                .and_then(|t| t.dequantize(device))
                .and_then(|t| t.to_dtype(DType::F32))
                .ok()
        };
        let attention_bq = opt_f32(gg, &format!("{prefix}.attn_q.bias"));
        let attention_bk = opt_f32(gg, &format!("{prefix}.attn_k.bias"));
        let attention_bv = opt_f32(gg, &format!("{prefix}.attn_v.bias"));
        let q_norm = gg
            .rms_norm(&format!("{prefix}.attn_q_norm.weight"), rms_norm_eps)
            .ok();
        let k_norm = gg
            .rms_norm(&format!("{prefix}.attn_k_norm.weight"), rms_norm_eps)
            .ok();
        Ok(Self {
            attention_wq,
            attention_wk,
            attention_wv,
            attention_bq,
            attention_bk,
            attention_bv,
            attention_wo,
            q_norm,
            k_norm,
            n_head: num_heads,
            n_kv_head: num_kv_heads,
            head_dim,
            num_kv_groups: num_heads / num_kv_heads,
            rotary_emb,
            dtype,
            kv_cache: ConcatKvCache::new(2),
        })
    }

    fn forward(&mut self, x: &Tensor, mask: Option<&Tensor>, input_pos: usize) -> Result<Tensor> {
        let (b, seq_len, _) = x.dims3()?;
        let in_dtype = x.dtype();
        let q = self.attention_wq.forward(x)?;
        let k = self.attention_wk.forward(x)?;
        let v = self.attention_wv.forward(x)?;

        let q = match &self.attention_bq {
            Some(bq) => q.broadcast_add(bq)?,
            None => q,
        };
        let k = match &self.attention_bk {
            Some(bk) => k.broadcast_add(bk)?,
            None => k,
        };
        let v = match &self.attention_bv {
            Some(bv) => v.broadcast_add(bv)?,
            None => v,
        };

        let q = q
            .reshape((1, seq_len, self.n_head, self.head_dim))?
            .transpose(1, 2)?
            .contiguous()?;
        let k = k
            .reshape((1, seq_len, self.n_kv_head, self.head_dim))?
            .transpose(1, 2)?
            .contiguous()?;
        let v = v
            .reshape((1, seq_len, self.n_kv_head, self.head_dim))?
            .transpose(1, 2)?
            .contiguous()?;

        let (q, k) = if let (Some(q_norm), Some(k_norm)) = (&self.q_norm, &self.k_norm) {
            let q_flat = q.flatten(0, 2)?;
            let k_flat = k.flatten(0, 2)?;
            let q = q_norm
                .forward(&q_flat)?
                .reshape((1, self.n_head, seq_len, self.head_dim))?;
            let k =
                k_norm
                    .forward(&k_flat)?
                    .reshape((1, self.n_kv_head, seq_len, self.head_dim))?;
            (q, k)
        } else {
            (q, k)
        };

        let (q, k, v) = (
            q.to_dtype(self.dtype)?,
            k.to_dtype(self.dtype)?,
            v.to_dtype(self.dtype)?,
        );

        let (q, k) = self.rotary_emb.apply(&q, &k, input_pos)?;
        let (k, v) = self.kv_cache.append(&k, &v)?;

        let k = repeat_kv(k, self.num_kv_groups)?.contiguous()?;
        let v = repeat_kv(v, self.num_kv_groups)?.contiguous()?;

        let scale = 1.0 / (self.head_dim as f64).sqrt();
        let mut scores = (q.matmul(&k.transpose(2, 3)?)? * scale)?;
        if let Some(m) = mask {
            let mask = if m.dtype() != scores.dtype() {
                m.to_dtype(scores.dtype())?
            } else {
                m.clone()
            };
            scores = scores.broadcast_add(&mask)?;
        }
        let probs = ops::softmax_last_dim(&scores)?;
        let ctx = probs.matmul(&v)?;
        let reshaped_ctx =
            ctx.transpose(1, 2)?
                .reshape((b, seq_len, self.n_head * self.head_dim))?;
        self.attention_wo.forward(&reshaped_ctx.to_dtype(in_dtype)?)
    }
}

struct LayerWeights {
    self_attn: Attention,
    attention_norm: RmsNorm,
    mlp: MoeOrMlp,
    ffn_norm: RmsNorm,
}

/// A loaded quantized MoE model — `forward` matches the
/// `NeuralBackend` contract the inference substrate boxes.
pub struct ModelWeights {
    tok_embeddings: Embedding,
    layers: Vec<LayerWeights>,
    norm: RmsNorm,
    output: QMatMul,
    dtype: DType,
    device: Device,
}

impl ModelWeights {
    /// Loads a MoE GGUF: `qwen2moe`, `qwen3moe`, or `llama` arch carrying
    /// `{arch}.expert_count` (Mixtral). The caller validates the arch and
    /// routes non-MoE files elsewhere.
    pub fn from_gguf<R: Read + Seek>(
        ct: gguf_file::Content,
        reader: &mut R,
        device: &Device,
        dtype: DType,
    ) -> Result<Self> {
        let mut gg = Gguf::new(ct, reader, device.clone());
        let md_get = |s: &str| match gg.metadata().get(s) {
            None => candle_core::bail!("cannot find {s} in metadata"),
            Some(v) => Ok(v),
        };
        let arch = md_get("general.architecture")?.to_string()?.to_owned();

        let head_count =
            md_get(format!("{arch}.attention.head_count").as_str())?.to_u32()? as usize;
        let head_count_kv =
            md_get(format!("{arch}.attention.head_count_kv").as_str())?.to_u32()? as usize;
        let embedding_length =
            md_get(format!("{arch}.embedding_length").as_str())?.to_u32()? as usize;
        let head_dim = md_get(format!("{arch}.attention.key_length").as_str())
            .and_then(|v| v.to_u32())
            .map(|v| v as usize)
            .unwrap_or(embedding_length / head_count);
        let context_length = md_get(format!("{arch}.context_length").as_str())?.to_u32()? as usize;
        let block_count = md_get(format!("{arch}.block_count").as_str())?.to_u32()? as usize;
        let rms_norm_eps =
            md_get(format!("{arch}.attention.layer_norm_rms_epsilon").as_str())?.to_f32()? as f64;
        let rope_freq_base = md_get(format!("{arch}.rope.freq_base").as_str())
            .and_then(|m| m.to_f32())
            .unwrap_or(10000f32);

        let num_experts = md_get(format!("{arch}.expert_count").as_str())
            .and_then(|v| v.to_u32())
            .unwrap_or(0) as usize;
        if num_experts == 0 {
            candle_core::bail!("{arch} GGUF carries no expert_count — not a MoE model");
        }
        let num_experts_per_tok =
            md_get(format!("{arch}.expert_used_count").as_str())?.to_u32()? as usize;
        let shared_ffn_len = md_get(format!("{arch}.expert_shared_feed_forward_length").as_str())
            .and_then(|v| v.to_u32())
            .unwrap_or(0) as usize;
        let routed_scale = md_get(format!("{arch}.expert_weights_scale").as_str())
            .and_then(|v| v.to_f32())
            .unwrap_or(1.0);
        // Mixtral renormalizes top-k weights unconditionally; models with a
        // shared expert (qwen2moe) declare the flag explicitly.
        let norm_topk_prob = md_get(format!("{arch}.expert_weights_norm").as_str())
            .and_then(|v| v.to_bool())
            .unwrap_or(shared_ffn_len == 0);

        let tok_embeddings = gg.tensor("token_embd.weight")?.dequantize(device)?;
        let norm = gg.rms_norm("output_norm.weight", rms_norm_eps)?;
        let output = match gg.qmatmul("output.weight") {
            Ok(v) => v,
            _ => gg.qmatmul("token_embd.weight")?,
        };

        let rotary_emb = Arc::new(RotaryEmbedding::new(
            dtype,
            head_dim,
            context_length,
            rope_freq_base as f64,
            device,
        )?);

        let mut layers = Vec::with_capacity(block_count);
        for layer_idx in 0..block_count {
            let prefix = format!("blk.{layer_idx}");
            // MoE is per-layer: qwen2moe keeps layer 0 dense. The router
            // tensor `ffn_gate_inp.weight` is the discriminator.
            let mlp = match gg.tensor(&format!("{prefix}.ffn_gate_inp.weight")) {
                Ok(gate_t) => {
                    let gate_ws = gate_t.dequantize(device)?.to_dtype(DType::F32)?;
                    let shared_expert = if shared_ffn_len > 0 {
                        Some(Mlp {
                            feed_forward_w1: gg
                                .qmatmul(&format!("{prefix}.ffn_gate_shexp.weight"))?,
                            feed_forward_w2: gg
                                .qmatmul(&format!("{prefix}.ffn_down_shexp.weight"))?,
                            feed_forward_w3: gg
                                .qmatmul(&format!("{prefix}.ffn_up_shexp.weight"))?,
                        })
                    } else {
                        None
                    };
                    let shared_expert_gate = gg
                        .tensor(&format!("{prefix}.ffn_gate_inp_shexp.weight"))
                        .and_then(|t| t.dequantize(device))
                        .and_then(|t| t.to_dtype(DType::F32))
                        .ok()
                        .map(|w| Linear::new(w, None));
                    MoeOrMlp::Moe(ExpertBank {
                        gate: Linear::new(gate_ws, None),
                        gate_experts: split_experts(
                            &gg.tensor(&format!("{prefix}.ffn_gate_exps.weight"))?,
                            device,
                        )?,
                        up_experts: split_experts(
                            &gg.tensor(&format!("{prefix}.ffn_up_exps.weight"))?,
                            device,
                        )?,
                        down_experts: split_experts(
                            &gg.tensor(&format!("{prefix}.ffn_down_exps.weight"))?,
                            device,
                        )?,
                        num_experts_per_tok,
                        norm_topk_prob,
                        routed_scale,
                        shared_expert,
                        shared_expert_gate,
                    })
                }
                Err(_) => MoeOrMlp::Mlp(Mlp {
                    feed_forward_w1: gg.qmatmul(&format!("{prefix}.ffn_gate.weight"))?,
                    feed_forward_w2: gg.qmatmul(&format!("{prefix}.ffn_down.weight"))?,
                    feed_forward_w3: gg.qmatmul(&format!("{prefix}.ffn_up.weight"))?,
                }),
            };

            let attention_norm =
                gg.rms_norm(&format!("{prefix}.attn_norm.weight"), rms_norm_eps)?;
            let ffn_norm = gg.rms_norm(&format!("{prefix}.ffn_norm.weight"), rms_norm_eps)?;
            let self_attn = Attention::new(
                &mut gg,
                &prefix,
                dtype,
                head_count,
                head_count_kv,
                head_dim,
                rms_norm_eps,
                device,
                rotary_emb.clone(),
            )?;
            layers.push(LayerWeights {
                self_attn,
                attention_norm,
                mlp,
                ffn_norm,
            });
        }

        Ok(Self {
            tok_embeddings: Embedding::new(tok_embeddings, embedding_length),
            layers,
            norm,
            output,
            dtype,
            device: device.clone(),
        })
    }

    pub fn forward(&mut self, x: &Tensor, offset: usize) -> Result<Tensor> {
        let mut xs = self.tok_embeddings.forward(x)?;
        let (_b, l) = x.dims2()?;
        let causal_mask = if l == 1 {
            None
        } else {
            Some(candle_transformers::utils::build_additive_causal_mask(
                l,
                offset,
                None,
                &self.device,
                self.dtype,
            )?)
        };
        for layer in self.layers.iter_mut() {
            let x = xs;
            let residual = &x;
            let x = layer.attention_norm.forward(&x)?;
            let attn = layer.self_attn.forward(&x, causal_mask.as_ref(), offset)?;
            let x = (attn + residual)?;
            let residual = &x;
            let x = layer.ffn_norm.forward(&x)?;
            let x = layer.mlp.forward(&x)?;
            xs = (x + residual)?;
        }
        let xs = xs.narrow(1, l - 1, 1)?;
        let xs = self.norm.forward(&xs)?;
        self.output.forward(&xs)?.to_dtype(DType::F32)?.squeeze(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::quantized::GgmlDType;

    /// Expert banks split on expert-contiguous byte ranges: a quantized
    /// (2,1,32) tensor must yield two (1,32) experts preserving values.
    #[test]
    fn split_experts_preserves_each_experts_weights() {
        let device = Device::Cpu;
        // Two experts with deliberately different rows.
        let data = (0..64).map(|i| i as f32).collect::<Vec<_>>();
        let full = Tensor::from_vec(data, (2, 1, 32), &device).unwrap();
        let qt = QTensor::quantize(&full, GgmlDType::Q8_0).unwrap();
        let experts = split_experts(&qt, &device).unwrap();
        assert_eq!(experts.len(), 2);
        for (e, expert) in experts.iter().enumerate() {
            let x = Tensor::ones((1, 32), DType::F32, &device).unwrap();
            let y = expert.forward(&x).unwrap();
            assert_eq!(y.dims(), &[1, 1]);
            // sum of row e = sum(32e..32e+32)
            let expected: f32 = (32 * e..32 * e + 32).map(|i| i as f32).sum();
            let got = y.flatten_all().unwrap().to_vec1::<f32>().unwrap()[0];
            // Q8_0 quantization is lossy; allow relative slack.
            assert!(
                (got - expected).abs() / expected < 0.02,
                "{got} vs {expected}"
            );
        }
    }

    /// A 2-D tensor is not an expert bank: the leading expert axis is
    /// mandatory, so the split must reject it rather than mis-slice.
    #[test]
    fn split_experts_rejects_non_stacked_tensor() {
        let device = Device::Cpu;
        let t = Tensor::zeros((1, 32), DType::F32, &device).unwrap();
        let qt = QTensor::quantize(&t, GgmlDType::Q8_0).unwrap();
        assert!(split_experts(&qt, &device).is_err());
    }
}

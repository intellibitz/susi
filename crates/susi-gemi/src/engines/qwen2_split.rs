//! Qwen2 model implementation with quantization support.
//!
//! Qwen2 is a chat-optimized language model that supports 8-bit quantization
//! for reduced memory usage and faster inference.
//!
//! Key characteristics:
//! - Group Query Attention (GQA)
//! - RMSNorm for layer normalization
//! - Rotary positional embeddings (RoPE)
//! - Support for 8-bit quantization
//!
//! References:
//! - [Model Card](https://huggingface.co/Qwen/Qwen2)
//!

use candle_core::{
    quantized::{gguf_file, QMatMul},
    DType, Device, IndexOp, Result, Tensor,
};
use candle_nn::{Embedding, Module};
use candle_transformers::quantized_nn::RmsNorm;
use candle_transformers::utils::repeat_kv;
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct Mlp {
    feed_forward_w1: QMatMul,
    feed_forward_w2: QMatMul,
    feed_forward_w3: QMatMul,
}

impl Module for Mlp {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let w1 = self.feed_forward_w1.forward(xs)?;
        let w3 = self.feed_forward_w3.forward(xs)?;
        self.feed_forward_w2
            .forward(&(candle_nn::ops::silu(&w1)? * w3)?)
    }
}

#[derive(Debug, Clone)]
struct LayerWeights {
    attention_wq: QMatMul,
    attention_wk: QMatMul,
    attention_wv: QMatMul,
    attention_bq: Tensor,
    attention_bk: Tensor,
    attention_bv: Tensor,
    attention_wo: QMatMul,
    attention_norm: RmsNorm,
    mlp: Mlp,
    ffn_norm: RmsNorm,
    n_head: usize,
    n_kv_head: usize,
    head_dim: usize,
    cos: Tensor,
    sin: Tensor,
    neg_inf: Tensor,
    /// Preallocated (1, n_kv_head, kv_cache_capacity, head_dim) buffers,
    /// written into in place via `Tensor::slice_set` as generation
    /// proceeds - see the module-level note on why this replaced growing
    /// the cache via `Tensor::cat` every token.
    kv_cache: (Tensor, Tensor),
    /// How many of `kv_cache`'s `kv_cache_capacity` positions are actually
    /// populated right now. `0` is the "no cache yet" state the old
    /// `Option<(Tensor, Tensor)>` used to represent with `None`.
    kv_cache_len: usize,
    span_attn: tracing::Span,
    span_rot: tracing::Span,
    span_mlp: tracing::Span,
    pub layer_device: Device,
}

fn masked_fill(on_false: &Tensor, mask: &Tensor, on_true: &Tensor) -> Result<Tensor> {
    let shape = mask.shape();
    let m = mask.where_cond(&on_true.broadcast_as(shape.dims())?, on_false)?;
    Ok(m)
}

impl LayerWeights {
    fn target_device(&self) -> &Device {
        &self.layer_device
    }

    fn apply_rotary_emb(&self, x: &Tensor, index_pos: usize) -> Result<Tensor> {
        let _enter = self.span_rot.enter();
        let (_b_sz, _n_head, seq_len, _n_embd) = x.dims4()?;
        let cos = self.cos.narrow(0, index_pos, seq_len)?;
        let sin = self.sin.narrow(0, index_pos, seq_len)?;
        candle_nn::rotary_emb::rope(&x.contiguous()?, &cos, &sin)
    }

    fn forward_attn(
        &mut self,
        x: &Tensor,
        mask: Option<&Tensor>,
        index_pos: usize,
    ) -> Result<Tensor> {
        let _enter = self.span_attn.enter();
        let (b_sz, seq_len, n_embd) = x.dims3()?;

        let q = self.attention_wq.forward(x)?;
        let k = self.attention_wk.forward(x)?;
        let v = self.attention_wv.forward(x)?;

        let q = q.broadcast_add(&self.attention_bq)?;
        let k = k.broadcast_add(&self.attention_bk)?;
        let v = v.broadcast_add(&self.attention_bv)?;

        let q = q
            .reshape((b_sz, seq_len, self.n_head, self.head_dim))?
            .transpose(1, 2)?
            .contiguous()?;
        let k = k
            .reshape((b_sz, seq_len, self.n_kv_head, self.head_dim))?
            .transpose(1, 2)?
            .contiguous()?;
        let v = v
            .reshape((b_sz, seq_len, self.n_kv_head, self.head_dim))?
            .transpose(1, 2)?
            .contiguous()?;

        // let (q, k) = self
        //     .rotary_embedding
        //     .apply_rotary_emb_qkv(&q, &k, index_pos)?;
        let q = self.apply_rotary_emb(&q, index_pos)?;
        let k = self.apply_rotary_emb(&k, index_pos)?;

        // A fresh generation call always starts at index_pos == 0; this is
        // also relied on to reset stale state left in a cached, reused
        // model handle from a previous, unrelated request (see
        // `InferenceHost::get_model`'s cache) - matches the old
        // `Option`-based cache's implicit reset-on-index_pos==0 behavior.
        if index_pos == 0 {
            self.kv_cache_len = 0;
        }
        let new_len = self.kv_cache_len + seq_len;
        if new_len > self.kv_cache.0.dim(2)? {
            candle_core::bail!(
                "KV cache capacity ({}) exceeded: request needs {new_len} positions - \
                 increase `kv_cache_capacity_tokens` in config.json",
                self.kv_cache.0.dim(2)?
            );
        }
        self.kv_cache
            .0
            .slice_set(&k.contiguous()?, 2, self.kv_cache_len)?;
        self.kv_cache
            .1
            .slice_set(&v.contiguous()?, 2, self.kv_cache_len)?;
        self.kv_cache_len = new_len;
        // Narrowing dim 2 out of a larger preallocated buffer is a *view*
        // sharing the buffer's storage - cheap, but non-contiguous whenever
        // kv_cache_len < capacity (its dim-1 stride is the buffer's fixed
        // capacity*head_dim, not this view's own shorter length). Left as
        // -is, that non-contiguity would hit `repeat_kv` below, whose own
        // internal `Tensor::cat` then has to read it with slow strided
        // access instead of a fast sequential copy - measured live, this
        // regressed a heavily-CPU-bound 14B run from 1.63 to 1.04 tok/s,
        // *worse* than the `Tensor::cat`-per-token baseline this whole
        // preallocation was meant to beat. One explicit, tight contiguous
        // copy here (unavoidable - matmul needs contiguous operands
        // regardless of caching strategy) is cheaper than letting several
        // downstream ops each pay their own strided-access tax on it.
        let k = self
            .kv_cache
            .0
            .narrow(2, 0, self.kv_cache_len)?
            .contiguous()?;
        let v = self
            .kv_cache
            .1
            .narrow(2, 0, self.kv_cache_len)?
            .contiguous()?;

        // Support for MQA, useful for 70B models and mistral.
        let k = repeat_kv(k, self.n_head / self.n_kv_head)?;
        let v = repeat_kv(v, self.n_head / self.n_kv_head)?;

        let att = (q.matmul(&k.t()?)? / (self.head_dim as f64).sqrt())?;
        let att = match mask {
            None => att,
            Some(mask) => {
                let mask = mask.broadcast_as(att.shape())?;
                masked_fill(&att, &mask, &self.neg_inf)?
            }
        };
        let att = candle_nn::ops::softmax_last_dim(&att)?;
        // Convert to contiguous as matmul doesn't support strided vs for now.
        let y = att.matmul(&v.contiguous()?)?;
        let y = y.transpose(1, 2)?.reshape(&[b_sz, seq_len, n_embd])?;
        let y = self.attention_wo.forward(&y)?;
        Ok(y)
    }
}

pub struct ModelWeights {
    tok_embeddings: Embedding,
    layers: Vec<LayerWeights>,
    norm: RmsNorm,
    output: QMatMul,
    /// Keyed by `(seq_len, index_pos)`: a causal mask for `seq_len` new
    /// query positions attending to themselves causally plus the
    /// `index_pos` already-cached positions unmasked. Speculative-decoding
    /// verification calls `forward`/`forward_all_logits` with `seq_len > 1`
    /// at `index_pos > 0` (continuing an existing KV cache with a whole
    /// drafted chunk at once), which the single-token generation loop never
    /// did, so the cache key must include the offset, not just the length.
    masks: HashMap<(usize, usize), Tensor>,
    masks_gpu: HashMap<(usize, usize), Tensor>,
    /// `tok_embeddings`/`norm`/`output` are always resident here (never
    /// split - see `from_gguf_split`), so any input arriving on a different
    /// device, and the final layer's activations if the last transformer
    /// block happened to run on `gpu_device`, must be moved back here
    /// before touching those weights.
    cpu_device: Device,
    gpu_device: Device,
    span: tracing::Span,
    span_output: tracing::Span,
}

fn precomput_freqs_cis(
    head_dim: usize,
    freq_base: f32,
    context_length: usize,
    device: &Device,
) -> Result<(Tensor, Tensor)> {
    let theta: Vec<_> = (0..head_dim)
        .step_by(2)
        .map(|i| 1f32 / freq_base.powf(i as f32 / head_dim as f32))
        .collect();
    let theta = Tensor::new(theta.as_slice(), device)?;
    let idx_theta = Tensor::arange(0, context_length as u32, device)?
        .to_dtype(DType::F32)?
        .reshape((context_length, 1))?
        .matmul(&theta.reshape((1, theta.elem_count()))?)?;
    let cos = idx_theta.cos()?;
    let sin = idx_theta.sin()?;
    Ok((cos, sin))
}

impl ModelWeights {
    /// Determines how many transformer blocks fit inside `vram_budget_bytes`
    /// of GPU memory, using exact per-tensor byte sizes read from GGUF
    /// metadata (`ct.tensor_infos`) rather than assuming a fixed layer count.
    /// The previous heuristic (`safe_vram / total_file_size` scaled against
    /// a hardcoded "standard 40 layers") is wrong for any model whose real
    /// `block_count` differs from 40 - e.g. Qwen2.5-32B has 64 blocks, so
    /// the old formula would compute each layer's average size against 40
    /// instead of 64, silently under-committing layers the GPU actually has
    /// room for. It also ignored the non-layer tensors (token embedding,
    /// output projection) that `from_gguf_split` always keeps on CPU, biasing
    /// the per-layer average upward.
    ///
    /// Also reserves VRAM for the KV cache each GPU-resident layer
    /// accumulates during generation (`LayerWeights::kv_cache` is f32),
    /// sized for `kv_cache_capacity` positions - the caller must pass the
    /// exact same value it will later pass to `from_gguf_split`, since that
    /// call preallocates the real buffer at this size; a mismatch here
    /// would either waste VRAM headroom this function thought it reserved,
    /// or (worse) under-reserve for what actually gets allocated. Capped by
    /// the model's own `context_length` (frequently 128K+) so a short-
    /// context model never over-reserves relative to what it can even use.
    pub fn plan_gpu_layers(
        ct: &gguf_file::Content,
        vram_budget_bytes: u64,
        kv_cache_capacity: usize,
    ) -> usize {
        if vram_budget_bytes == 0 {
            return 0;
        }
        let md_get_u32 = |s: &str| ct.metadata.get(s).and_then(|v| v.to_u32().ok());
        let block_count = match md_get_u32("qwen2.block_count") {
            Some(v) if v > 0 => v as usize,
            _ => return 0,
        };
        let head_count_kv = md_get_u32("qwen2.attention.head_count_kv")
            .unwrap_or(1)
            .max(1) as u64;
        let head_count = md_get_u32("qwen2.attention.head_count").unwrap_or(1).max(1) as u64;
        let embedding_length = md_get_u32("qwen2.embedding_length").unwrap_or(0) as u64;
        let head_dim = embedding_length / head_count;
        let context_length = md_get_u32("qwen2.context_length").unwrap_or(4096).max(1) as u64;

        let tensor_bytes = |name: &str| -> u64 {
            ct.tensor_infos
                .get(name)
                .map(|info| {
                    let elems = info.shape.elem_count() as u64;
                    let block = info.ggml_dtype.block_size() as u64;
                    let type_size = info.ggml_dtype.type_size() as u64;
                    if block == 0 {
                        0
                    } else {
                        (elems.checked_div(block).unwrap_or(0)) * type_size
                    }
                })
                .unwrap_or(0)
        };

        let seq_budget = (kv_cache_capacity as u64).min(context_length);
        let vram_budget_bytes = (vram_budget_bytes as f64 * 0.98) as u64; // Pack tight to GPU boundary
        let kv_bytes_per_layer = 2 * head_count_kv * head_dim * seq_budget * 4; // f32 k+v cache

        let mut used = 0u64;
        let mut n_gpu_layers = 0usize;
        for layer_idx in 0..block_count {
            let prefix = format!("blk.{layer_idx}");
            let layer_bytes: u64 = [
                "attn_q.weight",
                "attn_k.weight",
                "attn_v.weight",
                "attn_q.bias",
                "attn_k.bias",
                "attn_v.bias",
                "attn_output.weight",
                "attn_norm.weight",
                "ffn_gate.weight",
                "ffn_down.weight",
                "ffn_up.weight",
                "ffn_norm.weight",
            ]
            .iter()
            .map(|suffix| tensor_bytes(&format!("{prefix}.{suffix}")))
            .sum();

            let layer_total = layer_bytes + kv_bytes_per_layer;
            if used + layer_total > vram_budget_bytes {
                break;
            }
            used += layer_total;
            n_gpu_layers += 1;
        }
        n_gpu_layers
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_gguf_split<R: std::io::Seek + std::io::Read>(
        ct: gguf_file::Content,
        reader: &mut R,
        cpu_device: &Device,
        gpu_device: &Device,
        n_gpu_layers: usize,
        kv_cache_capacity: usize,
    ) -> Result<Self> {
        let device = cpu_device;
        let md_get = |s: &str| match ct.metadata.get(s) {
            None => candle_core::bail!("cannot find {s} in metadata"),
            Some(v) => Ok(v),
        };

        let head_count = md_get("qwen2.attention.head_count")?.to_u32()? as usize;
        let head_count_kv = md_get("qwen2.attention.head_count_kv")?.to_u32()? as usize;
        let embedding_length = md_get("qwen2.embedding_length")?.to_u32()? as usize;
        let context_length = md_get("qwen2.context_length")?.to_u32()? as usize;
        let block_count = md_get("qwen2.block_count")?.to_u32()? as usize;
        let rms_norm_eps = md_get("qwen2.attention.layer_norm_rms_epsilon")?.to_f32()? as f64;
        let rope_freq_base = md_get("qwen2.rope.freq_base")
            .and_then(|m| m.to_f32())
            .unwrap_or(10000f32);

        if head_count == 0
            || head_count_kv == 0
            || embedding_length == 0
            || context_length == 0
            || block_count == 0
            || kv_cache_capacity == 0
        {
            candle_core::bail!("Qwen2 model dimensions and KV cache capacity must be nonzero");
        }
        if !embedding_length.is_multiple_of(head_count) || !head_count.is_multiple_of(head_count_kv)
        {
            candle_core::bail!("Qwen2 attention dimensions must divide evenly");
        }
        if !rms_norm_eps.is_finite()
            || rms_norm_eps <= 0.0
            || !rope_freq_base.is_finite()
            || rope_freq_base <= 0.0
        {
            candle_core::bail!(
                "Qwen2 normalization epsilon and RoPE frequency must be positive and finite"
            );
        }

        let head_dim = embedding_length / head_count;

        let neg_inf_cpu = Tensor::new(f32::NEG_INFINITY, device)?;

        let tok_embeddings = ct.tensor(reader, "token_embd.weight", device)?;
        let tok_embeddings = tok_embeddings.dequantize(device)?;
        let norm = RmsNorm::from_qtensor(
            ct.tensor(reader, "output_norm.weight", device)?,
            rms_norm_eps,
        )?;
        let output = match ct.tensor(reader, "output.weight", device) {
            Ok(v) => QMatMul::from_qtensor(v)?,
            _ => {
                // use tie_word_embeddings
                QMatMul::from_qtensor(ct.tensor(reader, "token_embd.weight", device)?)?
            }
        };

        // A layer whose blocks were placed on `gpu_device` runs its
        // attention entirely in GPU-resident tensors; the rotary
        // embedding's `rope()` and the causal mask's `where_cond` both
        // require every operand on one device (`Storage::same_device`
        // hard-errors otherwise), so `cos`/`sin`/`neg_inf` must exist as a
        // real copy on whichever device each layer actually runs on, not
        // just on `cpu_device` cloned into every layer regardless of
        // placement - the bug that made every GPU-offloaded layer fail its
        // very first rotary embedding call.
        let same_device = gpu_device.same_device(cpu_device);
        let (cos_cpu, sin_cpu) =
            precomput_freqs_cis(head_dim, rope_freq_base, context_length, cpu_device)?;
        let (cos_gpu, sin_gpu, neg_inf_gpu) = if same_device {
            (cos_cpu.clone(), sin_cpu.clone(), neg_inf_cpu.clone())
        } else {
            let (c, s) = precomput_freqs_cis(head_dim, rope_freq_base, context_length, gpu_device)?;
            (c, s, Tensor::new(f32::NEG_INFINITY, gpu_device)?)
        };

        let mut layers = Vec::with_capacity(block_count);

        for layer_idx in 0..block_count {
            let target_device = if layer_idx < n_gpu_layers {
                gpu_device
            } else {
                cpu_device
            };
            let device = target_device;
            let on_gpu = layer_idx < n_gpu_layers && !same_device;
            let (cos, sin, neg_inf) = if on_gpu {
                (&cos_gpu, &sin_gpu, &neg_inf_gpu)
            } else {
                (&cos_cpu, &sin_cpu, &neg_inf_cpu)
            };
            let prefix = format!("blk.{layer_idx}");
            let attention_wq = ct.tensor(reader, &format!("{prefix}.attn_q.weight"), device)?;
            let attention_wk = ct.tensor(reader, &format!("{prefix}.attn_k.weight"), device)?;
            let attention_wv = ct.tensor(reader, &format!("{prefix}.attn_v.weight"), device)?;

            let attention_bq = ct.tensor(reader, &format!("{prefix}.attn_q.bias"), device)?;
            let attention_bk = ct.tensor(reader, &format!("{prefix}.attn_k.bias"), device)?;
            let attention_bv = ct.tensor(reader, &format!("{prefix}.attn_v.bias"), device)?;

            let attention_wo =
                ct.tensor(reader, &format!("{prefix}.attn_output.weight"), device)?;

            let mlp = {
                let feed_forward_w1 =
                    ct.tensor(reader, &format!("{prefix}.ffn_gate.weight"), device)?;
                let feed_forward_w2 =
                    ct.tensor(reader, &format!("{prefix}.ffn_down.weight"), device)?;
                let feed_forward_w3 =
                    ct.tensor(reader, &format!("{prefix}.ffn_up.weight"), device)?;
                Mlp {
                    feed_forward_w1: QMatMul::from_qtensor(feed_forward_w1)?,
                    feed_forward_w2: QMatMul::from_qtensor(feed_forward_w2)?,
                    feed_forward_w3: QMatMul::from_qtensor(feed_forward_w3)?,
                }
            };

            let attention_norm =
                ct.tensor(reader, &format!("{prefix}.attn_norm.weight"), device)?;
            let ffn_norm = ct.tensor(reader, &format!("{prefix}.ffn_norm.weight"), device)?;

            let span_attn = tracing::span!(tracing::Level::TRACE, "attn");
            let span_rot = tracing::span!(tracing::Level::TRACE, "attn-rot");
            let span_mlp = tracing::span!(tracing::Level::TRACE, "attn-mlp");

            // Preallocated once per layer, written into in place via
            // `slice_set` as generation proceeds instead of reallocating
            // and copying the whole accumulated cache on every token (see
            // the module-level note on `Tensor::cat`'s cost).
            let kv_cache_shape = (1, head_count_kv, kv_cache_capacity, head_dim);
            let kv_cache = (
                Tensor::zeros(kv_cache_shape, DType::F32, device)?,
                Tensor::zeros(kv_cache_shape, DType::F32, device)?,
            );

            layers.push(LayerWeights {
                attention_wq: QMatMul::from_qtensor(attention_wq)?,
                attention_wk: QMatMul::from_qtensor(attention_wk)?,
                attention_wv: QMatMul::from_qtensor(attention_wv)?,
                attention_bq: attention_bq.dequantize(device)?,
                attention_bk: attention_bk.dequantize(device)?,
                attention_bv: attention_bv.dequantize(device)?,
                attention_wo: QMatMul::from_qtensor(attention_wo)?,
                attention_norm: RmsNorm::from_qtensor(attention_norm, rms_norm_eps)?,
                cos: cos.clone(),
                sin: sin.clone(),
                mlp,
                ffn_norm: RmsNorm::from_qtensor(ffn_norm, rms_norm_eps)?,
                n_head: head_count,
                n_kv_head: head_count_kv,
                head_dim,
                neg_inf: neg_inf.clone(),
                kv_cache,
                kv_cache_len: 0,
                span_attn,
                span_rot,
                span_mlp,
                layer_device: device.clone(),
            });
        }

        let span = tracing::span!(tracing::Level::TRACE, "model");
        let span_output = tracing::span!(tracing::Level::TRACE, "output");

        Ok(Self {
            tok_embeddings: Embedding::new(tok_embeddings, embedding_length),
            layers,
            norm,
            output,
            masks: HashMap::new(),
            masks_gpu: HashMap::new(),
            cpu_device: cpu_device.clone(),
            gpu_device: gpu_device.clone(),
            span,
            span_output,
        })
    }

    /// Builds (or returns the cached) causal mask for `t` new query
    /// positions starting at `offset` in the sequence: position `i` (0..t)
    /// may attend to columns `0..=offset+i` and must not attend beyond it.
    /// `offset == 0, t == seq_len` reproduces the plain single-chunk causal
    /// mask; `offset > 0` is what a mid-KV-cache multi-token verification
    /// batch (speculative decoding) needs and the original implementation
    /// never had to build, since the single-token loop only ever passed
    /// `t == 1` (skipped entirely below) after the initial prefill.
    fn mask_on(
        &mut self,
        t: usize,
        offset: usize,
        device: &Device,
        use_gpu_cache: bool,
    ) -> Result<Tensor> {
        let cache = if use_gpu_cache {
            &mut self.masks_gpu
        } else {
            &mut self.masks
        };
        let key = (t, offset);
        if let Some(mask) = cache.get(&key) {
            Ok(mask.clone())
        } else {
            let total = offset + t;
            let mask: Vec<_> = (0..t)
                .flat_map(|i| (0..total).map(move |j| u8::from(j > offset + i)))
                .collect();
            let mask = Tensor::from_slice(&mask, (t, total), device)?;
            cache.insert(key, mask.clone());
            Ok(mask)
        }
    }

    /// Runs every transformer block plus the final `output_norm`, returning
    /// the *unsliced* (batch, seq_len, hidden) normed activations. Shared by
    /// `forward` (single next-token prediction: slices the last position)
    /// and `forward_all_logits` (speculative-decoding verification: needs
    /// the target model's own predicted token at every drafted position,
    /// not just the last).
    fn forward_hidden(&mut self, x: &Tensor, index_pos: usize) -> Result<Tensor> {
        let (_b_sz, seq_len) = x.dims2()?;

        let cpu_dev = self.cpu_device.clone();
        let gpu_dev = self.gpu_device.clone();
        let same_device = gpu_dev.same_device(&cpu_dev);
        let mask_cpu = if seq_len == 1 {
            None
        } else {
            Some(self.mask_on(seq_len, index_pos, &cpu_dev, false)?)
        };
        let mask_gpu = if seq_len > 1 && !same_device {
            Some(self.mask_on(seq_len, index_pos, &gpu_dev, true)?)
        } else {
            None
        };

        let _enter = self.span.enter();

        // token_embd.weight is always cpu_device-resident (see
        // from_gguf_split) regardless of what device the caller created `x`
        // on - e.g. a small model whose whole file fits in VRAM gets a
        // caller-selected GPU input tensor, which would otherwise mismatch
        // the CPU embedding table on the very first lookup.
        let x = x.to_device(&cpu_dev)?;
        let mut layer_in = self.tok_embeddings.forward(&x)?;

        for layer in self.layers.iter_mut() {
            let x = layer_in.to_device(layer.target_device())?;
            let residual = &x;
            let x = layer.attention_norm.forward(&x)?;
            let mask = if same_device || layer.layer_device.same_device(&cpu_dev) {
                mask_cpu.as_ref()
            } else {
                mask_gpu.as_ref()
            };
            let attn = layer.forward_attn(&x, mask, index_pos)?;
            let x = (attn + residual)?;

            // MLP
            let _enter = layer.span_mlp.enter();
            let residual = &x;
            let x = layer.ffn_norm.forward(&x)?;
            let x = layer.mlp.forward(&x)?;
            let x = (x + residual)?;
            layer_in = x
        }
        // output_norm.weight is always cpu_device-resident; the last
        // transformer block may have run on gpu_device (e.g. every layer
        // fit on GPU), so bring its activations back before touching it.
        let layer_in = layer_in.to_device(&self.cpu_device)?;
        self.norm.forward(&layer_in)
    }

    pub fn forward(&mut self, x: &Tensor, index_pos: usize) -> Result<Tensor> {
        let (_b_sz, seq_len) = x.dims2()?;
        let hidden = self.forward_hidden(x, index_pos)?;
        let last = hidden.i((.., seq_len - 1, ..))?;
        let _enter = self.span_output.enter();
        self.output.forward(&last)
    }

    /// Like `forward`, but returns logits for every position in `x`
    /// (batch, seq_len, vocab) instead of just the last - the target
    /// model's own greedy pick at each drafted position is exactly what
    /// speculative-decoding verification compares the draft model against.
    pub fn forward_all_logits(&mut self, x: &Tensor, index_pos: usize) -> Result<Tensor> {
        let hidden = self.forward_hidden(x, index_pos)?;
        let _enter = self.span_output.enter();
        self.output.forward(&hidden)
    }

    /// Truncates every layer's KV cache back to its first `new_len` cached
    /// positions, discarding anything beyond it (a no-op where a layer's
    /// cache is already that length or shorter). Needed by speculative
    /// decoding after a verification round only partially accepts a
    /// drafted chunk: `forward_all_logits` grew the cache by the *entire*
    /// chunk during verification, but only a prefix of it was confirmed
    /// correct, so the rejected tail's cached keys/values must be discarded
    /// before the next round - left in place, later attention would
    /// silently attend to key/value pairs for tokens that were never
    /// actually part of the real sequence.
    pub fn truncate_kv_cache(&mut self, new_len: usize) -> Result<()> {
        // The preallocated buffer never shrinks; "truncating" is just
        // moving the valid-length pointer back, discarding the rejected
        // tail without touching any tensor - O(1), not the O(cache_len)
        // reallocation the old `Option<(Tensor, Tensor)>` design needed.
        for layer in self.layers.iter_mut() {
            if layer.kv_cache_len > new_len {
                layer.kv_cache_len = new_len;
            }
        }
        Ok(())
    }

    /// Whether every transformer block landed on `gpu_device` (a real GPU,
    /// not a CPU fallback) - i.e. the model needed no CPU offload at all.
    /// Measured live with `nvidia-smi dmon`: CUDA's quantized matmul
    /// dispatch (`fast_mmq`/`fast_mmvq`) genuinely gets faster per-token
    /// with a wider batch, but candle's CPU quantized matmul does not - a
    /// 31-token CPU prefill on a 41-CPU-layer 32B split took ~59s (~1.9s/
    /// token), in line with 4 sequential single-token CPU forwards
    /// averaging ~2.6s/token, not meaningfully faster per-token when
    /// batched. Speculative decoding's whole value proposition is a wider
    /// batch being cheaper per-token; callers should only pay its added
    /// complexity and rejected-draft overhead when this returns `true`.
    pub fn is_fully_gpu_resident(&self) -> bool {
        !self.gpu_device.same_device(&self.cpu_device)
            && self
                .layers
                .iter()
                .all(|l| l.layer_device.same_device(&self.gpu_device))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_attention_dimensions_fail_before_reading_tensors() {
        for (heads, kv_heads, width) in [(0, 1, 64), (4, 0, 64), (3, 1, 64), (4, 3, 64)] {
            let ct = gguf_file::Content {
                magic: gguf_file::VersionedMagic::GgufV3,
                metadata: HashMap::from([
                    (
                        "qwen2.attention.head_count".into(),
                        gguf_file::Value::U32(heads),
                    ),
                    (
                        "qwen2.attention.head_count_kv".into(),
                        gguf_file::Value::U32(kv_heads),
                    ),
                    (
                        "qwen2.embedding_length".into(),
                        gguf_file::Value::U32(width),
                    ),
                    ("qwen2.context_length".into(), gguf_file::Value::U32(64)),
                    ("qwen2.block_count".into(), gguf_file::Value::U32(1)),
                    (
                        "qwen2.attention.layer_norm_rms_epsilon".into(),
                        gguf_file::Value::F32(1e-6),
                    ),
                ]),
                tensor_infos: HashMap::new(),
                tensor_data_offset: 0,
            };
            let result = ModelWeights::from_gguf_split(
                ct,
                &mut std::io::Cursor::new(Vec::<u8>::new()),
                &Device::Cpu,
                &Device::Cpu,
                0,
                32,
            );
            let error = match result {
                Ok(_) => panic!("invalid model accepted"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains("nonzero") || error.contains("divide evenly"),
                "{error}"
            );
        }
    }

    #[test]
    fn test_plan_gpu_layers_zero_budget_is_cpu_only() {
        let ct = gguf_file::Content {
            magic: gguf_file::VersionedMagic::GgufV3,
            metadata: HashMap::new(),
            tensor_infos: HashMap::new(),
            tensor_data_offset: 0,
        };
        assert_eq!(ModelWeights::plan_gpu_layers(&ct, 0, 2304), 0);
    }

    #[test]
    fn test_plan_gpu_layers_missing_block_count_is_cpu_only() {
        let ct = gguf_file::Content {
            magic: gguf_file::VersionedMagic::GgufV3,
            metadata: HashMap::new(),
            tensor_infos: HashMap::new(),
            tensor_data_offset: 0,
        };
        // Non-zero budget but no `qwen2.block_count` metadata at all - must
        // not panic or silently claim GPU layers it has no basis to place.
        assert_eq!(ModelWeights::plan_gpu_layers(&ct, 8_000_000_000, 2304), 0);
    }

    /// Regression: this used to assume "standard 40 layers" regardless of
    /// the model's real depth. A real Qwen2.5-32B-Instruct-Q4_K_M.gguf has
    /// 64 blocks and is ~19.85GB; against this host's actual free VRAM
    /// (~7.6GB, minus the fixed context reserve), the split must land
    /// strictly between "nothing fits" and "the whole model fits" - a
    /// partial offload - never 0 and never all 64 layers. Skips honestly
    /// (not silently) when the model isn't present on this host, since the
    /// weights aren't checked into the repo.
    #[test]
    fn test_plan_gpu_layers_partial_offload_on_real_32b_gguf() {
        let _home = crate::susi_paths::SusiDirs::home_dir();
        let model_path =
            crate::susi_paths::SusiDirs::data_dir().join("models/Qwen2.5-32B-Instruct-Q4_K_M.gguf");
        if !model_path.exists() {
            eprintln!(
                "skipping: {} not present on this host",
                model_path.display()
            );
            return;
        }
        let mut file = std::fs::File::open(&model_path).expect("open 32B gguf");
        let ct = gguf_file::Content::read(&mut file).expect("read gguf metadata");

        let block_count = ct
            .metadata
            .get("qwen2.block_count")
            .and_then(|v| v.to_u32().ok())
            .expect("qwen2.block_count present") as usize;
        assert_eq!(
            block_count, 64,
            "Qwen2.5-32B-Instruct is expected to have 64 blocks"
        );

        // ~7.6GB free on this host's RTX 2000 Ada Laptop GPU, minus the
        // fixed CUDA context reserve applied by `gpu_vram_budget_bytes`.
        let vram_budget_bytes: u64 = 7_600 * 1024 * 1024 - 512 * 1024 * 1024;
        let n_gpu_layers = ModelWeights::plan_gpu_layers(&ct, vram_budget_bytes, 2304);

        assert!(n_gpu_layers > 0, "some layers should fit in ~7GB of VRAM");
        assert!(
            n_gpu_layers < block_count,
            "a 19.85GB model must not fully fit in ~7GB VRAM: got {n_gpu_layers}/{block_count}"
        );
    }

    fn argmax(v: &[f32]) -> u32 {
        let mut best = 0usize;
        let mut best_v = f32::NEG_INFINITY;
        for (i, &x) in v.iter().enumerate() {
            if x > best_v {
                best_v = x;
                best = i;
            }
        }
        best as u32
    }

    /// The correctness property speculative decoding depends on entirely:
    /// verifying a multi-token draft in one batched `forward_all_logits`
    /// call against an existing KV cache (`index_pos > 0`, `seq_len > 1`)
    /// must predict exactly what feeding those same tokens one at a time
    /// through `forward` would have predicted at each position. If the new
    /// offset-aware causal mask were wrong (e.g. masking out cached
    /// positions, or misaligning the diagonal), this would silently corrupt
    /// verification into accepting/rejecting drafted tokens against the
    /// wrong ground truth instead of just erroring - so this checks the
    /// actual predicted token at each position, not merely "it ran".
    /// CPU-only by construction (`cpu_device == gpu_device == Cpu`), so it
    /// runs without a GPU; only needs the small local model present.
    #[test]
    #[ignore = "real tensor computation against a local model (~20s solo, slower under full-suite contention) - run via `cargo test -- --ignored` or the scheduled slow-tests CI workflow"]
    fn test_batched_verify_matches_sequential_one_token_at_a_time() {
        let _home = crate::susi_paths::SusiDirs::home_dir();
        let model_path = crate::susi_paths::SusiDirs::data_dir()
            .join("models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
        if !model_path.exists() {
            eprintln!(
                "skipping: {} not present on this host",
                model_path.display()
            );
            return;
        }
        let cpu = Device::Cpu;
        let load = || -> ModelWeights {
            let mut file = std::fs::File::open(&model_path).expect("open 0.5B gguf");
            let ct = gguf_file::Content::read(&mut file).expect("read gguf metadata");
            ModelWeights::from_gguf_split(ct, &mut file, &cpu, &cpu, 0, 2304).expect("load model")
        };

        let prompt = [100u32, 200, 300, 400];

        // Ground truth: one token at a time, greedy argmax, exactly like
        // the existing single-token generation loop.
        let mut seq_model = load();
        let mut sequential_tokens = Vec::new();
        let mut pos = 0usize;
        let mut next_input = prompt.to_vec();
        for step in 0..3 {
            let input = Tensor::new(next_input.as_slice(), &cpu)
                .unwrap()
                .unsqueeze(0)
                .unwrap();
            let logits = seq_model.forward(&input, pos).unwrap();
            let v: Vec<f32> = logits.flatten_all().unwrap().to_vec1().unwrap();
            let tok = argmax(&v);
            sequential_tokens.push(tok);
            pos = if step == 0 { prompt.len() } else { pos + 1 };
            next_input = vec![tok];
        }

        // Batched verification: same prefix, then all three tokens the
        // sequential run produced fed at once as a single continuation
        // chunk against the same KV-cache state.
        let mut batch_model = load();
        let prefix_input = Tensor::new(prompt.as_slice(), &cpu)
            .unwrap()
            .unsqueeze(0)
            .unwrap();
        let _ = batch_model.forward(&prefix_input, 0).unwrap();
        let continuation = Tensor::new(sequential_tokens.as_slice(), &cpu)
            .unwrap()
            .unsqueeze(0)
            .unwrap();
        let batched_logits = batch_model
            .forward_all_logits(&continuation, prompt.len())
            .expect("batched multi-token verification forward must not device/shape mismatch");

        let (_b, k, _vocab) = batched_logits.dims3().unwrap();
        assert_eq!(k, 3);
        // Row i's prediction is "the next token after having seen prefix +
        // sequential_tokens[0..=i]", which is exactly sequential_tokens[i+1]
        // (computed independently by the one-at-a-time ground truth above).
        for i in 0..2 {
            let row: Vec<f32> = batched_logits.i((0, i, ..)).unwrap().to_vec1().unwrap();
            let predicted = argmax(&row);
            assert_eq!(
                predicted,
                sequential_tokens[i + 1],
                "batched verification position {i} disagrees with sequential ground truth"
            );
        }
    }

    /// After a rejected speculative-decoding round, `truncate_kv_cache`
    /// must make the cache indistinguishable from having never fed the
    /// discarded tail at all - not merely "not crash". Grows one model's
    /// cache with a throwaway batch, truncates it back down, then checks
    /// that a subsequent forward call produces bit-identical logits to a
    /// second model that only ever saw the untruncated prefix.
    #[test]
    #[ignore = "real tensor computation against a local model (~22s solo, slower under full-suite contention) - run via `cargo test -- --ignored` or the scheduled slow-tests CI workflow"]
    fn test_truncate_kv_cache_restores_state_bit_identical_to_never_having_grown() {
        let _home = crate::susi_paths::SusiDirs::home_dir();
        let model_path = crate::susi_paths::SusiDirs::data_dir()
            .join("models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
        if !model_path.exists() {
            eprintln!(
                "skipping: {} not present on this host",
                model_path.display()
            );
            return;
        }
        let cpu = Device::Cpu;
        let load = || -> ModelWeights {
            let mut file = std::fs::File::open(&model_path).expect("open 0.5B gguf");
            let ct = gguf_file::Content::read(&mut file).expect("read gguf metadata");
            ModelWeights::from_gguf_split(ct, &mut file, &cpu, &cpu, 0, 2304).expect("load model")
        };
        let prompt = [100u32, 200, 300, 400];
        let t = |ids: &[u32]| Tensor::new(ids, &cpu).unwrap().unsqueeze(0).unwrap();

        // Reference model: prefix + t1 only, cache length 5.
        let mut reference = load();
        let logits = reference.forward(&t(&prompt), 0).unwrap();
        let t1 = argmax(&logits.flatten_all().unwrap().to_vec1().unwrap());

        reference.forward(&t(&[t1]), prompt.len()).unwrap();

        // Test model: same prefix + t1, then a throwaway 2-token batch
        // (arbitrary, deliberately different from anything meaningful)
        // grows the cache to length 7, which truncate_kv_cache must then
        // fully undo back down to length 5.
        let mut truncated = load();
        truncated.forward(&t(&prompt), 0).unwrap();
        truncated.forward(&t(&[t1]), prompt.len()).unwrap();
        truncated
            .forward_all_logits(&t(&[999u32, 888]), prompt.len() + 1)
            .unwrap();
        truncated.truncate_kv_cache(prompt.len() + 1).unwrap();

        // Both models' caches should now be identical (length 5, same
        // content), so feeding the same next token at the same position
        // must produce bit-identical logits.
        let probe = 42u32;
        let ref_logits = reference.forward(&t(&[probe]), prompt.len() + 1).unwrap();
        let trunc_logits = truncated.forward(&t(&[probe]), prompt.len() + 1).unwrap();

        let ref_v: Vec<f32> = ref_logits.flatten_all().unwrap().to_vec1().unwrap();
        let trunc_v: Vec<f32> = trunc_logits.flatten_all().unwrap().to_vec1().unwrap();
        assert_eq!(
            ref_v, trunc_v,
            "truncated cache must behave identically to a cache that never grew"
        );
    }

    fn cuda_device_or_skip() -> Option<Device> {
        if !cfg!(feature = "cuda") {
            return None;
        }
        std::panic::catch_unwind(|| Device::new_cuda(0)).ok()?.ok()
    }

    /// Regression: `cos`/`sin`/`neg_inf` used to be computed once on
    /// `cpu_device` and cloned into every `LayerWeights` regardless of that
    /// layer's actual device. Any layer placed on `gpu_device` then hard-
    /// errored on its very first `rope()` call (`Storage::same_device`
    /// rejects a GPU activation against a CPU `cos`/`sin`), and that error
    /// was silently swallowed by `GemiEngine::reason_internal`'s
    /// `if let Ok(res) = engine.run_inference_stream(...)`, so the whole
    /// GPU-split feature silently never ran on GPU at all - it looked like
    /// it worked (a fallback path produced *some* answer) while actually
    /// never exercising the split it had just computed. This loads the
    /// smallest local Qwen2 GGUF fully onto GPU (every layer, so
    /// `layer_in` also ends the loop GPU-resident, exercising the
    /// symmetric output_norm/output bug fixed alongside it) and asserts a
    /// real forward pass returns finite logits, not just that loading
    /// succeeded. Skips honestly when no CUDA device or model is present.
    #[test]
    fn test_forward_succeeds_fully_on_gpu() {
        let Some(gpu) = cuda_device_or_skip() else {
            eprintln!("skipping: no CUDA device available");
            return;
        };
        let _home = crate::susi_paths::SusiDirs::home_dir();
        let model_path = crate::susi_paths::SusiDirs::data_dir()
            .join("models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
        if !model_path.exists() {
            eprintln!(
                "skipping: {} not present on this host",
                model_path.display()
            );
            return;
        }
        let mut file = std::fs::File::open(&model_path).expect("open 0.5B gguf");
        let ct = gguf_file::Content::read(&mut file).expect("read gguf metadata");
        let cpu = Device::Cpu;

        let mut model = ModelWeights::from_gguf_split(ct, &mut file, &cpu, &gpu, 999, 2304)
            .expect("0.5B model should fully load onto GPU");

        let input = Tensor::new(&[1u32, 2, 3], &cpu)
            .unwrap()
            .unsqueeze(0)
            .unwrap();
        let logits = model.forward(&input, 0).expect(
            "forward pass across an all-GPU-resident model must not device-mismatch on output_norm/output",
        );
        let flat: Vec<f32> = logits.flatten_all().unwrap().to_vec1().unwrap();
        assert!(!flat.is_empty());
        assert!(
            flat.iter().all(|v| v.is_finite()),
            "logits must be finite, not NaN/Inf"
        );
        assert!(
            model.is_fully_gpu_resident(),
            "n_gpu_layers=999 should place every layer on GPU"
        );
    }

    #[test]
    fn test_is_fully_gpu_resident_false_for_cpu_only_load() {
        let _home = crate::susi_paths::SusiDirs::home_dir();
        let model_path = crate::susi_paths::SusiDirs::data_dir()
            .join("models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
        if !model_path.exists() {
            return;
        }
        let mut file = std::fs::File::open(&model_path).unwrap();
        let ct = gguf_file::Content::read(&mut file).unwrap();
        let cpu = Device::Cpu;
        // gpu_device == cpu_device (n_gpu_layers=0 too): not "resident on a
        // real GPU" in any meaningful sense, must report false.
        let model = ModelWeights::from_gguf_split(ct, &mut file, &cpu, &cpu, 0, 2304).unwrap();
        assert!(!model.is_fully_gpu_resident());
    }

    /// Same regression as above, but for a genuine partial CPU/GPU split
    /// (Qwen2.5-14B, ~8.99GB, guaranteed larger than this host's ~7GB VRAM
    /// budget): exercises a GPU-resident layer's `rope()` call, the
    /// mid-forward device handoff from GPU layers to CPU layers, and the
    /// causal-mask device selection (`mask_gpu` vs `mask_cpu`) added by
    /// this fix, all at once, using more than one prompt token so the
    /// causal mask path is actually taken (a single-token input skips it
    /// entirely). Skips honestly when no CUDA device or model is present.
    #[test]
    fn test_forward_succeeds_on_partial_cpu_gpu_split() {
        let Some(gpu) = cuda_device_or_skip() else {
            eprintln!("skipping: no CUDA device available");
            return;
        };
        let _home = crate::susi_paths::SusiDirs::home_dir();
        let model_path =
            crate::susi_paths::SusiDirs::data_dir().join("models/Qwen2.5-14B-Instruct-Q4_K_M.gguf");
        if !model_path.exists() {
            eprintln!(
                "skipping: {} not present on this host",
                model_path.display()
            );
            return;
        }
        let mut file = std::fs::File::open(&model_path).expect("open 14B gguf");
        let ct = gguf_file::Content::read(&mut file).expect("read gguf metadata");
        let cpu = Device::Cpu;

        let vram_budget_bytes: u64 = 7_600 * 1024 * 1024 - 512 * 1024 * 1024;
        let n_gpu_layers = ModelWeights::plan_gpu_layers(&ct, vram_budget_bytes, 2304);
        let block_count = ct
            .metadata
            .get("qwen2.block_count")
            .and_then(|v| v.to_u32().ok())
            .expect("qwen2.block_count present") as usize;
        assert!(
            n_gpu_layers > 0 && n_gpu_layers < block_count,
            "expected a genuine partial split for a ~9GB model against ~7GB VRAM, got {n_gpu_layers}/{block_count}"
        );

        let mut model =
            ModelWeights::from_gguf_split(ct, &mut file, &cpu, &gpu, n_gpu_layers, 2304)
                .expect("14B model should load across a CPU/GPU split");

        let input = Tensor::new(&[1u32, 2, 3, 4, 5], &cpu)
            .unwrap()
            .unsqueeze(0)
            .unwrap();
        let logits = model
            .forward(&input, 0)
            .expect("forward pass across a partial CPU/GPU split must not device-mismatch");
        let flat: Vec<f32> = logits.flatten_all().unwrap().to_vec1().unwrap();
        assert!(!flat.is_empty());
        assert!(
            flat.iter().all(|v| v.is_finite()),
            "logits must be finite, not NaN/Inf"
        );
        assert!(
            !model.is_fully_gpu_resident(),
            "a partial split must not report as fully GPU-resident"
        );
    }
}

//! KV cache sized from free memory, observed context use, and exact
//! Grouped-Query Attention (GQA) transformer geometry + PagedAttention roofline.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvCapacity {
    pub tokens: u64,
}

/// Rough bytes per token of KV for a typical mid-size model.
const BYTES_PER_TOKEN: u64 = 256;

/// PagedAttention block granularity in tokens.
pub const PAGE_BLOCK_TOKENS: u64 = 256;

/// Derive capacity from free RAM/VRAM bytes and the observed p95 context.
#[must_use]
pub fn kv_capacity(free_bytes: u64, observed_context_p95: u64) -> KvCapacity {
    // Reserve half of free memory for KV; clamp to a useful window.
    let from_mem = free_bytes / (2 * BYTES_PER_TOKEN);
    let floor = observed_context_p95.saturating_mul(2).max(2048);
    let capped = from_mem.max(floor).min(262_144);
    KvCapacity { tokens: capped }
}

/// KV cache quantization scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KvQuantScheme {
    /// 16-bit floating point (`FP16` / `BF16`): 2.0 bytes per element, zero quantization loss.
    Fp16,
    /// 8-bit block quantization (`Q8_0`): 1.0625 bytes per element (34 bytes per 32 elements).
    Q8_0,
    /// 4-bit block quantization (`Q4_0`): 0.5625 bytes per element (18 bytes per 32 elements).
    Q4_0,
}

impl KvQuantScheme {
    /// Exact numerator/denominator `(num, den)` bytes per element for integer-exact sizing.
    #[must_use]
    pub const fn bytes_ratio(self) -> (u64, u64) {
        match self {
            Self::Fp16 => (2, 1),
            Self::Q8_0 => (17, 16),
            Self::Q4_0 => (9, 16),
        }
    }
}

/// Exact architectural geometry of a Transformer model for KV-cache and roofline math.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransformerKvGeometry {
    pub num_layers: u32,
    pub num_attention_heads: u32,
    pub num_kv_heads: u32,
    pub head_dim: u32,
    pub max_position_embeddings: u64,
}

impl TransformerKvGeometry {
    /// Qwen2.5-7B geometry ($L=28, H_q=28, H_{kv}=4, d=128$, GQA $7\times$ compression).
    #[must_use]
    pub const fn qwen25_7b() -> Self {
        Self {
            num_layers: 28,
            num_attention_heads: 28,
            num_kv_heads: 4,
            head_dim: 128,
            max_position_embeddings: 131_072,
        }
    }

    /// Qwen2.5-32B / DeepSeek-R1-Distill-32B geometry ($L=64, H_q=40, H_{kv}=8, d=128$).
    #[must_use]
    pub const fn qwen25_32b() -> Self {
        Self {
            num_layers: 64,
            num_attention_heads: 40,
            num_kv_heads: 8,
            head_dim: 128,
            max_position_embeddings: 131_072,
        }
    }

    /// Exact KV-cache bytes per token across all layers for `scheme`:
    /// $\text{Bytes/Token} = 2 \cdot L \cdot H_{kv} \cdot d_{\text{head}} \cdot b(Q)$.
    #[must_use]
    pub const fn kv_bytes_per_token(self, scheme: KvQuantScheme) -> u64 {
        let elems = 2_u64
            .saturating_mul(self.num_layers as u64)
            .saturating_mul(self.num_kv_heads as u64)
            .saturating_mul(self.head_dim as u64);
        let (num, den) = scheme.bytes_ratio();
        let v = elems.saturating_mul(num).div_ceil(den);
        if v == 0 {
            1
        } else {
            v
        }
    }

    /// Grouped-Query Attention (GQA) KV memory reduction factor $H_q / H_{kv}$.
    #[must_use]
    pub fn gqa_compression_ratio(self) -> f64 {
        f64::from(self.num_attention_heads.max(1)) / f64::from(self.num_kv_heads.max(1))
    }
}

/// Complete PagedAttention KV-cache allocation plan produced by [`plan_kv_cache`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KvRooflinePlan {
    pub recommended_scheme: KvQuantScheme,
    pub bytes_per_token: u64,
    pub total_tokens: u64,
    pub paged_blocks: u64,
    pub max_concurrent_sequences: u32,
    pub gqa_compression_ratio: f64,
}

/// Compute the optimal KV quantization scheme, PagedAttention block count, and
/// concurrency ceiling for `geom` given `free_bytes` and `observed_context_p95`.
#[must_use]
pub fn plan_kv_cache(
    geom: TransformerKvGeometry,
    free_bytes: u64,
    observed_context_p95: u64,
) -> KvRooflinePlan {
    let budget_bytes = free_bytes / 2;
    let target_seq_tokens = observed_context_p95
        .saturating_mul(2)
        .max(2048)
        .min(geom.max_position_embeddings.max(2048));

    // Pick the highest-precision quantization scheme that fits at least one
    // full `target_seq_tokens` sequence inside `budget_bytes`.
    let scheme = [
        KvQuantScheme::Fp16,
        KvQuantScheme::Q8_0,
        KvQuantScheme::Q4_0,
    ]
    .into_iter()
    .find(|&s| geom.kv_bytes_per_token(s).saturating_mul(target_seq_tokens) <= budget_bytes)
    .unwrap_or(KvQuantScheme::Q4_0);

    let bpt = geom.kv_bytes_per_token(scheme);
    let raw_tokens = (budget_bytes / bpt)
        .max(target_seq_tokens)
        .min(geom.max_position_embeddings.max(2048));
    let paged_blocks = raw_tokens.div_ceil(PAGE_BLOCK_TOKENS).max(1);
    let total_tokens = paged_blocks.saturating_mul(PAGE_BLOCK_TOKENS);
    let max_concurrent_sequences =
        u32::try_from((total_tokens / target_seq_tokens).clamp(1, 64)).unwrap_or(1);

    KvRooflinePlan {
        recommended_scheme: scheme,
        bytes_per_token: bpt,
        total_tokens,
        paged_blocks,
        max_concurrent_sequences,
        gqa_compression_ratio: geom.gqa_compression_ratio(),
    }
}

#[cfg(test)]
mod zc_kv_auto_tests {
    use super::*;

    #[test]
    fn zc_kv_auto_scales_from_vram_and_context() {
        let small = kv_capacity(64 * 1024 * 1024, 1024);
        let large = kv_capacity(8 * 1024 * 1024 * 1024, 8192);
        assert!(large.tokens > small.tokens);
        assert!(kv_capacity(0, 4096).tokens >= 8192);
    }

    #[test]
    fn gqa_roofline_planner_selects_quantization_and_aligns_paged_blocks() {
        let geom = TransformerKvGeometry::qwen25_7b();
        // 2 * 28 * 4 * 128 * 2 = 57,344 bytes/token in FP16
        assert_eq!(geom.kv_bytes_per_token(KvQuantScheme::Fp16), 57_344);
        assert!((geom.gqa_compression_ratio() - 7.0).abs() < 1e-6);

        // Abundant VRAM (8 GiB free, p95=4096) keeps lossless FP16 KV cache
        let spacious = plan_kv_cache(geom, 8 * 1024 * 1024 * 1024, 4096);
        assert_eq!(spacious.recommended_scheme, KvQuantScheme::Fp16);
        assert_eq!(spacious.total_tokens % PAGE_BLOCK_TOKENS, 0);
        assert!(spacious.max_concurrent_sequences >= 4);

        // Tight VRAM (600 MiB free, p95=4096 -> target=8192 tokens -> ~470 MiB in FP16 > 300 MiB budget)
        // automatically downshifts KV cache to Q8_0 / Q4_0 to fit without OOM!
        let tight = plan_kv_cache(geom, 600 * 1024 * 1024, 4096);
        assert_ne!(tight.recommended_scheme, KvQuantScheme::Fp16);
        assert!(tight.total_tokens >= 8192);
    }
}

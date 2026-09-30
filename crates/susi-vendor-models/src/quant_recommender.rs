//! Quantisation recommender — picks the highest-quality GGUF quantisation
//! that fits a host's memory with headroom for the KV cache at the requested
//! context length, and explains the choice.
//!
//! Weight sizing uses the effective bits-per-weight of each quant variant
//! (weight bytes ≈ params × bpw / 8 plus a small container overhead). KV cache
//! sizing uses the model's layer count and embedding length when known (from
//! [`crate::gguf_inspector`]): `2 (K+V) × layers × embed × ctx × 2 bytes` —
//! an upper bound; GQA/MLA models use less in practice.

/// A quantisation candidate, ordered best-quality first.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quant {
    pub name: &'static str,
    /// Effective bits per weight including tensor overhead.
    pub bpw: f64,
    /// Relative quality rank — lower is better.
    pub quality: u8,
}

/// Supported candidates, best quality first.
pub const CANDIDATES: &[Quant] = &[
    Quant {
        name: "BF16",
        bpw: 16.0,
        quality: 0,
    },
    Quant {
        name: "F16",
        bpw: 16.0,
        quality: 1,
    },
    Quant {
        name: "Q8_0",
        bpw: 8.5,
        quality: 2,
    },
    Quant {
        name: "Q6_K",
        bpw: 6.5625,
        quality: 3,
    },
    Quant {
        name: "Q5_K_M",
        bpw: 5.5,
        quality: 4,
    },
    Quant {
        name: "Q4_K_M",
        bpw: 4.8,
        quality: 5,
    },
    Quant {
        name: "Q4_K_S",
        bpw: 4.6,
        quality: 6,
    },
    Quant {
        name: "Q3_K_L",
        bpw: 4.3,
        quality: 7,
    },
    Quant {
        name: "Q3_K_M",
        bpw: 3.9,
        quality: 8,
    },
    Quant {
        name: "IQ3_XXS",
        bpw: 3.06,
        quality: 9,
    },
    Quant {
        name: "Q2_K",
        bpw: 2.5625,
        quality: 10,
    },
];

/// What the model needs.
#[derive(Debug, Clone, Copy)]
pub struct ModelNeeds {
    /// Parameter count.
    pub params: u64,
    /// Transformer layer count, when known (GGUF `block_count`).
    pub layers: Option<u64>,
    /// Embedding/hidden width, when known (GGUF `embedding_length`).
    pub embedding: Option<u64>,
    /// Target context length for the KV cache budget.
    pub context: u64,
}

/// What the host offers.
#[derive(Debug, Clone, Copy, Default)]
pub struct Host {
    /// Free VRAM across the device the model would run on (bytes).
    pub vram: u64,
    /// Free system RAM available to the process (bytes).
    pub ram: u64,
    /// Free disk for the weight file itself (bytes).
    pub disk: u64,
}

/// Fraction of each memory pool we refuse to spend — leaves room for the
/// runtime, activations and the OS.
pub const HEADROOM: f64 = 0.85;

/// Container/mapping overhead applied to raw weight bytes.
const WEIGHT_OVERHEAD: f64 = 1.05;

#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    pub quant: &'static str,
    /// Estimated weight bytes at this quantisation.
    pub weight_bytes: u64,
    /// Estimated KV cache at the target context.
    pub kv_bytes: u64,
    /// `vram` when the GPU pool fits, `ram` for CPU/offload, `none` when even
    /// RAM cannot hold the smallest candidate.
    pub target: &'static str,
    pub why: String,
}

/// Bytes the weight file occupies for `params` at `bpw`.
pub fn weight_bytes(params: u64, bpw: f64) -> u64 {
    (params as f64 * bpw / 8.0 * WEIGHT_OVERHEAD) as u64
}

/// KV cache estimate. Without layer/embedding data we fall back to
/// `params / 40` bytes-per-token, a mid-range empirical ratio.
pub fn kv_bytes(needs: &ModelNeeds) -> u64 {
    match (needs.layers, needs.embedding) {
        (Some(l), Some(e)) => 2 * l * e * needs.context * 2,
        _ => needs.params / 40 * needs.context / 1024,
    }
}

/// Choose the best quantisation for `needs` on `host`.
///
/// Prefers VRAM (GPU-resident) fits; degrades to system RAM (partial or full
/// offload); reports `target: "none"` when the smallest candidate cannot fit
/// anywhere — the caller should refuse rather than thrash.
pub fn recommend(needs: &ModelNeeds, host: &Host) -> Recommendation {
    let kv = kv_bytes(needs);
    let vram_cap = (host.vram as f64 * HEADROOM) as u64;
    let ram_cap = (host.ram as f64 * HEADROOM) as u64;

    for q in CANDIDATES {
        let w = weight_bytes(needs.params, q.bpw);
        if host.disk > 0 && w > host.disk {
            continue;
        }
        if w + kv <= vram_cap {
            return Recommendation {
                quant: q.name,
                weight_bytes: w,
                kv_bytes: kv,
                target: "vram",
                why: format!(
                    "{} fits in VRAM ({:.1} GiB weights + {:.1} GiB KV of {:.1} GiB usable)",
                    q.name,
                    gib(w),
                    gib(kv),
                    gib(vram_cap)
                ),
            };
        }
    }
    for q in CANDIDATES {
        let w = weight_bytes(needs.params, q.bpw);
        if host.disk > 0 && w > host.disk {
            continue;
        }
        if w + kv <= ram_cap {
            return Recommendation {
                quant: q.name,
                weight_bytes: w,
                kv_bytes: kv,
                target: "ram",
                why: format!(
                    "{} exceeds VRAM but fits in RAM ({:.1} GiB weights + {:.1} GiB KV of {:.1} GiB usable) — expect offload or CPU inference",
                    q.name,
                    gib(w),
                    gib(kv),
                    gib(ram_cap)
                ),
            };
        }
    }
    let smallest = CANDIDATES.last().map(|q| q.name).unwrap_or("Q2_K");
    let w = weight_bytes(needs.params, CANDIDATES.last().map_or(2.0, |q| q.bpw));
    Recommendation {
        quant: smallest,
        weight_bytes: w,
        kv_bytes: kv,
        target: "none",
        why: format!(
            "even {smallest} ({:.1} GiB weights + {:.1} GiB KV) exceeds usable VRAM ({:.1} GiB) and RAM ({:.1} GiB)",
            gib(w),
            gib(kv),
            gib(vram_cap),
            gib(ram_cap)
        ),
    }
}

fn gib(b: u64) -> f64 {
    b as f64 / (1 << 30) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn llama_7b() -> ModelNeeds {
        ModelNeeds {
            params: 7_000_000_000,
            layers: Some(32),
            embedding: Some(4096),
            context: 4096,
        }
    }

    #[test]
    fn quant_recommender_picks_best_fit_for_vram() {
        // 24 GiB card, 7B model: Q8_0 ≈ 7.8 GiB + ~2 GiB KV fits; F16 ≈ 14.7 GiB
        // + KV also fits — F16 is better quality and wins.
        let host = Host {
            vram: 24 * GIB,
            ram: 64 * GIB,
            disk: 500 * GIB,
        };
        let r = recommend(&llama_7b(), &host);
        assert_eq!(r.target, "vram");
        assert_eq!(r.quant, "BF16");
    }

    #[test]
    fn quant_recommender_steps_down_until_it_fits() {
        // 8 GiB card: F16 7B needs ~16.7 GiB — no; Q4_K_M ~3.5+2 = 5.5 GiB fits.
        let host = Host {
            vram: 8 * GIB,
            ram: 32 * GIB,
            disk: 500 * GIB,
        };
        let r = recommend(&llama_7b(), &host);
        assert_eq!(r.target, "vram");
        assert_eq!(r.quant, "Q5_K_M");
        assert!(r.weight_bytes + r.kv_bytes <= (8.0 * GIB as f64 * HEADROOM) as u64);
    }

    #[test]
    fn quant_recommender_falls_back_to_ram() {
        // 4 GiB card but 64 GiB RAM: Q8_0 doesn't fit VRAM; several quants fit RAM.
        let host = Host {
            vram: 4 * GIB,
            ram: 64 * GIB,
            disk: 500 * GIB,
        };
        let r = recommend(&llama_7b(), &host);
        assert_eq!(r.target, "ram");
        assert_eq!(r.quant, "BF16");
        assert!(r.why.contains("VRAM"));
    }

    #[test]
    fn quant_recommender_reports_none_when_nothing_fits() {
        // 70B model, 8 GiB card, 16 GiB RAM: even Q2_K (~20 GiB) fits nowhere.
        let needs = ModelNeeds {
            params: 70_000_000_000,
            layers: Some(80),
            embedding: Some(8192),
            context: 4096,
        };
        let host = Host {
            vram: 8 * GIB,
            ram: 16 * GIB,
            disk: 500 * GIB,
        };
        let r = recommend(&needs, &host);
        assert_eq!(r.target, "none");
        assert!(r.why.contains("exceeds"));
    }

    #[test]
    fn quant_recommender_respects_disk_budget() {
        let needs = llama_7b();
        let host = Host {
            vram: 24 * GIB,
            ram: 64 * GIB,
            disk: 5 * GIB,
        };
        // F16 weights (~14 GiB) don't fit on disk; a small quant does.
        let r = recommend(&needs, &host);
        assert!(r.weight_bytes <= 5 * GIB);
        assert_ne!(r.quant, "BF16");
    }

    #[test]
    fn quant_recommender_uses_estimated_kv_without_gguf() {
        let needs = ModelNeeds {
            params: 7_000_000_000,
            layers: None,
            embedding: None,
            context: 8192,
        };
        let kv = kv_bytes(&needs);
        assert!(kv > 0);
    }

    #[test]
    fn quant_recommender_long_context_steers_smaller() {
        // 128k ctx inflates KV: 2*32*4096*131072*2 B ≈ 68 GiB — nothing fits
        // an 80 GiB card at high quant, so the recommender must degrade.
        let needs = ModelNeeds {
            params: 7_000_000_000,
            layers: Some(32),
            embedding: Some(4096),
            context: 131_072,
        };
        let host = Host {
            vram: 80 * GIB,
            ram: 256 * GIB,
            disk: 0,
        };
        let r = recommend(&needs, &host);
        assert_ne!(r.quant, "BF16");
        assert_ne!(r.target, "none");
    }
}

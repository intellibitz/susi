//! KV cache sized from free memory and observed context use.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvCapacity {
    pub tokens: u64,
}

/// Rough bytes per token of KV for a typical mid-size model.
const BYTES_PER_TOKEN: u64 = 256;

/// Derive capacity from free RAM/VRAM bytes and the observed p95 context.
#[must_use]
pub fn kv_capacity(free_bytes: u64, observed_context_p95: u64) -> KvCapacity {
    // Reserve half of free memory for KV; clamp to a useful window.
    let from_mem = free_bytes / (2 * BYTES_PER_TOKEN);
    let floor = observed_context_p95.saturating_mul(2).max(2048);
    let capped = from_mem.max(floor).min(262_144);
    KvCapacity { tokens: capped }
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
}

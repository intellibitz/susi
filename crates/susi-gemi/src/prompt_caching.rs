//! Use vendor prompt caching to cut cost.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachePlan {
    pub enabled: bool,
    pub prefix_tokens: u64,
    pub estimated_savings_usd: f64,
}

/// Enable prompt caching when the static prefix is large enough.
#[must_use]
pub fn prompt_cache_plan(prefix_tokens: u64, price_per_mtok_usd: f64) -> CachePlan {
    let enabled = prefix_tokens >= 1024;
    let savings = if enabled {
        (prefix_tokens as f64 / 1_000_000.0) * price_per_mtok_usd * 0.5
    } else {
        0.0
    };
    CachePlan {
        enabled,
        prefix_tokens,
        estimated_savings_usd: savings,
    }
}

#[cfg(test)]
mod prompt_caching_tests {
    use super::*;

    #[test]
    fn prompt_caching_enables_for_large_prefix() {
        assert!(!prompt_cache_plan(100, 3.0).enabled);
        let p = prompt_cache_plan(10_000, 3.0);
        assert!(p.enabled);
        assert!(p.estimated_savings_usd > 0.0);
    }
}

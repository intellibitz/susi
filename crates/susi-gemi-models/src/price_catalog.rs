//! Versioned price catalog: USD per 1M input/output tokens, preferred by the
//! cost module when an entry exists. Bundles can be delivered through the
//! signed catalog channel.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PRICE_CHANNEL: &str = "model-prices";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceEntry {
    pub model_id: String,
    /// USD per 1M input tokens.
    pub input_usd_per_1m: f64,
    /// USD per 1M output tokens.
    pub output_usd_per_1m: f64,
    /// USD per 1M prompt-cache-read tokens; absent = billed at input rate.
    /// Cache-hit pricing is what makes long repeated context cheap on
    /// vendors that offer it (VC-202-002).
    #[serde(default)]
    pub cache_hit_usd_per_1m: Option<f64>,
    /// USD per 1M prompt-cache-write/miss tokens; absent = billed at input
    /// rate.
    #[serde(default)]
    pub cache_miss_usd_per_1m: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceCatalog {
    pub version: u64,
    pub entries: BTreeMap<String, PriceEntry>,
}

impl PriceCatalog {
    #[must_use]
    pub fn empty(version: u64) -> Self {
        Self {
            version,
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, entry: PriceEntry) {
        self.entries.insert(entry.model_id.clone(), entry);
    }

    #[must_use]
    pub fn lookup(&self, model_id: &str) -> Option<&PriceEntry> {
        self.entries.get(model_id)
    }

    /// Expected USD for one call on `model_id`, priced from the cache hit
    /// rate the workload actually achieves (VC-202-002): hit-priced tokens
    /// are billed at `cache_hit`, misses at `cache_miss`, and either absent
    /// rate falls back to the plain input price. `None` when the model has
    /// no price record — the caller decides the fallback, this function
    /// never invents a price.
    #[must_use]
    pub fn expected_cost_usd(
        &self,
        model_id: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_hit_ratio: f64,
    ) -> Option<f64> {
        let e = self.lookup(model_id)?;
        let hit = cache_hit_ratio.clamp(0.0, 1.0);
        let hit_rate = e.cache_hit_usd_per_1m.unwrap_or(e.input_usd_per_1m);
        let miss_rate = e.cache_miss_usd_per_1m.unwrap_or(e.input_usd_per_1m);
        let input_rate = hit * hit_rate + (1.0 - hit) * miss_rate;
        Some(
            (prompt_tokens as f64) * input_rate / 1_000_000.0
                + (completion_tokens as f64) * e.output_usd_per_1m / 1_000_000.0,
        )
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod price_catalog_tests {
    use super::*;

    #[test]
    fn price_catalog_json_roundtrip() {
        let mut cat = PriceCatalog::empty(2);
        cat.insert(PriceEntry {
            model_id: "claude".into(),
            input_usd_per_1m: 3.0,
            output_usd_per_1m: 15.0,
            cache_hit_usd_per_1m: None,
            cache_miss_usd_per_1m: None,
        });
        let json = cat.to_json().unwrap();
        let back = PriceCatalog::from_json(&json).unwrap();
        assert_eq!(back.version, 2);
        assert_eq!(back.lookup("claude").unwrap().output_usd_per_1m, 15.0);
        assert_eq!(PRICE_CHANNEL, "model-prices");
    }

    #[test]
    fn expected_cost_usd_blends_cache_hit_and_miss_rates_and_clamps_ratio() {
        let mut cat = PriceCatalog::empty(3);
        cat.insert(PriceEntry {
            model_id: "cached-model".into(),
            input_usd_per_1m: 4.0,
            output_usd_per_1m: 10.0,
            cache_hit_usd_per_1m: Some(1.0),
            cache_miss_usd_per_1m: Some(5.0),
        });
        let blended = cat
            .expected_cost_usd("cached-model", 1_000_000, 1_000_000, 0.5)
            .unwrap();
        assert!((blended - 13.0).abs() < 1e-9);

        let clamped_low = cat
            .expected_cost_usd("cached-model", 1_000_000, 1_000_000, -1.0)
            .unwrap();
        assert!((clamped_low - 15.0).abs() < 1e-9);

        let clamped_high = cat
            .expected_cost_usd("cached-model", 1_000_000, 1_000_000, 2.0)
            .unwrap();
        assert!((clamped_high - 11.0).abs() < 1e-9);

        assert!(cat.expected_cost_usd("unknown", 100, 100, 0.5).is_none());
    }

    #[test]
    fn expected_cost_usd_falls_back_to_input_rate_when_cache_rates_are_absent() {
        let mut cat = PriceCatalog::empty(4);
        cat.insert(PriceEntry {
            model_id: "plain-model".into(),
            input_usd_per_1m: 2.0,
            output_usd_per_1m: 8.0,
            cache_hit_usd_per_1m: None,
            cache_miss_usd_per_1m: None,
        });
        let cost = cat
            .expected_cost_usd("plain-model", 500_000, 250_000, 0.75)
            .unwrap();
        assert!((cost - 3.0).abs() < 1e-9);
    }
}

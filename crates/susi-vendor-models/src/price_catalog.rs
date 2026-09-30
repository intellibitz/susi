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

    /// Prefer catalog price when present; else return the coarse tier estimate.
    #[must_use]
    pub fn cost_usd(
        &self,
        model_id: &str,
        input_tokens: u64,
        output_tokens: u64,
        coarse_tier_usd: f64,
    ) -> f64 {
        match self.lookup(model_id) {
            Some(e) => {
                (input_tokens as f64) * e.input_usd_per_1m / 1_000_000.0
                    + (output_tokens as f64) * e.output_usd_per_1m / 1_000_000.0
            }
            None => coarse_tier_usd,
        }
    }

    pub fn from_json(s: &str) -> Result<Self, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

/// Reject installing a catalog whose version does not exceed `installed`.
pub fn accept_version(installed: u64, candidate: u64) -> Result<(), String> {
    if candidate <= installed {
        return Err(format!(
            "price catalog version {candidate} does not exceed installed {installed}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod price_catalog_tests {
    use super::*;

    #[test]
    fn price_catalog_prefers_entry_over_coarse_tier() {
        let mut cat = PriceCatalog::empty(1);
        cat.insert(PriceEntry {
            model_id: "gpt-4o".into(),
            input_usd_per_1m: 2.5,
            output_usd_per_1m: 10.0,
        });
        let cost = cat.cost_usd("gpt-4o", 1_000_000, 500_000, 99.0);
        assert!((cost - 7.5).abs() < 1e-9);
        assert_eq!(cat.cost_usd("unknown", 1, 1, 3.0), 3.0);
    }

    #[test]
    fn price_catalog_version_rejects_downgrade() {
        assert!(accept_version(3, 4).is_ok());
        assert!(accept_version(3, 3)
            .unwrap_err()
            .contains("does not exceed"));
        assert!(accept_version(3, 2).is_err());
    }

    #[test]
    fn price_catalog_json_roundtrip() {
        let mut cat = PriceCatalog::empty(2);
        cat.insert(PriceEntry {
            model_id: "claude".into(),
            input_usd_per_1m: 3.0,
            output_usd_per_1m: 15.0,
        });
        let json = cat.to_json().unwrap();
        let back = PriceCatalog::from_json(&json).unwrap();
        assert_eq!(back.version, 2);
        assert_eq!(back.lookup("claude").unwrap().output_usd_per_1m, 15.0);
        assert_eq!(PRICE_CHANNEL, "model-prices");
    }
}

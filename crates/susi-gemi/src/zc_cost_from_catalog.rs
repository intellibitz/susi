//! Cost tiers derived from a price catalog (no hand-edited tier file).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogPrice {
    pub input_usd_per_1m: f64,
    pub output_usd_per_1m: f64,
}

#[must_use]
pub fn estimate_usd(
    catalog: &BTreeMap<String, CatalogPrice>,
    model: &str,
    in_tok: u64,
    out_tok: u64,
) -> Option<f64> {
    let e = catalog.get(model)?;
    Some((in_tok as f64) * e.input_usd_per_1m / 1e6 + (out_tok as f64) * e.output_usd_per_1m / 1e6)
}

#[cfg(test)]
mod zc_cost_from_catalog_tests {
    use super::*;

    #[test]
    fn zc_cost_from_catalog_uses_catalog_not_tiers() {
        let mut cat = BTreeMap::new();
        cat.insert(
            "m".into(),
            CatalogPrice {
                input_usd_per_1m: 1.0,
                output_usd_per_1m: 2.0,
            },
        );
        assert!((estimate_usd(&cat, "m", 1_000_000, 500_000).unwrap() - 2.0).abs() < 1e-9);
        assert!(estimate_usd(&cat, "missing", 1, 1).is_none());
    }
}

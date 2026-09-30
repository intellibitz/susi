//! Peer capability marketplace with cost and latency.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketOffer {
    pub peer: String,
    pub capability: String,
    pub cost_usd: f64,
    pub latency_ms: u64,
}

/// Pick the best offer by cost then latency.
#[must_use]
pub fn pick_offer(offers: &[MarketOffer], capability: &str) -> Option<MarketOffer> {
    offers
        .iter()
        .filter(|o| o.capability == capability)
        .min_by(|a, b| {
            a.cost_usd
                .partial_cmp(&b.cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.latency_ms.cmp(&b.latency_ms))
        })
        .cloned()
}

#[cfg(test)]
mod capability_market_tests {
    use super::*;

    #[test]
    fn capability_market_picks_cheapest_then_fastest() {
        let offers = [
            MarketOffer {
                peer: "a".into(),
                capability: "code".into(),
                cost_usd: 0.2,
                latency_ms: 100,
            },
            MarketOffer {
                peer: "b".into(),
                capability: "code".into(),
                cost_usd: 0.1,
                latency_ms: 200,
            },
        ];
        let p = pick_offer(&offers, "code").unwrap();
        assert_eq!(p.peer, "b");
    }
}

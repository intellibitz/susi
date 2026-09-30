//! Regional endpoint latency probing and failover.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionLatency {
    pub region: String,
    pub rtt_ms: u64,
}

/// Pick the lowest-RTT healthy region.
#[must_use]
pub fn failover(regions: &[RegionLatency], unhealthy: &[&str]) -> Option<String> {
    regions
        .iter()
        .filter(|r| !unhealthy.iter().any(|u| *u == r.region))
        .min_by_key(|r| r.rtt_ms)
        .map(|r| r.region.clone())
}

#[cfg(test)]
mod region_failover_tests {
    use super::*;

    #[test]
    fn region_failover_picks_lowest_rtt() {
        let regs = [
            RegionLatency {
                region: "us-east".into(),
                rtt_ms: 80,
            },
            RegionLatency {
                region: "eu-west".into(),
                rtt_ms: 40,
            },
            RegionLatency {
                region: "ap-south".into(),
                rtt_ms: 120,
            },
        ];
        assert_eq!(failover(&regs, &[]).as_deref(), Some("eu-west"));
        assert_eq!(failover(&regs, &["eu-west"]).as_deref(), Some("us-east"));
    }
}

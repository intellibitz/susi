//! Latency SLO routing per task class.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SloPick {
    pub model: String,
    pub within_slo: bool,
}

/// Prefer models whose p95 meets the task-class SLO.
#[must_use]
pub fn route_latency_slo(models: &[(String, u64 /* p95_ms */)], slo_ms: u64) -> Option<SloPick> {
    models
        .iter()
        .filter(|(_, p95)| *p95 <= slo_ms)
        .min_by_key(|(_, p95)| *p95)
        .map(|(name, p95)| SloPick {
            model: name.clone(),
            within_slo: *p95 <= slo_ms,
        })
}

#[cfg(test)]
mod latency_slo_tests {
    use super::*;

    #[test]
    fn latency_slo_filters_slow_models() {
        let models = [("slow".into(), 2000u64), ("fast".into(), 200u64)];
        let p = route_latency_slo(&models, 500).unwrap();
        assert_eq!(p.model, "fast");
        assert!(p.within_slo);
        assert!(route_latency_slo(&models, 100).is_none());
    }
}

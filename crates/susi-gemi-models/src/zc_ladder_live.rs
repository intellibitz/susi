//! Model ladder rebuilt from the live catalog and hardware.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderStep {
    pub model: String,
    pub min_ram_gb: u32,
}

/// Rebuild the ladder: keep steps that fit free RAM, ordered ascending.
#[must_use]
pub fn ladder_live(catalog: &[LadderStep], free_ram_gb: u32) -> Vec<LadderStep> {
    let mut steps: Vec<_> = catalog
        .iter()
        .filter(|s| s.min_ram_gb <= free_ram_gb)
        .cloned()
        .collect();
    steps.sort_by_key(|s| s.min_ram_gb);
    steps
}

#[cfg(test)]
mod zc_ladder_live_tests {
    use super::*;

    #[test]
    fn zc_ladder_live_filters_by_hardware() {
        let cat = [
            LadderStep {
                model: "tiny".into(),
                min_ram_gb: 4,
            },
            LadderStep {
                model: "mid".into(),
                min_ram_gb: 16,
            },
            LadderStep {
                model: "huge".into(),
                min_ram_gb: 64,
            },
        ];
        let live = ladder_live(&cat, 24);
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].model, "tiny");
        assert_eq!(live[1].model, "mid");
    }
}

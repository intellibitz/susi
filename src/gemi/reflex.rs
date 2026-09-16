// SUSI-Alpha: Tier 0 Reflex Reasoning Engine
// 100% Rust implementation for Hyper-Optimized Protocol Routing (<10ms)

use std::path::Path;
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum ReflexDecision {
    Solved(String),
    RequiresDeepReasoning,
}

pub struct ReflexEngine;

impl ReflexEngine {
    pub fn scout_tier0_assets() -> Vec<crate::gawd::agents::DiscoverableAsset> {
        let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        cfg.discoverable_assets
            .into_iter()
            .map(|a| crate::gawd::agents::DiscoverableAsset {
                tier: a.tier,
                name: a.name,
                provider: a.provider,
                url: a.url,
            })
            .collect()
    }

    /// Attempts to solve the mission using the Tier 0 SusiPulse Bootstrap Brain.
    pub fn try_solve(intent: &str, workspace: &Path) -> (ReflexDecision, u128) {
        let start = Instant::now();

        match crate::gemi::pulse::SusiPulse::reason(intent, workspace) {
            Ok(action) => (ReflexDecision::Solved(action), start.elapsed().as_micros()),
            Err(_) => (
                ReflexDecision::RequiresDeepReasoning,
                start.elapsed().as_micros(),
            ),
        }
    }
}

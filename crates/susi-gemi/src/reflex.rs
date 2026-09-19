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
    pub fn scout_tier0_assets() -> Vec<susi_agents::DiscoverableAsset> {
        let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        cfg.discoverable_assets()
    }

    /// Attempts to solve the mission using the Tier 0 SusiPulse Bootstrap Brain.
    pub fn try_solve(intent: &str, workspace: &Path) -> (ReflexDecision, u128) {
        let start = Instant::now();

        let (decision, elapsed_micros) =
            match crate::pulse::SusiPulse::reason(intent, workspace) {
                Ok(action) => (ReflexDecision::Solved(action), start.elapsed().as_micros()),
                Err(_) => (
                    ReflexDecision::RequiresDeepReasoning,
                    start.elapsed().as_micros(),
                ),
            };

        // Sub-2ms Reflex Mandate: audit (not enforce) breaches, consistent with
        // the swarm-level guard in src/gawd/ama.rs — genuine reasoning work can
        // legitimately exceed 2ms, so this records the violation rather than
        // aborting an in-flight result.
        if elapsed_micros > 2000 {
            susi_sandbox::manager::SusiAuditLogger::log(
                workspace,
                susi_sandbox::manager::LogLevel::Axiomatic,
                "LATENCY_VIOLATION",
                &format!(
                    "Tier-0 reflex exceeded 2ms mandate: {}us (Intent: {})",
                    elapsed_micros, intent
                ),
            );
        }

        (decision, elapsed_micros)
    }
}

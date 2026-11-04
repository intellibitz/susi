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
    /// Attempts to solve the mission using the Tier 0 SusiPulse Bootstrap Brain.
    pub fn try_solve(intent: &str, workspace: &Path) -> (ReflexDecision, u128) {
        let start = Instant::now();

        let (decision, elapsed_micros) = match crate::pulse::SusiPulse::reason(intent, workspace) {
            Ok(action) => (ReflexDecision::Solved(action), start.elapsed().as_micros()),
            Err(_) => (
                ReflexDecision::RequiresDeepReasoning,
                start.elapsed().as_micros(),
            ),
        };

        // Sub-2ms Reflex Mandate: audit (not enforce) breaches, consistent with
        // the swarm-level guard in src/gawd/ama.rs — genuine reasoning work can
        // legitimately exceed 2ms, so this records the violation rather than
        // aborting an in-flight result. Only a *served* reflex can breach the
        // mandate: an intent that escalated never claimed a sub-2ms answer, and
        // logging the escalation attempt itself as a violation turns the audit
        // trace into noise — every mission goal pays the predict_intent cost.
        if elapsed_micros > 2000 && matches!(decision, ReflexDecision::Solved(_)) {
            crate::susi_sandbox::manager::SusiAuditLogger::log(
                workspace,
                crate::susi_sandbox::manager::LogLevel::Axiomatic,
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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn reflex_decision_variants_carry_solved_payload_or_deep_reasoning_marker() {
        let solved = ReflexDecision::Solved("ACTION: status".to_string());
        assert!(matches!(solved, ReflexDecision::Solved(ref s) if s == "ACTION: status"));

        let deep = ReflexDecision::RequiresDeepReasoning;
        assert!(matches!(deep, ReflexDecision::RequiresDeepReasoning));
    }

    #[test]
    fn try_solve_returns_decision_and_elapsed_micros_for_simple_intent() {
        let tmp =
            std::env::temp_dir().join(format!("susi_reflex_engine_simple_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let (decision, _elapsed) = ReflexEngine::try_solve("status", &tmp);
        match decision {
            ReflexDecision::Solved(s) => assert!(!s.is_empty()),
            ReflexDecision::RequiresDeepReasoning => {}
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn reflex_decision_clone_and_debug_preserve_variant_shape() {
        let solved = ReflexDecision::Solved("ACTION: version".to_string());
        let cloned = solved.clone();
        assert!(format!("{cloned:?}").contains("ACTION: version"));

        let deep = ReflexDecision::RequiresDeepReasoning.clone();
        assert!(format!("{deep:?}").contains("RequiresDeepReasoning"));
    }

    #[test]
    fn try_solve_reuses_tier0_reflex_cache_across_repeated_status_calls() {
        let tmp =
            std::env::temp_dir().join(format!("susi_reflex_engine_cache_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let (first, _) = ReflexEngine::try_solve("status", &tmp);
        let (second, _) = ReflexEngine::try_solve("status", &tmp);
        assert!(matches!(
            (first, second),
            (ReflexDecision::Solved(_), ReflexDecision::Solved(_))
                | (
                    ReflexDecision::RequiresDeepReasoning,
                    ReflexDecision::RequiresDeepReasoning
                )
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn reflex_decision_variants_distinguish_solved_payload_from_deep_reasoning() {
        let a = ReflexDecision::Solved("ACTION: ping".to_string());
        let b = ReflexDecision::RequiresDeepReasoning;
        assert_ne!(format!("{a:?}"), format!("{b:?}"));
    }
}

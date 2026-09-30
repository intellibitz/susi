//! Idle-engine autopull offer — when a local engine sits idle and the
//! recommended model for this host isn't present, surface a one-time pull
//! suggestion instead of silently downloading (T-CLAUDE-187).

use crate::quant_recommender::Recommendation;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EngineState {
    /// Engine process is up and not serving a request.
    Idle,
    /// Engine is serving — never queue a surprise pull underneath it.
    Busy,
    /// Engine not running; an autopull would still warm it for next launch.
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Autopull {
    /// Offer the pull; carries the suggested model and reason.
    Offer { model: String, reason: String },
    /// Already present or pull in progress.
    Noop { why: String },
    /// Do not even offer (engine busy, host constrained).
    Skip { why: String },
}

/// Decide whether to offer pulling `recommended` into the engine.
///
/// `present` — model ids the engine already has; `recommended` — the
/// recommender's pick (`quant` label + model id); `pulling` — a pull of this
/// model is already underway; `state` — engine liveness.
pub fn decide(
    state: EngineState,
    present: &[String],
    recommended_model: &str,
    rec: &Recommendation,
    pulling: bool,
) -> Autopull {
    if present.iter().any(|m| m == recommended_model) {
        return Autopull::Noop {
            why: format!("{recommended_model} already present"),
        };
    }
    if pulling {
        return Autopull::Noop {
            why: format!("pull of {recommended_model} already running"),
        };
    }
    if rec.target == "none" {
        return Autopull::Skip {
            why: format!("{recommended_model} cannot fit this host — {}", rec.why),
        };
    }
    match state {
        EngineState::Busy => Autopull::Skip { why: "engine is serving — defer the pull offer".into() },
        EngineState::Idle => Autopull::Offer {
            model: recommended_model.into(),
            reason: format!(
                "engine is idle and {recommended_model} ({}≈{:.1} GiB) is the best fit",
                rec.quant,
                rec.weight_bytes as f64 / (1 << 30) as f64
            ),
        },
        EngineState::Stopped => Autopull::Offer {
            model: recommended_model.into(),
            reason: format!(
                "engine is stopped; prefetching {recommended_model} ({}≈{:.1} GiB) warms next launch",
                rec.quant,
                rec.weight_bytes as f64 / (1 << 30) as f64
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(target: &'static str) -> Recommendation {
        Recommendation {
            quant: "Q4_K_M",
            weight_bytes: 4 * (1 << 30),
            kv_bytes: 1 << 30,
            target,
            why: "test".into(),
        }
    }

    #[test]
    fn zc_engine_autopull_offers_when_idle_and_missing() {
        match decide(
            EngineState::Idle,
            &[],
            "llama-3.1-8b-q4",
            &rec("vram"),
            false,
        ) {
            Autopull::Offer { model, .. } => assert_eq!(model, "llama-3.1-8b-q4"),
            other => panic!("expected offer, got {other:?}"),
        }
    }

    #[test]
    fn zc_engine_autopull_noop_when_present() {
        let present = vec!["llama-3.1-8b-q4".to_string()];
        match decide(
            EngineState::Idle,
            &present,
            "llama-3.1-8b-q4",
            &rec("vram"),
            false,
        ) {
            Autopull::Noop { why } => assert!(why.contains("already present")),
            other => panic!("expected noop, got {other:?}"),
        }
    }

    #[test]
    fn zc_engine_autopull_skips_when_busy() {
        match decide(EngineState::Busy, &[], "m", &rec("vram"), false) {
            Autopull::Skip { why } => assert!(why.contains("serving")),
            other => panic!("expected skip, got {other:?}"),
        }
    }

    #[test]
    fn zc_engine_autopull_skips_unfittable_model() {
        match decide(EngineState::Idle, &[], "huge-model", &rec("none"), false) {
            Autopull::Skip { why } => assert!(why.contains("cannot fit")),
            other => panic!("expected skip, got {other:?}"),
        }
    }

    #[test]
    fn zc_engine_autopull_stopped_still_offers_warmup() {
        match decide(EngineState::Stopped, &[], "m", &rec("ram"), false) {
            Autopull::Offer { reason, .. } => assert!(reason.contains("warm")),
            other => panic!("expected offer, got {other:?}"),
        }
    }

    #[test]
    fn zc_engine_autopull_noop_when_pull_running() {
        match decide(EngineState::Idle, &[], "m", &rec("vram"), true) {
            Autopull::Noop { why } => assert!(why.contains("already running")),
            other => panic!("expected noop, got {other:?}"),
        }
    }
}

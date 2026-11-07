//! Explain any routing decision — human-readable score trace plus 3D Pareto
//! frontier, Wilson 95% lower confidence bound, Bayesian UCB1 exploration
//! index, and Boltzmann-Shannon routing entropy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingExplanation {
    pub chosen: String,
    pub reasons: Vec<String>,
}

impl RoutingExplanation {
    /// Verify structural invariants on a routing explanation.
    pub fn verify_explanation_invariants(&self) -> Result<(), String> {
        if self.chosen.trim().is_empty() {
            return Err("RoutingExplanation chosen provider must not be empty".to_string());
        }
        if self.reasons.is_empty() {
            return Err("RoutingExplanation reasons must not be empty".to_string());
        }
        for reason in &self.reasons {
            if reason.trim().is_empty() {
                return Err("RoutingExplanation reason entry must not be empty".to_string());
            }
        }
        Ok(())
    }
}

/// Build a human-readable explanation for a routing choice.
#[must_use]
pub fn explain_route(chosen: &str, candidates: &[(String, f64)], why: &str) -> RoutingExplanation {
    let mut reasons = vec![why.to_string()];
    for (name, score) in candidates {
        reasons.push(format!("{name}: score={score:.3}"));
    }
    RoutingExplanation {
        chosen: chosen.into(),
        reasons,
    }
}

/// Algorithmic telemetry for one candidate in a masterpiece routing explanation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExplainCandidate {
    pub provider: String,
    pub score: f64,
    pub wilson_lcb_95: f64,
    pub ucb1_index: f64,
    pub effective_cost_usd: Option<f64>,
    pub avg_latency_ms: Option<u32>,
    pub is_pareto_optimal: bool,
    pub dominated_by: Vec<String>,
}

impl ExplainCandidate {
    /// Verify structural and mathematical invariants on a candidate explanation row.
    pub fn verify_candidate_invariants(&self) -> Result<(), String> {
        if self.provider.trim().is_empty() {
            return Err("ExplainCandidate provider must not be empty".to_string());
        }
        if !self.score.is_finite() || !(0.0..=1.0).contains(&self.score) {
            return Err(format!(
                "ExplainCandidate score ({}) must be finite and in 0.0..=1.0",
                self.score
            ));
        }
        if !self.wilson_lcb_95.is_finite() || !(0.0..=1.0).contains(&self.wilson_lcb_95) {
            return Err(format!(
                "ExplainCandidate wilson_lcb_95 ({}) must be finite and in 0.0..=1.0",
                self.wilson_lcb_95
            ));
        }
        if !self.ucb1_index.is_finite() || self.ucb1_index < 0.0 {
            return Err(format!(
                "ExplainCandidate ucb1_index ({}) must be finite and >= 0.0",
                self.ucb1_index
            ));
        }
        if let Some(cost) = self.effective_cost_usd {
            if !cost.is_finite() || cost < 0.0 {
                return Err(format!(
                    "ExplainCandidate effective_cost_usd ({cost}) must be finite and >= 0.0"
                ));
            }
        }
        if let Some(ms) = self.avg_latency_ms {
            if ms == 0 {
                return Err("ExplainCandidate avg_latency_ms must be > 0".to_string());
            }
        }
        if self.is_pareto_optimal && !self.dominated_by.is_empty() {
            return Err(
                "ExplainCandidate is_pareto_optimal cannot have non-empty dominated_by".to_string(),
            );
        }
        if !self.is_pareto_optimal && self.dominated_by.is_empty() {
            return Err(
                "ExplainCandidate non-Pareto candidate must list dominating providers".to_string(),
            );
        }
        for dom in &self.dominated_by {
            if dom.trim().is_empty() || dom == &self.provider {
                return Err(format!(
                    "ExplainCandidate invalid dominating provider '{dom}'"
                ));
            }
        }
        Ok(())
    }
}

/// Complete mathematical proof of a brain routing decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MasterpieceExplanation {
    pub task_class: String,
    pub budget: String,
    pub chosen: String,
    /// Providers lying on the non-dominated 3D Pareto frontier
    /// $(\text{reliability}\uparrow, \text{cost}\downarrow, \text{latency}\downarrow)$.
    pub pareto_frontier: Vec<String>,
    /// Candidate maximizing the Bayesian UCB1 exploration-exploitation index.
    pub ucb1_leader: Option<String>,
    /// Boltzmann-softmax Shannon entropy $H(P)$ in bits over candidate scores.
    pub shannon_entropy_bits: f64,
    pub candidates: Vec<ExplainCandidate>,
    pub reasons: Vec<String>,
}

impl MasterpieceExplanation {
    /// Verify structural invariants on a full routing explanation proof.
    pub fn verify_masterpiece_invariants(&self) -> Result<(), String> {
        if self.task_class.trim().is_empty() {
            return Err("MasterpieceExplanation task_class must not be empty".to_string());
        }
        if self.budget.trim().is_empty() {
            return Err("MasterpieceExplanation budget must not be empty".to_string());
        }
        if self.chosen.trim().is_empty() {
            return Err("MasterpieceExplanation chosen must not be empty".to_string());
        }
        if !self.shannon_entropy_bits.is_finite() || self.shannon_entropy_bits < 0.0 {
            return Err(format!(
                "MasterpieceExplanation shannon_entropy_bits ({}) must be finite and >= 0.0",
                self.shannon_entropy_bits
            ));
        }
        if self.candidates.is_empty() {
            return Err("MasterpieceExplanation candidates must not be empty".to_string());
        }
        for candidate in &self.candidates {
            candidate.verify_candidate_invariants()?;
        }
        if !self.candidates.iter().any(|c| c.provider == self.chosen) {
            return Err(format!(
                "MasterpieceExplanation chosen '{}' must exist in candidates",
                self.chosen
            ));
        }
        for p in &self.pareto_frontier {
            if p.trim().is_empty()
                || !self
                    .candidates
                    .iter()
                    .any(|c| &c.provider == p && c.is_pareto_optimal)
            {
                return Err(format!(
                    "MasterpieceExplanation pareto_frontier entry '{p}' must match a Pareto-optimal candidate"
                ));
            }
        }
        if let Some(leader) = &self.ucb1_leader {
            if leader.trim().is_empty() || !self.candidates.iter().any(|c| &c.provider == leader) {
                return Err(format!(
                    "MasterpieceExplanation ucb1_leader '{leader}' must exist in candidates"
                ));
            }
        }
        if self.reasons.is_empty() || self.reasons.iter().any(|r| r.trim().is_empty()) {
            return Err(
                "MasterpieceExplanation reasons must be non-empty and non-blank".to_string(),
            );
        }
        Ok(())
    }
}

/// Compute Boltzmann-softmax Shannon entropy in bits over candidate scores at
/// temperature $\tau = 0.15$.
#[must_use]
pub fn routing_entropy_bits(scores: &[f64]) -> f64 {
    if scores.len() <= 1 {
        return 0.0;
    }
    const TEMP: f64 = 0.15;
    let max_s = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = scores.iter().map(|&s| ((s - max_s) / TEMP).exp()).collect();
    let sum: f64 = exps.iter().sum();
    if sum <= 0.0 {
        return 0.0;
    }
    exps.into_iter()
        .map(|e| e / sum)
        .filter(|&p| p > 1e-12)
        .map(|p| -p * p.log2())
        .sum::<f64>()
        .max(0.0)
}

#[cfg(test)]
mod brain_explain_tests {
    use super::*;

    #[test]
    fn brain_explain_lists_scores() {
        let e = explain_route(
            "b",
            &[("a".into(), 0.4), ("b".into(), 0.9)],
            "highest success rate",
        );
        assert_eq!(e.chosen, "b");
        assert!(e.reasons.iter().any(|r| r.contains("0.900")));
    }

    #[test]
    fn routing_entropy_is_highest_on_ties_and_low_on_clear_winner() {
        let tie = routing_entropy_bits(&[0.75, 0.75, 0.75, 0.75]);
        let clear = routing_entropy_bits(&[0.95, 0.20, 0.15, 0.10]);
        assert!((tie - 2.0).abs() < 1e-6);
        assert!(clear < 0.35);
    }

    #[test]
    fn routing_entropy_bits_returns_zero_for_empty_and_singleton_slices() {
        assert_eq!(routing_entropy_bits(&[]), 0.0);
        assert_eq!(routing_entropy_bits(&[0.8]), 0.0);
    }

    #[test]
    fn explain_candidate_and_masterpiece_explanation_serde_json_roundtrip() {
        let candidate = ExplainCandidate {
            provider: "gemini-2.5-pro".into(),
            score: 0.94,
            wilson_lcb_95: 0.88,
            ucb1_index: 1.12,
            effective_cost_usd: Some(0.0025),
            avg_latency_ms: Some(420),
            is_pareto_optimal: true,
            dominated_by: vec![],
        };
        let explanation = MasterpieceExplanation {
            task_class: "code_refactor".into(),
            budget: "high".into(),
            chosen: "gemini-2.5-pro".into(),
            pareto_frontier: vec!["gemini-2.5-pro".into()],
            ucb1_leader: Some("gemini-2.5-pro".into()),
            shannon_entropy_bits: 0.25,
            candidates: vec![candidate],
            reasons: vec!["pareto optimal leader".into()],
        };
        let encoded = serde_json::to_string(&explanation).expect("serialize explanation");
        let decoded: MasterpieceExplanation =
            serde_json::from_str(&encoded).expect("deserialize explanation");
        assert_eq!(decoded, explanation);
    }

    #[test]
    fn explain_route_empty_candidates_and_routing_explanation_serde_roundtrip() {
        let explanation = explain_route("fallback", &[], "no candidate scores");
        assert_eq!(explanation.chosen, "fallback");
        assert_eq!(explanation.reasons, vec!["no candidate scores".to_string()]);
        let encoded = serde_json::to_string(&explanation).expect("serialize RoutingExplanation");
        let decoded: RoutingExplanation =
            serde_json::from_str(&encoded).expect("deserialize RoutingExplanation");
        assert_eq!(decoded, explanation);
    }

    #[test]
    fn verify_brain_wave13_invariants_brain_explain() {
        let route = explain_route("deepseek", &[("deepseek".into(), 0.92)], "best score");
        assert!(route.verify_explanation_invariants().is_ok());

        let blank_chosen = RoutingExplanation {
            chosen: "  ".into(),
            reasons: vec!["ok".into()],
        };
        assert!(blank_chosen.verify_explanation_invariants().is_err());

        let empty_reasons = RoutingExplanation {
            chosen: "deepseek".into(),
            reasons: vec![],
        };
        assert!(empty_reasons.verify_explanation_invariants().is_err());

        let valid_candidate = ExplainCandidate {
            provider: "gemini-2.5-pro".into(),
            score: 0.94,
            wilson_lcb_95: 0.88,
            ucb1_index: 1.12,
            effective_cost_usd: Some(0.0025),
            avg_latency_ms: Some(420),
            is_pareto_optimal: true,
            dominated_by: vec![],
        };
        assert!(valid_candidate.verify_candidate_invariants().is_ok());

        let bad_score = ExplainCandidate {
            score: 1.5,
            ..valid_candidate.clone()
        };
        assert!(bad_score.verify_candidate_invariants().is_err());

        let contradictory_pareto = ExplainCandidate {
            is_pareto_optimal: true,
            dominated_by: vec!["other".into()],
            ..valid_candidate.clone()
        };
        assert!(contradictory_pareto.verify_candidate_invariants().is_err());

        let valid_masterpiece = MasterpieceExplanation {
            task_class: "code_refactor".into(),
            budget: "high".into(),
            chosen: "gemini-2.5-pro".into(),
            pareto_frontier: vec!["gemini-2.5-pro".into()],
            ucb1_leader: Some("gemini-2.5-pro".into()),
            shannon_entropy_bits: 0.25,
            candidates: vec![valid_candidate],
            reasons: vec!["pareto optimal leader".into()],
        };
        assert!(valid_masterpiece.verify_masterpiece_invariants().is_ok());

        let missing_chosen = MasterpieceExplanation {
            chosen: "nonexistent".into(),
            ..valid_masterpiece.clone()
        };
        assert!(missing_chosen.verify_masterpiece_invariants().is_err());

        let neg_entropy = MasterpieceExplanation {
            shannon_entropy_bits: -0.1,
            ..valid_masterpiece
        };
        assert!(neg_entropy.verify_masterpiece_invariants().is_err());
    }
}

//! Explain any routing decision — human-readable score trace plus 3D Pareto
//! frontier, Wilson 95% lower confidence bound, Bayesian UCB1 exploration
//! index, and Boltzmann-Shannon routing entropy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingExplanation {
    pub chosen: String,
    pub reasons: Vec<String>,
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
}

//! Explain any routing decision.

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
}

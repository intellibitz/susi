//! Confidence-calibrated escalation via a cheap judge.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Escalation {
    pub escalate: bool,
    pub reason: String,
}

/// Escalate when the cheap judge confidence is below threshold.
#[must_use]
pub fn confidence_escalate(judge_confidence: f64, threshold: f64) -> Escalation {
    if judge_confidence < threshold {
        Escalation {
            escalate: true,
            reason: format!("confidence {judge_confidence:.2} < {threshold:.2}"),
        }
    } else {
        Escalation {
            escalate: false,
            reason: "confidence sufficient".into(),
        }
    }
}

#[cfg(test)]
mod confidence_escalation_tests {
    use super::*;

    #[test]
    fn confidence_escalation_triggers_below_threshold() {
        assert!(confidence_escalate(0.4, 0.7).escalate);
        assert!(!confidence_escalate(0.9, 0.7).escalate);
    }
}

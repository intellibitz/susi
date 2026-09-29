//! Separate candidate generation from judging (VC-201-005).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateArtifact {
    pub id: String,
    pub expected_results: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldOutSuite {
    pub inputs: Vec<String>,
    pub expected: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionGate {
    Pass,
    FailSelfAlteredExpectations,
    FailWriteAttempt,
}

/// Evaluator: held-out inputs; candidate write perms unavailable.
pub fn judge(
    candidate: &CandidateArtifact,
    suite: &HeldOutSuite,
    candidate_can_write: bool,
) -> PromotionGate {
    if candidate_can_write {
        return PromotionGate::FailWriteAttempt;
    }
    // Patch altering its own expected results cannot satisfy the gate.
    if !candidate.expected_results.is_subset(&suite.expected)
        && candidate
            .expected_results
            .difference(&suite.expected)
            .next()
            .is_some()
    {
        // Candidate invented expectations not in held-out suite.
        return PromotionGate::FailSelfAlteredExpectations;
    }
    if candidate.expected_results == suite.expected {
        // Matching by rewriting expectations is still a fail if they diverge from held-out truth.
    }
    let invented: BTreeSet<_> = candidate
        .expected_results
        .difference(&suite.expected)
        .cloned()
        .collect();
    if !invented.is_empty() {
        return PromotionGate::FailSelfAlteredExpectations;
    }
    PromotionGate::Pass
}

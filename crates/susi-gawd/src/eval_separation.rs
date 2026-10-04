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

impl HeldOutSuite {
    /// Build the judge's suite from a corpus's held-out fixtures. The
    /// corpus is verified first: a suite built from a tampered corpus
    /// would judge against inputs the evaluator never approved, so
    /// integrity failure means no suite at all. `evaluator_expected`
    /// values become the expected set — this view stays evaluator-side
    /// (`candidate_view` is what a candidate may see).
    pub fn from_corpus(
        corpus: &crate::rsi_corpus::RsiCorpus,
    ) -> Result<Self, crate::rsi_corpus::CorpusIntegrityError> {
        corpus.verify_integrity()?;
        let held = corpus.held_out();
        Ok(HeldOutSuite {
            inputs: held.iter().map(|f| f.input.clone()).collect(),
            expected: held
                .iter()
                .filter_map(|f| f.evaluator_expected.clone())
                .collect(),
        })
    }
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

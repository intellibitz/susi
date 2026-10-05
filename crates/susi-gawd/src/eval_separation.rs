//! Separate candidate generation from judging (VC-201-005).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateArtifact {
    pub id: String,
    /// What the candidate actually produced on the suite's inputs. The
    /// gate judges this, never `expected_results` — a candidate's own
    /// claim about what it expects is not evidence of what it did.
    pub actual_output: BTreeSet<String>,
    /// The candidate's own claim about what it expects to satisfy. Used
    /// only to catch a candidate inventing truth outside the held-out
    /// suite — it never substitutes for `actual_output` in the verdict.
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
    /// The candidate's actual output does not cover the held-out truth —
    /// including producing nothing at all.
    FailWrongOutput,
    FailWriteAttempt,
}

/// Evaluator: held-out inputs; candidate write perms unavailable.
///
/// Judges what the candidate actually produced, not what it claims: a
/// candidate inventing self-reported expectations outside the held-out
/// suite is caught first, and only a candidate whose `actual_output`
/// genuinely covers the suite's `expected` set — not an empty or partial
/// run — passes.
pub fn judge(
    candidate: &CandidateArtifact,
    suite: &HeldOutSuite,
    candidate_can_write: bool,
) -> PromotionGate {
    if candidate_can_write {
        return PromotionGate::FailWriteAttempt;
    }
    // Patch altering its own expected results cannot satisfy the gate.
    let invented: BTreeSet<_> = candidate
        .expected_results
        .difference(&suite.expected)
        .cloned()
        .collect();
    if !invented.is_empty() {
        return PromotionGate::FailSelfAlteredExpectations;
    }
    // The gate must judge what the candidate actually produced. A
    // candidate that produced nothing, or whose output does not cover
    // the held-out truth, fails — declaring a correct expectation is not
    // evidence of a correct result.
    if !suite.expected.is_subset(&candidate.actual_output) {
        return PromotionGate::FailWrongOutput;
    }
    PromotionGate::Pass
}

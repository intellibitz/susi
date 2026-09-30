//! Retrieval quality evaluation before promoting memory changes (VC-201-082).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalSuite {
    pub version: u32,
    pub answer_correctness: f64,
    pub private_isolation: f64,
    pub recall: f64,
    pub index_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromoteVerdict {
    Promote,
    RejectCorrectnessRegression,
    RejectIsolationRegression,
}

/// Compare candidate against baseline; larger index / higher recall alone
/// cannot hide answer-correctness or private-data isolation regressions.
#[must_use]
pub fn evaluate_promotion(baseline: &RetrievalSuite, candidate: &RetrievalSuite) -> PromoteVerdict {
    if candidate.answer_correctness < baseline.answer_correctness {
        return PromoteVerdict::RejectCorrectnessRegression;
    }
    if candidate.private_isolation < baseline.private_isolation {
        return PromoteVerdict::RejectIsolationRegression;
    }
    PromoteVerdict::Promote
}

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
    RejectStaleSuiteVersion,
    RejectUnmeasuredMetrics,
    RejectCorrectnessRegression,
    RejectIsolationRegression,
}

/// Compare candidate against baseline; version must be current, metrics must be valid/measured,
/// and larger index / higher recall alone cannot hide answer-correctness or private-data
/// isolation regressions.
#[must_use]
pub fn evaluate_promotion(baseline: &RetrievalSuite, candidate: &RetrievalSuite) -> PromoteVerdict {
    if candidate.version < baseline.version {
        return PromoteVerdict::RejectStaleSuiteVersion;
    }
    if candidate.answer_correctness.is_nan()
        || candidate.private_isolation.is_nan()
        || candidate.recall.is_nan()
    {
        return PromoteVerdict::RejectUnmeasuredMetrics;
    }
    if candidate.answer_correctness < baseline.answer_correctness {
        return PromoteVerdict::RejectCorrectnessRegression;
    }
    if candidate.private_isolation < baseline.private_isolation {
        return PromoteVerdict::RejectIsolationRegression;
    }
    PromoteVerdict::Promote
}

/// A versioned index candidate undergoing promotion evaluation.
pub struct MemoryIndexCandidate {
    pub suite: RetrievalSuite,
    pub index_data: Vec<u8>,
}

impl MemoryIndexCandidate {
    pub fn new(suite: RetrievalSuite, index_data: Vec<u8>) -> Self {
        Self { suite, index_data }
    }

    /// Gate promotion against a baseline retrieval suite.
    pub fn try_promote(&self, baseline: &RetrievalSuite) -> Result<(), PromoteVerdict> {
        let verdict = evaluate_promotion(baseline, &self.suite);
        if verdict == PromoteVerdict::Promote {
            Ok(())
        } else {
            Err(verdict)
        }
    }
}

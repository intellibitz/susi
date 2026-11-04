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

#[cfg(test)]
mod inline_tests {
    use super::*;

    fn baseline() -> RetrievalSuite {
        RetrievalSuite {
            version: 3,
            answer_correctness: 0.90,
            private_isolation: 1.00,
            recall: 0.85,
            index_size: 1024,
        }
    }

    #[test]
    fn evaluate_promotion_accepts_equal_or_improved_candidate() {
        let base = baseline();
        let candidate = RetrievalSuite {
            version: 4,
            answer_correctness: 0.92,
            private_isolation: 1.00,
            recall: 0.88,
            index_size: 2048,
        };
        assert_eq!(
            evaluate_promotion(&base, &candidate),
            PromoteVerdict::Promote
        );
    }

    #[test]
    fn evaluate_promotion_rejects_stale_version_and_nan_metrics() {
        let base = baseline();
        let stale = RetrievalSuite {
            version: 2,
            ..base.clone()
        };
        assert_eq!(
            evaluate_promotion(&base, &stale),
            PromoteVerdict::RejectStaleSuiteVersion
        );

        let unmeasured = RetrievalSuite {
            recall: f64::NAN,
            ..base.clone()
        };
        assert_eq!(
            evaluate_promotion(&base, &unmeasured),
            PromoteVerdict::RejectUnmeasuredMetrics
        );
    }

    #[test]
    fn evaluate_promotion_rejects_correctness_and_isolation_regressions() {
        let base = baseline();
        let worse_correctness = RetrievalSuite {
            answer_correctness: 0.89,
            recall: 0.99,
            ..base.clone()
        };
        assert_eq!(
            evaluate_promotion(&base, &worse_correctness),
            PromoteVerdict::RejectCorrectnessRegression
        );

        let worse_isolation = RetrievalSuite {
            private_isolation: 0.99,
            recall: 0.99,
            ..base
        };
        assert_eq!(
            evaluate_promotion(&baseline(), &worse_isolation),
            PromoteVerdict::RejectIsolationRegression
        );
    }

    #[test]
    fn memory_index_candidate_try_promote_gates_on_verdict() {
        let base = baseline();
        let good = MemoryIndexCandidate::new(base.clone(), vec![1, 2, 3]);
        assert_eq!(good.try_promote(&base), Ok(()));
        assert_eq!(good.index_data, vec![1, 2, 3]);

        let bad_suite = RetrievalSuite {
            version: 1,
            ..base.clone()
        };
        let bad = MemoryIndexCandidate::new(bad_suite, vec![]);
        assert_eq!(
            bad.try_promote(&base),
            Err(PromoteVerdict::RejectStaleSuiteVersion)
        );
    }

    #[test]
    fn evaluate_promotion_rejects_nan_answer_correctness_or_private_isolation() {
        let base = baseline();
        let nan_correctness = RetrievalSuite {
            answer_correctness: f64::NAN,
            ..base.clone()
        };
        assert_eq!(
            evaluate_promotion(&base, &nan_correctness),
            PromoteVerdict::RejectUnmeasuredMetrics
        );

        let nan_isolation = RetrievalSuite {
            private_isolation: f64::NAN,
            ..base
        };
        assert_eq!(
            evaluate_promotion(&baseline(), &nan_isolation),
            PromoteVerdict::RejectUnmeasuredMetrics
        );
    }
}

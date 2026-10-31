//! Mastery verification for VC-201-082: retrieval quality gating before
//! promoting memory changes.
//!
//! The cited tests show the three-way verdict on clean inputs. The
//! distinguishing properties are: the suite is *versioned* (a stale or
//! unversioned candidate must not promote), and an unmeasured metric must
//! not silently pass the gate — plus whether any production path actually
//! consults the verdict.

use crate::retrieval_eval::{evaluate_promotion, PromoteVerdict, RetrievalSuite};

fn suite(
    version: u32,
    correctness: f64,
    isolation: f64,
    recall: f64,
    index: u64,
) -> RetrievalSuite {
    RetrievalSuite {
        version,
        answer_correctness: correctness,
        private_isolation: isolation,
        recall,
        index_size: index,
    }
}

/// Mastery: `version` is enforced. A candidate suite older than the baseline
/// is rejected as a stale evaluation, even if raw metrics hold.
#[test]
fn vc_201_082_mastery_stale_suite_version_rejected() {
    let baseline = suite(5, 0.9, 1.0, 0.5, 100);
    let candidate = suite(1, 0.95, 1.0, 0.6, 150);
    assert_eq!(
        evaluate_promotion(&baseline, &candidate),
        PromoteVerdict::RejectStaleSuiteVersion,
        "a suite behind the baseline version must be rejected"
    );
}

/// Mastery: unmeasured metrics (NaN) are detected and rejected.
#[test]
fn vc_201_082_mastery_nan_metrics_rejected_as_unmeasured() {
    let baseline = suite(1, 0.9, 1.0, 0.5, 100);
    let candidate = suite(2, f64::NAN, 1.0, 0.6, 150);
    assert_eq!(
        evaluate_promotion(&baseline, &candidate),
        PromoteVerdict::RejectUnmeasuredMetrics,
        "NaN correctness must be rejected as unmeasured metrics"
    );
    let candidate_iso = suite(2, 0.95, f64::NAN, 0.6, 150);
    assert_eq!(
        evaluate_promotion(&baseline, &candidate_iso),
        PromoteVerdict::RejectUnmeasuredMetrics,
        "NaN isolation must be rejected as unmeasured metrics"
    );
}

/// Mastery: production promotion path is gated by evaluate_promotion.
#[test]
fn vc_201_082_mastery_promotion_path_is_gated() {
    use crate::retrieval_eval::MemoryIndexCandidate;

    let baseline = suite(2, 0.90, 1.0, 0.70, 500);

    // Stale candidate fails promotion
    let stale_cand = MemoryIndexCandidate::new(suite(1, 0.95, 1.0, 0.80, 600), vec![1, 2, 3]);
    assert_eq!(
        stale_cand.try_promote(&baseline),
        Err(PromoteVerdict::RejectStaleSuiteVersion)
    );

    // Regressed candidate fails promotion
    let regressed_cand = MemoryIndexCandidate::new(suite(3, 0.80, 1.0, 0.95, 1000), vec![4, 5, 6]);
    assert_eq!(
        regressed_cand.try_promote(&baseline),
        Err(PromoteVerdict::RejectCorrectnessRegression)
    );

    // Honest candidate promotes successfully
    let good_cand = MemoryIndexCandidate::new(suite(3, 0.92, 1.0, 0.75, 550), vec![7, 8, 9]);
    assert_eq!(good_cand.try_promote(&baseline), Ok(()));
}

/// What does hold: quality promotion, correctness-regression rejection
/// despite higher recall, and isolation-regression rejection — on honest
/// finite numbers.
#[test]
fn vc_201_082_mastery_honest_verdicts_hold() {
    let baseline = suite(1, 0.9, 1.0, 0.5, 100);
    assert_eq!(
        evaluate_promotion(&baseline, &suite(2, 0.91, 1.0, 0.4, 50)),
        PromoteVerdict::Promote
    );
    assert_eq!(
        evaluate_promotion(&baseline, &suite(2, 0.8, 1.0, 0.99, 10_000)),
        PromoteVerdict::RejectCorrectnessRegression
    );
    assert_eq!(
        evaluate_promotion(&baseline, &suite(2, 0.95, 0.5, 0.9, 200)),
        PromoteVerdict::RejectIsolationRegression
    );
}

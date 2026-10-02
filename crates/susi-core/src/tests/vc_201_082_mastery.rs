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

/// Falsification: `version` is dead weight. A candidate suite *older* than
/// the baseline — a stale evaluation — promotes as long as its numbers hold.
/// Nothing in evaluate_promotion compares versions, so "versioned suite"
/// is a field, not a gate.
#[test]
fn vc_201_082_mastery_stale_suite_version_promotes() {
    let baseline = suite(5, 0.9, 1.0, 0.5, 100);
    let candidate = suite(1, 0.95, 1.0, 0.6, 150);
    assert_eq!(
        evaluate_promotion(&baseline, &candidate),
        PromoteVerdict::Promote,
        "a suite four versions behind the baseline promoted"
    );
}

/// Falsification: an *unmeasured* metric passes the gate. NaN fails every
/// `<` comparison, so a candidate whose correctness was never computed
/// promotes — an empty evaluation reads as no regression.
#[test]
fn vc_201_082_mastery_nan_correctness_promotes_as_unmeasured() {
    let baseline = suite(1, 0.9, 1.0, 0.5, 100);
    let candidate = suite(2, f64::NAN, 1.0, 0.6, 150);
    assert_eq!(
        evaluate_promotion(&baseline, &candidate),
        PromoteVerdict::Promote,
        "NaN correctness bypasses the regression check"
    );
    let candidate_iso = suite(2, 0.95, f64::NAN, 0.6, 150);
    assert_eq!(
        evaluate_promotion(&baseline, &candidate_iso),
        PromoteVerdict::Promote,
        "NaN isolation bypasses the regression check"
    );
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

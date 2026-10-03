//! Mastery verification for VC-201-003: a measurable improvement scorecard
//! gating on separate dimensions with predeclared thresholds and reported
//! uncertainty.
//!
//! The cited tests show dimensions improve individually. The
//! distinguishing properties are that *all five* dimensions gate the
//! verdict (not a subset), and that an undersampled result cannot claim
//! an improvement.

use crate::scorecard::{DimensionScore, ImprovementScorecard};

fn dim(name: &str, value: f64, threshold: f64, uncertainty: f64) -> DimensionScore {
    DimensionScore {
        name: name.into(),
        value,
        threshold,
        uncertainty,
    }
}

/// Falsification: `summary_ok` consults only three of the five dimensions
/// — resource and operator_effort are never checked. An experiment that
/// doubles operator toil and halves resource efficiency still 'improves'.
#[test]
fn vc_201_003_mastery_regressed_dimensions_do_not_block_the_verdict() {
    let s = ImprovementScorecard {
        quality: dim("quality", 0.95, 0.8, 0.01),
        reliability: dim("reliability", 0.99, 0.9, 0.01),
        latency: dim("latency", 50.0, 100.0, 5.0),
        // Catastrophic regressions in the two un-gated dimensions:
        resource: dim("resource", 0.0, 0.9, 0.0),
        operator_effort: dim("operator_effort", 99.0, 5.0, 0.0),
        task_count: 100,
    };
    assert!(
        s.summary_ok(),
        "resource and operator_effort regressions are invisible to the verdict"
    );
}

/// Falsification: an undersampled result claims an improvement. With
/// task_count = 0 — literally no tasks run — a caller-supplied low
/// uncertainty makes summary_ok true. 'Undersampled cannot claim' has no
/// mechanism: sample size is never consulted anywhere.
#[test]
fn vc_201_003_mastery_zero_tasks_claims_improvement() {
    let s = ImprovementScorecard {
        quality: dim("quality", 0.95, 0.8, 0.01),
        reliability: dim("reliability", 0.99, 0.9, 0.01),
        latency: dim("latency", 50.0, 100.0, 5.0),
        resource: dim("resource", 1.0, 0.9, 0.01),
        operator_effort: dim("operator_effort", 1.0, 5.0, 0.1),
        task_count: 0,
    };
    assert!(
        s.summary_ok(),
        "a scorecard built on zero tasks declares improvement"
    );
}

/// Falsification: thresholds are carried on the same record as the value
/// they judge — 'predeclared' is unenforced. The same measurement flips
/// from failure to improvement by editing the threshold after the fact;
/// nothing distinguishes a predeclared bound from a post-hoc one.
#[test]
fn vc_201_003_mastery_thresholds_are_post_hoc_not_predeclared() {
    let s = ImprovementScorecard {
        quality: dim("quality", 0.82, 0.9, 0.01), // declared bound: miss
        reliability: dim("r", 0.99, 0.9, 0.01),
        latency: dim("l", 50.0, 100.0, 5.0),
        resource: dim("res", 1.0, 0.9, 0.0),
        operator_effort: dim("o", 1.0, 5.0, 0.0),
        task_count: 50,
    };
    assert!(
        !s.summary_ok(),
        "quality missed its threshold — correctly not improved"
    );
    // Now move the goalposts: identical measurement, lowered threshold.
    let mut moved = s.clone();
    moved.quality.threshold = 0.8;
    assert!(
        moved.summary_ok(),
        "editing the 'predeclared' threshold post-hoc flips the verdict"
    );
}

/// What does hold: uncertainty genuinely blocks a marginal claim on a
/// gated dimension, and refuses_task_count_as_percent is at least an
/// honest constant (it cannot say no because no API would pass one).
#[test]
fn vc_201_003_mastery_uncertainty_blocks_gated_dimensions() {
    let s = ImprovementScorecard {
        quality: dim("quality", 0.81, 0.8, 0.05), // band covers threshold
        reliability: dim("r", 0.99, 0.9, 0.01),
        latency: dim("l", 50.0, 100.0, 5.0),
        resource: dim("res", 1.0, 0.9, 0.0),
        operator_effort: dim("o", 1.0, 5.0, 0.0),
        task_count: 50,
    };
    assert!(!s.summary_ok(), "wide uncertainty blocks the quality claim");
}

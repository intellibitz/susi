use crate::scorecard::{
    DimensionScore, DimensionSpec, ImprovementBasis, ImprovementScorecard, ScorecardSpec,
};

fn dim(name: &str, value: f64, uncertainty: f64) -> DimensionScore {
    DimensionScore {
        name: name.into(),
        value,
        uncertainty,
    }
}

fn spec() -> ScorecardSpec {
    ScorecardSpec {
        quality: DimensionSpec {
            threshold: 0.8,
            lower_is_better: false,
        },
        reliability: DimensionSpec {
            threshold: 0.9,
            lower_is_better: false,
        },
        latency: DimensionSpec {
            threshold: 100.0,
            lower_is_better: true,
        },
        resource: DimensionSpec {
            threshold: 0.7,
            lower_is_better: false,
        },
        operator_effort: DimensionSpec {
            threshold: 5.0,
            lower_is_better: true,
        },
        min_task_count: 30,
    }
}

#[test]
fn vc_201_003_separate_dimensions_with_thresholds_and_uncertainty() {
    let spec = spec();
    let s = ImprovementScorecard {
        quality: dim("quality", 0.9, 0.02),
        reliability: dim("reliability", 0.99, 0.01),
        latency: dim("latency", 80.0, 5.0),
        resource: dim("resource", 0.95, 0.05),
        operator_effort: dim("operator_effort", 2.0, 0.5),
        task_count: 42,
    };
    assert!(s.dimension_improved(&s.quality, &spec.quality));
    assert!(s.dimension_improved(&s.latency, &spec.latency));
    assert!(s.summary_ok(&spec));
    assert_eq!(s.task_count, 42);
    assert!(s.refuses_task_count_as_percent(ImprovementBasis::TaskCount));
    assert!(!s.refuses_task_count_as_percent(ImprovementBasis::MeasuredDimension));
}

#[test]
fn vc_201_003_uncertainty_blocks_claim() {
    let spec = spec();
    let d = dim("quality", 0.81, 0.05);
    let s = ImprovementScorecard {
        quality: d.clone(),
        reliability: dim("r", 1.0, 0.0),
        latency: dim("l", 1.0, 0.0),
        resource: dim("res", 1.0, 0.0),
        operator_effort: dim("o", 1.0, 0.0),
        task_count: 50,
    };
    assert!(!s.dimension_improved(&d, &spec.quality));
    assert!(!s.summary_ok(&spec));
}

#[test]
fn vc_201_003_insufficient_task_count_blocks_summary_even_when_dimensions_pass() {
    let spec = spec();
    let s = ImprovementScorecard {
        quality: dim("quality", 0.95, 0.01),
        reliability: dim("reliability", 0.99, 0.01),
        latency: dim("latency", 60.0, 2.0),
        resource: dim("resource", 0.90, 0.02),
        operator_effort: dim("operator_effort", 1.5, 0.2),
        task_count: 15,
    };
    assert!(!s.summary_ok(&spec));
}

#[test]
fn vc_201_003_lower_is_better_dimension_fails_when_upper_bound_exceeds_threshold() {
    let spec = spec();
    let high_latency = dim("latency", 97.0, 5.0);
    let s = ImprovementScorecard {
        quality: dim("quality", 0.95, 0.01),
        reliability: dim("reliability", 0.99, 0.01),
        latency: high_latency.clone(),
        resource: dim("resource", 0.90, 0.02),
        operator_effort: dim("operator_effort", 1.5, 0.2),
        task_count: 45,
    };
    assert!(!s.dimension_improved(&high_latency, &spec.latency));
    assert!(!s.summary_ok(&spec));
}

#[test]
fn vc_201_003_exact_min_task_count_passes_when_all_dimensions_clear_thresholds() {
    let spec = spec();
    let s = ImprovementScorecard {
        quality: dim("quality", 0.85, 0.02),
        reliability: dim("reliability", 0.95, 0.02),
        latency: dim("latency", 90.0, 5.0),
        resource: dim("resource", 0.80, 0.05),
        operator_effort: dim("operator_effort", 3.0, 1.0),
        task_count: spec.min_task_count,
    };
    assert!(s.summary_ok(&spec));
}

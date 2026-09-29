use crate::scorecard::{DimensionScore, ImprovementScorecard};

fn dim(name: &str, value: f64, threshold: f64, uncertainty: f64) -> DimensionScore {
    DimensionScore {
        name: name.into(),
        value,
        threshold,
        uncertainty,
    }
}

#[test]
fn vc_201_003_separate_dimensions_with_thresholds_and_uncertainty() {
    let s = ImprovementScorecard {
        quality: dim("quality", 0.9, 0.8, 0.02),
        reliability: dim("reliability", 0.99, 0.95, 0.01),
        latency: dim("latency", 80.0, 100.0, 5.0),
        resource: dim("resource", 0.5, 0.7, 0.05),
        operator_effort: dim("operator_effort", 2.0, 5.0, 0.5),
        task_count: 42,
    };
    assert!(s.dimension_improved(&s.quality, false));
    assert!(s.dimension_improved(&s.latency, true));
    assert!(s.summary_ok());
    assert_eq!(s.task_count, 42);
    assert!(s.refuses_task_count_as_percent());
}

#[test]
fn vc_201_003_uncertainty_blocks_claim() {
    let d = dim("quality", 0.81, 0.8, 0.05);
    let s = ImprovementScorecard {
        quality: d.clone(),
        reliability: dim("r", 1.0, 0.5, 0.0),
        latency: dim("l", 1.0, 10.0, 0.0),
        resource: dim("res", 1.0, 2.0, 0.0),
        operator_effort: dim("o", 1.0, 2.0, 0.0),
        task_count: 1,
    };
    assert!(!s.dimension_improved(&d, false));
}

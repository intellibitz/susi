//! Measurable improvement scorecard (VC-201-003).
//!
//! The verdict is judged against a `ScorecardSpec`: thresholds,
//! per-dimension direction and the sample floor are predeclared there,
//! before the run. `ImprovementScorecard` carries only what was
//! measured — value, uncertainty, task_count — so the judged record
//! holds no goalpost a post-hoc edit could move.

use serde::{Deserialize, Serialize};

/// One measured dimension: what was observed, never what was hoped for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionScore {
    pub name: String,
    pub value: f64,
    pub uncertainty: f64,
}

/// Predeclared bound for one dimension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionSpec {
    pub threshold: f64,
    pub lower_is_better: bool,
}

/// Bounds and the sample floor, fixed before the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScorecardSpec {
    pub quality: DimensionSpec,
    pub reliability: DimensionSpec,
    pub latency: DimensionSpec,
    pub resource: DimensionSpec,
    pub operator_effort: DimensionSpec,
    pub min_task_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImprovementScorecard {
    pub quality: DimensionScore,
    pub reliability: DimensionScore,
    pub latency: DimensionScore,
    pub resource: DimensionScore,
    pub operator_effort: DimensionScore,
    pub task_count: u64,
}

/// What a reported "percent improvement" rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImprovementBasis {
    /// A gated dimension's measured value against its predeclared bound.
    MeasuredDimension,
    /// The number of tasks executed — workload, never a percent gain.
    TaskCount,
}

impl ImprovementScorecard {
    /// A dimension improves only when its whole uncertainty band clears
    /// the predeclared bound on the better side.
    #[must_use]
    pub fn dimension_improved(&self, d: &DimensionScore, spec: &DimensionSpec) -> bool {
        if d.uncertainty >= spec.threshold.abs() {
            return false; // band swallows the bound itself
        }
        if spec.lower_is_better {
            d.value + d.uncertainty < spec.threshold
        } else {
            d.value - d.uncertainty > spec.threshold
        }
    }

    /// Undersampled cannot claim: the run must reach the predeclared
    /// floor, and no spec may set that floor below one observed task.
    #[must_use]
    pub fn sufficiently_sampled(&self, spec: &ScorecardSpec) -> bool {
        self.task_count >= spec.min_task_count.max(1)
    }

    /// A percent-improvement claim resting on the task count is
    /// refused; one resting on a measured dimension is not refused on
    /// that ground. Running N tasks says nothing about how well they
    /// went.
    #[must_use]
    pub fn refuses_task_count_as_percent(&self, basis: ImprovementBasis) -> bool {
        matches!(basis, ImprovementBasis::TaskCount)
    }

    /// Improvement only when every dimension clears its predeclared
    /// bound and the run carried enough tasks to mean it.
    #[must_use]
    pub fn summary_ok(&self, spec: &ScorecardSpec) -> bool {
        self.sufficiently_sampled(spec)
            && self.dimension_improved(&self.quality, &spec.quality)
            && self.dimension_improved(&self.reliability, &spec.reliability)
            && self.dimension_improved(&self.latency, &spec.latency)
            && self.dimension_improved(&self.resource, &spec.resource)
            && self.dimension_improved(&self.operator_effort, &spec.operator_effort)
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    fn sample_spec() -> ScorecardSpec {
        ScorecardSpec {
            quality: DimensionSpec {
                threshold: 0.80,
                lower_is_better: false,
            },
            reliability: DimensionSpec {
                threshold: 0.90,
                lower_is_better: false,
            },
            latency: DimensionSpec {
                threshold: 100.0,
                lower_is_better: true,
            },
            resource: DimensionSpec {
                threshold: 50.0,
                lower_is_better: true,
            },
            operator_effort: DimensionSpec {
                threshold: 2.0,
                lower_is_better: true,
            },
            min_task_count: 10,
        }
    }

    fn passing_scorecard() -> ImprovementScorecard {
        ImprovementScorecard {
            quality: DimensionScore {
                name: "quality".to_string(),
                value: 0.90,
                uncertainty: 0.02,
            },
            reliability: DimensionScore {
                name: "reliability".to_string(),
                value: 0.95,
                uncertainty: 0.01,
            },
            latency: DimensionScore {
                name: "latency".to_string(),
                value: 80.0,
                uncertainty: 5.0,
            },
            resource: DimensionScore {
                name: "resource".to_string(),
                value: 40.0,
                uncertainty: 2.0,
            },
            operator_effort: DimensionScore {
                name: "operator_effort".to_string(),
                value: 1.0,
                uncertainty: 0.2,
            },
            task_count: 12,
        }
    }

    #[test]
    fn dimension_improved_respects_uncertainty_band_and_direction() {
        let card = passing_scorecard();
        let spec = sample_spec();
        assert!(card.dimension_improved(&card.quality, &spec.quality));
        assert!(card.dimension_improved(&card.latency, &spec.latency));

        let wide_band = DimensionScore {
            name: "quality".to_string(),
            value: 0.95,
            uncertainty: 0.85,
        };
        assert!(!card.dimension_improved(&wide_band, &spec.quality));
    }

    #[test]
    fn sufficiently_sampled_enforces_min_task_count_and_floor_of_one() {
        let mut card = passing_scorecard();
        let mut spec = sample_spec();
        assert!(card.sufficiently_sampled(&spec));
        card.task_count = 9;
        assert!(!card.sufficiently_sampled(&spec));

        spec.min_task_count = 0;
        card.task_count = 0;
        assert!(!card.sufficiently_sampled(&spec));
    }

    #[test]
    fn refuses_task_count_as_percent_distinguishes_workload_from_dimension() {
        let card = passing_scorecard();
        assert!(card.refuses_task_count_as_percent(ImprovementBasis::TaskCount));
        assert!(!card.refuses_task_count_as_percent(ImprovementBasis::MeasuredDimension));
    }

    #[test]
    fn summary_ok_requires_all_five_dimensions_and_sampling_floor() {
        let mut card = passing_scorecard();
        let spec = sample_spec();
        assert!(card.summary_ok(&spec));
        card.latency.value = 98.0;
        assert!(!card.summary_ok(&spec));
    }
}

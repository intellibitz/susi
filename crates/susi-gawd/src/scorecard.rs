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

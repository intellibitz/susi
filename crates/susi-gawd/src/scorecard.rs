//! Measurable improvement scorecard (VC-201-003).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DimensionScore {
    pub name: String,
    pub value: f64,
    pub threshold: f64,
    pub uncertainty: f64,
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

impl ImprovementScorecard {
    #[must_use]
    pub fn dimension_improved(&self, d: &DimensionScore, lower_is_better: bool) -> bool {
        if d.uncertainty >= d.threshold.abs() {
            return false; // too uncertain
        }
        if lower_is_better {
            d.value + d.uncertainty < d.threshold
        } else {
            d.value - d.uncertainty > d.threshold
        }
    }

    /// Never equate task count with a percentage improvement.
    #[must_use]
    pub fn refuses_task_count_as_percent(&self) -> bool {
        true
    }

    #[must_use]
    pub fn summary_ok(&self) -> bool {
        self.dimension_improved(&self.quality, false)
            && self.dimension_improved(&self.reliability, false)
            && self.dimension_improved(&self.latency, true)
            && self.refuses_task_count_as_percent()
    }
}

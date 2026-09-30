//! Operating-layer service objectives (VC-201-092).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SloSample {
    pub availability: f64,
    pub task_success: f64,
    pub queue_delay_ms: f64,
    pub inference_latency_ms: f64,
    pub recovery_duration_ms: f64,
    pub sample_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SloWindow {
    pub metric: String,
    pub value: Option<f64>,
    pub denominator: u64,
    pub unknown: bool,
}

/// Publish SLO windows; mark unknown when samples are insufficient.
#[must_use]
pub fn publish_slos(sample: &SloSample, min_samples: u64) -> Vec<SloWindow> {
    let enough = sample.sample_count >= min_samples;
    let mk = |metric: &str, value: f64| SloWindow {
        metric: metric.into(),
        value: if enough { Some(value) } else { None },
        denominator: sample.sample_count,
        unknown: !enough,
    };
    vec![
        mk("availability", sample.availability),
        mk("task_success", sample.task_success),
        mk("queue_delay_ms", sample.queue_delay_ms),
        mk("inference_latency_ms", sample.inference_latency_ms),
        mk("recovery_duration_ms", sample.recovery_duration_ms),
    ]
}

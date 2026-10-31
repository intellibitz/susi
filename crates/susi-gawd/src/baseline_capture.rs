//! Reproducible released baseline capture (VC-201-002).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineRecord {
    pub commit: String,
    pub hardware: String,
    pub runtime: String,
    pub dataset: String,
    pub success_rate: f64,
    pub latency_ms: u64,
    pub tokens: u64,
    pub cost_micros: u64,
    pub command: String,
    pub is_release_reference: bool,
}

impl BaselineRecord {
    #[must_use]
    pub fn release_reference(
        commit: impl Into<String>,
        hardware: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Self {
            commit: commit.into(),
            hardware: hardware.into(),
            runtime: "release".into(),
            dataset: "default".into(),
            success_rate: 0.0,
            latency_ms: 0,
            tokens: 0,
            cost_micros: 0,
            command: command.into(),
            is_release_reference: true,
        }
    }

    #[must_use]
    pub fn candidate_on_dev(
        commit: impl Into<String>,
        hardware: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Self {
            commit: commit.into(),
            hardware: hardware.into(),
            runtime: "dev-instance".into(),
            dataset: "default".into(),
            success_rate: 0.0,
            latency_ms: 0,
            tokens: 0,
            cost_micros: 0,
            command: command.into(),
            is_release_reference: false,
        }
    }

    pub fn with_metrics(
        mut self,
        success_rate: f64,
        latency_ms: u64,
        tokens: u64,
        cost_micros: u64,
    ) -> Self {
        self.success_rate = success_rate;
        self.latency_ms = latency_ms;
        self.tokens = tokens;
        self.cost_micros = cost_micros;
        self
    }

    #[must_use]
    pub fn is_comparable_to(&self, other: &Self) -> bool {
        self.dataset == other.dataset
            && self.hardware == other.hardware
            && !self.command.is_empty()
            && !other.command.is_empty()
    }
}

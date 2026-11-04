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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn release_reference_sets_release_runtime_default_dataset_and_reference_flag() {
        let rec = BaselineRecord::release_reference("abc1234", "x86_64-win", "susi bench");
        assert_eq!(rec.commit, "abc1234");
        assert_eq!(rec.hardware, "x86_64-win");
        assert_eq!(rec.runtime, "release");
        assert_eq!(rec.dataset, "default");
        assert_eq!(rec.command, "susi bench");
        assert!(rec.is_release_reference);
    }

    #[test]
    fn candidate_on_dev_sets_dev_instance_runtime_and_non_reference_flag() {
        let rec = BaselineRecord::candidate_on_dev("def5678", "x86_64-win", "susi bench");
        assert_eq!(rec.commit, "def5678");
        assert_eq!(rec.runtime, "dev-instance");
        assert!(!rec.is_release_reference);
        assert_eq!(rec.latency_ms, 0);
    }

    #[test]
    fn with_metrics_populates_success_rate_latency_tokens_and_cost() {
        let rec = BaselineRecord::release_reference("c1", "hw1", "cmd")
            .with_metrics(0.98, 120, 4096, 850);
        assert!((rec.success_rate - 0.98).abs() < 1e-9);
        assert_eq!(rec.latency_ms, 120);
        assert_eq!(rec.tokens, 4096);
        assert_eq!(rec.cost_micros, 850);
    }

    #[test]
    fn is_comparable_to_requires_matching_dataset_hardware_and_non_empty_commands() {
        let base = BaselineRecord::release_reference("c1", "hw1", "susi bench");
        let cand = BaselineRecord::candidate_on_dev("c2", "hw1", "susi bench --fast");
        assert!(base.is_comparable_to(&cand));

        let diff_hw = BaselineRecord::candidate_on_dev("c2", "hw2", "susi bench");
        assert!(!base.is_comparable_to(&diff_hw));

        let empty_cmd = BaselineRecord::candidate_on_dev("c2", "hw1", "");
        assert!(!base.is_comparable_to(&empty_cmd));
    }
}

//! The measured capability matrix (VC-202-022 / T-DEEPSEEK-125).
//!
//! The question is never which model is best, but which model is best at
//! *this*. Ranking already consults per-(provider, class) evidence rather
//! than a single global score — [`crate::engines::brain::Store::rank`]
//! keys every record by `(provider, class)`. This module makes that matrix
//! a readable artifact: one row per provider per task class carrying the
//! measured success rate, EMA latency and cost per verified outcome.
//! `susi brain matrix` renders it for a human, and the scheduled scout
//! refreshes it every run (persisted next to the scout journal), so the
//! matrix is refreshed by scouting, not remembered.

use crate::engines::brain::{self, Ranked, TaskClass};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Every measured dimension of one provider at one task class — the row
/// the ranking itself is computed from, so what you read is what routed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatrixCell {
    pub provider: String,
    /// Per-class capability tier (`coding`/`reasoning`/`basic`) and
    /// whether it meets the class floor — floor before price (VC-202-004).
    pub capability: String,
    pub meets_floor: bool,
    /// Failing right now with poor evidence — never leads (Mandate 56).
    pub unfit: bool,
    /// Evidence samples behind the rates below.
    pub samples: u32,
    /// Verified success fraction at this class, `None` untried.
    pub success_rate: Option<f32>,
    /// EMA latency of successful calls at this class, `None` unmeasured.
    pub avg_latency_ms: Option<u32>,
    /// Expected USD for one whole task at this class (VC-202-002).
    pub expected_cost_usd: Option<f64>,
    /// Price of one verified useful outcome at this class — the currency
    /// the ranking trades in (VC-202-003). `None` when unpriced.
    pub cost_per_outcome_usd: Option<f64>,
    /// Kind and streak of the most recent failure in the fitness window.
    pub last_failure: Option<(brain::FailureKind, u32)>,
}

impl MatrixCell {
    fn of(r: Ranked) -> Self {
        Self {
            provider: r.provider,
            capability: r.capability.to_string(),
            meets_floor: r.meets_floor,
            unfit: r.unfit,
            samples: r.samples,
            success_rate: r.success_rate,
            avg_latency_ms: r.avg_latency_ms,
            expected_cost_usd: r.expected_cost_usd,
            cost_per_outcome_usd: r.cost_per_outcome_usd,
            last_failure: r.last_failure,
        }
    }

    /// Verify structural invariants on a single capability matrix cell.
    pub fn verify_cell_invariants(&self) -> Result<(), String> {
        if self.provider.trim().is_empty() {
            return Err("MatrixCell provider must not be empty".to_string());
        }
        if self.capability.trim().is_empty() {
            return Err("MatrixCell capability must not be empty".to_string());
        }
        if let Some(rate) = self.success_rate {
            if !rate.is_finite() || !(0.0..=1.0).contains(&rate) {
                return Err(format!(
                    "MatrixCell success_rate ({rate}) must be finite and in 0.0..=1.0"
                ));
            }
        }
        if let Some(ms) = self.avg_latency_ms {
            if ms == 0 {
                return Err("MatrixCell avg_latency_ms must be > 0".to_string());
            }
        }
        if let Some(cost) = self.expected_cost_usd {
            if !cost.is_finite() || cost < 0.0 {
                return Err(format!(
                    "MatrixCell expected_cost_usd ({cost}) must be finite and >= 0.0"
                ));
            }
        }
        if let Some(cost) = self.cost_per_outcome_usd {
            if !cost.is_finite() || cost < 0.0 {
                return Err(format!(
                    "MatrixCell cost_per_outcome_usd ({cost}) must be finite and >= 0.0"
                ));
            }
        }
        if let Some((_, streak)) = self.last_failure {
            if streak == 0 {
                return Err("MatrixCell last_failure streak must be > 0".to_string());
            }
        }
        Ok(())
    }
}

/// The full matrix: task-class label → provider → cell, in rank order per
/// class (the order the cascade would consult).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityMatrix {
    pub unix: u64,
    pub classes: BTreeMap<String, Vec<MatrixCell>>,
}

impl CapabilityMatrix {
    /// Verify structural invariants on the full capability matrix.
    pub fn verify_matrix_invariants(&self) -> Result<(), String> {
        if self.unix == 0 {
            return Err("CapabilityMatrix unix timestamp must be > 0".to_string());
        }
        for (class_label, cells) in &self.classes {
            if class_label.trim().is_empty() {
                return Err("CapabilityMatrix class label must not be empty".to_string());
            }
            let mut seen_providers = HashSet::new();
            for cell in cells {
                cell.verify_cell_invariants()?;
                if !seen_providers.insert(cell.provider.as_str()) {
                    return Err(format!(
                        "CapabilityMatrix duplicate provider '{}' in class '{class_label}'",
                        cell.provider
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Build the matrix for `providers` from the live evidence store — the
/// same `rank` calls the cascade consumes, so the matrix cannot disagree
/// with routing.
#[must_use]
pub fn build_in(store: &brain::Store, providers: &[String], unix: u64) -> CapabilityMatrix {
    let names = providers.to_vec();
    let classes = TaskClass::ALL
        .iter()
        .map(|class| {
            (
                class.label().to_string(),
                store
                    .rank(&names, *class)
                    .into_iter()
                    .map(MatrixCell::of)
                    .collect(),
            )
        })
        .collect();
    CapabilityMatrix { unix, classes }
}

/// The matrix over every provider that has evidence — the human surface
/// (`susi brain matrix`) and the file the scout refreshes.
#[must_use]
pub fn build() -> CapabilityMatrix {
    let store = brain::load();
    let providers = store.providers();
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    build_in(&store, &providers, unix)
}

/// Where the scout refreshes the readable matrix each run.
#[must_use]
pub fn default_path() -> std::path::PathBuf {
    susi_paths::SusiDirs::config_dir().join("brain_capability_matrix.json")
}

/// Persist the matrix so a human (or the next run) reads what the ranking
/// sees. Atomic write — a torn matrix is worse than a stale one.
pub fn persist(path: &Path, matrix: &CapabilityMatrix) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(matrix)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::susi_config::atomic_write_bytes(path, &bytes)
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn build_in_empty_providers_populates_all_task_classes() {
        let store = brain::Store::default();
        let matrix = build_in(&store, &[], 1_700_000_000);
        assert_eq!(matrix.unix, 1_700_000_000);
        assert_eq!(matrix.classes.len(), TaskClass::ALL.len());
        for class in TaskClass::ALL {
            let cells = matrix.classes.get(class.label()).unwrap();
            assert!(cells.is_empty());
        }
    }

    #[test]
    fn build_in_populated_providers_emits_ranked_cells_per_class() {
        let store = brain::Store::default();
        let providers = vec!["deepseek".to_string(), "ollama".to_string()];
        let matrix = build_in(&store, &providers, 42);
        for class in TaskClass::ALL {
            let cells = matrix.classes.get(class.label()).unwrap();
            assert_eq!(cells.len(), 2);
            let names: Vec<&str> = cells.iter().map(|c| c.provider.as_str()).collect();
            assert!(names.contains(&"deepseek"));
            assert!(names.contains(&"ollama"));
        }
    }

    #[test]
    fn persist_writes_valid_json_that_roundtrips_matrix() {
        let store = brain::Store::default();
        let providers = vec!["deepseek".to_string()];
        let matrix = build_in(&store, &providers, 123_456);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("matrix.json");
        persist(&path, &matrix).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let decoded: CapabilityMatrix = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, matrix);
    }

    #[test]
    fn default_path_points_to_brain_capability_matrix_json() {
        let path = default_path();
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("brain_capability_matrix.json")
        );
    }

    #[test]
    fn build_returns_matrix_covering_all_canonical_task_classes() {
        let matrix = build();
        assert_eq!(matrix.classes.len(), TaskClass::ALL.len());
        let cell = MatrixCell {
            provider: "deepseek".into(),
            capability: "coding".into(),
            meets_floor: true,
            unfit: false,
            samples: 18,
            success_rate: Some(0.95),
            avg_latency_ms: Some(350),
            expected_cost_usd: Some(0.0012),
            cost_per_outcome_usd: Some(0.0013),
            last_failure: None,
        };
        let encoded = serde_json::to_string(&cell).unwrap();
        let decoded: MatrixCell = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, cell);
    }

    #[test]
    fn verify_cell_and_matrix_invariants_reject_corrupt_states() {
        let valid_cell = MatrixCell {
            provider: "deepseek".into(),
            capability: "coding".into(),
            meets_floor: true,
            unfit: false,
            samples: 18,
            success_rate: Some(0.95),
            avg_latency_ms: Some(350),
            expected_cost_usd: Some(0.0012),
            cost_per_outcome_usd: Some(0.0013),
            last_failure: Some((brain::FailureKind::RateLimit, 2)),
        };
        assert!(valid_cell.verify_cell_invariants().is_ok());

        let mut blank_provider = valid_cell.clone();
        blank_provider.provider = "  ".into();
        assert!(blank_provider.verify_cell_invariants().is_err());

        let mut blank_capability = valid_cell.clone();
        blank_capability.capability = String::new();
        assert!(blank_capability.verify_cell_invariants().is_err());

        let mut bad_rate = valid_cell.clone();
        bad_rate.success_rate = Some(1.05);
        assert!(bad_rate.verify_cell_invariants().is_err());

        let mut zero_latency = valid_cell.clone();
        zero_latency.avg_latency_ms = Some(0);
        assert!(zero_latency.verify_cell_invariants().is_err());

        let mut neg_expected = valid_cell.clone();
        neg_expected.expected_cost_usd = Some(-0.001);
        assert!(neg_expected.verify_cell_invariants().is_err());

        let mut nan_outcome = valid_cell.clone();
        nan_outcome.cost_per_outcome_usd = Some(f64::NAN);
        assert!(nan_outcome.verify_cell_invariants().is_err());

        let mut zero_streak = valid_cell.clone();
        zero_streak.last_failure = Some((brain::FailureKind::RateLimit, 0));
        assert!(zero_streak.verify_cell_invariants().is_err());

        let store = brain::Store::default();
        let providers = vec!["deepseek".to_string(), "ollama".to_string()];
        let valid_matrix = build_in(&store, &providers, 1_700_000_000);
        assert!(valid_matrix.verify_matrix_invariants().is_ok());

        let mut zero_unix = valid_matrix.clone();
        zero_unix.unix = 0;
        assert!(zero_unix.verify_matrix_invariants().is_err());

        let mut blank_class = valid_matrix.clone();
        blank_class.classes.insert("   ".to_string(), Vec::new());
        assert!(blank_class.verify_matrix_invariants().is_err());

        let mut duplicate_provider = valid_matrix.clone();
        duplicate_provider
            .classes
            .insert("code".to_string(), vec![valid_cell.clone(), valid_cell]);
        assert!(duplicate_provider.verify_matrix_invariants().is_err());
    }
}

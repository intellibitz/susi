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
use std::collections::BTreeMap;
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
}

/// The full matrix: task-class label → provider → cell, in rank order per
/// class (the order the cascade would consult).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityMatrix {
    pub unix: u64,
    pub classes: BTreeMap<String, Vec<MatrixCell>>,
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
}

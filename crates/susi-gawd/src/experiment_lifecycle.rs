//! Persist self-improvement experiment lifecycle (VC-201-011).

use crate::scorecard::{ImprovementScorecard, ScorecardSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentState {
    Proposed,
    Isolated,
    Evaluated,
    Rejected,
    PromotionReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Experiment {
    pub id: String,
    pub state: ExperimentState,
}

/// One experiment's durable record: its current state plus the
/// transitions already applied to it, so a reload can rebuild both the
/// state map and the duplicate-transition guard.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExperimentRecord {
    experiment: Experiment,
    applied_transitions: Vec<(ExperimentState, ExperimentState)>,
}

#[derive(Debug, Default)]
pub struct ExperimentLog {
    pub experiments: BTreeMap<String, Experiment>,
    pub applied_transitions: Vec<(String, ExperimentState, ExperimentState)>,
    /// Durable store root; `None` is a pure in-memory log (tests).
    dir: Option<PathBuf>,
}

/// A filename-safe stand-in for an experiment id: only `[A-Za-z0-9_-]`
/// survives, so a crafted id cannot escape `dir` or collide with another
/// experiment's file by punctuation alone.
fn safe_file_stem(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        "_".to_string()
    } else {
        safe
    }
}

impl ExperimentLog {
    /// Load a durable log from `dir` — one JSON file per experiment,
    /// named after its id. A fresh process that loads the same `dir`
    /// resumes with every experiment and applied transition intact, so
    /// kill-and-resume survives the process rather than just the struct.
    #[must_use]
    pub fn load(dir: PathBuf) -> Self {
        let mut log = Self {
            dir: Some(dir.clone()),
            ..Default::default()
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return log;
        };
        for entry in entries.flatten() {
            if !entry.path().extension().is_some_and(|x| x == "json") {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<ExperimentRecord>(&body) else {
                continue;
            };
            let id = record.experiment.id.clone();
            log.experiments.insert(id.clone(), record.experiment);
            log.applied_transitions.extend(
                record
                    .applied_transitions
                    .into_iter()
                    .map(|(from, to)| (id.clone(), from, to)),
            );
        }
        log
    }

    fn persist(&self, id: &str) {
        let Some(dir) = &self.dir else {
            return;
        };
        let Some(experiment) = self.experiments.get(id) else {
            return;
        };
        let applied_transitions = self
            .applied_transitions
            .iter()
            .filter(|(i, _, _)| i == id)
            .map(|(_, from, to)| (*from, *to))
            .collect();
        let record = ExperimentRecord {
            experiment: experiment.clone(),
            applied_transitions,
        };
        let _ = std::fs::create_dir_all(dir);
        if let Ok(body) = serde_json::to_string_pretty(&record) {
            let _ = std::fs::write(dir.join(format!("{}.json", safe_file_stem(id))), body);
        }
    }

    /// Register a new experiment. Refuses to silently reset one already
    /// mid-flight — re-proposing a live id while its applied-transition
    /// history survives is exactly what wedged it unrecoverably before.
    /// Call this only for an id that has never been proposed.
    pub fn propose(&mut self, id: &str) -> Result<(), String> {
        if self.experiments.contains_key(id) {
            return Err(format!(
                "experiment {id} is already proposed; re-proposing would reset its state while its transition history survives"
            ));
        }
        self.experiments.insert(
            id.to_string(),
            Experiment {
                id: id.to_string(),
                state: ExperimentState::Proposed,
            },
        );
        self.persist(id);
        Ok(())
    }

    pub fn transition(&mut self, id: &str, to: ExperimentState) -> Result<(), String> {
        let exp = self
            .experiments
            .get_mut(id)
            .ok_or_else(|| "unknown".to_string())?;
        let from = exp.state;
        let ok = matches!(
            (from, to),
            (ExperimentState::Proposed, ExperimentState::Isolated)
                | (ExperimentState::Isolated, ExperimentState::Evaluated)
                // A failed isolation leg must be concludable, not wedged
                // mid-flight forever.
                | (ExperimentState::Isolated, ExperimentState::Rejected)
                | (ExperimentState::Evaluated, ExperimentState::Rejected)
                | (ExperimentState::Evaluated, ExperimentState::PromotionReady)
        );
        if !ok {
            return Err(format!("illegal {from:?} -> {to:?}"));
        }
        // Kill-and-resume: refuse duplicate applied transition.
        if self
            .applied_transitions
            .iter()
            .any(|(i, f, t)| i == id && *f == from && *t == to)
        {
            return Err("duplicate transition".into());
        }
        exp.state = to;
        self.applied_transitions.push((id.to_string(), from, to));
        self.persist(id);
        Ok(())
    }

    /// Promotion is earned, not declared: the Evaluated ->
    /// PromotionReady transition goes through only when the
    /// experiment's scorecard clears the predeclared spec — every
    /// dimension within its bound and the sample floor reached.
    pub fn promote_with_scorecard(
        &mut self,
        id: &str,
        scorecard: &ImprovementScorecard,
        spec: &ScorecardSpec,
    ) -> Result<(), String> {
        if !scorecard.summary_ok(spec) {
            return Err("scorecard fails the predeclared spec".into());
        }
        self.transition(id, ExperimentState::PromotionReady)
    }

    /// After crash: resume from durable log without skipping evaluation.
    pub fn resume_requires_evaluation(&self, id: &str) -> bool {
        matches!(
            self.experiments.get(id).map(|e| e.state),
            Some(ExperimentState::Isolated)
        )
    }
}

//! Persist self-improvement experiment lifecycle (VC-201-011).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

#[derive(Debug, Default)]
pub struct ExperimentLog {
    pub experiments: BTreeMap<String, Experiment>,
    pub applied_transitions: Vec<(String, ExperimentState, ExperimentState)>,
}

impl ExperimentLog {
    pub fn propose(&mut self, id: &str) {
        self.experiments.insert(
            id.to_string(),
            Experiment {
                id: id.to_string(),
                state: ExperimentState::Proposed,
            },
        );
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
        Ok(())
    }

    /// After crash: resume from durable log without skipping evaluation.
    pub fn resume_requires_evaluation(&self, id: &str) -> bool {
        matches!(
            self.experiments.get(id).map(|e| e.state),
            Some(ExperimentState::Isolated)
        )
    }
}

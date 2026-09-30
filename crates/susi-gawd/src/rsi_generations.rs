//! Multi-generation recursive improvement (VC-201-020).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Generation {
    pub index: u32,
    pub delta: f64,
    pub budget_remaining: f64,
    pub rejected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MultiGenResult {
    Continue,
    StopNonImprovement,
    StopRegression,
    StopBudget,
}

/// Require at least three generations; stop on non-improvement or regression.
#[must_use]
pub fn next_action(history: &[Generation], min_gens: u32) -> MultiGenResult {
    if history.last().is_some_and(|g| g.budget_remaining <= 0.0) {
        return MultiGenResult::StopBudget;
    }
    if history.len() < min_gens as usize {
        return MultiGenResult::Continue;
    }
    let last = history.last().unwrap();
    if last.rejected || last.delta < 0.0 {
        return MultiGenResult::StopRegression;
    }
    if last.delta == 0.0 {
        return MultiGenResult::StopNonImprovement;
    }
    MultiGenResult::Continue
}

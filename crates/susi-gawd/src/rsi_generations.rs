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
    let Some(last) = history.last() else {
        return MultiGenResult::Continue;
    };
    if last.rejected || last.delta < 0.0 {
        return MultiGenResult::StopRegression;
    }
    if last.delta == 0.0 {
        return MultiGenResult::StopNonImprovement;
    }
    MultiGenResult::Continue
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn next_action_continues_while_below_min_generations_with_budget() {
        assert_eq!(next_action(&[], 3), MultiGenResult::Continue);
        let gens = vec![
            Generation {
                index: 0,
                delta: -0.5,
                budget_remaining: 10.0,
                rejected: true,
            },
            Generation {
                index: 1,
                delta: 0.0,
                budget_remaining: 5.0,
                rejected: false,
            },
        ];
        assert_eq!(next_action(&gens, 3), MultiGenResult::Continue);
    }

    #[test]
    fn next_action_stops_immediately_when_budget_is_exhausted() {
        let gens = vec![Generation {
            index: 0,
            delta: 1.0,
            budget_remaining: 0.0,
            rejected: false,
        }];
        assert_eq!(next_action(&gens, 3), MultiGenResult::StopBudget);
    }

    #[test]
    fn next_action_stops_on_regression_or_rejection_at_min_generations() {
        let regressed = vec![
            Generation {
                index: 0,
                delta: 0.2,
                budget_remaining: 8.0,
                rejected: false,
            },
            Generation {
                index: 1,
                delta: 0.3,
                budget_remaining: 6.0,
                rejected: false,
            },
            Generation {
                index: 2,
                delta: -0.1,
                budget_remaining: 4.0,
                rejected: false,
            },
        ];
        assert_eq!(next_action(&regressed, 3), MultiGenResult::StopRegression);

        let mut rejected = regressed;
        rejected[2].delta = 0.5;
        rejected[2].rejected = true;
        assert_eq!(next_action(&rejected, 3), MultiGenResult::StopRegression);
    }

    #[test]
    fn next_action_stops_on_zero_delta_and_continues_on_positive_delta() {
        let mut gens = vec![
            Generation {
                index: 0,
                delta: 0.1,
                budget_remaining: 9.0,
                rejected: false,
            },
            Generation {
                index: 1,
                delta: 0.2,
                budget_remaining: 7.0,
                rejected: false,
            },
            Generation {
                index: 2,
                delta: 0.0,
                budget_remaining: 5.0,
                rejected: false,
            },
        ];
        assert_eq!(next_action(&gens, 3), MultiGenResult::StopNonImprovement);
        gens[2].delta = 0.4;
        assert_eq!(next_action(&gens, 3), MultiGenResult::Continue);
    }

    #[test]
    fn next_action_handles_single_generation_regression_and_non_improvement_at_min_one() {
        let regressed = vec![Generation {
            index: 0,
            delta: -0.01,
            budget_remaining: 5.0,
            rejected: false,
        }];
        assert_eq!(next_action(&regressed, 1), MultiGenResult::StopRegression);

        let flat = vec![Generation {
            index: 0,
            delta: 0.0,
            budget_remaining: 5.0,
            rejected: false,
        }];
        assert_eq!(next_action(&flat, 1), MultiGenResult::StopNonImprovement);
    }
}

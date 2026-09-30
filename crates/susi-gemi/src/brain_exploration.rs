//! Bounded exploration of untried providers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorePick {
    pub provider: String,
    pub explore: bool,
}

/// Occasionally try an untried provider within an exploration budget.
#[must_use]
pub fn explore_pick(
    tried: &[&str],
    candidates: &[&str],
    explore_budget_remaining: u32,
    roll_01: f64,
) -> ExplorePick {
    let untried: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|c| !tried.iter().any(|t| t == c))
        .collect();
    if explore_budget_remaining > 0 && !untried.is_empty() && roll_01 < 0.1 {
        return ExplorePick {
            provider: untried[0].into(),
            explore: true,
        };
    }
    ExplorePick {
        provider: candidates.first().copied().unwrap_or("none").into(),
        explore: false,
    }
}

#[cfg(test)]
mod brain_exploration_tests {
    use super::*;

    #[test]
    fn brain_exploration_bounded() {
        let e = explore_pick(&["a"], &["a", "b"], 1, 0.05);
        assert!(e.explore);
        assert_eq!(e.provider, "b");
        assert!(!explore_pick(&["a"], &["a", "b"], 0, 0.05).explore);
    }
}

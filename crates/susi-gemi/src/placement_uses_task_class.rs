//! Route local-vs-cloud placement by task class and budget.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Placement {
    Local,
    Cloud,
}

/// Place work locally for cheap/coding classes when budget is tight.
#[must_use]
pub fn place(task_class: &str, daily_budget_usd: f64) -> Placement {
    let cheap = matches!(
        task_class,
        "coding" | "summarize" | "classify" | "local_only"
    );
    if cheap || daily_budget_usd < 1.0 {
        Placement::Local
    } else {
        Placement::Cloud
    }
}

#[cfg(test)]
mod placement_uses_task_class_tests {
    use super::*;

    #[test]
    fn placement_uses_task_class_and_budget() {
        assert_eq!(place("coding", 50.0), Placement::Local);
        assert_eq!(place("research", 0.5), Placement::Local);
        assert_eq!(place("research", 20.0), Placement::Cloud);
    }
}

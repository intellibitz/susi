//! `susi tasks next` picks and claims the best unclaimed task.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCandidate {
    pub id: String,
    pub size: String,
    pub has_deps: bool,
}

/// Prefer small, unblocked tasks first.
#[must_use]
pub fn pick_next(candidates: &[TaskCandidate]) -> Option<String> {
    candidates
        .iter()
        .filter(|t| !t.has_deps)
        .min_by_key(|t| match t.size.as_str() {
            "s" => 0,
            "m" => 1,
            _ => 2,
        })
        .map(|t| t.id.clone())
}

#[cfg(test)]
mod zc_tasks_next_tests {
    use super::*;

    #[test]
    fn zc_tasks_next_prefers_small_unblocked() {
        let pick = pick_next(&[
            TaskCandidate {
                id: "T-L".into(),
                size: "l".into(),
                has_deps: false,
            },
            TaskCandidate {
                id: "T-S".into(),
                size: "s".into(),
                has_deps: false,
            },
            TaskCandidate {
                id: "T-blocked".into(),
                size: "s".into(),
                has_deps: true,
            },
        ]);
        assert_eq!(pick.as_deref(), Some("T-S"));
    }
}

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

    #[test]
    fn zc_tasks_next_empty_slice_returns_none() {
        assert_eq!(pick_next(&[]), None);
    }

    #[test]
    fn zc_tasks_next_all_blocked_returns_none() {
        let pick = pick_next(&[
            TaskCandidate {
                id: "T-1".into(),
                size: "s".into(),
                has_deps: true,
            },
            TaskCandidate {
                id: "T-2".into(),
                size: "m".into(),
                has_deps: true,
            },
        ]);
        assert_eq!(pick, None);
    }

    #[test]
    fn zc_tasks_next_selects_medium_over_large_when_no_small_exists() {
        let pick = pick_next(&[
            TaskCandidate {
                id: "T-LARGE".into(),
                size: "l".into(),
                has_deps: false,
            },
            TaskCandidate {
                id: "T-MEDIUM".into(),
                size: "m".into(),
                has_deps: false,
            },
        ]);
        assert_eq!(pick.as_deref(), Some("T-MEDIUM"));
    }

    #[test]
    fn zc_tasks_next_selects_large_when_only_unblocked_large_candidate_exists() {
        let pick = pick_next(&[
            TaskCandidate {
                id: "T-BLOCKED-S".into(),
                size: "s".into(),
                has_deps: true,
            },
            TaskCandidate {
                id: "T-UNBLOCKED-L".into(),
                size: "l".into(),
                has_deps: false,
            },
        ]);
        assert_eq!(pick.as_deref(), Some("T-UNBLOCKED-L"));
    }
}

//! Guard open task acceptance / deps edits (Mandate 50 integrity).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub id: String,
    pub created_by: String,
    pub accept_cmd: Vec<String>,
    pub deps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditDecision {
    Allow,
    Reject(&'static str),
}

/// Flag weaken/change of accept or deps unless by creator or explicit override.
#[must_use]
pub fn guard_edit(
    before: &TaskSnapshot,
    after: &TaskSnapshot,
    editor: &str,
    explicit_override: bool,
) -> EditDecision {
    if before.id != after.id {
        return EditDecision::Reject("task id mismatch");
    }
    let accept_changed = before.accept_cmd != after.accept_cmd;
    let deps_changed = before.deps != after.deps;
    if !accept_changed && !deps_changed {
        return EditDecision::Allow;
    }
    if explicit_override {
        return EditDecision::Allow;
    }
    if editor == before.created_by {
        return EditDecision::Allow;
    }
    EditDecision::Reject("open task accept/deps change requires creator or override")
}

#[cfg(test)]
mod task_edit_guard_tests {
    use super::*;

    fn base() -> TaskSnapshot {
        TaskSnapshot {
            id: "T-1".into(),
            created_by: "CLAUDE".into(),
            accept_cmd: vec!["cargo".into(), "test".into()],
            deps: vec!["T-0".into()],
        }
    }

    #[test]
    fn task_edit_guard_blocks_foreign_accept_weaken() {
        let before = base();
        let mut after = before.clone();
        after.accept_cmd = vec!["true".into()];
        assert!(matches!(
            guard_edit(&before, &after, "CURSOR", false),
            EditDecision::Reject(_)
        ));
        assert_eq!(
            guard_edit(&before, &after, "CLAUDE", false),
            EditDecision::Allow
        );
        assert_eq!(
            guard_edit(&before, &after, "CURSOR", true),
            EditDecision::Allow
        );
    }

    #[test]
    fn task_edit_guard_allows_unrelated_metadata() {
        let before = base();
        let after = before.clone();
        assert_eq!(
            guard_edit(&before, &after, "OTHER", false),
            EditDecision::Allow
        );
    }
}

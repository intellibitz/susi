//! Clean merged worktrees and branches automatically.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeGcPlan {
    pub remove: Vec<String>,
    pub keep_primary: bool,
}

/// Plan removal for worktrees whose branch is already merged.
#[must_use]
pub fn gc_merged(
    worktrees: &[(String, bool /* is_primary */, bool /* merged */)],
) -> WorktreeGcPlan {
    let mut remove = Vec::new();
    let mut keep_primary = false;
    for (path, is_primary, merged) in worktrees {
        if *is_primary {
            keep_primary = true;
            continue;
        }
        if *merged {
            remove.push(path.clone());
        }
    }
    WorktreeGcPlan {
        remove,
        keep_primary,
    }
}

#[cfg(test)]
mod zc_worktree_gc_tests {
    use super::*;

    #[test]
    fn zc_worktree_gc_removes_merged_keeps_primary() {
        let plan = gc_merged(&[
            ("/repo".into(), true, true),
            ("/repo-wt-a".into(), false, true),
            ("/repo-wt-b".into(), false, false),
        ]);
        assert!(plan.keep_primary);
        assert_eq!(plan.remove, vec!["/repo-wt-a".to_string()]);
    }
}

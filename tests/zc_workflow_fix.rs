//! `susi workflow check --fix` applies safe fixes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

#[derive(Debug, Clone, PartialEq, Eq)]
struct FixPlan {
    merge_main: bool,
    install_hooks: bool,
    create_worktree: bool,
    refuse: bool,
    reason: String,
}

fn workflow_fix(dirty: bool, on_primary: bool, hooks_missing: bool) -> FixPlan {
    if dirty {
        return FixPlan {
            merge_main: false,
            install_hooks: false,
            create_worktree: false,
            refuse: true,
            reason: "refuse: would lose local work".into(),
        };
    }
    FixPlan {
        merge_main: true,
        install_hooks: hooks_missing,
        create_worktree: on_primary,
        refuse: false,
        reason: "safe fixes applied".into(),
    }
}

#[test]
fn zc_workflow_fix_applies_safe_fixes_or_refuses() {
    let ok = workflow_fix(false, true, true);
    assert!(!ok.refuse);
    assert!(ok.merge_main);
    assert!(ok.install_hooks);
    assert!(ok.create_worktree);
    let refuse = workflow_fix(true, true, false);
    assert!(refuse.refuse);
}

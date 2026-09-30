use crate::workspace_txn::{unrelated_preserved, ApplyOutcome, Patch, WorkspaceTxn};
use std::collections::BTreeSet;

#[test]
fn vc_201_029_applies_non_overlapping_and_conflicts_overlap() {
    let mut ws = WorkspaceTxn::new();
    let a = Patch {
        agent: "alice".into(),
        base_rev: 0,
        paths: BTreeSet::from(["a.rs".into()]),
        body: "alice-edit".into(),
    };
    assert!(matches!(
        ws.apply(a, &[]),
        ApplyOutcome::Applied { new_rev: 1 }
    ));
    let bob = Patch {
        agent: "bob".into(),
        base_rev: 1,
        paths: BTreeSet::from(["b.rs".into()]),
        body: "bob-edit".into(),
    };
    let carol_conflict = Patch {
        agent: "carol".into(),
        base_rev: 1,
        paths: BTreeSet::from(["b.rs".into()]),
        body: "carol-edit".into(),
    };
    let staging = vec![carol_conflict.clone()];
    let outcome = ws.apply(bob, &staging);
    match outcome {
        ApplyOutcome::Conflict { overlapping } => {
            assert!(overlapping.contains("b.rs"));
        }
        other => panic!("expected conflict, got {other:?}"),
    }
    // Unrelated alice edit preserved.
    let preserved = unrelated_preserved(
        &[Patch {
            agent: "alice".into(),
            base_rev: 0,
            paths: BTreeSet::from(["a.rs".into()]),
            body: "alice-edit".into(),
        }],
        &BTreeSet::from(["b.rs".into()]),
    );
    assert_eq!(preserved, vec!["alice-edit".to_string()]);
}

#[test]
fn vc_201_029_rejects_stale_base() {
    let mut ws = WorkspaceTxn::new();
    let _ = ws.apply(
        Patch {
            agent: "a".into(),
            base_rev: 0,
            paths: BTreeSet::from(["x".into()]),
            body: "1".into(),
        },
        &[],
    );
    let stale = ws.apply(
        Patch {
            agent: "b".into(),
            base_rev: 0,
            paths: BTreeSet::from(["y".into()]),
            body: "2".into(),
        },
        &[],
    );
    assert!(matches!(stale, ApplyOutcome::StaleBase { current_rev: 1 }));
}

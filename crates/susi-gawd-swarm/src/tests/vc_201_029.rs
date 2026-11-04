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

#[test]
fn vc_201_029_applies_sequential_disjoint_revisions_and_tracks_count() {
    let mut ws = WorkspaceTxn::new();
    assert_eq!(ws.rev(), 0);
    assert_eq!(ws.applied_count(), 0);

    for (idx, (agent, path)) in [("a1", "p1.rs"), ("a2", "p2.rs"), ("a3", "p3.rs")]
        .into_iter()
        .enumerate()
    {
        let base_rev = idx as u64;
        let outcome = ws.apply(
            Patch {
                agent: agent.into(),
                base_rev,
                paths: BTreeSet::from([path.into()]),
                body: format!("edit-{idx}"),
            },
            &[],
        );
        assert_eq!(
            outcome,
            ApplyOutcome::Applied {
                new_rev: base_rev + 1
            }
        );
    }
    assert_eq!(ws.rev(), 3);
    assert_eq!(ws.applied_count(), 3);

    // Another agent attempting to overwrite an already-owned path conflicts.
    let owned_conflict = ws.apply(
        Patch {
            agent: "intruder".into(),
            base_rev: 3,
            paths: BTreeSet::from(["p2.rs".into()]),
            body: "intruder-edit".into(),
        },
        &[],
    );
    assert_eq!(
        owned_conflict,
        ApplyOutcome::Conflict {
            overlapping: BTreeSet::from(["p2.rs".into()])
        }
    );
}

#[test]
fn vc_201_029_unrelated_preserved_filters_multi_file_overlapping_patches() {
    let applied = vec![
        Patch {
            agent: "a1".into(),
            base_rev: 0,
            paths: BTreeSet::from(["clean1.rs".into(), "clean2.rs".into()]),
            body: "keep-clean".into(),
        },
        Patch {
            agent: "a2".into(),
            base_rev: 1,
            paths: BTreeSet::from(["clean3.rs".into(), "dirty.rs".into()]),
            body: "drop-partial-overlap".into(),
        },
        Patch {
            agent: "a3".into(),
            base_rev: 2,
            paths: BTreeSet::from(["clean4.rs".into()]),
            body: "keep-second".into(),
        },
    ];
    let conflicted = BTreeSet::from(["dirty.rs".into()]);
    let preserved = unrelated_preserved(&applied, &conflicted);
    assert_eq!(
        preserved,
        vec!["keep-clean".to_string(), "keep-second".to_string()]
    );
}

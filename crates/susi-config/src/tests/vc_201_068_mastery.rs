//! Mastery verification for VC-201-068: staged fleet configuration rollout.
//!
//! The claim under test is not "pending() lists the open wave" — the cited
//! `vc_201_068` tests show that. The distinguishing properties are that the
//! wave bound is *enforced* (a node outside the open wave cannot take the
//! pinned revision early), and that resume-after-failure requires verified
//! health rather than an unverified rollback flag.

use crate::fleet_rollout::{Rollout, RolloutStatus};

fn fleet() -> Vec<(String, Option<String>)> {
    ["n1", "n2", "n3", "n4", "n5"]
        .iter()
        .map(|s| (s.to_string(), Some("rev-1".to_string())))
        .collect()
}

#[test]
fn vc_201_068_mastery_mark_applied_enforces_wave_bound() {
    let mut r = Rollout::new("rev-2", &fleet(), &["n1".into()], 2).unwrap();
    assert_eq!(r.pending(), vec!["n1".to_string()]);
    assert!(
        !r.pending().contains(&"n5".to_string()),
        "n5 is in a later wave — not pending"
    );
    assert!(
        r.mark_applied("n5", "rev-2").is_err(),
        "later-wave node must not apply while wave 0 is open"
    );
    assert_eq!(r.nodes["n5"].current.as_deref(), Some("rev-1"));
}

#[test]
fn vc_201_068_mastery_rollback_requires_verified_health_before_resume() {
    let mut r = Rollout::new("rev-2", &fleet(), &["n1".into()], 2).unwrap();
    r.mark_applied("n1", "rev-2").unwrap();
    r.report("n1", true).unwrap();
    r.mark_applied("n2", "rev-2").unwrap();
    r.report("n2", false).unwrap();
    assert_eq!(r.status, RolloutStatus::Paused);

    r.rollback("n2").unwrap();
    assert!(
        !r.nodes["n2"].healthy,
        "rollback must invalidate health until a fresh report"
    );
    assert!(r.resume().is_err(), "unverified rollback must block resume");
    r.report("n2", true).unwrap();
    r.resume().expect("verified rollback should permit resume");
    assert_eq!(r.status, RolloutStatus::Running);
}

#[test]
fn vc_201_068_mastery_no_canary_is_rejected() {
    assert!(
        Rollout::new("rev-2", &fleet(), &[], 2).is_err(),
        "a non-empty fleet must have a canary wave"
    );
}

/// What does hold: the pin is enforced on apply, a reported-unhealthy node
/// pauses and empties pending, per-node rollback state is exposed, and a
/// fully pinned+healthy fleet completes.
#[test]
fn vc_201_068_mastery_pin_pause_rollback_state_and_completion_hold() {
    let mut r = Rollout::new("rev-2", &fleet(), &["n1".into()], 2).unwrap();
    assert!(
        r.mark_applied("n1", "rev-9").is_err(),
        "unpinned apply refused"
    );
    r.mark_applied("n1", "rev-2").unwrap();
    r.report("n1", false).unwrap();
    assert_eq!(r.status, RolloutStatus::Paused);
    assert!(r.pending().is_empty());
    assert_eq!(r.rollback_state().len(), 5);
    assert!(r
        .rollback_state()
        .values()
        .all(|v| v.as_deref() == Some("rev-1")));
    assert!(
        r.rollback("ghost").is_err(),
        "unknown node refuses rollback"
    );

    // Fully pinned + healthy fleet completes.
    r.rollback("n1").unwrap();
    r.report("n1", true).unwrap();
    r.resume().unwrap();
    for id in ["n1", "n2", "n3", "n4", "n5"] {
        r.mark_applied(id, "rev-2").unwrap();
        r.report(id, true).unwrap();
    }
    assert_eq!(r.status, RolloutStatus::Done);
    assert!(r.pending().is_empty());
}

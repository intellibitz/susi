//! Staged fleet configuration rollout (VC-201-068).

use crate::fleet_rollout::{Rollout, RolloutStatus};

fn fleet() -> Vec<(String, Option<String>)> {
    ["n1", "n2", "n3", "n4", "n5"]
        .iter()
        .map(|s| (s.to_string(), Some("rev-1".to_string())))
        .collect()
}

#[test]
fn vc_201_068_canary_wave_opens_first() {
    let r = Rollout::new("rev-2", &fleet(), &["n1".into()], 2).unwrap();
    assert_eq!(r.open_wave, 0);
    assert_eq!(r.status, RolloutStatus::Running);
    assert_eq!(r.pending(), vec!["n1".to_string()]);
    assert_eq!(r.nodes["n1"].target, "rev-2");
    assert_eq!(r.nodes["n1"].rollback_to.as_deref(), Some("rev-1"));
}

#[test]
fn vc_201_068_waves_advance_only_after_health() {
    let mut r = Rollout::new("rev-2", &fleet(), &["n1".into()], 2).unwrap();
    // Later waves never become pending until canary is pinned+healthy.
    assert!(!r.pending().contains(&"n3".to_string()));
    r.mark_applied("n1", "rev-2").unwrap();
    r.report("n1", true).unwrap();
    assert_eq!(r.open_wave, 1);
    let pending = r.pending();
    assert_eq!(pending.len(), 2, "wave_size bounds the wave");
    assert!(pending.contains(&"n2".to_string()));
    assert!(pending.contains(&"n3".to_string()));
}

#[test]
fn vc_201_068_unhealthy_node_pauses_and_blocks_later_waves() {
    let mut r = Rollout::new("rev-2", &fleet(), &["n1".into()], 2).unwrap();
    r.mark_applied("n1", "rev-2").unwrap();
    r.report("n1", true).unwrap();
    r.mark_applied("n2", "rev-2").unwrap();
    r.report("n2", false).unwrap();
    assert_eq!(r.status, RolloutStatus::Paused);
    assert!(r.pending().is_empty(), "paused rollout applies nothing");
    // Rollback restores the node's pre-rollout revision.
    assert_eq!(r.rollback("n2").unwrap().as_deref(), Some("rev-1"));
    assert_eq!(r.nodes["n2"].current.as_deref(), Some("rev-1"));
    // Still paused state — n3 remains unapplied; n4/n5 untouched.
    assert_eq!(r.nodes["n4"].current.as_deref(), Some("rev-1"));
}

#[test]
fn vc_201_068_resume_requires_all_healthy_then_completes() {
    let mut r = Rollout::new("rev-2", &fleet(), &["n1".into()], 4).unwrap();
    r.mark_applied("n1", "rev-2").unwrap();
    r.report("n1", true).unwrap();
    r.mark_applied("n2", "rev-2").unwrap();
    r.report("n2", false).unwrap();
    assert!(r.resume().is_err(), "unhealthy node blocks resume");
    r.rollback("n2").unwrap();
    r.resume().unwrap();
    assert_eq!(r.status, RolloutStatus::Running);
    // n2 rolled back, n3..n5 not yet applied — pending reflects wave 1
    // remainder minus n2 which is back on rev-1.
    let pending = r.pending();
    assert!(pending.contains(&"n3".to_string()));
    assert!(
        pending.contains(&"n2".to_string()),
        "rolled-back node re-pends"
    );
    // Finish the fleet.
    for id in ["n2", "n3", "n4", "n5"] {
        r.mark_applied(id, "rev-2").unwrap();
        r.report(id, true).unwrap();
    }
    assert_eq!(r.status, RolloutStatus::Done);
}

#[test]
fn vc_201_068_pinned_revision_is_enforced_and_rollback_state_exposed() {
    let mut r = Rollout::new("rev-2", &fleet(), &["n1".into()], 2).unwrap();
    assert!(r.mark_applied("n1", "rev-9").is_err());
    r.mark_applied("n1", "rev-2").unwrap();
    let state = r.rollback_state();
    assert_eq!(state.len(), 5);
    assert!(state.values().all(|v| v.as_deref() == Some("rev-1")));
}

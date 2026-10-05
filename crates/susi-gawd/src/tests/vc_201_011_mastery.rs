//! Mastery verification for VC-201-011: a durable self-improvement
//! experiment lifecycle.
//!
//! The cited tests exercise the transition table in memory. The
//! distinguishing properties are that transitions are *durable* (the log
//! survives a restart — 'kill-and-resume' is the claim), that a failed
//! leg has an exit path, and that re-proposing cannot corrupt state.

use crate::experiment_lifecycle::{ExperimentLog, ExperimentState};

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("susi-explog-mastery-{tag}-{nanos}"))
}

/// Fixed: a log loaded from a durable directory survives a restart — a
/// fresh `ExperimentLog::load` of the SAME directory sees the experiment
/// and its applied transitions, so kill-and-resume now distinguishes
/// 'e1 mid-flight' from 'never ran'.
#[test]
fn vc_201_011_mastery_restart_forgets_everything() {
    let dir = tmp_dir("restart");
    let mut log = ExperimentLog::load(dir.clone());
    log.propose("e1").unwrap();
    log.transition("e1", ExperimentState::Isolated).unwrap();
    assert!(log.resume_requires_evaluation("e1"));
    drop(log);

    // Simulate a process restart: load the same directory fresh.
    let restarted = ExperimentLog::load(dir.clone());
    assert!(
        restarted.resume_requires_evaluation("e1"),
        "a restart that reloads the same durable dir must resume mid-flight state"
    );
    assert!(restarted.applied_transitions.contains(&(
        "e1".to_string(),
        ExperimentState::Proposed,
        ExperimentState::Isolated
    )));

    // An in-memory-only log (no dir) still starts empty, as before.
    let scratch = ExperimentLog::default();
    assert!(scratch.experiments.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fixed: `Isolated -> Rejected` is now a legal transition, so a failed
/// isolation leg has an exit instead of sitting mid-flight forever.
/// `Proposed -> Rejected` is still refused: an obviously bad proposal
/// must run isolation before it can be discarded.
#[test]
fn vc_201_011_mastery_failed_isolation_has_no_exit() {
    let mut log = ExperimentLog::default();
    log.propose("e1").unwrap();
    log.transition("e1", ExperimentState::Isolated).unwrap();
    // The isolation run failed — reject the experiment.
    log.transition("e1", ExperimentState::Rejected)
        .expect("a failed isolation leg must have an exit");
    assert!(!log.resume_requires_evaluation("e1"));

    log.propose("e2").unwrap();
    assert!(log.transition("e2", ExperimentState::Rejected).is_err());
}

/// Fixed: `propose` on an existing id is now refused instead of silently
/// resetting its state while the surviving transition log wedges it.
#[test]
fn vc_201_011_mastery_repropose_wedges_the_experiment() {
    let mut log = ExperimentLog::default();
    log.propose("e1").unwrap();
    log.transition("e1", ExperimentState::Isolated).unwrap();

    let err = log
        .propose("e1")
        .expect_err("re-proposing a live experiment must be refused");
    assert!(err.contains("e1"), "{err}");
    assert_eq!(
        log.experiments["e1"].state,
        ExperimentState::Isolated,
        "the existing experiment's state must be untouched"
    );
}

/// What does hold: the legal chain Proposed→Isolated→Evaluated→
/// PromotionReady advances, skipping evaluation is refused, and a
/// duplicate applied edge errors.
#[test]
fn vc_201_011_mastery_transition_table_holds() {
    let mut log = ExperimentLog::default();
    log.propose("e3").unwrap();
    assert!(log
        .transition("e3", ExperimentState::PromotionReady)
        .is_err());
    log.transition("e3", ExperimentState::Isolated).unwrap();
    log.transition("e3", ExperimentState::Evaluated).unwrap();
    log.transition("e3", ExperimentState::PromotionReady)
        .unwrap();
    assert!(log
        .transition("e3", ExperimentState::PromotionReady)
        .is_err());
}

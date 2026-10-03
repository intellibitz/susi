//! Mastery verification for VC-201-011: a durable self-improvement
//! experiment lifecycle.
//!
//! The cited tests exercise the transition table in memory. The
//! distinguishing properties are that transitions are *durable* (the log
//! survives a restart — 'kill-and-resume' is the claim), that a failed
//! leg has an exit path, and that re-proposing cannot corrupt state.

use crate::experiment_lifecycle::{ExperimentLog, ExperimentState};

/// Falsification: nothing is persisted. 'Kill-and-resume' is untestable
/// because a restart is just a new empty log — every experiment and every
/// applied transition is forgotten. Durable means surviving the process;
/// this log lives and dies in one process's memory.
#[test]
fn vc_201_011_mastery_restart_forgets_everything() {
    let mut log = ExperimentLog::default();
    log.propose("e1");
    log.transition("e1", ExperimentState::Isolated).unwrap();
    assert!(log.resume_requires_evaluation("e1"));

    let restarted = ExperimentLog::default();
    assert!(restarted.experiments.is_empty());
    assert!(restarted.applied_transitions.is_empty());
    // The resumed world cannot distinguish 'e1 mid-flight' from 'never ran'.
    assert!(!restarted.resume_requires_evaluation("e1"));
}

/// Falsification: a failed isolation leg has no exit. Only Evaluated can
/// reach Rejected — an experiment that fails while Isolated can never be
/// concluded: it sits mid-flight forever, forever blocking on
/// 'resume_requires_evaluation'.
#[test]
fn vc_201_011_mastery_failed_isolation_has_no_exit() {
    let mut log = ExperimentLog::default();
    log.propose("e1");
    log.transition("e1", ExperimentState::Isolated).unwrap();
    // The isolation run failed — try to reject the experiment.
    assert!(
        log.transition("e1", ExperimentState::Rejected).is_err(),
        "an experiment that fails in isolation is wedged forever"
    );
    assert!(log.resume_requires_evaluation("e1"));
    // And Proposed cannot be rejected either — an obviously bad proposal
    // must run isolation before it can be discarded.
    log.propose("e2");
    assert!(log.transition("e2", ExperimentState::Rejected).is_err());
}

/// Falsification: `propose` on an existing id silently resets the state
/// while `applied_transitions` is retained — the experiment is then
/// permanently wedged: Proposed again, but re-applying Isolated is a
/// 'duplicate' and refused. Two memory writes corrupt the lifecycle.
#[test]
fn vc_201_011_mastery_repropose_wedges_the_experiment() {
    let mut log = ExperimentLog::default();
    log.propose("e1");
    log.transition("e1", ExperimentState::Isolated).unwrap();

    log.propose("e1"); // overwrite mid-flight experiment
    assert_eq!(
        log.experiments["e1"].state,
        ExperimentState::Proposed,
        "state silently reset"
    );
    assert!(
        log.transition("e1", ExperimentState::Isolated).is_err(),
        "the surviving transition log refuses the retry — e1 is unrecoverable"
    );
}

/// What does hold: the legal chain Proposed→Isolated→Evaluated→
/// PromotionReady advances, skipping evaluation is refused, and a
/// duplicate applied edge errors.
#[test]
fn vc_201_011_mastery_transition_table_holds() {
    let mut log = ExperimentLog::default();
    log.propose("e3");
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

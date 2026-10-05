use crate::experiment_lifecycle::{ExperimentLog, ExperimentState};

#[test]
fn vc_201_011_durable_transitions_and_no_duplicate() {
    let mut log = ExperimentLog::default();
    log.propose("e1").unwrap();
    log.transition("e1", ExperimentState::Isolated).unwrap();
    assert!(log.resume_requires_evaluation("e1"));
    log.transition("e1", ExperimentState::Evaluated).unwrap();
    log.transition("e1", ExperimentState::PromotionReady)
        .unwrap();
    assert!(log
        .transition("e1", ExperimentState::PromotionReady)
        .unwrap_err()
        .contains("illegal"));
}

#[test]
fn vc_201_011_cannot_skip_evaluation() {
    let mut log = ExperimentLog::default();
    log.propose("e2").unwrap();
    log.transition("e2", ExperimentState::Isolated).unwrap();
    assert!(log
        .transition("e2", ExperimentState::PromotionReady)
        .is_err());
}

#[test]
fn vc_201_011_duplicate_applied_transition_refused() {
    let mut log = ExperimentLog::default();
    log.propose("e3").unwrap();
    log.transition("e3", ExperimentState::Isolated).unwrap();
    // Forge a duplicate in the log history then retry same edge after reset state.
    log.experiments.get_mut("e3").unwrap().state = ExperimentState::Proposed;
    assert!(log
        .transition("e3", ExperimentState::Isolated)
        .unwrap_err()
        .contains("duplicate"));
}

#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::expect_used)] // the checked-in chaos matrix must contain every required fault

use susi_gawd_swarm::chaos_deployment::{
    run_dev_matrix, DegradedReason, DeploymentState, DevFault,
};

#[test]
fn vc_201_095_mastery() {
    let report = run_dev_matrix();
    assert_eq!(report.scenarios.len(), 4);
    assert!(report.all_passed(), "chaos report: {report:?}");

    for (fault, reason) in [
        (DevFault::DiskFull, DegradedReason::DiskFull),
        (DevFault::CorruptState, DegradedReason::CorruptState),
        (
            DevFault::RevokedCredential,
            DegradedReason::RevokedCredential,
        ),
    ] {
        let scenario = report
            .scenarios
            .iter()
            .find(|scenario| scenario.fault == fault)
            .expect("required chaos scenario");
        assert_eq!(scenario.degraded_state, DeploymentState::Degraded(reason));
        assert_eq!(scenario.recovered_state, DeploymentState::Ready);
    }

    let crash = report
        .scenarios
        .iter()
        .find(|scenario| scenario.fault == DevFault::RuntimeCrash)
        .expect("runtime-crash scenario");
    assert_eq!(crash.recovered_state, DeploymentState::Ready);
    assert!(report
        .scenarios
        .iter()
        .all(|scenario| scenario.bounded_recovery
            && scenario.privacy_preserved
            && scenario.budget_preserved
            && scenario.isolation_preserved));
}

//! Mastery verification for VC-201-095 ecosystem chaos drills.

use crate::chaos_deployment::{
    run_dev_matrix, ChaosReport, DegradedReason, DeploymentState, DevDeployment, DevFault,
};

#[test]
fn vc_201_095_mastery() {
    let report: ChaosReport = run_dev_matrix();
    assert_eq!(report.scenarios.len(), 4);
    assert!(report.all_passed(), "chaos report: {report:?}");

    let disk = report
        .scenarios
        .iter()
        .find(|scenario| scenario.fault == DevFault::DiskFull)
        .expect("disk-full scenario is required");
    assert_eq!(
        disk.degraded_state,
        DeploymentState::Degraded(DegradedReason::DiskFull)
    );

    let crash = report
        .scenarios
        .iter()
        .find(|scenario| scenario.fault == DevFault::RuntimeCrash)
        .expect("runtime-crash scenario is required");
    assert_eq!(crash.recovered_state, DeploymentState::Ready);

    let revoked = report
        .scenarios
        .iter()
        .find(|scenario| scenario.fault == DevFault::RevokedCredential)
        .expect("revoked-credential scenario is required");
    assert_eq!(
        revoked.degraded_state,
        DeploymentState::Degraded(DegradedReason::RevokedCredential)
    );
    assert_eq!(revoked.recovered_state, DeploymentState::Ready);

    for scenario in &report.scenarios {
        assert!(scenario.bounded_recovery);
        assert!(scenario.privacy_preserved);
        assert!(scenario.budget_preserved);
        assert!(scenario.isolation_preserved);
        assert!(scenario
            .trace
            .iter()
            .all(|entry| !entry.contains("secret-")));
    }
}

#[test]
fn vc_201_095_mastery_failed_writes_do_not_spend_budget() {
    let mut deployment = DevDeployment::new("isolated", "credential", 20, 4);
    deployment.start("credential", 10).expect("start succeeds");
    let before = deployment.budget().spent();
    assert!(deployment.write_artifact("too-large", b"12345", 7).is_err());
    assert_eq!(deployment.budget().spent(), before);
    assert_eq!(
        deployment.state(),
        DeploymentState::Degraded(DegradedReason::DiskFull)
    );
}

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

#[test]
fn vc_201_095_mastery_rejects_wrong_startup_credential_without_charging_budget() {
    let mut deployment = DevDeployment::new("isolated-auth", "valid-cred", 25, 16);
    assert!(deployment.start("wrong-cred", 10).is_err());
    assert_eq!(deployment.state(), DeploymentState::Stopped);
    assert_eq!(deployment.budget().spent(), 0);
}

#[test]
fn vc_201_095_mastery_revoked_credential_requires_reauthorization_to_recover() {
    let mut deployment = DevDeployment::new("isolated-revoke", "cred-v1", 30, 16);
    deployment.start("cred-v1", 5).expect("start succeeds");
    deployment.inject(DevFault::RevokedCredential);
    assert_eq!(
        deployment.state(),
        DeploymentState::Degraded(DegradedReason::RevokedCredential)
    );
    // Plain recover() cannot bypass credential revocation; reauthorize() with a fresh credential restores Ready.
    assert!(deployment.recover().is_err());
    assert!(deployment.reauthorize("cred-v1").is_err());
    deployment
        .reauthorize("cred-v2")
        .expect("fresh credential restores Ready");
    assert_eq!(deployment.state(), DeploymentState::Ready);
    assert_eq!(deployment.budget().spent(), 5);
}

#[test]
fn vc_201_095_mastery_runtime_crash_recovers_within_remaining_budget() {
    let mut deployment = DevDeployment::new("isolated-crash", "cred-ok", 30, 16);
    deployment.start("cred-ok", 5).expect("start succeeds");
    deployment.inject(DevFault::RuntimeCrash);
    assert_eq!(deployment.state(), DeploymentState::Recovering);
    deployment
        .recover()
        .expect("runtime crash recovers cleanly");
    assert_eq!(deployment.state(), DeploymentState::Ready);
}

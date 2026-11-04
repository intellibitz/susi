#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::expect_used)] // the checked-in chaos matrix must contain every required fault

use susi_gawd_swarm::chaos_deployment::{
    run_dev_matrix, DegradedReason, DeploymentError, DeploymentState, DevDeployment, DevFault,
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

#[test]
fn vc_201_095_start_rejects_wrong_or_revoked_credential_and_preserves_budget() {
    let mut dep = DevDeployment::new("ns-auth", "valid-cred", 50, 128);
    assert_eq!(dep.namespace(), "ns-auth");
    assert_eq!(dep.state(), DeploymentState::Stopped);
    assert_eq!(dep.budget().limit(), 50);

    assert_eq!(
        dep.start("wrong-cred", 10),
        Err(DeploymentError::CredentialRejected)
    );
    assert_eq!(dep.budget().spent(), 0);

    dep.start("valid-cred", 10).expect("valid start");
    assert_eq!(dep.state(), DeploymentState::Ready);
    assert_eq!(dep.budget().spent(), 10);

    dep.inject(DevFault::RevokedCredential);
    assert_eq!(
        dep.state(),
        DeploymentState::Degraded(DegradedReason::RevokedCredential)
    );
    assert_eq!(dep.recover(), Err(DeploymentError::CredentialRejected));
    assert_eq!(
        dep.reauthorize(""),
        Err(DeploymentError::CredentialRejected)
    );
    assert_eq!(
        dep.reauthorize("valid-cred"),
        Err(DeploymentError::CredentialRejected)
    );
    dep.reauthorize("rotated-cred").expect("rotate credential");
    assert_eq!(dep.state(), DeploymentState::Ready);
    assert!(dep.trace().iter().all(|line| !line.contains("valid-cred")));
}

#[test]
fn vc_201_095_disk_full_and_budget_exhaustion_on_write_artifact() {
    let mut dep = DevDeployment::new("ns-storage", "cred-1", 20, 8);
    dep.start("cred-1", 5).expect("start deployment");

    assert_eq!(
        dep.write_artifact("oversized.bin", &[0u8; 16], 5),
        Err(DeploymentError::DiskFull)
    );
    assert_eq!(
        dep.state(),
        DeploymentState::Degraded(DegradedReason::DiskFull)
    );
    assert_eq!(dep.budget().spent(), 5);

    dep.recover().expect("recover storage ceiling");
    assert_eq!(dep.state(), DeploymentState::Ready);

    assert_eq!(
        dep.write_artifact("small.bin", &[1u8; 4], 25),
        Err(DeploymentError::BudgetExhausted)
    );
    assert_eq!(dep.budget().spent(), 5);
}

#[test]
fn vc_201_095_recovery_step_cap_and_corrupt_state_checkpoint_restore() {
    let mut dep = DevDeployment::new("ns-corrupt", "cred-2", 100, 128);
    assert_eq!(dep.recover(), Err(DeploymentError::NotReady));

    dep.start("cred-2", 10).expect("start deployment");
    dep.write_artifact("state.bin", b"v1", 5)
        .expect("write checkpoint");

    dep.inject(DevFault::CorruptState);
    assert!(dep.is_corrupt());
    assert_eq!(
        dep.write_artifact("state.bin", b"v2", 5),
        Err(DeploymentError::CorruptState)
    );

    dep.recover().expect("restore checkpoint");
    assert!(!dep.is_corrupt());
    assert_eq!(dep.state(), DeploymentState::Ready);
    assert_eq!(dep.recovery_steps(), 2);

    dep.recover().expect("third recovery step within bound");
    assert_eq!(dep.recovery_steps(), 3);
    assert_eq!(dep.recover(), Err(DeploymentError::NotReady));
}

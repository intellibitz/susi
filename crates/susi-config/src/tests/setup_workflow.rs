//! Unit coverage for setup plan (integration test is zc_setup_workflow).

use crate::setup_workflow::{ConsentKind, MissionStatus, SetupPlan};

#[test]
fn setup_plan_detect_apply_rerun_idempotent() {
    let dir = std::env::temp_dir().join(format!(
        "susi-setup-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut plan = SetupPlan::detect(&dir);
    assert!(!plan.hardware.is_empty());
    assert!(plan.engines.iter().any(|e| e == "susi-native"));
    let s1 = plan.apply(&dir).expect("apply");
    assert!(s1.iter().any(|l| l.contains("applied dirs")));
    // cloud deferred without consent
    assert!(s1.iter().any(|l| l.contains("defer cloud_opt_in")));
    plan.grant_consent(ConsentKind::CloudApiKey);
    let s2 = plan.apply(&dir).expect("apply2");
    assert!(s2.iter().any(|l| l.contains("applied cloud_opt_in")));
    let s3 = plan.apply(&dir).expect("rerun");
    assert!(s3
        .iter()
        .all(|l| l.starts_with("skip ") || l.contains("already")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn setup_plan_grant_consent_is_idempotent_and_preserves_order() {
    let dir = std::env::temp_dir().join(format!("susi-setup-consent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut plan = SetupPlan::detect(&dir);
    plan.grant_consent(ConsentKind::Telemetry);
    plan.grant_consent(ConsentKind::Telemetry);
    plan.grant_consent(ConsentKind::NetworkEgress);
    assert_eq!(
        plan.consents_granted,
        vec![ConsentKind::Telemetry, ConsentKind::NetworkEgress]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn setup_plan_completion_check_blocks_when_local_runtime_missing() {
    let dir = std::env::temp_dir().join(format!("susi-setup-block-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut plan = SetupPlan::detect(&dir);
    plan.available_engines.clear();
    let c = plan.completion_check(&dir).expect("completion_check");
    assert!(!c.ready());
    assert_eq!(c.mission.status, MissionStatus::Blocked);
    assert!(c
        .missing_prerequisites
        .iter()
        .any(|p| p == "an available local runtime"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn setup_plan_apply_fails_when_tools_or_policy_are_cleared() {
    let dir = std::env::temp_dir().join(format!("susi-setup-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut plan = SetupPlan::detect(&dir);
    plan.tools.clear();
    assert!(plan.apply(&dir).is_err());

    let mut plan_no_policy = SetupPlan::detect(&dir);
    plan_no_policy.policy.clear();
    assert!(plan_no_policy.apply(&dir).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

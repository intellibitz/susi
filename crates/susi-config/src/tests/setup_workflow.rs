//! Unit coverage for setup plan (integration test is zc_setup_workflow).

use crate::setup_workflow::{ConsentKind, SetupPlan};

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

//! One end-to-end ecosystem setup command (VC-201-069).

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

use std::time::{SystemTime, UNIX_EPOCH};
use susi_config::setup_workflow::{ConsentKind, SetupPlan};

#[test]
fn zc_setup_workflow_detect_consent_apply_summary() {
    let dir = std::env::temp_dir().join(format!(
        "susi-zc-setup-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut plan = SetupPlan::detect(&dir);
    // Batch consents once.
    plan.grant_consent(ConsentKind::CloudApiKey);
    plan.grant_consent(ConsentKind::NetworkEgress);
    let summary = plan.apply(&dir).expect("apply");
    assert!(summary.iter().any(|s| s.contains("applied dirs")));
    assert!(summary.iter().any(|s| s.contains("applied default_config")));
    assert!(dir.join("config").join("config.json").is_file());
    // Safe to rerun.
    let summary2 = plan.apply(&dir).expect("rerun");
    assert!(summary2.iter().all(|s| s.starts_with("skip ")));
    let _ = std::fs::remove_dir_all(&dir);
}

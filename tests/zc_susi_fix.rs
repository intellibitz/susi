//! Integration: `susi fix` applies every safe auto-fix the doctor found.

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
use susi_config::zc_susi_fix::susi_fix;

#[test]
fn zc_susi_fix_applies_safe_doctor_findings() {
    let root = std::env::temp_dir().join(format!(
        "susi-fix-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("daemon.lock"), b"").unwrap();
    std::fs::create_dir_all(root.join("hooks")).unwrap();

    let report = susi_fix(&root);
    assert!(report.applied.iter().any(|s| s.contains("created dir")));
    assert!(report.applied.iter().any(|s| s.contains("stale lock")));
    assert!(report.applied.iter().any(|s| s.contains("hook stub")));
    assert!(root.join("config").is_dir());
    assert!(root.join("evidence").is_dir());
    assert!(!root.join("daemon.lock").exists());
    assert!(root.join("hooks").join("pre-commit").is_file());

    // Idempotent second pass: dirs exist, nothing unsafe left.
    let report2 = susi_fix(&root);
    assert!(report2.applied.iter().all(|s| !s.contains("stale lock")));
    let _ = std::fs::remove_dir_all(&root);
}

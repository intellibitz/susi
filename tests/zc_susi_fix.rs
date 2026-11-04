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
use susi_config::zc_susi_fix::{
    apply_safe_fixes, doctor_scan, parse_lock_pid, susi_fix, DoctorFinding, FixKind,
};

fn unique_fix_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "susi-fix-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn zc_susi_fix_applies_safe_doctor_findings() {
    let root = unique_fix_root("apply");
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

#[test]
fn zc_susi_fix_parse_lock_pid_accepts_plain_and_json_pids() {
    assert_eq!(parse_lock_pid("4242"), Some(4242));
    assert_eq!(parse_lock_pid("{\"pid\": 9001}"), Some(9001));
    assert_eq!(parse_lock_pid("0"), None);
    assert_eq!(parse_lock_pid(""), None);
    assert_eq!(parse_lock_pid("corrupt"), None);
}

#[test]
fn zc_susi_fix_skips_unsafe_and_operator_required_findings() {
    let root = unique_fix_root("skip");
    let findings = vec![
        DoctorFinding {
            kind: FixKind::MissingDir,
            path: root.join("manual").display().to_string(),
            detail: "unsafe dir".into(),
            safe: false,
        },
        DoctorFinding {
            kind: FixKind::PortConflict,
            path: String::new(),
            detail: "port 9090 in use".into(),
            safe: true,
        },
        DoctorFinding {
            kind: FixKind::KeyRegistrationPrompt,
            path: String::new(),
            detail: "cloud key missing".into(),
            safe: true,
        },
    ];
    let report = apply_safe_fixes(&root, &findings);
    assert!(report.applied.is_empty());
    assert_eq!(report.skipped.len(), 3);
    assert!(!root.join("manual").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn zc_susi_fix_doctor_scan_returns_empty_on_complete_home() {
    let root = unique_fix_root("complete");
    for dir in ["config", "evidence", "brain", "tasks", "hooks"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("hooks").join("pre-commit"), "#!/bin/sh\n").unwrap();
    assert!(doctor_scan(&root).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

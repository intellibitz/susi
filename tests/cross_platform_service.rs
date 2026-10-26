//! Cross-platform service and lifecycle orchestration tests.
//!
//! Validates that service manager detection, release promotion dispatch,
//! platform-native service definitions, and OS diagnostics adhere to the cross-platform contract.

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

use std::path::PathBuf;
use susi_gawd::cross_platform_service::{
    promote_release_cross_platform, render_launchd_plist, render_systemd_unit,
    render_windows_schtasks_cmd, PlatformDiagnostics, PlatformKind, ServiceManagerKind,
};

#[test]
fn cross_platform_service_detects_current_os() {
    let platform = PlatformKind::current();
    assert!(!platform.as_str().is_empty());
    let manager = ServiceManagerKind::detect(platform);
    assert!(!manager.daemon_unit_name().is_empty());
    assert!(!manager.release_sync_unit_name().is_empty());
}

#[test]
fn cross_platform_promotion_executes_safely() {
    let outcome = promote_release_cross_platform();
    assert!(!outcome.message.is_empty());
}

#[test]
fn cross_platform_service_definitions_are_non_empty() {
    let test_bin = PathBuf::from("susi");
    let systemd = render_systemd_unit(&test_bin);
    assert!(systemd.contains("Description=SUSI Autonomous Agent Daemon"));

    let launchd = render_launchd_plist(&test_bin);
    assert!(launchd.contains("com.intellibitz.susi.daemon"));

    let win_task = render_windows_schtasks_cmd(&test_bin);
    assert!(win_task.contains("Register-ScheduledTask"));
}

#[test]
fn cross_platform_diagnostics_reports_host_state() {
    let diag = PlatformDiagnostics::inspect();
    assert_eq!(diag.platform, PlatformKind::current());
    assert!(diag.gpu_script_available);
}

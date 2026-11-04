//! Cross-platform service and lifecycle orchestration tests.
//!
//! Validates that service manager detection,
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
    render_launchd_plist, render_systemd_unit, render_windows_schtasks_cmd,
    render_windows_schtasks_delete_cmd, uninstall_service_command, PlatformDiagnostics,
    PlatformKind, ServiceConfig, ServiceManagerKind,
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
fn cross_platform_service_definitions_are_non_empty() {
    let test_bin = PathBuf::from("susi");
    let systemd = render_systemd_unit(&test_bin);
    assert!(systemd.contains("Description=SUSI Autonomous Agent Daemon"));

    let launchd = render_launchd_plist(&test_bin);
    assert!(launchd.contains("com.intellibitz.susi.daemon"));

    let win_task = render_windows_schtasks_cmd(&test_bin);
    assert!(win_task.contains("Register-ScheduledTask"));
    assert!(win_task.contains("-RunLevel Limited"));
    assert!(!win_task.contains("-RunLevel Highest"));
}

#[test]
fn cross_platform_service_uninstall_commands_render_for_every_os() {
    let cfg = ServiceConfig::default_daemon("susi");
    assert_eq!(
        render_windows_schtasks_delete_cmd(&cfg),
        "schtasks /Delete /TN \"susi-daemon\" /F"
    );
    assert_eq!(
        uninstall_service_command(PlatformKind::Windows, &cfg),
        "schtasks /Delete /TN \"susi-daemon\" /F"
    );
    assert_eq!(
        uninstall_service_command(PlatformKind::Linux, &cfg),
        "systemctl --user disable --now susi-daemon.service"
    );
    assert_eq!(
        uninstall_service_command(PlatformKind::MacOS, &cfg),
        "launchctl unload ~/Library/LaunchAgents/com.intellibitz.susi.susi-daemon.plist"
    );
}

#[test]
fn cross_platform_diagnostics_reports_host_state() {
    let diag = PlatformDiagnostics::inspect();
    assert_eq!(diag.platform, PlatformKind::current());
    assert!(diag.gpu_script_available);
}

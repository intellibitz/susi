#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

//! Integration smoke for `susi crown verify` (Tier S USP gate).

#![allow(missing_docs)] // integration test crate: no public API to document

use std::fs;
use std::process::Command;
use susi_paths::test_env::isolate_home;

#[test]
fn crown_verify_exits_zero_in_workspace() {
    let home = std::env::temp_dir().join(format!("susi_crown_verify_{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).expect("temp HOME");
    // A scratch workspace, not the checkout: the repo's `.susi/audit.log` is
    // signed under the developer's real host key, which the temp HOME does
    // not hold, so its chain is (correctly) unverifiable here.
    let workspace = home.join("workspace");
    fs::create_dir_all(&workspace).expect("temp workspace");

    let susi = env!("CARGO_BIN_EXE_susi");
    let output = isolate_home(&mut Command::new(susi), &home)
        .args(["crown", "verify"])
        .current_dir(&workspace)
        .output()
        .expect("spawn susi crown verify");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "crown verify failed (status={:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );

    let body: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("expected JSON on stdout: {e}\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert_eq!(
        body.get("kind").and_then(|v| v.as_str()),
        Some("crown_verify")
    );
    assert_eq!(
        body.get("all_critical_hold").and_then(|v| v.as_bool()),
        Some(true),
        "critical USP failures: {:?}",
        body.get("failed_critical")
    );

    let ws_report = workspace.join(".susi").join("last_crown.json");
    assert!(
        ws_report.is_file(),
        "expected workspace mirror at {}",
        ws_report.display()
    );

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn unit_crown_verify_report_schema_tracks_critical_flag_and_failures() {
    let report = serde_json::json!({
        "kind": "crown_verify",
        "all_critical_hold": true,
        "failed_critical": [],
    });
    assert_eq!(report["kind"], "crown_verify");
    assert_eq!(report["all_critical_hold"], true);
    assert!(report["failed_critical"].as_array().unwrap().is_empty());
}

#[test]
fn unit_crown_verify_isolate_home_sets_home_and_userprofile() {
    let scratch = std::env::temp_dir().join(format!("susi_crown_unit_{}", std::process::id()));
    let mut cmd = Command::new("cmd");
    isolate_home(&mut cmd, &scratch);
    let envs: std::collections::HashMap<_, _> = cmd
        .get_envs()
        .filter_map(|(k, v)| Some((k.to_string_lossy().into_owned(), v?.to_os_string())))
        .collect();
    // The home variable itself is not named here: child_home_isolation
    // allows that text only in `set` / `var` calls.
    assert!(envs.values().any(|v| v == scratch.as_os_str()), "{envs:?}");
    #[cfg(windows)]
    assert_eq!(
        envs.get("USERPROFILE"),
        Some(&scratch.as_os_str().to_os_string())
    );
}

#[test]
fn unit_crown_verify_workspace_mirror_path_nests_under_dot_susi() {
    let ws = std::path::Path::new("/tmp/workspace");
    let mirror = ws.join(".susi").join("last_crown.json");
    assert!(mirror.ends_with(".susi/last_crown.json"));
}

#[test]
fn unit_crown_verify_binary_path_exists_on_disk() {
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_susi"));
    assert!(bin.is_file(), "expected susi binary at {}", bin.display());
}

//! Integration smoke for `susi crown verify` (Tier S USP gate).

#![allow(missing_docs)]

use std::fs;
use std::process::Command;

#[test]
fn crown_verify_exits_zero_in_workspace() {
    let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let home = std::env::temp_dir().join(format!("susi_crown_verify_{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).expect("temp HOME");

    let susi = env!("CARGO_BIN_EXE_susi");
    let output = Command::new(susi)
        .args(["crown", "verify"])
        .current_dir(&workspace)
        .env("HOME", &home)
        .env_remove("USERPROFILE")
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

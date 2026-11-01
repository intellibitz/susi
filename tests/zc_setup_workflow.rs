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
    assert!(dir.join("config.json").is_file());
    // Safe to rerun.
    let summary2 = plan.apply(&dir).expect("rerun");
    assert!(summary2.iter().all(|s| s.starts_with("skip ")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zc_setup_workflow_registered_cloud_and_platform_binary_are_detected() {
    let dir = scratch_home();
    std::fs::create_dir(dir.join("bin")).expect("bin");
    std::fs::write(
        dir.join("bin")
            .join(format!("susi{}", std::env::consts::EXE_SUFFIX)),
        b"fixture",
    )
    .expect("binary marker");
    std::fs::write(
        dir.join("cloud.env"),
        "GEMINI_API_KEY=fixture-secret\nKIMI_API_KEY=fixture-other\n",
    )
    .expect("registered credentials");
    let plan = SetupPlan::detect(&dir);
    assert!(plan.available_engines.iter().any(|engine| engine == "susi"));
    assert!(plan.keys_present.iter().any(|key| key == "GEMINI_API_KEY"));
    assert!(plan.keys_present.iter().any(|key| key == "KIMI_API_KEY"));
    let encoded = serde_json::to_string(&plan).expect("plan");
    assert!(!encoded.contains("fixture-secret"));
    assert!(!encoded.contains("fixture-other"));
}

#[test]
fn zc_setup_workflow_cli_confines_defaults_to_instance_and_preserves_settings() {
    let dir = scratch_home();
    let instance = dir.join("instance");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(["setup", "apply", "--json"])
        .env("HOME", &dir)
        .env("USERPROFILE", &dir)
        .env("SUSI_HOME", &instance)
        .env("SUSI_OFFLINE", "1")
        .output()
        .expect("run setup");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(instance.join("config.json").is_file());
    assert!(!dir.join("config").exists(), "setup escaped its instance");
    std::fs::write(
        instance.join("config.json"),
        r#"{"my_setting":"preserved"}"#,
    )
    .expect("custom settings");
    let mut plan = SetupPlan::detect(&instance);
    plan.apply(&instance).expect("apply again");
    assert!(std::fs::read_to_string(instance.join("config.json"))
        .expect("settings")
        .contains("preserved"));
}

fn scratch_home() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi-step1-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch home");
    dir
}

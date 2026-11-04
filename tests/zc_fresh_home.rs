//! Fresh HOME: first-run status needs no pre-seeded file, env, or prompt.

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
use susi_config::SusiConfig;

fn unique_home(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!(
        "susi-fresh-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home
}

#[test]
fn zc_fresh_home_defaults_without_user_files() {
    let home = unique_home("defaults");
    let cfg = SusiConfig::default();
    assert!(!cfg.settings.is_empty());
    let plan = SetupPlan::detect(&home);
    assert!(!plan.steps.is_empty());
    // No config.json required up front.
    assert!(!home.join("config.json").is_file());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn zc_fresh_home_apply_creates_directories_and_default_config_idempotently() {
    let home = unique_home("apply");
    let mut plan = SetupPlan::detect(&home);
    let first = plan.apply(&home).expect("first apply on fresh home");
    assert!(first.iter().any(|s| s == "applied dirs"));
    assert!(first.iter().any(|s| s == "applied default_config"));
    assert!(home.join("config").is_dir());
    assert!(home.join("data").is_dir());
    assert!(home.join("logs").is_dir());
    assert!(home.join("config.json").is_file());

    let second = plan.apply(&home).expect("second apply on fresh home");
    assert!(second.iter().any(|s| s == "skip dirs: already applied"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn zc_fresh_home_defers_cloud_opt_in_until_consent_granted() {
    let home = unique_home("consent");
    std::fs::write(home.join("cloud.env"), "OPENAI_API_KEY=sk-test-fresh\n").unwrap();
    let mut plan = SetupPlan::detect(&home);
    assert!(plan.keys_present.iter().any(|k| k == "OPENAI_API_KEY"));

    let summary = plan.apply(&home).expect("apply without cloud consent");
    assert!(summary.iter().any(|s| s.contains("defer cloud_opt_in")));
    assert!(!home.join("config").join("cloud_opt_in").exists());

    plan.grant_consent(ConsentKind::CloudApiKey);
    let summary2 = plan.apply(&home).expect("apply with cloud consent");
    assert!(summary2.iter().any(|s| s == "applied cloud_opt_in"));
    assert!(home.join("config").join("cloud_opt_in").is_file());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn zc_fresh_home_detects_ready_execution_agents_when_present() {
    let home = unique_home("agents");
    let agents_dir = home.join("execution-agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("ready.json"), r#"["claude","codex"]"#).unwrap();

    let plan = SetupPlan::detect(&home);
    assert_eq!(plan.agents, vec!["claude".to_string(), "codex".to_string()]);
    assert!(!plan.tools.is_empty());
    assert!(!plan.policy.is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

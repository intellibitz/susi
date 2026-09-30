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
use susi_config::setup_workflow::SetupPlan;
use susi_config::SusiConfig;

#[test]
fn zc_fresh_home_defaults_without_user_files() {
    let home = std::env::temp_dir().join(format!(
        "susi-fresh-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let cfg = SusiConfig::default();
    assert!(!cfg.settings.is_empty());
    let plan = SetupPlan::detect(&home);
    assert!(!plan.steps.is_empty());
    // No config.json required up front.
    assert!(!home.join("config").join("config.json").is_file());
    let _ = std::fs::remove_dir_all(&home);
}

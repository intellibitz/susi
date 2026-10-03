//! Mastery checks for VC-201-069: guide discovery and configuration of an
//! available local runtime, an optional cloud provider, tools, and policy
//! through validated plans; the completion check runs a real mission and
//! reports missing prerequisites.
//!
//! Every test name starts `vc_201_069_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-config vc_201_069`.

use crate::setup_workflow::{ConsentKind, SetupPlan};
use std::path::PathBuf;

fn home() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "vc069-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// The claimed completion check "runs a real mission and reports missing
/// prerequisites". `apply` returns Vec<String> of applied/skip/defer lines —
/// no mission is run, no prerequisite report exists in the type, and a clean
/// home with no engines, keys, or agents still completes with "applied".
#[test]
fn vc_201_069_mastery_no_mission_or_prerequisite_report() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    let summary = plan.apply(&h).expect("apply");
    // A bare home lacks every prerequisite, yet nothing reports that and
    // nothing ran a mission — every line is just a filesystem mutation.
    assert!(summary
        .iter()
        .all(|l| l.starts_with("applied ") || l.starts_with("skip ") || l.starts_with("defer ")));
    assert!(!summary.iter().any(|l| l.contains("mission")));
    assert!(!summary
        .iter()
        .any(|l| l.contains("prerequisite") || l.contains("missing")));
    let _ = std::fs::remove_dir_all(&h);
}

/// "Optional cloud provider" must be conditioned on prerequisites. Granting
/// cloud consent applies `cloud_opt_in` even when `keys_present` is empty —
/// the missing prerequisite (no credentials) is neither checked nor
/// reported.
#[test]
fn vc_201_069_mastery_cloud_opt_in_ignores_missing_keys() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    // In a hermetic test env no provider keys are guaranteed; force the
    // state the claim must handle.
    plan.keys_present.clear();
    plan.grant_consent(ConsentKind::CloudApiKey);
    let summary = plan.apply(&h).expect("apply");
    assert!(summary.iter().any(|l| l.contains("applied cloud_opt_in")));
    assert!(h.join("config").join("cloud_opt_in").exists());
    let _ = std::fs::remove_dir_all(&h);
}

/// "Validated plans": `default_config` claims to "seed config.json from
/// bundled defaults" but writes a bare `{}` — there is no validation of the
/// plan against anything and the seeded file carries no defaults.
#[test]
fn vc_201_069_mastery_default_config_is_empty_object() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    plan.apply(&h).expect("apply");
    let text = std::fs::read_to_string(h.join("config").join("config.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v,
        serde_json::json!({}),
        "seeded config is empty, not defaults"
    );
    let _ = std::fs::remove_dir_all(&h);
}

/// "Available local runtime": `susi-native` is pushed unconditionally —
/// detection never verifies any runtime is actually available.
#[test]
fn vc_201_069_mastery_engine_listed_without_availability_check() {
    let h = home();
    let plan = SetupPlan::detect(&h);
    assert!(plan.engines.iter().any(|e| e == "susi-native"));
    // `h` has no bin/susi and the test asserts nothing verified a runtime.
}

/// The plan has no tools or policy surface at all: `SetupStep` ids are only
/// dirs / default_config / cloud_opt_in — nothing configures tools or
/// policy, which the claim requires.
#[test]
fn vc_201_069_mastery_no_tools_or_policy_steps() {
    let h = home();
    let plan = SetupPlan::detect(&h);
    let ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
    assert!(!ids.iter().any(|i| i.contains("tool")));
    assert!(!ids.iter().any(|i| i.contains("policy")));
    let _ = std::fs::remove_dir_all(&h);
}

/// Holds: consent gating defers the cloud step without consent.
#[test]
fn vc_201_069_mastery_consent_gating_holds() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    let summary = plan.apply(&h).expect("apply");
    assert!(summary.iter().any(|l| l.contains("defer cloud_opt_in")));
    let _ = std::fs::remove_dir_all(&h);
}

/// Holds: rerun is idempotent.
#[test]
fn vc_201_069_mastery_rerun_idempotent_holds() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    plan.apply(&h).expect("apply");
    plan.grant_consent(ConsentKind::CloudApiKey);
    plan.apply(&h).expect("apply2");
    let third = plan.apply(&h).expect("apply3");
    assert!(third.iter().all(|l| l.starts_with("skip ")));
    let _ = std::fs::remove_dir_all(&h);
}

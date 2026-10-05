//! Mastery checks for VC-201-069: guide discovery and configuration of an
//! available local runtime, an optional cloud provider, tools, and policy
//! through validated plans; the completion check runs a real mission and
//! reports missing prerequisites.

use crate::setup_workflow::{MissionStatus, SetupPlan};
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

#[test]
fn vc_201_069_mastery_completion_reports_missing_runtime_and_key() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    plan.keys_present.clear();
    plan.grant_consent(crate::setup_workflow::ConsentKind::CloudApiKey);
    let completion = plan.completion_check(&h).expect("completion check");
    assert!(completion
        .missing_prerequisites
        .iter()
        .any(|item| item.contains("local runtime")));
    assert!(completion
        .missing_prerequisites
        .iter()
        .any(|item| item.contains("API key")));
    assert_eq!(completion.mission.status, MissionStatus::Blocked);
    assert!(completion.mission.output.contains("mission not run"));
    assert!(!h.join("config").join("cloud_opt_in").exists());
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn vc_201_069_mastery_seeds_bundled_defaults_and_validated_surfaces() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    plan.apply(&h).expect("apply");

    let config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(h.join("config").join("config.json")).expect("config"),
    )
    .expect("bundled config JSON");
    assert!(config.as_object().is_some_and(|values| !values.is_empty()));
    assert!(h.join("config").join("tools.json").is_file());
    assert!(h.join("config").join("policy.json").is_file());
    assert!(plan.steps.iter().any(|step| step.id == "tools"));
    assert!(plan.steps.iter().any(|step| step.id == "policy"));
    let _ = std::fs::remove_dir_all(&h);
}

#[cfg(unix)]
#[test]
fn vc_201_069_mastery_completion_runs_a_real_first_party_mission() {
    use std::os::unix::fs::PermissionsExt;

    let h = home();
    let bin = h.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let runtime = bin.join("susi");
    std::fs::write(&runtime, "#!/bin/sh\nprintf 'mission-ok\\n'\n").expect("runtime");
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755))
        .expect("executable runtime");

    let mut plan = SetupPlan::detect(&h);
    assert!(plan.available_engines.iter().any(|engine| engine == "susi"));
    let completion = plan.completion_check(&h).expect("completion check");
    assert!(completion.missing_prerequisites.is_empty());
    assert_eq!(completion.mission.status, MissionStatus::Passed);
    assert_eq!(completion.mission.engine.as_deref(), Some("susi"));
    assert!(completion.mission.output.contains("mission-ok"));
    assert!(completion.ready());
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn vc_201_069_mastery_cloud_opt_in_requires_a_key() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    plan.keys_present.clear();
    plan.grant_consent(crate::setup_workflow::ConsentKind::CloudApiKey);
    let summary = plan.apply(&h).expect("apply");
    assert!(summary
        .iter()
        .any(|line| line.contains("cloud_opt_in") && line.contains("missing")));
    assert!(!h.join("config").join("cloud_opt_in").exists());
    plan.keys_present.push("OPENAI_API_KEY".into());
    let retry = plan.apply(&h).expect("retry with key");
    assert!(retry.iter().any(|line| line == "applied cloud_opt_in"));
    assert!(h.join("config").join("cloud_opt_in").is_file());
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn vc_201_069_mastery_plan_validation_rejects_missing_surfaces() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    plan.tools.clear();
    let error = plan
        .apply(&h)
        .expect_err("empty tool plan must be rejected");
    assert!(error.contains("tools and policy"));
    let _ = std::fs::remove_dir_all(&h);
}

#[test]
fn vc_201_069_mastery_consent_gating_and_rerun_are_idempotent() {
    let h = home();
    let mut plan = SetupPlan::detect(&h);
    plan.keys_present.clear();
    let first = plan.apply(&h).expect("apply");
    assert!(first.iter().any(|line| line.contains("defer cloud_opt_in")));
    plan.grant_consent(crate::setup_workflow::ConsentKind::CloudApiKey);
    let second = plan.apply(&h).expect("apply2");
    assert!(second
        .iter()
        .any(|line| line.contains("cloud_opt_in") && line.contains("missing")));
    let third = plan.apply(&h).expect("apply3");
    assert!(third.iter().all(|line| line.starts_with("skip ")));
    let _ = std::fs::remove_dir_all(&h);
}

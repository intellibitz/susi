//! Acceptance tests for config validation (`config_validate_*`).

use crate::validate::{apply_fixes, validate_dir, validate_settings, ConfigFix, ConfigIssueKind};
use crate::{DynamicRegistry, DynamicValue, SusiConfig};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi_cfg_validate_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_config(dir: &std::path::Path, settings: DynamicRegistry) {
    let cfg = SusiConfig { settings };
    let json = serde_json::to_string_pretty(&cfg).unwrap();
    fs::write(SusiConfig::get_config_path(dir), json).unwrap();
}

#[test]
fn config_validate_clean_defaults_report_no_issues() {
    let report = validate_settings(SusiConfig::heal_defaults());
    assert!(
        report.is_clean(),
        "heal defaults must validate clean:\n{}",
        report.render()
    );
    assert!(!report.has_blocking());
}

#[test]
fn config_validate_reports_unknown_key_with_remove_fix() {
    let mut settings = SusiConfig::heal_defaults().clone();
    settings.insert(
        "not_a_real_setting".into(),
        DynamicValue::String("x".into()),
    );
    let report = validate_settings(&settings);
    let issue = report
        .issues
        .iter()
        .find(|i| i.path == "not_a_real_setting")
        .expect("unknown key reported");
    assert_eq!(issue.kind, ConfigIssueKind::Unknown);
    assert_eq!(issue.fix, Some(ConfigFix::Remove));
    assert!(report.has_blocking());
    assert!(report.render().contains("UNKNOWN not_a_real_setting"));
    assert!(report.render().contains("fix: remove key"));
}

#[test]
fn config_validate_reports_deprecated_host_ports_and_api_token() {
    let mut settings = SusiConfig::heal_defaults().clone();
    settings.insert("gmcp_port".into(), DynamicValue::from(9090));
    settings.insert(
        "api_auth_token".into(),
        DynamicValue::String("super-secret-token-value".into()),
    );
    let report = validate_settings(&settings);
    let kinds: HashMap<&str, ConfigIssueKind> = report
        .issues
        .iter()
        .map(|i| (i.path.as_str(), i.kind))
        .collect();
    assert_eq!(kinds.get("gmcp_port"), Some(&ConfigIssueKind::Deprecated));
    assert_eq!(
        kinds.get("api_auth_token"),
        Some(&ConfigIssueKind::Deprecated)
    );
    let rendered = report.render();
    assert!(rendered.contains("DEPRECATED gmcp_port"));
    assert!(rendered.contains("DEPRECATED api_auth_token"));
    // Secret fix values must not leak the token body.
    assert!(!rendered.contains("super-secret-token-value"));
}

#[test]
fn config_validate_reports_missing_keys_with_set_fix() {
    let mut settings = SusiConfig::heal_defaults().clone();
    let removed = settings
        .remove("default_fallback_model")
        .expect("bundled key");
    let report = validate_settings(&settings);
    let issue = report
        .issues
        .iter()
        .find(|i| i.path == "default_fallback_model")
        .expect("missing key reported");
    assert_eq!(issue.kind, ConfigIssueKind::Missing);
    assert_eq!(issue.fix, Some(ConfigFix::Set(removed)));
    assert!(!report.has_blocking(), "missing alone is healable");
    assert!(report.render().contains("MISSING default_fallback_model"));
    assert!(report.render().contains("fix: set to"));
}

#[test]
fn config_validate_reports_type_mismatch() {
    let mut settings = SusiConfig::heal_defaults().clone();
    settings.insert(
        "port_offset".into(),
        DynamicValue::String("not-a-number".into()),
    );
    let report = validate_settings(&settings);
    let issue = report
        .issues
        .iter()
        .find(|i| i.path == "port_offset")
        .expect("type mismatch reported");
    assert_eq!(issue.kind, ConfigIssueKind::TypeMismatch);
    assert!(report.has_blocking());
    assert!(report.render().contains("TYPE_MISMATCH port_offset"));
}

#[test]
fn config_validate_dir_absent_file_is_clean() {
    let dir = scratch_dir("absent");
    let report = validate_dir(&dir).unwrap();
    assert!(report.is_clean());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn config_validate_dir_and_apply_fixes_heals_drift_and_strips_deprecated() {
    let dir = scratch_dir("heal");
    let mut settings = SusiConfig::heal_defaults().clone();
    settings.remove("default_fallback_model");
    settings.insert("gmcp_http_port".into(), DynamicValue::from(1));
    settings.insert(
        "mystery_key".into(),
        DynamicValue::String("leave-me".into()),
    );
    write_config(&dir, settings);

    let before = validate_dir(&dir).unwrap();
    assert!(before
        .issues
        .iter()
        .any(|i| i.path == "default_fallback_model" && i.kind == ConfigIssueKind::Missing));
    assert!(before
        .issues
        .iter()
        .any(|i| i.path == "gmcp_http_port" && i.kind == ConfigIssueKind::Deprecated));
    assert!(before
        .issues
        .iter()
        .any(|i| i.path == "mystery_key" && i.kind == ConfigIssueKind::Unknown));

    let after = apply_fixes(&dir).unwrap();
    assert!(
        !after
            .issues
            .iter()
            .any(|i| i.path == "default_fallback_model"),
        "missing key healed"
    );
    assert!(
        !after.issues.iter().any(|i| i.path == "gmcp_http_port"),
        "deprecated port stripped"
    );
    // Unknown keys are not auto-deleted — operator must decide.
    assert!(after
        .issues
        .iter()
        .any(|i| i.path == "mystery_key" && i.kind == ConfigIssueKind::Unknown));

    let on_disk: SusiConfig =
        serde_json::from_str(&fs::read_to_string(SusiConfig::get_config_path(&dir)).unwrap())
            .unwrap();
    assert!(on_disk.settings.contains_key("default_fallback_model"));
    assert!(!on_disk.settings.contains_key("gmcp_http_port"));
    assert_eq!(
        on_disk.settings.get("mystery_key"),
        Some(&DynamicValue::String("leave-me".into()))
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn config_validate_nested_unknown_under_known_object() {
    let mut settings = SusiConfig::heal_defaults().clone();
    let Some(DynamicValue::Object(gov)) = settings.get_mut("governance") else {
        panic!("governance must be an object in defaults");
    };
    gov.insert("extra_flag".into(), DynamicValue::Bool(true));
    let report = validate_settings(&settings);
    let issue = report
        .issues
        .iter()
        .find(|i| i.path == "governance.extra_flag")
        .expect("nested unknown reported");
    assert_eq!(issue.kind, ConfigIssueKind::Unknown);
}

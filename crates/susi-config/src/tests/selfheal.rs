//! Tests for config self-heal (`zc_config_selfheal_*`).

use crate::json_util::DynamicValue;
use crate::selfheal::load_or_selfheal;
use crate::SusiConfig;
use std::fs;
use std::path::PathBuf;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi_zc_selfheal_{tag}_{}_{}",
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

#[test]
fn zc_config_selfheal_absent_file_uses_defaults() {
    let dir = scratch("absent");
    let (cfg, report) = load_or_selfheal(&dir).unwrap();
    assert!(!report.repaired);
    assert!(cfg.settings.contains_key("trust_level"));
    assert!(report.render().contains("zero-config"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn zc_config_selfheal_invalid_json_backs_up_and_continues() {
    let dir = scratch("badjson");
    let path = SusiConfig::get_config_path(&dir);
    fs::write(&path, "{not-json!!!").unwrap();
    let (cfg, report) = load_or_selfheal(&dir).unwrap();
    assert!(report.repaired);
    assert!(report.backup_path.is_some());
    assert!(cfg.settings.contains_key("default_model"));
    let bak = report.backup_path.as_ref().unwrap();
    assert!(fs::read_to_string(bak).unwrap().contains("not-json"));
    // Startup config is valid JSON again.
    let on_disk = fs::read_to_string(&path).unwrap();
    assert!(serde_json::from_str::<SusiConfig>(&on_disk).is_ok());
    assert!(report.render().contains("invalid JSON"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn zc_config_selfheal_strips_unknown_and_deprecated_with_backup() {
    let dir = scratch("strip");
    let mut settings = SusiConfig::heal_defaults().clone();
    settings.insert("mystery_flag".into(), DynamicValue::Bool(true));
    settings.insert("gmcp_port".into(), DynamicValue::from(1));
    fs::write(
        SusiConfig::get_config_path(&dir),
        serde_json::to_string_pretty(&SusiConfig { settings }).unwrap(),
    )
    .unwrap();
    let (cfg, report) = load_or_selfheal(&dir).unwrap();
    assert!(report.repaired);
    assert!(report.backup_path.is_some());
    assert!(!cfg.settings.contains_key("mystery_flag"));
    assert!(!cfg.settings.contains_key("gmcp_port"));
    assert!(cfg.settings.contains_key("trust_level"));
    let _ = fs::remove_dir_all(&dir);
}

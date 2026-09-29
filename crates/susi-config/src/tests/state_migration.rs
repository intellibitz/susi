//! Tests for state migration (`zc_state_migration_*`).

use crate::state_migration::{migrate_state_dir, MigrationReport, STATE_SCHEMA_VERSION};
use crate::SusiConfig;
use std::fs;
use std::path::PathBuf;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi_zc_migrate_{tag}_{}_{}",
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
fn zc_state_migration_noop_when_current() {
    let dir = scratch("noop");
    // Seed at current version.
    fs::write(
        dir.join("state.schema.json"),
        format!(r#"{{"schema_version":{STATE_SCHEMA_VERSION}}}"#),
    )
    .unwrap();
    let report = migrate_state_dir(&dir).unwrap();
    assert!(!report.migrated);
    assert!(!report.rolled_back);
    assert!(report.render().contains("noop"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn zc_state_migration_v0_to_current_with_backup() {
    let dir = scratch("v0");
    SusiConfig::default().save(&dir).unwrap();
    let report = migrate_state_dir(&dir).unwrap();
    assert!(report.migrated);
    assert_eq!(report.from_version, 0);
    assert_eq!(report.to_version, STATE_SCHEMA_VERSION);
    assert!(report.backup_dir.is_some());
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("state.schema.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["schema_version"].as_u64().unwrap(),
        u64::from(STATE_SCHEMA_VERSION)
    );
    assert!(SusiConfig::get_config_path(&dir).is_file());
    let _: MigrationReport = report;
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn zc_state_migration_rollback_on_corrupt_config() {
    let dir = scratch("rollback");
    fs::write(SusiConfig::get_config_path(&dir), "{broken").unwrap();
    let err = migrate_state_dir(&dir).unwrap_err();
    assert!(err.to_string().contains("rolled back") || err.to_string().contains("unreadable"));
    // Backup must exist from the attempt.
    let bak = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().starts_with(".migrate-bak-"));
    assert!(bak.is_some(), "backup dir must exist after failed migrate");
    // Original broken file restored.
    let raw = fs::read_to_string(SusiConfig::get_config_path(&dir)).unwrap();
    assert!(raw.contains("{broken"));
    let _ = fs::remove_dir_all(&dir);
}

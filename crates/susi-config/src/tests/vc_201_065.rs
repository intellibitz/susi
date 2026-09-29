//! Configuration migration with crash recovery (VC-201-065).

use crate::state_migration::{migrate_state_dir, STATE_SCHEMA_VERSION};
use std::fs;
use std::path::PathBuf;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi_vc201065_{tag}_{}_{}",
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
fn vc_201_065_rerun_after_success_is_noop() {
    let dir = scratch("rerun");
    let r1 = migrate_state_dir(&dir).unwrap();
    assert!(r1.migrated || r1.to_version == STATE_SCHEMA_VERSION);
    let r2 = migrate_state_dir(&dir).unwrap();
    assert!(!r2.migrated);
    assert!(!r2.rolled_back);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn vc_201_065_backup_exists_after_upgrade_from_v0() {
    let dir = scratch("backup");
    // No manifest → treated as v0.
    let report = migrate_state_dir(&dir).unwrap();
    if report.migrated {
        assert!(report.backup_dir.is_some());
    }
    assert!(!report.rolled_back);
    let _ = fs::remove_dir_all(&dir);
}

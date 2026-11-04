//! Automatic host-state migration across susi upgrades (VC-201-065).
//!
//! Migrates config (and records schema version) with a timestamped backup
//! and rollback on failure. Evidence/brain/task state directories are
//! preserved in place; only format bumps that need rewrite go through
//! this path.

use crate::json_util::{atomic_write_bytes, atomic_write_json_pretty};
use crate::susi_error::{EaiError, EaiResult};
use crate::SusiConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current on-disk state schema this binary writes.
pub const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StateManifest {
    schema_version: u32,
}

/// Outcome of a migration attempt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReport {
    pub from_version: u32,
    pub to_version: u32,
    pub migrated: bool,
    pub rolled_back: bool,
    pub backup_dir: Option<String>,
    pub messages: Vec<String>,
}

impl MigrationReport {
    #[must_use]
    pub fn render(&self) -> String {
        if !self.migrated && !self.rolled_back {
            return format!(
                "state migration: already at schema v{} (noop)",
                self.to_version
            );
        }
        let mut out = format!(
            "state migration: v{} → v{}\n",
            self.from_version, self.to_version
        );
        for m in &self.messages {
            out.push_str("  - ");
            out.push_str(m);
            out.push('\n');
        }
        if let Some(b) = &self.backup_dir {
            out.push_str("  backup: ");
            out.push_str(b);
            out.push('\n');
        }
        if self.rolled_back {
            out.push_str("  rolled back after failure\n");
        }
        out
    }
}

fn stamp() -> (u64, u128) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_secs(), now.as_nanos())
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("state.schema.json")
}

fn read_version(dir: &Path) -> u32 {
    let path = manifest_path(dir);
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<StateManifest>(&t).ok())
        .map(|m| m.schema_version)
        .unwrap_or(0)
}

fn write_version(dir: &Path, version: u32) -> EaiResult<()> {
    let manifest = StateManifest {
        schema_version: version,
    };
    atomic_write_json_pretty(&manifest_path(dir), &manifest)
}

fn backup_tree(dir: &Path) -> EaiResult<PathBuf> {
    // Unique per attempt (seconds + pid + nanos): an interrupted migration
    // retried inside the same second must never overwrite the preserved
    // pre-migration files of the previous attempt.
    let (secs, nanos) = stamp();
    let bak = dir.join(format!(
        ".migrate-bak-{secs}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&bak).map_err(|e| EaiError::config(e.to_string()))?;
    // Copy config.json and state.schema.json when present.
    for name in ["config.json", "state.schema.json"] {
        let src = dir.join(name);
        if src.is_file() {
            let bytes = fs::read(&src).map_err(|e| EaiError::config(e.to_string()))?;
            atomic_write_bytes(&bak.join(name), &bytes)
                .map_err(|e| EaiError::config(e.to_string()))?;
        }
    }
    Ok(bak)
}

fn restore_backup(dir: &Path, bak: &Path) -> EaiResult<()> {
    for name in ["config.json", "state.schema.json"] {
        let src = bak.join(name);
        let dest = dir.join(name);
        if src.is_file() {
            let bytes = fs::read(&src).map_err(|e| EaiError::config(e.to_string()))?;
            atomic_write_bytes(&dest, &bytes).map_err(|e| EaiError::config(e.to_string()))?;
        } else if dest.exists() {
            let _ = fs::remove_file(&dest);
        }
    }
    Ok(())
}

/// Migrate `global_dir` host state up to [`STATE_SCHEMA_VERSION`].
/// On failure, restores the backup and reports rollback.
pub fn migrate_state_dir(global_dir: &Path) -> EaiResult<MigrationReport> {
    fs::create_dir_all(global_dir).map_err(|e| EaiError::config(e.to_string()))?;
    let from = read_version(global_dir);
    let mut report = MigrationReport {
        from_version: from,
        to_version: STATE_SCHEMA_VERSION,
        ..MigrationReport::default()
    };
    if from >= STATE_SCHEMA_VERSION {
        report.to_version = from;
        return Ok(report);
    }

    let bak = backup_tree(global_dir)?;
    report.backup_dir = Some(bak.display().to_string());
    report.migrated = true;
    report.messages.push(format!(
        "backed up pre-migration state to {}",
        bak.display()
    ));

    match run_migrations(global_dir, from, &mut report) {
        Ok(()) => {
            write_version(global_dir, STATE_SCHEMA_VERSION)?;
            report
                .messages
                .push(format!("wrote state.schema.json v{STATE_SCHEMA_VERSION}"));
            Ok(report)
        }
        Err(e) => {
            restore_backup(global_dir, &bak)?;
            report.rolled_back = true;
            report
                .messages
                .push(format!("migration failed ({e}); restored backup"));
            Err(EaiError::config(format!(
                "state migration failed and rolled back: {e}"
            )))
        }
    }
}

fn run_migrations(global_dir: &Path, from: u32, report: &mut MigrationReport) -> EaiResult<()> {
    // v0 → v1: ensure config.json parses (heal via load) and exists.
    if from < 1 {
        let path = SusiConfig::get_config_path(global_dir);
        if path.exists() {
            // Round-trip through SusiConfig so heal_in_place runs on next load.
            match SusiConfig::load(global_dir) {
                Ok(cfg) => {
                    cfg.save(global_dir)?;
                    report
                        .messages
                        .push("v0→v1: re-saved healed config.json".to_string());
                }
                Err(e) => {
                    return Err(EaiError::config(format!(
                        "v0→v1: config.json unreadable: {e}"
                    )));
                }
            }
        } else {
            SusiConfig::default().save(global_dir)?;
            report
                .messages
                .push("v0→v1: wrote bundled config.json".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_state_mig_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn migration_report_render_formats_noop_migrated_and_rolled_back_states() {
        let noop = MigrationReport {
            from_version: 1,
            to_version: 1,
            migrated: false,
            rolled_back: false,
            backup_dir: None,
            messages: Vec::new(),
        };
        assert_eq!(
            noop.render(),
            "state migration: already at schema v1 (noop)"
        );

        let migrated = MigrationReport {
            from_version: 0,
            to_version: 1,
            migrated: true,
            rolled_back: false,
            backup_dir: Some("/tmp/.migrate-bak".into()),
            messages: vec!["v0→v1: wrote bundled config.json".into()],
        };
        let mig_txt = migrated.render();
        assert!(mig_txt.contains("state migration: v0 → v1\n"));
        assert!(mig_txt.contains("  - v0→v1: wrote bundled config.json\n"));
        assert!(mig_txt.contains("  backup: /tmp/.migrate-bak\n"));
        assert!(!mig_txt.contains("rolled back"));

        let rolled = MigrationReport {
            from_version: 0,
            to_version: 1,
            migrated: true,
            rolled_back: true,
            backup_dir: Some("/tmp/.migrate-bak".into()),
            messages: vec!["migration failed".into()],
        };
        assert!(rolled.render().contains("  rolled back after failure\n"));
    }

    #[test]
    fn migrate_state_dir_upgrades_v0_is_idempotent_and_rolls_back_on_corrupt_config() {
        // 1. Fresh v0 directory migrates to STATE_SCHEMA_VERSION and second run is noop.
        let dir = temp_dir("fresh");
        let first = migrate_state_dir(&dir).expect("v0->v1 migration succeeds");
        assert!(first.migrated);
        assert!(!first.rolled_back);
        assert_eq!(first.from_version, 0);
        assert_eq!(first.to_version, STATE_SCHEMA_VERSION);
        assert!(first.backup_dir.is_some());
        assert!(SusiConfig::get_config_path(&dir).is_file());
        assert_eq!(read_version(&dir), STATE_SCHEMA_VERSION);

        let second = migrate_state_dir(&dir).expect("idempotent noop succeeds");
        assert!(!second.migrated);
        assert!(!second.rolled_back);
        assert_eq!(second.from_version, STATE_SCHEMA_VERSION);
        let _ = fs::remove_dir_all(&dir);

        // 2. Corrupt v0 config.json triggers rollback, restoring original bytes and no manifest.
        let bad_dir = temp_dir("rollback");
        let cfg_path = SusiConfig::get_config_path(&bad_dir);
        fs::write(&cfg_path, "{broken").unwrap();

        let err = migrate_state_dir(&bad_dir).unwrap_err().to_string();
        assert!(err.contains("state migration failed and rolled back"));
        assert_eq!(fs::read_to_string(&cfg_path).unwrap(), "{broken");
        assert!(!manifest_path(&bad_dir).exists());
        let _ = fs::remove_dir_all(&bad_dir);
    }

    #[test]
    fn migrate_state_dir_heals_existing_v0_config_and_second_run_is_idempotent_noop() {
        let dir = temp_dir("existing-v0");
        let mut custom = SusiConfig::default();
        custom.settings.insert(
            "default_model".to_string(),
            serde_json::Value::String("gemini-2.5-pro".to_string()),
        );
        custom.save(&dir).expect("save pre-existing v0 config.json");
        assert_eq!(read_version(&dir), 0);

        let first = migrate_state_dir(&dir).expect("v0->v1 migration on existing config");
        assert!(first.migrated);
        assert!(!first.rolled_back);
        assert_eq!(first.from_version, 0);
        assert_eq!(first.to_version, STATE_SCHEMA_VERSION);
        assert!(
            first
                .messages
                .iter()
                .any(|m| m == "v0→v1: re-saved healed config.json"),
            "unexpected messages: {:?}",
            first.messages
        );

        let reloaded = SusiConfig::load(&dir).expect("reload migrated config.json");
        assert_eq!(reloaded.default_model(), "gemini-2.5-pro");

        let second = migrate_state_dir(&dir).expect("second migration is idempotent noop");
        assert!(!second.migrated);
        assert!(!second.rolled_back);
        assert_eq!(second.from_version, STATE_SCHEMA_VERSION);
        assert_eq!(second.to_version, STATE_SCHEMA_VERSION);
        assert!(second.messages.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_state_dir_treats_corrupt_schema_manifest_as_v0_and_preserves_future_schema_version()
    {
        let corrupt_dir = temp_dir("corrupt-manifest");
        fs::write(manifest_path(&corrupt_dir), "{bad").expect("write corrupt state.schema.json");
        assert_eq!(read_version(&corrupt_dir), 0);

        let report =
            migrate_state_dir(&corrupt_dir).expect("migrate_state_dir on corrupt schema manifest");
        assert!(report.migrated);
        assert!(!report.rolled_back);
        assert_eq!(report.from_version, 0);
        assert_eq!(report.to_version, STATE_SCHEMA_VERSION);
        assert_eq!(read_version(&corrupt_dir), STATE_SCHEMA_VERSION);
        let bak_dir = PathBuf::from(report.backup_dir.expect("backup_dir recorded"));
        assert_eq!(
            fs::read_to_string(bak_dir.join("state.schema.json")).expect("read backed up manifest"),
            "{bad"
        );
        let _ = fs::remove_dir_all(&corrupt_dir);

        let future_dir = temp_dir("future-manifest");
        write_version(&future_dir, 99).expect("write v99 manifest");
        let future_report =
            migrate_state_dir(&future_dir).expect("migrate_state_dir on future schema version");
        assert!(!future_report.migrated);
        assert!(!future_report.rolled_back);
        assert_eq!(future_report.from_version, 99);
        assert_eq!(future_report.to_version, 99);
        assert_eq!(
            future_report.render(),
            "state migration: already at schema v99 (noop)"
        );
        let _ = fs::remove_dir_all(&future_dir);
    }

    #[test]
    fn state_schema_version_constant_is_positive_and_manifest_path_ends_with_state_schema_json() {
        const { assert!(STATE_SCHEMA_VERSION >= 1) };
        let p = manifest_path(Path::new("/tmp/susi-home"));
        assert!(p.ends_with("state.schema.json"));
    }
}

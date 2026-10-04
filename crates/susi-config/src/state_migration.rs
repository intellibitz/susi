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

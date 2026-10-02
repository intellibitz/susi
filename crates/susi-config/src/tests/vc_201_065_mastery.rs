//! Mastery verification for VC-201-065: versioned migrations with durable
//! backups and crash-resumable behavior.
//!
//! The claim under test is not "migrate_state_dir can round-trip" — the
//! cited `vc_201_065`/`zc_state_migration` tests already show that. The
//! distinguishing properties are that the backup is *durable* (a re-run
//! after interruption cannot destroy it) and that the function is reachable
//! on a production upgrade path. These tests attack both.

use crate::state_migration::{migrate_state_dir, STATE_SCHEMA_VERSION};
use crate::SusiConfig;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi_vc201065m_{tag}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Falsification: the "durable" backup is named `.migrate-bak-<epoch seconds>`.
/// A migration interrupted after its backup completed but before the manifest
/// write is re-run on next launch — within the same second on any fast retry —
/// and the second `backup_tree` writes the live (possibly already modified)
/// `config.json` over the preserved original inside the same backup dir.
/// The previous usable configuration is destroyed; a later rollback can only
/// restore the mid-migration state.
#[test]
fn vc_201_065_mastery_rerun_in_same_second_clobbers_durable_backup() {
    let dir = scratch("clobber");
    fs::write(
        SusiConfig::get_config_path(&dir),
        r#"{"settings":{"trust_level":"original-user-value"}}"#,
    )
    .unwrap();

    // Pre-seed every backup dir a retry could collide with: the same second
    // and the next few, so the test does not depend on a clock boundary.
    let t0 = now_secs();
    for s in t0..=t0 + 5 {
        let bak = dir.join(format!(".migrate-bak-{s}"));
        fs::create_dir_all(&bak).unwrap();
        fs::write(
            bak.join("config.json"),
            r#"{"settings":{"trust_level":"PRESERVED-ORIGINAL"}}"#,
        )
        .unwrap();
    }

    let report = migrate_state_dir(&dir).unwrap();
    assert!(report.migrated);
    let bak = PathBuf::from(report.backup_dir.unwrap());
    let preserved = fs::read_to_string(bak.join("config.json")).unwrap();
    assert!(
        !preserved.contains("PRESERVED-ORIGINAL"),
        "a same-second re-run overwrote the durable pre-migration backup"
    );
    assert!(
        preserved.contains("original-user-value"),
        "the backup now holds the mid-migration file, not the original"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// What does hold: a successful v0→v1 run preserves the pre-migration
/// `config.json` byte-for-byte in the backup (the backup runs before the
/// heal-and-save), writes the version manifest, and a rerun is a noop.
#[test]
fn vc_201_065_mastery_success_path_preserves_backup_and_marks_version() {
    let dir = scratch("ok");
    let original = r#"{"settings":{"trust_level":"original-user-value"}}"#;
    fs::write(SusiConfig::get_config_path(&dir), original).unwrap();

    let report = migrate_state_dir(&dir).unwrap();
    assert!(report.migrated);
    let bak = PathBuf::from(report.backup_dir.unwrap());
    assert_eq!(
        fs::read_to_string(bak.join("config.json")).unwrap(),
        original,
        "the first-run backup preserves pre-migration bytes"
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("state.schema.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["schema_version"].as_u64().unwrap(),
        u64::from(STATE_SCHEMA_VERSION)
    );

    // A rerun after the version marker lands is a clean noop.
    let r2 = migrate_state_dir(&dir).unwrap();
    assert!(!r2.migrated && !r2.rolled_back);
    let _ = fs::remove_dir_all(&dir);
}

/// What does hold: a migration that cannot produce a usable config rolls
/// back — the prior file is restored byte-for-byte and no version marker is
/// written, so the next attempt retries the migration rather than recording
/// a state that never activated.
#[test]
fn vc_201_065_mastery_failed_migration_rolls_back_and_retries() {
    let dir = scratch("rollback");
    fs::write(SusiConfig::get_config_path(&dir), "{broken").unwrap();

    assert!(migrate_state_dir(&dir).is_err());
    assert_eq!(
        fs::read_to_string(SusiConfig::get_config_path(&dir)).unwrap(),
        "{broken",
        "rollback restores the prior file"
    );
    assert!(
        !dir.join("state.schema.json").exists(),
        "no version marker for a migration that never activated"
    );
    // Retry still attempts migration — the interrupted state is resumable.
    assert!(migrate_state_dir(&dir).is_err());
    let _ = fs::remove_dir_all(&dir);
}

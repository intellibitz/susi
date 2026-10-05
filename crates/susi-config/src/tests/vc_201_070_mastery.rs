//! Mastery checks for VC-201-070: export versioned configuration, artifact
//! references, and encrypted secret material only when explicitly included;
//! restore to a clean dev instance verifies references and reports
//! unavailable external resources.
//!
//! Every test name starts `vc_201_070_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-config vc_201_070`.

use crate::state_backup::{
    backup, backup_with_options, collect_sections, collect_sections_report, restore, BackupOptions,
    BACKUP_FORMAT_VERSION,
};
use std::collections::BTreeMap;

#[test]
fn vc_201_070_mastery_secret_sections_require_explicit_inclusion() {
    let mut sections = BTreeMap::new();
    sections.insert("config".to_string(), b"{}".to_vec());
    sections.insert("keys".to_string(), b"sk-live-secret".to_vec());
    let refused = backup(&sections, 0x42, false).expect_err("secret export must be opt-in");
    assert!(refused.contains("keys"));
    let blob = backup_with_options(&sections, 0x42, BackupOptions::new(false).with_secrets())
        .expect("explicit secret inclusion succeeds");
    assert!(blob.sections.contains_key("keys"));
    assert!(blob.manifest.sections.contains(&"keys".to_string()));
}

#[test]
fn vc_201_070_mastery_wrong_key_is_authenticated_and_refused() {
    let mut sections = BTreeMap::new();
    sections.insert("config".to_string(), b"{\"a\":1}".to_vec());
    let blob = backup(&sections, 0x42, false).expect("backup");
    let error = restore(&blob, 0x99).expect_err("wrong key must fail authentication");
    assert!(error.contains("authentication"));
}

#[test]
fn vc_201_070_mastery_known_plaintext_does_not_reveal_key() {
    let mut sections = BTreeMap::new();
    sections.insert("config".to_string(), vec![0u8; 4]);
    let blob = backup(&sections, 0xAB, false).expect("backup");
    assert_ne!(blob.sections["config"], "abababab");
    assert_eq!(restore(&blob, 0xAB).unwrap()["config"], vec![0; 4]);
}

#[test]
fn vc_201_070_mastery_missing_resources_are_reported() {
    let dir = std::env::temp_dir().join(format!("vc070-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("present"), b"x").unwrap();
    let error = collect_sections(&dir, &["present", "absent", "also-absent"])
        .expect_err("missing requested resources must be reported");
    assert!(error.contains("absent"));
    assert!(error.contains("also-absent"));
    let report = collect_sections_report(&dir, &["present", "absent"]).unwrap();
    assert_eq!(report.sections.len(), 1);
    assert_eq!(report.missing, vec!["absent"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn vc_201_070_mastery_restore_reports_dangling_references() {
    let mut sections = BTreeMap::new();
    sections.insert(
        "artifacts".to_string(),
        b"{\"ref\": \"file:///does/not/exist.bin\"}".to_vec(),
    );
    let blob = backup(&sections, 0x42, false).expect("backup");
    let error = restore(&blob, 0x42).expect_err("dangling reference must be refused");
    assert!(error.contains("unavailable external resource"));
    assert!(error.contains("does/not/exist.bin"));
}

/// Holds: format-version mismatch is refused.
#[test]
fn vc_201_070_mastery_version_check_holds() {
    let mut sections = BTreeMap::new();
    sections.insert("config".to_string(), b"{}".to_vec());
    let mut blob = backup(&sections, 0x42, false).expect("backup");
    blob.manifest.format_version = BACKUP_FORMAT_VERSION + 1;
    assert!(restore(&blob, 0x42).is_err());
}

/// Holds: a dry-run backup refuses restore.
#[test]
fn vc_201_070_mastery_dry_run_refuses_restore_holds() {
    let mut sections = BTreeMap::new();
    sections.insert("config".to_string(), b"{}".to_vec());
    let blob = backup(&sections, 0x42, true).expect("backup");
    assert!(blob.sections.is_empty());
    assert!(restore(&blob, 0x42).is_err());
}

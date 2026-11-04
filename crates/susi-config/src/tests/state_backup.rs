use crate::state_backup::{backup, restore, BackupBlob, BACKUP_FORMAT_VERSION};
use std::collections::BTreeMap;

#[test]
fn state_backup_roundtrip_encrypts_sections() {
    let mut sections = BTreeMap::new();
    sections.insert("config".into(), b"{\"a\":1}".to_vec());
    sections.insert("evidence".into(), b"[]".to_vec());
    let blob = backup(&sections, 0x3C, false).unwrap();
    assert_eq!(blob.manifest.format_version, BACKUP_FORMAT_VERSION);
    assert!(!blob.sections["config"].contains("a\":1"));
    let back = restore(&blob, 0x3C).unwrap();
    assert_eq!(back.get("config").unwrap(), b"{\"a\":1}");
    let mut tampered = blob.clone();
    let payload = tampered.sections.get_mut("config").unwrap();
    let replacement = if payload.ends_with('0') { '1' } else { '0' };
    payload.pop();
    payload.push(replacement);
    assert!(restore(&tampered, 0x3C)
        .unwrap_err()
        .contains("authentication"));
}

#[test]
fn state_backup_dry_run_and_version_checks() {
    let sections = BTreeMap::new();
    let dry = backup(&sections, 1, true).unwrap();
    assert!(dry.manifest.dry_run);
    assert!(restore(&dry, 1).unwrap_err().contains("dry-run"));
    let bad = BackupBlob {
        manifest: crate::state_backup::BackupManifest {
            format_version: 99,
            sections: vec![],
            dry_run: false,
        },
        sections: BTreeMap::new(),
    };
    assert!(restore(&bad, 1).unwrap_err().contains("unsupported"));
}

#[test]
fn state_backup_wrong_key_fails_authentication() {
    let mut sections = BTreeMap::new();
    sections.insert("config".into(), b"{\"ok\":true}".to_vec());
    let bundle = backup(&sections, 7, false).unwrap();
    let err = restore(&bundle, 8).unwrap_err();
    assert!(err.contains("authentication"), "{err}");
}

#[test]
fn state_backup_empty_sections_non_dry_run_roundtrips_cleanly() {
    let empty = BTreeMap::new();
    let bundle = backup(&empty, 9, false).unwrap();
    assert!(!bundle.manifest.dry_run);
    assert!(bundle.manifest.sections.is_empty());
    let out = restore(&bundle, 9).unwrap();
    assert!(out.is_empty());
}

#[test]
fn state_backup_manifest_lists_all_encrypted_section_keys_in_order() {
    let mut sections = BTreeMap::new();
    sections.insert("alpha".into(), b"1".to_vec());
    sections.insert("beta".into(), b"2".to_vec());
    sections.insert("gamma".into(), b"3".to_vec());
    let blob = backup(&sections, 0x42, false).unwrap();
    assert_eq!(
        blob.manifest.sections,
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
    );
    let restored = restore(&blob, 0x42).unwrap();
    assert_eq!(restored.len(), 3);
    assert_eq!(restored["beta"], b"2");
}

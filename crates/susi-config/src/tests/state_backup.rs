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

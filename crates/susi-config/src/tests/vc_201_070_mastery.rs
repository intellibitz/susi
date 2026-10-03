//! Mastery checks for VC-201-070: export versioned configuration, artifact
//! references, and encrypted secret material only when explicitly included;
//! restore to a clean dev instance verifies references and reports
//! unavailable external resources.
//!
//! Every test name starts `vc_201_070_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-config vc_201_070`.

use crate::state_backup::{backup, collect_sections, restore, BACKUP_FORMAT_VERSION};
use std::collections::BTreeMap;

/// The claim says secret material is included "only when explicitly
/// included". `backup` takes a flat section map — there is no
/// include-secrets flag, no per-section sensitivity, no gate: a `keys`
/// section rides the export identically to `config`.
#[test]
fn vc_201_070_mastery_secret_sections_have_no_inclusion_gate() {
    let mut sections = BTreeMap::new();
    sections.insert("config".to_string(), b"{}".to_vec());
    sections.insert("keys".to_string(), b"sk-live-secret".to_vec());
    let blob = backup(&sections, 0x42, false).expect("backup succeeds");
    // No opt-in was ever expressed, yet the secret section exported.
    assert!(blob.sections.contains_key("keys"));
    assert!(blob.manifest.sections.contains(&"keys".to_string()));
}

/// "Encrypted" is `xor_hex` with a single u8 byte — the code's own comment
/// calls it "test-grade encryption". A 255-key keyspace: every candidate key
/// restores *successfully* because restore applies `key.max(1)` and has no
/// authentication or integrity check at all. Wrong key → silent garbage.
#[test]
fn vc_201_070_mastery_wrong_key_restores_garbage_without_error() {
    let mut sections = BTreeMap::new();
    sections.insert("config".to_string(), b"{\"a\":1}".to_vec());
    let blob = backup(&sections, 0x42, false).expect("backup");
    let back = restore(&blob, 0x99).expect("wrong key still 'restores'");
    assert_eq!(
        back["config"],
        b"{\"a\":1}"
            .iter()
            .map(|b| b ^ 0x42 ^ 0x99)
            .collect::<Vec<u8>>(),
        "restore produced corrupted plaintext and reported no error"
    );
}

/// Any known plaintext byte recovers the key: a zero byte encrypts to the key
/// itself. This is not encryption in any sense the claim implies.
#[test]
fn vc_201_070_mastery_known_plaintext_recovers_key() {
    let mut sections = BTreeMap::new();
    sections.insert("config".to_string(), vec![0u8; 4]);
    let blob = backup(&sections, 0xAB, false).expect("backup");
    assert_eq!(blob.sections["config"], "abababab");
}

/// The claim requires "reports unavailable external resources".
/// `collect_sections` silently skips missing files — a section the caller
/// named simply isn't in the map, with no report, no list of absent inputs.
#[test]
fn vc_201_070_mastery_missing_resources_are_silently_dropped() {
    let dir = std::env::temp_dir().join(format!("vc070-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("present"), b"x").unwrap();
    let map = collect_sections(&dir, &["present", "absent", "also-absent"]).unwrap();
    assert_eq!(map.len(), 1);
    assert!(map.contains_key("present"));
    // `absent` / `also-absent` were requested and are missing — nothing
    // reported that; the caller cannot distinguish "not present" from
    // "not requested".
    std::fs::remove_dir_all(&dir).ok();
}

/// Restore verifies nothing about references: a section whose payload is a
/// dangling artifact reference restores "successfully" with no reference
/// check and no unavailable-resource report.
#[test]
fn vc_201_070_mastery_restore_verifies_no_references() {
    let mut sections = BTreeMap::new();
    sections.insert(
        "artifacts".to_string(),
        b"{\"ref\": \"file:///does/not/exist.bin\"}".to_vec(),
    );
    let blob = backup(&sections, 0x42, false).expect("backup");
    let back = restore(&blob, 0x42).expect("dangling ref restores fine");
    assert!(back["artifacts"]
        .windows(b"does/not/exist".len())
        .any(|w| w == b"does/not/exist"));
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

//! Checksummed offline bootstrap bundles (VC-201-050).

use crate::offline_bundle::{import_bundle, pack_bundle};
use std::collections::BTreeSet;

fn avail(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// Acceptance: a checksummed bundle packs, imports, verifies, and diagnoses
/// missing artifacts on the production path — with no cloud fallback.
#[test]
fn vc_201_050_offline_bundle_pack_and_import_checksummed() {
    let bundle = pack_bundle(
        vec!["susi".into(), "susi-daemon".into()],
        vec!["llama-3.2-3b.gguf".into()],
        vec!["extensions.json".into()],
    );

    // The bundle is content-addressed and deterministic under reordering.
    let reordered = pack_bundle(
        vec!["susi-daemon".into(), "susi".into()],
        vec!["llama-3.2-3b.gguf".into()],
        vec!["extensions.json".into()],
    );
    assert_eq!(bundle.checksum, reordered.checksum);
    assert!(!bundle.checksum.is_empty());

    // A complete host imports cleanly.
    let complete = import_bundle(
        &bundle,
        &avail(&[
            "susi",
            "susi-daemon",
            "llama-3.2-3b.gguf",
            "extensions.json",
        ]),
    );
    assert!(complete.checksum_ok);
    assert!(complete.is_complete());

    // A host missing artifacts gets a precise diagnosis, never a fetch.
    let partial = import_bundle(&bundle, &avail(&["susi", "extensions.json"]));
    assert!(partial.checksum_ok);
    assert!(!partial.is_complete());
    assert_eq!(partial.missing_binaries, vec!["susi-daemon".to_string()]);
    assert_eq!(
        partial.missing_models,
        vec!["llama-3.2-3b.gguf".to_string()]
    );
    assert!(partial.missing_extension_metadata.is_empty());

    // Tampering with the content is detected by the checksum.
    let mut tampered = bundle.clone();
    tampered.binaries.push("extra-bin".into());
    let report = import_bundle(
        &tampered,
        &avail(&[
            "susi",
            "susi-daemon",
            "extra-bin",
            "llama-3.2-3b.gguf",
            "extensions.json",
        ]),
    );
    assert!(!report.checksum_ok);
    assert!(!report.is_complete());
}

#[test]
fn vc_201_050_empty_bundle_packs_with_deterministic_checksum_and_imports_cleanly() {
    let bundle = pack_bundle(vec![], vec![], vec![]);
    assert!(!bundle.checksum.is_empty());
    let report = import_bundle(&bundle, &avail(&[]));
    assert!(report.checksum_ok);
    assert!(report.is_complete());
}

#[test]
fn vc_201_050_missing_extension_metadata_is_diagnosed_without_false_positives() {
    let bundle = pack_bundle(
        vec!["susi".into()],
        vec!["m.gguf".into()],
        vec!["ext-a.json".into(), "ext-b.json".into()],
    );
    let report = import_bundle(&bundle, &avail(&["susi", "m.gguf", "ext-a.json"]));
    assert!(report.checksum_ok);
    assert!(!report.is_complete());
    assert!(report.missing_binaries.is_empty());
    assert!(report.missing_models.is_empty());
    assert_eq!(
        report.missing_extension_metadata,
        vec!["ext-b.json".to_string()]
    );
}

#[test]
fn vc_201_050_tampered_checksum_string_fails_closed_even_when_all_artifacts_exist() {
    let mut bundle = pack_bundle(
        vec!["susi".into()],
        vec!["m.gguf".into()],
        vec!["ext.json".into()],
    );
    bundle.checksum = "0000".into();
    let report = import_bundle(&bundle, &avail(&["susi", "m.gguf", "ext.json"]));
    assert!(!report.checksum_ok);
    assert!(!report.is_complete());
}

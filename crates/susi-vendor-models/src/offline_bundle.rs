//! Checksummed offline bootstrap bundles (VC-201-050).
//!
//! An isolated host boots from a bundle that carries the supported binaries,
//! models, and extension metadata it needs while the network is disabled.
//! The bundle is content-addressed by a deterministic checksum; import
//! verifies that checksum and reports any required artifact that is missing
//! locally. There is no cloud fallback: a missing artifact is diagnosed,
//! never fetched.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfflineBundle {
    pub binaries: Vec<String>,
    pub models: Vec<String>,
    pub extension_metadata: Vec<String>,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub checksum_ok: bool,
    pub missing_binaries: Vec<String>,
    pub missing_models: Vec<String>,
    pub missing_extension_metadata: Vec<String>,
}

impl ImportReport {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.checksum_ok
            && self.missing_binaries.is_empty()
            && self.missing_models.is_empty()
            && self.missing_extension_metadata.is_empty()
    }
}

/// Package a bundle. Content is sorted so the same set of artifacts always
/// yields the same checksum regardless of input order.
#[must_use]
pub fn pack_bundle(
    mut binaries: Vec<String>,
    mut models: Vec<String>,
    mut extension_metadata: Vec<String>,
) -> OfflineBundle {
    binaries.sort();
    models.sort();
    extension_metadata.sort();
    let checksum = content_checksum(&binaries, &models, &extension_metadata);
    OfflineBundle {
        binaries,
        models,
        extension_metadata,
        checksum,
    }
}

/// Import a bundle into a host carrying `available` artifacts. The checksum is
/// verified and any required artifact absent from `available` is reported.
/// Missing artifacts are never fetched from a remote.
#[must_use]
pub fn import_bundle(bundle: &OfflineBundle, available: &BTreeSet<String>) -> ImportReport {
    let checksum_ok =
        content_checksum(&bundle.binaries, &bundle.models, &bundle.extension_metadata)
            == bundle.checksum;
    ImportReport {
        checksum_ok,
        missing_binaries: bundle
            .binaries
            .iter()
            .filter(|a| !available.contains(*a))
            .cloned()
            .collect(),
        missing_models: bundle
            .models
            .iter()
            .filter(|a| !available.contains(*a))
            .cloned()
            .collect(),
        missing_extension_metadata: bundle
            .extension_metadata
            .iter()
            .filter(|a| !available.contains(*a))
            .cloned()
            .collect(),
    }
}

fn content_checksum(
    binaries: &[String],
    models: &[String],
    extension_metadata: &[String],
) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in binaries.iter().chain(models).chain(extension_metadata) {
        for b in (s.len() as u64).to_le_bytes() {
            h = fnv1a(h, b);
        }
        for b in s.as_bytes() {
            h = fnv1a(h, *b);
        }
    }
    format!("{h:016x}")
}

const fn fnv1a(h: u64, b: u8) -> u64 {
    (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn pack_bundle_sorts_inputs_and_produces_order_independent_checksum() {
        let b1 = pack_bundle(
            vec!["susi".into(), "susi-daemon".into()],
            vec!["qwen".into(), "llama".into()],
            vec!["ext-b".into(), "ext-a".into()],
        );
        let b2 = pack_bundle(
            vec!["susi-daemon".into(), "susi".into()],
            vec!["llama".into(), "qwen".into()],
            vec!["ext-a".into(), "ext-b".into()],
        );
        assert_eq!(b1.checksum, b2.checksum);
        assert_eq!(b1.models, vec!["llama", "qwen"]);
    }

    #[test]
    fn import_bundle_succeeds_when_all_artifacts_are_present_and_checksum_matches() {
        let bundle = pack_bundle(
            vec!["susi".into()],
            vec!["qwen-coder".into()],
            vec!["pack-v1".into()],
        );
        let available: BTreeSet<String> = ["susi", "qwen-coder", "pack-v1"]
            .into_iter()
            .map(String::from)
            .collect();
        let report = import_bundle(&bundle, &available);
        assert!(report.checksum_ok);
        assert!(report.is_complete());
    }

    #[test]
    fn import_bundle_reports_missing_artifacts_without_remote_fallback() {
        let bundle = pack_bundle(
            vec!["susi".into()],
            vec!["qwen-coder".into()],
            vec!["pack-v1".into()],
        );
        let available: BTreeSet<String> = ["susi"].into_iter().map(String::from).collect();
        let report = import_bundle(&bundle, &available);
        assert!(report.checksum_ok);
        assert!(!report.is_complete());
        assert_eq!(report.missing_models, vec!["qwen-coder"]);
        assert_eq!(report.missing_extension_metadata, vec!["pack-v1"]);
    }

    #[test]
    fn import_bundle_detects_tampered_checksum_and_marks_incomplete() {
        let mut bundle = pack_bundle(vec!["susi".into()], vec![], vec![]);
        bundle.checksum = "0000000000000000".into();
        let available: BTreeSet<String> = ["susi"].into_iter().map(String::from).collect();
        let report = import_bundle(&bundle, &available);
        assert!(!report.checksum_ok);
        assert!(!report.is_complete());
    }

    #[test]
    fn pack_bundle_sorts_all_three_collections_and_produces_sixteen_hex_checksum() {
        let bundle = pack_bundle(
            vec!["z-bin".into(), "a-bin".into()],
            vec!["phi4".into(), "deepseek".into()],
            vec!["ext-2".into(), "ext-1".into()],
        );
        assert_eq!(bundle.checksum.len(), 16);
        assert_eq!(bundle.binaries, vec!["a-bin", "z-bin"]);
        assert_eq!(bundle.models, vec!["deepseek", "phi4"]);
        assert_eq!(bundle.extension_metadata, vec!["ext-1", "ext-2"]);
    }
}

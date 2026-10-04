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

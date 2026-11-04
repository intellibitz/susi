//! Authenticate snapshot-based peer bootstrap (VC-201-036).
//!
//! Bootstrap from a signed snapshot manifest plus verified ledger tail;
//! reject tampered state, missing anchors, and unauthorized roster history.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub snapshot_id: String,
    pub ledger_tail: String,
    pub roster: BTreeSet<String>,
    /// Detached signature over (snapshot_id || ledger_tail || roster).
    pub signature: String,
    pub signer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustAnchor {
    pub peer_id: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapResult {
    Ok,
    Tampered,
    MissingAnchor,
    UnauthorizedRoster,
}

/// Simplified verify: signature must equal `sign(signer, payload)` and signer
/// must be in the trust anchors; every roster member must appear in history.
#[must_use]
pub fn sign(
    signer: &str,
    snapshot_id: &str,
    ledger_tail: &str,
    roster: &BTreeSet<String>,
) -> String {
    let mut members: Vec<_> = roster.iter().cloned().collect();
    members.sort();
    format!("{signer}:{snapshot_id}:{ledger_tail}:{}", members.join(","))
}

#[must_use]
pub fn bootstrap(
    manifest: &SnapshotManifest,
    anchors: &[TrustAnchor],
    roster_history: &BTreeSet<String>,
) -> BootstrapResult {
    let Some(anchor) = anchors.iter().find(|a| a.peer_id == manifest.signer) else {
        return BootstrapResult::MissingAnchor;
    };
    let expected = sign(
        &manifest.signer,
        &manifest.snapshot_id,
        &manifest.ledger_tail,
        &manifest.roster,
    );
    if manifest.signature != expected || anchor.public_key.is_empty() {
        return BootstrapResult::Tampered;
    }
    if !manifest.roster.is_subset(roster_history) {
        return BootstrapResult::UnauthorizedRoster;
    }
    BootstrapResult::Ok
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    fn sample_roster() -> BTreeSet<String> {
        BTreeSet::from(["node-b".to_string(), "node-a".to_string()])
    }

    #[test]
    fn sign_formats_sorted_roster_deterministically() {
        let roster = sample_roster();
        assert_eq!(
            sign("leader-1", "snap-42", "tail-hash", &roster),
            "leader-1:snap-42:tail-hash:node-a,node-b"
        );
    }

    #[test]
    fn bootstrap_accepts_valid_signed_manifest_with_trusted_anchor_and_roster() {
        let roster = sample_roster();
        let sig = sign("leader-1", "snap-1", "tail-1", &roster);
        let manifest = SnapshotManifest {
            snapshot_id: "snap-1".to_string(),
            ledger_tail: "tail-1".to_string(),
            roster: roster.clone(),
            signature: sig,
            signer: "leader-1".to_string(),
        };
        let anchors = vec![TrustAnchor {
            peer_id: "leader-1".to_string(),
            public_key: "ed25519:pub".to_string(),
        }];
        assert_eq!(bootstrap(&manifest, &anchors, &roster), BootstrapResult::Ok);
    }

    #[test]
    fn bootstrap_rejects_missing_anchor_or_empty_public_key_or_tampered_sig() {
        let roster = sample_roster();
        let manifest = SnapshotManifest {
            snapshot_id: "snap-1".to_string(),
            ledger_tail: "tail-1".to_string(),
            roster: roster.clone(),
            signature: sign("leader-1", "snap-1", "tail-1", &roster),
            signer: "leader-1".to_string(),
        };
        assert_eq!(
            bootstrap(&manifest, &[], &roster),
            BootstrapResult::MissingAnchor
        );

        let empty_key = vec![TrustAnchor {
            peer_id: "leader-1".to_string(),
            public_key: String::new(),
        }];
        assert_eq!(
            bootstrap(&manifest, &empty_key, &roster),
            BootstrapResult::Tampered
        );

        let mut tampered = manifest;
        tampered.signature = "forged".to_string();
        let anchors = vec![TrustAnchor {
            peer_id: "leader-1".to_string(),
            public_key: "ed25519:pub".to_string(),
        }];
        assert_eq!(
            bootstrap(&tampered, &anchors, &roster),
            BootstrapResult::Tampered
        );
    }

    #[test]
    fn bootstrap_rejects_unauthorized_roster_not_in_history() {
        let roster = sample_roster();
        let manifest = SnapshotManifest {
            snapshot_id: "snap-1".to_string(),
            ledger_tail: "tail-1".to_string(),
            roster: roster.clone(),
            signature: sign("leader-1", "snap-1", "tail-1", &roster),
            signer: "leader-1".to_string(),
        };
        let anchors = vec![TrustAnchor {
            peer_id: "leader-1".to_string(),
            public_key: "ed25519:pub".to_string(),
        }];
        let partial_history = BTreeSet::from(["node-a".to_string()]);
        assert_eq!(
            bootstrap(&manifest, &anchors, &partial_history),
            BootstrapResult::UnauthorizedRoster
        );
    }
}

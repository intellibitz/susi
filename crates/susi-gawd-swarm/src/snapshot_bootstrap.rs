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

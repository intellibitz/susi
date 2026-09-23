//! Commit-record ledger for swarm quorum commits (VC-200-001).
//!
//! When `supervise_mission` reaches a `QUORUM_COMMIT`, the decision only
//! ever existed in the coordinator's memory — a coordinator crash
//! mid-round lost the outcome, and no voter could prove what was
//! committed. This module is the replicated-commit-log half of that gap:
//! every quorum commit produces a `CommitRecord` sealed with the cluster
//! key (`susi_config::cluster_key`, the same HMAC-SHA256 secret that
//! authenticates peer ping/pong). The coordinator appends it locally and
//! pushes it to each voting peer, which verifies the signature and
//! appends to its own ledger (`~/.susi/commit_log.jsonl`).
//!
//! Membership-authenticity is the security property that matters: a peer
//! without `cluster.key` cannot forge a record, and a forged/tampered
//! record fails `verify` before it ever touches the ledger. This is not
//! a Raft log — entries are commit decisions, not state-machine ops, and
//! there is no index/term ordering — but a restarted node can now recover
//! and audit every quorum decision its cluster made.
//!
//! ## Byte-identical vendoring
//!
//! Copied into every consumer's `src/susi_core/` tree. Sign and verify
//! must be the *same* code on both sides of the wire, so the record
//! format lives in the kernel ABI rather than in either peer-plane crate.

use std::fs;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::susi_error::{EaiError, EaiResult};
use crate::susi_paths::SusiDirs;

/// A single quorum-commit decision, signed by the coordinator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitRecord {
    /// Round identifier: hex digest of the sorted electorate plus the
    /// commit timestamp — unique per (voter set, decision) pair.
    pub epoch: String,
    /// `node_id` of the coordinator that ran the vote.
    pub coordinator: String,
    /// Pinned voter set at broadcast time (sorted), so a receiver can
    /// reconstruct exactly who was eligible to vote.
    pub electorate: Vec<String>,
    /// How many pinned voters agreed on `value`.
    pub tally: usize,
    /// `electorate.len() / 2 + 1` at commit time — recorded so a reader
    /// can check the tally really crossed quorum.
    pub quorum_threshold: usize,
    /// SHA-256 hex of the normalized committed output; lets an auditor
    /// verify `value` integrity without trusting the text itself.
    pub value_hash: String,
    /// The committed output (trimmed, case-preserved representative).
    pub value: String,
    /// Unix seconds when the coordinator committed.
    pub committed_at: u64,
    /// HMAC-SHA256 hex over `signed_payload()` under `cluster.key`.
    /// Empty until `seal` runs; a record with an empty signature never
    /// verifies.
    pub signature: String,
}

/// Everything the signature covers — the record minus the signature
/// itself, serialized in a fixed field order. Both sides of the wire
/// serialize this identical shape, so the HMAC is reproducible anywhere
/// the vendored copy runs.
#[derive(Serialize)]
struct SignedFields<'a> {
    epoch: &'a str,
    coordinator: &'a str,
    electorate: &'a [String],
    tally: usize,
    quorum_threshold: usize,
    value_hash: &'a str,
    value: &'a str,
    committed_at: u64,
}

impl CommitRecord {
    /// Build and seal a record for a freshly committed quorum decision.
    /// Returns `None` when the cluster key is absent — a node outside the
    /// cluster cannot mint commit records.
    pub fn seal(
        coordinator: &str,
        electorate: Vec<String>,
        tally: usize,
        quorum_threshold: usize,
        value: &str,
    ) -> Option<Self> {
        let key = crate::susi_config::cluster_key::cluster_key()?;
        let committed_at = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut sorted = electorate;
        sorted.sort();
        let epoch = hex::encode(Sha256::digest(
            format!("{}|{}", sorted.join(","), committed_at).as_bytes(),
        ));
        let value_hash = hex::encode(Sha256::digest(value.as_bytes()));
        let mut rec = CommitRecord {
            epoch,
            coordinator: coordinator.to_string(),
            electorate: sorted,
            tally,
            quorum_threshold,
            value_hash,
            value: value.to_string(),
            committed_at,
            signature: String::new(),
        };
        rec.signature =
            crate::susi_config::cluster_key::hmac_sha256_hex(&key, rec.signed_payload().as_bytes());
        Some(rec)
    }

    /// The exact bytes the signature covers (compact JSON of
    /// `SignedFields` — serde emits struct fields in declaration order,
    /// so this is stable across processes and vendored copies).
    fn signed_payload(&self) -> String {
        serde_json::to_string(&SignedFields {
            epoch: &self.epoch,
            coordinator: &self.coordinator,
            electorate: &self.electorate,
            tally: self.tally,
            quorum_threshold: self.quorum_threshold,
            value_hash: &self.value_hash,
            value: &self.value,
            committed_at: self.committed_at,
        })
        .unwrap_or_default()
    }

    /// Verify a received record against the local cluster key and its own
    /// internal consistency (tally crossed quorum, hash matches value).
    /// Rejects unsigned, forged, tampered, and self-inconsistent records.
    pub fn verify(&self) -> bool {
        if self.signature.is_empty() {
            return false;
        }
        let Some(key) = crate::susi_config::cluster_key::cluster_key() else {
            return false;
        };
        let expected = crate::susi_config::cluster_key::hmac_sha256_hex(
            &key,
            self.signed_payload().as_bytes(),
        );
        if expected != self.signature {
            return false;
        }
        // Internal consistency: a correctly-signed record can still claim
        // a tally that never reached quorum, or carry a value that does
        // not match its hash — check both before trusting it.
        if self.quorum_threshold != self.electorate.len() / 2 + 1
            || self.tally < self.quorum_threshold
        {
            return false;
        }
        hex::encode(Sha256::digest(self.value.as_bytes())) == self.value_hash
    }
}

/// Shared env-mutation lock for tests: `cluster_key()` resolves through
/// XDG env vars, so any test that sets `XDG_CONFIG_HOME`/`HOME` must hold
/// this guard across the whole seal → verify → append sequence, or a
/// parallel test can swap the env mid-sequence and break signature
/// agreement. `pub(crate)` so vendored copies share the lock with their
/// consumer crate's own tests.
#[doc(hidden)]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Where the ledger lives: `~/.susi/commit_log.jsonl` — one JSON record
/// per line, append-only, shared with peers under the same cluster key.
pub fn ledger_path() -> PathBuf {
    SusiDirs::config_dir().join("commit_log.jsonl")
}

/// Append one record. Verifies the signature first — nothing unsigned or
/// forged is ever written to the ledger.
pub fn append(record: &CommitRecord) -> EaiResult<()> {
    append_to(&ledger_path(), record)
}

/// Test seam: append to an explicit path.
pub fn append_to(path: &PathBuf, record: &CommitRecord) -> EaiResult<()> {
    if !record.verify() {
        return Err(EaiError::protocol(
            "refusing to append a commit record that fails signature or consistency verification",
        ));
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| EaiError::filesystem(format!("create {}: {e}", dir.display())))?;
    }
    let mut line = serde_json::to_string(record)
        .map_err(|e| EaiError::internal(format!("serialize commit record: {e}")))?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| EaiError::filesystem(format!("open {}: {e}", path.display())))?;
    file.write_all(line.as_bytes())
        .map_err(|e| EaiError::filesystem(format!("append {}: {e}", path.display())))
}

/// Load every well-formed record, oldest first. Malformed lines are
/// skipped rather than fatal — a torn final line from a mid-append crash
/// must not hide the valid history before it.
pub fn load() -> Vec<CommitRecord> {
    load_from(&ledger_path())
}

/// Test seam: load from an explicit path.
pub fn load_from(path: &PathBuf) -> Vec<CommitRecord> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(_) => return Vec::new(),
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<CommitRecord>(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn sealed_record_verifies_and_tampering_fails() {
        let _g = test_key_guard();
        let Some(rec) = CommitRecord::seal(
            "node-a",
            vec!["AgentA".into(), "AgentB".into(), "PeerNode_1".into()],
            2,
            2,
            "the answer is 42",
        ) else {
            eprintln!("skip: no cluster.key on this host");
            return;
        };
        assert!(rec.verify(), "freshly sealed record must verify");

        let mut forged = rec.clone();
        forged.value = "the answer is 43".into();
        assert!(!forged.verify(), "tampered value must fail");

        let mut inflated = rec.clone();
        inflated.tally = 99; // signature covers tally → signature breaks
        assert!(!inflated.verify());

        let mut unsigned = rec.clone();
        unsigned.signature.clear();
        assert!(!unsigned.verify(), "empty signature must fail");
    }

    #[test]
    fn verify_rejects_tally_below_quorum() {
        let _g = test_key_guard();
        let Some(mut rec) = CommitRecord::seal(
            "node-a",
            vec!["A".into(), "B".into(), "C".into(), "D".into(), "E".into()],
            2, // below the threshold of 3
            3,
            "v",
        ) else {
            eprintln!("skip: no cluster.key on this host");
            return;
        };
        // Re-sign the dishonest record so only the consistency check can
        // catch it.
        if let Some(key) = crate::susi_config::cluster_key::cluster_key() {
            rec.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &key,
                rec.signed_payload().as_bytes(),
            );
        }
        assert!(!rec.verify(), "tally < quorum must fail even when signed");
    }

    #[test]
    fn ledger_round_trip_skips_malformed_lines() {
        let _g = test_key_guard();
        let dir = std::env::temp_dir().join(format!("susi_commit_{}", std::process::id()));
        let path = dir.join("commit_log.jsonl");
        let _ = fs::remove_dir_all(&dir);

        let Some(rec) = CommitRecord::seal("node-a", vec!["A".into(), "B".into()], 2, 2, "v1")
        else {
            eprintln!("skip: no cluster.key on this host");
            return;
        };
        append_to(&path, &rec).unwrap();
        // A torn append must not hide the valid record before it.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{torn").unwrap();
        drop(f);

        let loaded = load_from(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], rec);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_refuses_unverified_record() {
        let _g = test_key_guard();
        let dir = std::env::temp_dir().join(format!("susi_commit_neg_{}", std::process::id()));
        let path = dir.join("commit_log.jsonl");
        let unsigned = CommitRecord {
            epoch: "e".into(),
            coordinator: "x".into(),
            electorate: vec!["A".into(), "B".into()],
            tally: 2,
            quorum_threshold: 2,
            value_hash: "h".into(),
            value: "v".into(),
            committed_at: 0,
            signature: String::new(),
        };
        assert!(append_to(&path, &unsigned).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}

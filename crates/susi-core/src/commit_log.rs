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
//! record fails `verify` before it ever touches the ledger. Ordering is
//! two-layered, Raft-lite: each record carries a consensus `term`
//! (`term.json` bumps on leader transitions; stale-term pushes are
//! rejected, newer terms adopted) and a per-coordinator `seq` (gaps are
//! detected and self-healed via anti-entropy pull). `replay()` folds the
//! ledger into `ClusterState`, so a restarted node reconstructs the
//! cluster's consensus view as a pure function of the log.
//!
//! ## Byte-identical vendoring
//!
//! Copied into every consumer's `src/susi_core/` tree. Sign and verify
//! must be the *same* code on both sides of the wire, so the record
//! format lives in the kernel ABI rather than in either peer-plane crate.

use std::fs;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
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
    /// Per-coordinator monotonic sequence (1-based): the count of records
    /// this coordinator has committed before this one, plus one. Receivers
    /// detect replication gaps when a record's `seq` skips ahead of what
    /// they hold — the signature covers it, so a dropped or forged seq is
    /// detectable rather than silent.
    #[serde(default)]
    pub seq: u64,
    /// The elected cluster leader's `node_id` as the coordinator saw it at
    /// commit time (`SusiSupervisor::elect_leader` — bully over the
    /// verified roster). Audit context: a commit from a coordinator that
    /// isn't the elected leader is an anomaly worth flagging, not a
    /// protocol violation — per-node missions are legitimately coordinated
    /// by their initiator.
    #[serde(default)]
    pub leader: String,
    /// Consensus term the record was sealed under (Raft's term concept):
    /// persisted cluster-wide in `~/.susi/term.json` and bumped by
    /// `claim_leadership` whenever the elected leader changes. A *pushed*
    /// record whose term is older than the receiver's current term is
    /// rejected — the coordinator is working from stale leadership — while
    /// older-term records fetched to fill history are kept (terms gate
    /// new writes, not the log's past). `0` marks pre-term records.
    #[serde(default)]
    pub term: u64,
    /// HMAC-SHA256 hex over `signed_payload()` under `cluster.key`.
    /// Empty until `seal` runs; a record with an empty signature never
    /// verifies.
    pub signature: String,
}

/// Inputs for `CommitRecord::seal` — the decision fields a coordinator
/// knows at commit time. `seq` and `signature` are derived by `seal`.
#[derive(Debug)]
pub struct CommitInput<'a> {
    /// `node_id` of the coordinator that ran the vote.
    pub coordinator: &'a str,
    /// Elected cluster leader the coordinator observed (`elect_leader`);
    /// audit context, see `CommitRecord::leader`.
    pub leader: &'a str,
    /// Pinned voter set (will be sorted before signing).
    pub electorate: Vec<String>,
    /// How many pinned voters agreed on `value`.
    pub tally: usize,
    /// `electorate.len() / 2 + 1` at commit time.
    pub quorum_threshold: usize,
    /// The committed output.
    pub value: &'a str,
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
    seq: u64,
    leader: &'a str,
    term: u64,
}

impl CommitRecord {
    /// Build and seal a record for a freshly committed quorum decision.
    /// `seq` is assigned from the local ledger — the count of this
    /// coordinator's existing records plus one — so per-coordinator
    /// ordering survives restarts. Returns `None` when the cluster key is
    /// absent — a node outside the cluster cannot mint commit records.
    pub fn seal(input: CommitInput<'_>) -> Option<Self> {
        let key = crate::susi_config::cluster_key::cluster_key()?;
        let committed_at = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut sorted = input.electorate;
        sorted.sort();
        let epoch = hex::encode(Sha256::digest(
            format!("{}|{}", sorted.join(","), committed_at).as_bytes(),
        ));
        let value_hash = hex::encode(Sha256::digest(input.value.as_bytes()));
        let mut rec = CommitRecord {
            epoch,
            coordinator: input.coordinator.to_string(),
            electorate: sorted,
            tally: input.tally,
            quorum_threshold: input.quorum_threshold,
            value_hash,
            value: input.value.to_string(),
            committed_at,
            seq: next_seq_for(&load(), input.coordinator),
            leader: input.leader.to_string(),
            term: load_term().term,
            signature: String::new(),
        };
        rec.signature =
            crate::susi_config::cluster_key::hmac_sha256_hex(&key, rec.signed_payload().as_bytes());
        Some(rec)
    }

    /// The exact bytes the signature covers (compact JSON of
    /// `SignedFields` — serde emits struct fields in declaration order,
    /// so this is stable across processes and vendored copies).
    /// `pub(crate)` so consumer-crate tests can re-sign a record after
    /// mutating fields (e.g. fabricating a seq gap); production callers
    /// go through `seal`.
    #[doc(hidden)]
    pub(crate) fn signed_payload(&self) -> String {
        serde_json::to_string(&SignedFields {
            epoch: &self.epoch,
            coordinator: &self.coordinator,
            electorate: &self.electorate,
            tally: self.tally,
            quorum_threshold: self.quorum_threshold,
            value_hash: &self.value_hash,
            value: &self.value,
            committed_at: self.committed_at,
            seq: self.seq,
            leader: &self.leader,
            term: self.term,
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

/// Next sequence number for `coordinator` in `records` — the count of
/// that coordinator's existing entries plus one.
pub fn next_seq_for(records: &[CommitRecord], coordinator: &str) -> u64 {
    records
        .iter()
        .filter(|r| r.coordinator == coordinator)
        .count() as u64
        + 1
}

/// Sequence numbers from `coordinator` that a holder of `records` is
/// missing below `incoming_seq` — non-empty when a replicated record
/// arrives with gaps (dropped replication, or a forged seq that slipped
/// past nothing). `incoming_seq` is the seq of a record about to append.
pub fn missing_seqs(records: &[CommitRecord], coordinator: &str, incoming_seq: u64) -> Vec<u64> {
    let held: std::collections::BTreeSet<u64> = records
        .iter()
        .filter(|r| r.coordinator == coordinator)
        .map(|r| r.seq)
        .collect();
    (1..incoming_seq).filter(|s| !held.contains(s)).collect()
}

/// Persisted consensus term state (`~/.susi/term.json`). Raft's
/// `currentTerm`/`votedFor` analog simplified for susi's deterministic
/// bully election: `leader` is the last elected leader this node
/// observed, and `term` bumps every time leadership changes. Signed
/// into every `CommitRecord`, so a node that fell behind can adopt the
/// newer term from any replicated record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TermState {
    /// Monotonic consensus term — `0` until the first leadership claim.
    pub term: u64,
    /// `node_id` of the leader elected under `term`.
    pub leader: String,
    /// Unix seconds of the last term update.
    pub updated_at: u64,
}

/// Where term state lives.
pub fn term_path() -> PathBuf {
    SusiDirs::config_dir().join("term.json")
}

/// Current term state; defaults to `term 0, no leader` when the file is
/// absent or unreadable — a node with no term state has never observed
/// an election, which is a valid starting point.
pub fn load_term() -> TermState {
    load_term_from(&term_path())
}

/// Test seam: load term state from an explicit path.
pub fn load_term_from(path: &PathBuf) -> TermState {
    let Ok(text) = fs::read_to_string(path) else {
        return TermState {
            term: 0,
            leader: String::new(),
            updated_at: 0,
        };
    };
    serde_json::from_str(&text).unwrap_or(TermState {
        term: 0,
        leader: String::new(),
        updated_at: 0,
    })
}

/// Persist term state atomically (temp + rename — a crash mid-write
/// must never corrupt the term file into a state a peer could adopt).
pub fn save_term(state: &TermState) -> EaiResult<()> {
    save_term_to(&term_path(), state)
}

/// Test seam: persist term state to an explicit path.
pub fn save_term_to(path: &PathBuf, state: &TermState) -> EaiResult<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| EaiError::filesystem(format!("create {}: {e}", dir.display())))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string(state)
        .map_err(|e| EaiError::internal(format!("serialize term state: {e}")))?;
    fs::write(&tmp, body)
        .map_err(|e| EaiError::filesystem(format!("write {}: {e}", tmp.display())))?;
    fs::rename(&tmp, path)
        .map_err(|e| EaiError::filesystem(format!("rename {}: {e}", path.display())))
}

/// Serializes term read-modify-write sequences (claim + adopt) within
/// the process — two threads claiming leadership concurrently must not
/// interleave a lost bump.
static TERM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Cross-process mutual exclusion for file read-modify-write.
///
/// The in-process mutexes (TERM_LOCK, APPEND_LOCK) cover threads; this
/// lockfile covers sibling processes sharing a config dir — without it,
/// two processes could both read state N and each write N+1, forking the
/// term sequence or interleaving torn JSONL lines into the ledger.
/// Acquisition is `O_CREAT|O_EXCL` on `<name>.lock` inside `dir`; the
/// file records the holder pid for stale detection.
struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// ~3s of 10ms retries — the guarded sections are millisecond-scale,
    /// so a longer wait means a wedged holder, not contention.
    fn acquire(dir: &Path, name: &str) -> Option<Self> {
        // The guarded file may not exist yet — the lockfile lives in the
        // same directory, so it must be created before O_EXCL can succeed.
        fs::create_dir_all(dir).ok()?;
        let lock = dir.join(format!("{name}.lock"));
        for _ in 0..300 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(f, "{}", std::process::id());
                    return Some(Self { path: lock });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::is_stale(&lock) {
                        let _ = fs::remove_file(&lock);
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return None,
            }
        }
        None
    }

    /// A lockfile is stale when its recorded pid no longer exists
    /// (Linux `/proc`), or it has sat unclaimed for over a minute —
    /// either means the holder died mid-claim.
    fn is_stale(lock: &Path) -> bool {
        if let Ok(body) = fs::read_to_string(lock) {
            if let Ok(pid) = body.trim().parse::<u32>() {
                #[cfg(target_os = "linux")]
                {
                    if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                        return true;
                    }
                }
            }
        }
        fs::metadata(lock)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|e| e.as_secs() > 60)
            .unwrap_or(false)
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Called by a coordinator before sealing a commit: records the elected
/// leader and bumps the term when leadership changed, returning the
/// term to stamp on the record. Same-leader re-elections reuse the
/// current term — terms move only on real leadership transitions.
pub fn claim_leadership(leader: &str) -> u64 {
    claim_leadership_at(&term_path(), leader)
}

/// Test seam: claim leadership against an explicit term file.
pub fn claim_leadership_at(path: &PathBuf, leader: &str) -> u64 {
    let _g = TERM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Serialize the read-modify-write across processes. On lock failure we
    // skip the write entirely rather than race it — a skipped claim leaves
    // the file consistent (the next claim retries), while a racing write
    // is exactly the lost-bump this lock exists to prevent.
    let _file_lock = path.parent().and_then(|dir| FileLock::acquire(dir, "term"));
    let mut state = load_term_from(path);
    if state.leader != leader {
        state.term += 1;
        state.leader = leader.to_string();
        state.updated_at = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if _file_lock.is_some() {
            // Best-effort beyond the lock: a transient IO error still
            // seals with the in-memory value; the next load re-reads.
            let _ = save_term_to(path, &state);
        }
    }
    state.term
}

/// What a receiver should do with an incoming record's term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermVerdict {
    /// Record is current-term — accept normally.
    Current,
    /// Record carries a newer term — adopted into `term.json` (returns
    /// the adopted state). Raft's step-down rule: observe a higher term,
    /// update your own.
    Adopted(TermState),
    /// Record's term is behind the local term — the coordinator is
    /// working from stale leadership. Reject pushes; anti-entropy
    /// history fills bypass this check by calling `append` directly.
    Stale,
    /// Same term but a different leader than persisted — two leaders
    /// cannot legitimately share a term (split-brain evidence). The
    /// record is still appended (it is validly signed history), but the
    /// verdict surfaces the anomaly for the tool response.
    LeaderConflict,
}

/// Evaluate an incoming record's term against persisted state, adopting
/// higher terms. Call before `append` on the push path.
pub fn check_term(record: &CommitRecord) -> TermVerdict {
    check_term_at(&term_path(), record)
}

/// Test seam: `check_term` against an explicit term file.
pub fn check_term_at(path: &PathBuf, record: &CommitRecord) -> TermVerdict {
    let _g = TERM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Same cross-process serialization as claim_leadership_at — term
    // adoption is a read-modify-write and must not interleave a claim.
    let _file_lock = path.parent().and_then(|dir| FileLock::acquire(dir, "term"));
    let state = load_term_from(path);
    if record.term > state.term {
        let next = TermState {
            term: record.term,
            leader: record.leader.clone(),
            updated_at: std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        if _file_lock.is_some() {
            let _ = save_term_to(path, &next);
        }
        return TermVerdict::Adopted(next);
    }
    if record.term < state.term {
        return TermVerdict::Stale;
    }
    if !state.leader.is_empty() && !record.leader.is_empty() && record.leader != state.leader {
        return TermVerdict::LeaderConflict;
    }
    TermVerdict::Current
}

/// The cluster's consensus view reconstructed by folding the whole
/// ledger — the honest "state-machine replay": this struct is a pure
/// function of `commit_log.jsonl`, so any node holding the same records
/// derives the same view.
#[derive(Debug, Clone, Default)]
pub struct ClusterState {
    /// Highest term observed in the ledger.
    pub term: u64,
    /// Leader stamped on the highest-term records.
    pub leader: String,
    /// Per-coordinator high-water sequence number.
    pub coordinators: std::collections::BTreeMap<String, u64>,
    /// Total records that passed signature + consistency verification.
    pub decisions: usize,
    /// Anomalies found while replaying (signature failures, seq
    /// regressions, term regressions, equivocation).
    pub anomalies: Vec<String>,
}

/// Fold `records` into a `ClusterState`. File order is NOT the
/// authoritative order — anti-entropy legitimately appends repaired
/// history after newer records — so invariants are checked along each
/// coordinator's `seq` order instead:
///
/// - same `(coordinator, seq)` twice: identical → duplicate, different
///   content → equivocation
/// - term decreasing along a coordinator's seq order → term regression
/// - holes in a coordinator's seq range → sequence gap
pub fn replay_records(records: &[CommitRecord]) -> ClusterState {
    let mut state = ClusterState::default();
    let mut by_coord: std::collections::BTreeMap<String, Vec<&CommitRecord>> =
        std::collections::BTreeMap::new();
    for r in records {
        if !r.verify() {
            state.anomalies.push(format!(
                "invalid signature: {} seq {}",
                r.coordinator, r.seq
            ));
            continue;
        }
        state.decisions += 1;
        if r.term > state.term {
            state.term = r.term;
            state.leader = r.leader.clone();
        }
        by_coord.entry(r.coordinator.clone()).or_default().push(r);
    }
    for (coord, mut recs) in by_coord {
        recs.sort_by_key(|r| r.seq);
        for w in recs.windows(2) {
            if w[0].seq == w[1].seq {
                state.anomalies.push(if w[0] == w[1] {
                    format!("duplicate: {coord} seq {} appended twice", w[0].seq)
                } else {
                    format!(
                        "equivocation: {coord} seq {} has two different signed records",
                        w[0].seq
                    )
                });
            }
            if w[1].term < w[0].term && w[1].seq > w[0].seq {
                state.anomalies.push(format!(
                    "term regression: {coord} seq {} carries term {} below seq {}'s term {}",
                    w[1].seq, w[1].term, w[0].seq, w[0].term
                ));
            }
        }
        if let Some(high) = recs.iter().map(|r| r.seq).max() {
            let held: std::collections::BTreeSet<u64> = recs.iter().map(|r| r.seq).collect();
            let missing: Vec<u64> = (1..high).filter(|s| !held.contains(s)).collect();
            if !missing.is_empty() {
                state
                    .anomalies
                    .push(format!("sequence gap: {coord} missing seq {missing:?}"));
            }
            state.coordinators.insert(coord, high);
        }
    }
    state
}

/// Replay the local ledger into the cluster's consensus view.
pub fn replay() -> ClusterState {
    replay_records(&load())
}

/// Where the ledger lives: `~/.susi/commit_log.jsonl` — one JSON record
/// per line, append-only, shared with peers under the same cluster key.
pub fn ledger_path() -> PathBuf {
    SusiDirs::config_dir().join("commit_log.jsonl")
}

/// Serializes the verify→dedup-check→write sequence in-process: two
/// threads appending different records for the same `(coordinator, seq)`
/// must not both pass the equivocation check and both write.
static APPEND_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Append one record. Verifies the signature first — nothing unsigned or
/// forged is ever written to the ledger.
pub fn append(record: &CommitRecord) -> EaiResult<()> {
    append_to(&ledger_path(), record)
}

/// Test seam: append to an explicit path.
///
/// Delivery is idempotent: re-appending a record the ledger already
/// holds is a no-op success, so replication retries and anti-entropy
/// re-fetches can never duplicate an entry (which would corrupt the
/// count-based `next_seq_for` assignment). A *different* record claiming
/// the same `(coordinator, seq)` slot is equivocation — two
/// cluster-signed records cannot legitimately share a sequence number —
/// and is refused rather than silently ordering both. `seq == 0`
/// (pre-sequencing records) dedups on full-record equality only.
pub fn append_to(path: &PathBuf, record: &CommitRecord) -> EaiResult<()> {
    let _g = APPEND_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Cross-process: two nodes sharing the config dir must not interleave
    // the check+write — without the lockfile, both can pass the
    // equivocation check and both append, and JSONL writes can tear.
    // On lock failure we still proceed: a skipped lock risks a torn line
    // (which load skips) while blocking forever is worse.
    let _file_lock = path
        .parent()
        .and_then(|dir| FileLock::acquire(dir, "commit_log"));
    if !record.verify() {
        return Err(EaiError::protocol(
            "refusing to append a commit record that fails signature or consistency verification",
        ));
    }
    let held = load_from(path);
    if record.seq > 0 {
        if let Some(existing) = held
            .iter()
            .find(|r| r.coordinator == record.coordinator && r.seq == record.seq)
        {
            if existing == record {
                return Ok(());
            }
            return Err(EaiError::protocol(format!(
                "commit equivocation: {} seq {} already held with different content",
                record.coordinator, record.seq
            )));
        }
    } else if held.iter().any(|r| r == record) {
        return Ok(());
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
        let Some(rec) = CommitRecord::seal(CommitInput {
            coordinator: "node-a",
            leader: "node-a",
            electorate: vec!["AgentA".into(), "AgentB".into(), "PeerNode_1".into()],
            tally: 2,
            quorum_threshold: 2,
            value: "the answer is 42",
        }) else {
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
        let Some(mut rec) = CommitRecord::seal(CommitInput {
            coordinator: "node-a",
            leader: "node-a",
            electorate: vec!["A".into(), "B".into(), "C".into(), "D".into(), "E".into()],
            tally: 2, // below the threshold of 3
            quorum_threshold: 3,
            value: "v",
        }) else {
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

        let Some(rec) = CommitRecord::seal(CommitInput {
            coordinator: "node-a",
            leader: "node-a",
            electorate: vec!["A".into(), "B".into()],
            tally: 2,
            quorum_threshold: 2,
            value: "v1",
        }) else {
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
            seq: 0,
            leader: String::new(),
            term: 0,
            signature: String::new(),
        };
        assert!(append_to(&path, &unsigned).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn seq_advances_per_coordinator_and_gaps_are_detected() {
        let _g = test_key_guard();
        let dir = std::env::temp_dir().join(format!("susi_seq_{}", std::process::id()));
        let path = dir.join("commit_log.jsonl");
        let _ = fs::remove_dir_all(&dir);

        // Seal reads the shared ledger path for seq — bypass it in the
        // test by asserting on next_seq_for/missing_seqs directly, then
        // append two records under the test path.
        assert_eq!(next_seq_for(&[], "node-a"), 1);
        let Some(mut r1) = CommitRecord::seal(CommitInput {
            coordinator: "node-a",
            leader: "node-a",
            electorate: vec!["A".into(), "B".into()],
            tally: 2,
            quorum_threshold: 2,
            value: "v1",
        }) else {
            eprintln!("skip: no cluster.key on this host");
            return;
        };
        r1.seq = 1;
        if let Some(key) = crate::susi_config::cluster_key::cluster_key() {
            r1.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &key,
                r1.signed_payload().as_bytes(),
            );
        }
        append_to(&path, &r1).unwrap();
        let held = load_from(&path);
        assert_eq!(next_seq_for(&held, "node-a"), 2);
        assert_eq!(next_seq_for(&held, "node-b"), 1);
        // Receiving seq 4 while holding only seq 1 → missing 2 and 3.
        assert_eq!(missing_seqs(&held, "node-a", 4), vec![2, 3]);
        assert!(missing_seqs(&held, "node-a", 2).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_is_idempotent_and_detects_equivocation() {
        let _g = test_key_guard();
        let dir = std::env::temp_dir().join(format!("susi_equiv_{}", std::process::id()));
        let path = dir.join("commit_log.jsonl");
        let _ = fs::remove_dir_all(&dir);

        let Some(r1) = CommitRecord::seal(CommitInput {
            coordinator: "node-a",
            leader: "node-a",
            electorate: vec!["A".into(), "B".into()],
            tally: 2,
            quorum_threshold: 2,
            value: "v1",
        }) else {
            eprintln!("skip: no cluster.key on this host");
            return;
        };
        append_to(&path, &r1).unwrap();
        // Re-delivery of the identical record is a no-op, not a dup.
        append_to(&path, &r1).unwrap();
        assert_eq!(load_from(&path).len(), 1);

        // A *different* signed record claiming the same (coordinator, seq)
        // slot is equivocation — refuse it rather than ordering both.
        let Some(mut conflict) = CommitRecord::seal(CommitInput {
            coordinator: "node-a",
            leader: "node-a",
            electorate: vec!["A".into(), "B".into()],
            tally: 2,
            quorum_threshold: 2,
            value: "a different decision",
        }) else {
            eprintln!("skip: no cluster.key on this host");
            return;
        };
        conflict.seq = r1.seq;
        if let Some(key) = crate::susi_config::cluster_key::cluster_key() {
            conflict.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &key,
                conflict.signed_payload().as_bytes(),
            );
        }
        let err = append_to(&path, &conflict);
        assert!(err.is_err(), "equivocating record must be refused");
        assert!(err.unwrap_err().to_string().contains("equivocation"));
        // The original record still stands alone.
        let held = load_from(&path);
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].value, "v1");
        let _ = fs::remove_dir_all(&dir);
    }

    fn resign(rec: &mut CommitRecord) {
        if let Some(key) = crate::susi_config::cluster_key::cluster_key() {
            rec.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &key,
                rec.signed_payload().as_bytes(),
            );
        }
    }

    #[test]
    fn claim_leadership_bumps_term_and_check_term_classifies() {
        let _g = test_key_guard();
        let dir = std::env::temp_dir().join(format!("susi_term_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let tpath = dir.join("term.json");

        // First claim establishes term 1; same leader keeps it; a real
        // transition bumps it.
        assert_eq!(claim_leadership_at(&tpath, "node-a"), 1);
        assert_eq!(load_term_from(&tpath).leader, "node-a");
        assert_eq!(claim_leadership_at(&tpath, "node-a"), 1);
        assert_eq!(claim_leadership_at(&tpath, "node-b"), 2);
        assert_eq!(load_term_from(&tpath).term, 2);

        let Some(rec) = CommitRecord::seal(CommitInput {
            coordinator: "node-b",
            leader: "node-b",
            electorate: vec!["A".into(), "B".into()],
            tally: 2,
            quorum_threshold: 2,
            value: "v",
        }) else {
            eprintln!("skip: no cluster.key on this host");
            return;
        };
        let mut cur = rec.clone();
        cur.term = 2;
        cur.leader = "node-b".into();
        resign(&mut cur);
        assert_eq!(check_term_at(&tpath, &cur), TermVerdict::Current);

        // Older term → stale push.
        let mut stale = rec.clone();
        stale.term = 1;
        resign(&mut stale);
        assert_eq!(check_term_at(&tpath, &stale), TermVerdict::Stale);

        // Newer term → adopted into the term file.
        let mut ahead = rec.clone();
        ahead.term = 5;
        ahead.leader = "node-c".into();
        resign(&mut ahead);
        match check_term_at(&tpath, &ahead) {
            TermVerdict::Adopted(s) => {
                assert_eq!(s.term, 5);
                assert_eq!(s.leader, "node-c");
            }
            v => panic!("expected Adopted, got {v:?}"),
        }
        assert_eq!(load_term_from(&tpath).term, 5);

        // Same term, different leader → split-brain evidence.
        let mut conflict = rec.clone();
        conflict.term = 5;
        conflict.leader = "node-d".into();
        resign(&mut conflict);
        assert_eq!(
            check_term_at(&tpath, &conflict),
            TermVerdict::LeaderConflict
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_reconstructs_cluster_state_and_flags_anomalies() {
        let _g = test_key_guard();

        let seal_at = |coord: &str, leader: &str, seq: u64, term: u64, value: &str| {
            let mut r = CommitRecord::seal(CommitInput {
                coordinator: coord,
                leader,
                electorate: vec!["A".into(), "B".into()],
                tally: 2,
                quorum_threshold: 2,
                value,
            })?;
            r.seq = seq;
            r.term = term;
            resign(&mut r);
            Some(r)
        };

        // node-a seq 1..3 across terms 1→2, node-b seq 1..2 at term 2,
        // plus a node-a seq-1 equivocation and a term regression.
        let Some(records_base) = (|| {
            Some(vec![
                seal_at("node-a", "node-a", 1, 1, "a1")?,
                seal_at("node-a", "node-a", 2, 1, "a2")?,
                seal_at("node-a", "node-b", 3, 2, "a3")?,
                seal_at("node-b", "node-b", 1, 2, "b1")?,
                seal_at("node-b", "node-b", 3, 2, "b3")?, // seq 2 missing → gap
                seal_at("node-a", "node-a", 4, 1, "a4-old-term")?, // term regression
            ])
        })() else {
            eprintln!("skip: no cluster.key on this host");
            return;
        };
        let mut records = records_base;
        let mut equivocating = records[0].clone();
        equivocating.value = "a1-forked".into();
        equivocating.value_hash = hex::encode(Sha256::digest(equivocating.value.as_bytes()));
        resign(&mut equivocating);
        records.push(equivocating);

        let state = replay_records(&records);
        assert_eq!(state.term, 2);
        assert_eq!(state.leader, "node-b");
        assert_eq!(state.decisions, 7);
        assert_eq!(state.coordinators.get("node-a"), Some(&4));
        assert_eq!(state.coordinators.get("node-b"), Some(&3));
        assert!(
            state.anomalies.iter().any(|a| a.contains("equivocation")),
            "equivocation must be flagged: {:?}",
            state.anomalies
        );
        assert!(
            state.anomalies.iter().any(|a| a.contains("sequence gap")),
            "node-b seq gap must be flagged: {:?}",
            state.anomalies
        );
        assert!(
            state
                .anomalies
                .iter()
                .any(|a| a.contains("term regression")),
            "node-a seq4 term1 regression must be flagged: {:?}",
            state.anomalies
        );
    }

    #[test]
    fn file_lock_excludes_second_holder_and_releases_on_drop() {
        let dir = std::env::temp_dir().join(format!(
            "susi_locktest_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        {
            let _a = FileLock::acquire(&dir, "term").expect("first acquire");
            // A second acquire while the first is held must not steal —
            // it should either time out or fail fast.
            assert!(FileLock::acquire(&dir, "term").is_none());
            // A different lock name in the same dir is independent.
            let _b = FileLock::acquire(&dir, "commit_log").expect("independent name");
        }
        // Both dropped — reacquisition succeeds.
        let _c = FileLock::acquire(&dir, "term").expect("reacquire after drop");
        drop(_c);
        // Stale detection: a lockfile naming a dead pid is reclaimed.
        let stale = dir.join("term.lock");
        fs::write(&stale, "999999\n").unwrap();
        let _d = FileLock::acquire(&dir, "term").expect("stale pid reclaim");
        drop(_d);
        let _ = fs::remove_dir_all(&dir);
    }
}

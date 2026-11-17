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
//! ## Committed membership
//!
//! `kind` carries `member_add`/`member_remove`/`member_unban` records —
//! Raft's committed configuration-entry analog. They gate through the
//! same signature/term/seq/chain checks, then apply their roster delta
//! to `peers.json`/`peers_banned.json` beside the ledger (the ledger IS
//! applied state). Two authority rules make eviction real: member
//! deltas only apply when the sealing coordinator is a current explicit
//! member (`member_coordinator_known` — an evicted node still holds
//! cluster.key, so HMAC alone cannot authorize roster changes), and
//! term adoption likewise only honors member coordinators
//! (`coordinator_known`). A delta naming this node never writes a
//! roster row: self-remove instead lands `cluster_evicted.json`, and
//! the node stands down until a committed unban clears it.
//!
//! The record format and read/verification contracts remain here. Durable
//! mutation and compaction run through the authenticated cluster socket in
//! the separately hosted implementation, sharing the same on-disk state.
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use susi_paths::SusiDirs;
#[path = "commit_log_socket.rs"]
mod socket;
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
    /// Raft's prevLogIndex/prevLogTerm analog: the `epoch` of this
    /// coordinator's immediately preceding record (its chain head when
    /// this record was sealed), covered by the signature. A receiver
    /// holding the predecessor whose epoch differs sees proof the
    /// coordinator's log diverged — a fork, not a gap. Empty on genesis
    /// records and pre-linkage history.
    #[serde(default)]
    pub prev_epoch: String,
    /// Record kind: empty is a quorum decision (the common case).
    /// `member_add`/`member_remove` are leader-signed roster deltas
    /// committed through the same ledger — receivers apply them to
    /// `peers.json` on append. Skipped when empty so decision records
    /// stay byte-compatible with the pre-membership format.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// HMAC-SHA256 hex over `signed_payload()` under `cluster.key`.
    /// Empty until `seal` runs; a record with an empty signature never
    /// verifies.
    pub signature: String,
    /// Ed25519 signature (hex) by the coordinator's `node.key` over
    /// `signature` — non-repudiable coordinator attribution. The HMAC
    /// proves cluster-key possession (membership); this proves WHICH
    /// member sealed the record. Enforced at intake once the roster
    /// binds the coordinator's pubkey (`attribution_valid_at`);
    /// unsigned records stay admissible only in the pre-binding
    /// window. Skipped when empty for wire compatibility with
    /// pre-PKI ledgers.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member_sig: String,
    /// The subject's Ed25519 pubkey, carried only on `member_add`
    /// records — the ledger half of key binding: every receiver binds
    /// the member's signing key when the committed add applies, so
    /// attribution enforcement converges with the roster itself.
    /// First write wins; re-binding requires remove + re-add.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member_pubkey: String,
    /// The subject's own Ed25519 signature over
    /// `susi-bind-v1:{node_id}:{member_pubkey}` — the binding
    /// attestation sourced from the v3 handshake pong, not the
    /// proposer. Required at intake when `member_pubkey` is present:
    /// a proposer can no longer bind a key the subject never claimed
    /// (wrong-key bindings were a DoS on the victim's records).
    /// Signature-transparent like `member_sig` — outside `SignedFields`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject_sig: String,
    /// Member endorsements for privileged (member/rekey) records — the
    /// joint-consensus half of roster safety: once a quorum of the
    /// electorate's members have bound pubkeys, a privileged record is
    /// admissible only when a majority of the members *bound at
    /// `committed_at`* signed `susi-endorse-v1:{signature}` (the
    /// coordinator's own `member_sig` counts as its endorsement). A
    /// rogue or stale leader can no longer rewrite the roster alone —
    /// a bound electorate must agree, the same quorum a Raft config
    /// change asks of `C_old`. Endorsements sign the record's HMAC
    /// signature, not the record, so they sit outside `signed_payload`.
    /// Skipped when empty for wire compatibility with pre-endorsement
    /// ledgers; the requirement itself is computed receiver-side from
    /// roster bindings, never from field presence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endorsements: Vec<MemberEndorsement>,
}
/// One member's endorsement of a privileged record: `sig` is the
/// member's Ed25519 signature over `susi-endorse-v1:{record.signature}`,
/// verifiable against the pubkey bound for `node` in the roster.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemberEndorsement {
    /// `node_id` of the endorsing member.
    pub node: String,
    /// `member_sign(endorsement_payload(record.signature))` hex.
    pub sig: String,
}
/// Membership record kinds — roster deltas committed through the same
/// signed ledger as quorum decisions (Raft's committed configuration
/// change analog). Carried in `CommitRecord::kind`; the member spec is
/// `value` as `node_id@address`.
pub const KIND_MEMBER_ADD: &str = "member_add";
/// Drop the member and ban re-verification — eviction must take effect
/// on every receiver the moment the record lands.
pub const KIND_MEMBER_REMOVE: &str = "member_remove";
/// Lift a previously committed ban — the member can re-verify naturally
/// on its next signed handshake (it is not re-added).
pub const KIND_MEMBER_UNBAN: &str = "member_unban";
/// Cluster-key rotation, prepare phase (Raft's epoch/leader-change
/// analog): the `value` is the SHA-256 fingerprint of the next-epoch
/// key, sealed under the CURRENT key. Applying this record does NOT
/// rotate — it only commits which key the cluster agreed to stage.
/// Receivers stage the matching key via `cluster_rekey_stage`; rotation
/// happens when a `cluster_rekey_activate` record lands. A rekey that
/// never reaches activate leaves the fingerprint committed but the
/// epoch unchanged — the abortable half of the two-phase rotation.
pub const KIND_CLUSTER_REKEY: &str = "cluster_rekey";
/// Cluster-key rotation, commit phase: applying this record activates
/// the staged `cluster.key.next` (fingerprint match pins the file to
/// this record — an activate can never swap in an arbitrary key). The
/// old key is retained as `cluster.key.prev` so pre-rotation history
/// stays verifiable. Members that never staged are cryptographically
/// stranded — revocation of evicted key-holders is the point.
pub const KIND_CLUSTER_REKEY_ACTIVATE: &str = "cluster_rekey_activate";
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
/// serialize this identical shape, so the HMAC is reproducible in every
/// process that carries the ledger.
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
    /// Omitted when empty so records sealed before the linkage field
    /// existed still verify against their original signature payload —
    /// wire compatibility is signature compatibility here.
    #[serde(skip_serializing_if = "str::is_empty")]
    prev_epoch: &'a str,
    /// Same omission rule as `prev_epoch` — decision records must not
    /// grow a field their existing signatures never covered.
    #[serde(skip_serializing_if = "str::is_empty")]
    kind: &'a str,
}
impl CommitRecord {
    /// Build and seal a record for a freshly committed quorum decision.
    /// `seq` is assigned from the local ledger — the count of this
    /// coordinator's existing records plus one — so per-coordinator
    /// ordering survives restarts. Returns `None` when the cluster key is
    /// absent — a node outside the cluster cannot mint commit records.
    pub fn seal(input: CommitInput<'_>) -> Option<Self> {
        let key = crate::susi_config::cluster_key::cluster_key()?;
        let committed_at = susi_paths::unix_epoch_elapsed().as_secs();
        let mut sorted = input.electorate;
        sorted.sort();
        let epoch = susi_paths::hex::encode(Sha256::digest(
            format!("{}|{}", sorted.join(","), committed_at).as_bytes(),
        ));
        let value_hash = susi_paths::hex::encode(Sha256::digest(input.value.as_bytes()));
        let held = load();
        let mut rec = CommitRecord {
            epoch,
            coordinator: input.coordinator.to_string(),
            electorate: sorted,
            tally: input.tally,
            quorum_threshold: input.quorum_threshold,
            value_hash,
            value: input.value.to_string(),
            committed_at,
            seq: next_seq_for(&held, input.coordinator),
            leader: input.leader.to_string(),
            term: load_term().term,
            // Chain link: the epoch of this coordinator's current head
            // (highest seq) — "" on its first record. Receivers holding
            // a different head at seq-1 see fork evidence, not a gap.
            prev_epoch: chain_head_epoch(&held, input.coordinator).unwrap_or_default(),
            kind: String::new(),
            signature: String::new(),
            member_sig: String::new(),
            member_pubkey: String::new(),
            subject_sig: String::new(),
            endorsements: Vec::new(),
        };
        rec.signature =
            crate::susi_config::cluster_key::hmac_sha256_hex(&key, rec.signed_payload().as_bytes());
        rec.member_sig =
            crate::susi_config::cluster_key::member_sign(&rec.signature).unwrap_or_default();
        Some(rec)
    }
    /// Seal a membership-change record (Raft's committed configuration
    /// entry analog). `member` is `node_id@address`; `electorate` is the
    /// roster as the coordinator saw it — audit context, not a vote.
    /// `member_pubkey` is the subject's Ed25519 verifying key as
    /// attested by the sealer's handshake — carried only on
    /// `member_add` so receivers bind the member's signing key when
    /// the committed add applies (first write wins). Like Raft's
    /// config entries, roster deltas are leader-signed and replicated
    /// rather than voted on per entry, so they carry
    /// `tally = quorum_threshold = 0` honestly: `verify` checks the
    /// signature and value integrity, not a quorum that never happened.
    /// Receivers apply the delta to `peers.json` on append — the ledger
    /// is the applied membership state.
    #[allow(clippy::too_many_arguments)]
    // The six parameters are one flat delta spec (who seals, under what
    // leadership, which kind, which subject, which electorate, which
    // subject key) — a wrapper struct would only rename the same list.
    pub fn seal_member(
        coordinator: &str,
        leader: &str,
        kind: &str,
        member: &str,
        electorate: Vec<String>,
        member_pubkey: &str,
    ) -> Option<Self> {
        // A member spec must be `id@address` and `kind` a known member
        // kind — sealing either malformed produces a record that fails
        // verify on every receiver.
        if !matches!(
            kind,
            KIND_MEMBER_ADD | KIND_MEMBER_REMOVE | KIND_MEMBER_UNBAN
        ) {
            return None;
        }
        let (id, addr) = member.split_once('@')?;
        if id.is_empty() || addr.is_empty() {
            return None;
        }
        let mut rec = Self::seal_signed(coordinator, leader, kind, member, electorate)?;
        // Only adds carry a key binding — remove/unban deltas act on
        // members whose binding already exists (or is moot).
        if kind == KIND_MEMBER_ADD
            && susi_paths::hex::decode(member_pubkey)
                .ok()
                .is_some_and(|b| b.len() == 32)
        {
            rec.member_pubkey = member_pubkey.to_string();
        }
        Some(rec)
    }
    /// Seal a cluster-key rotation record (prepare phase).
    /// `fingerprint` is the SHA-256 hex of the next-epoch key
    /// (cluster_key::key_fingerprint); the record is signed under the
    /// CURRENT key — receivers verify it pre-rotation and stage the
    /// matching key. Applying this record does NOT rotate; that is the
    /// job of `seal_rekey_activate`'s record.
    pub fn seal_rekey(
        coordinator: &str,
        leader: &str,
        fingerprint: &str,
        electorate: Vec<String>,
    ) -> Option<Self> {
        if susi_paths::hex::decode(fingerprint).ok()?.len() != 32 {
            return None;
        }
        Self::seal_signed(
            coordinator,
            leader,
            KIND_CLUSTER_REKEY,
            fingerprint,
            electorate,
        )
    }
    /// Seal the commit-phase record for a staged rotation — applying it
    /// activates the staged key whose fingerprint it carries. Sealed
    /// under the CURRENT (pre-rotation) key: members receive it before
    /// they activate, so it must verify under the old epoch.
    pub fn seal_rekey_activate(
        coordinator: &str,
        leader: &str,
        fingerprint: &str,
        electorate: Vec<String>,
    ) -> Option<Self> {
        if susi_paths::hex::decode(fingerprint).ok()?.len() != 32 {
            return None;
        }
        Self::seal_signed(
            coordinator,
            leader,
            KIND_CLUSTER_REKEY_ACTIVATE,
            fingerprint,
            electorate,
        )
    }
    /// Shared seal path for the non-decision record kinds — signature,
    /// seq, term, and chain linkage are identical; only the kind's
    /// value validation differs (done by each caller).
    fn seal_signed(
        coordinator: &str,
        leader: &str,
        kind: &str,
        value: &str,
        electorate: Vec<String>,
    ) -> Option<Self> {
        let key = crate::susi_config::cluster_key::cluster_key()?;
        let committed_at = susi_paths::unix_epoch_elapsed().as_secs();
        let mut sorted = electorate;
        sorted.sort();
        let epoch = susi_paths::hex::encode(Sha256::digest(
            format!("{}|{}|{}", kind, sorted.join(","), committed_at).as_bytes(),
        ));
        let held = load();
        let mut rec = CommitRecord {
            epoch,
            coordinator: coordinator.to_string(),
            electorate: sorted,
            tally: 0,
            quorum_threshold: 0,
            value_hash: susi_paths::hex::encode(Sha256::digest(value.as_bytes())),
            value: value.to_string(),
            committed_at,
            seq: next_seq_for(&held, coordinator),
            leader: leader.to_string(),
            term: load_term().term,
            prev_epoch: chain_head_epoch(&held, coordinator).unwrap_or_default(),
            kind: kind.to_string(),
            signature: String::new(),
            member_sig: String::new(),
            member_pubkey: String::new(),
            subject_sig: String::new(),
            endorsements: Vec::new(),
        };
        rec.signature =
            crate::susi_config::cluster_key::hmac_sha256_hex(&key, rec.signed_payload().as_bytes());
        rec.member_sig =
            crate::susi_config::cluster_key::member_sign(&rec.signature).unwrap_or_default();
        Some(rec)
    }
    /// `(kind, node_id, address)` for a membership record — `None` for
    /// decisions and member values that are not `id@address`.
    pub fn member_delta(&self) -> Option<(&str, &str, &str)> {
        // Only known member kinds — an arbitrary non-empty `kind` with
        // a well-formed `id@address` value would otherwise verify,
        // append, and inflate replay's membership count while applying
        // nothing. Unknown kinds must fail verify below.
        if !matches!(
            self.kind.as_str(),
            KIND_MEMBER_ADD | KIND_MEMBER_REMOVE | KIND_MEMBER_UNBAN
        ) {
            return None;
        }
        let (id, addr) = self.value.split_once('@')?;
        if id.is_empty() || addr.is_empty() {
            return None;
        }
        Some((self.kind.as_str(), id, addr))
    }
    /// The committed key fingerprint for either rekey-phase record —
    /// `None` for other kinds or a malformed (non-32-byte-hex) value.
    pub fn rekey_fingerprint(&self) -> Option<&str> {
        if !matches!(
            self.kind.as_str(),
            KIND_CLUSTER_REKEY | KIND_CLUSTER_REKEY_ACTIVATE
        ) {
            return None;
        }
        if susi_paths::hex::decode(&self.value).ok()?.len() != 32 {
            return None;
        }
        Some(self.value.as_str())
    }
    /// The exact bytes the signature covers (compact JSON of
    /// `SignedFields` — serde emits struct fields in declaration order,
    /// so this is stable across processes).
    /// Public so consumer-crate tests can re-sign a record after mutating
    /// fields (e.g. fabricating a seq gap); production callers go through
    /// `seal`.
    #[doc(hidden)]
    pub fn signed_payload(&self) -> String {
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
            prev_epoch: &self.prev_epoch,
            kind: &self.kind,
        })
        .unwrap_or_default()
    }
    /// Which key epoch this record's signature verifies under. `Current`
    /// is the live cluster key; `Prev` is the key retired by the most
    /// recent `cluster_rekey` activation (`cluster.key.prev`). A `Prev`
    /// signature proves the record is genuine pre-rotation history — it
    /// can never authorize new appends at the frontier (see `append_to`),
    /// but it can fill chain-pinned gaps so rotated members keep one
    /// convergent ledger.
    pub fn signature_epoch(&self) -> Option<KeyEpoch> {
        if self.signature.is_empty() {
            return None;
        }
        let payload = self.signed_payload();
        if let Some(key) = crate::susi_config::cluster_key::cluster_key() {
            let expected =
                crate::susi_config::cluster_key::hmac_sha256_hex(&key, payload.as_bytes());
            if expected == self.signature {
                return Some(KeyEpoch::Current);
            }
        }
        if let Some(prev) = crate::susi_config::cluster_key::prev_key() {
            let expected =
                crate::susi_config::cluster_key::hmac_sha256_hex(&prev, payload.as_bytes());
            if expected == self.signature {
                return Some(KeyEpoch::Prev);
            }
        }
        None
    }
    /// Internal consistency independent of the signature: a correctly
    /// signed decision record can still claim a tally that never reached
    /// quorum — check it. Membership records carry no vote by design
    /// (leader-signed replication), so only decisions get the quorum
    /// check; member kinds instead require a well-formed `id@address`
    /// value and a rekey record a well-formed fingerprint.
    fn internally_consistent(&self) -> bool {
        if self.kind.is_empty() {
            if self.quorum_threshold != self.electorate.len() / 2 + 1
                || self.tally < self.quorum_threshold
            {
                return false;
            }
        } else if self.member_delta().is_none() && self.rekey_fingerprint().is_none() {
            return false;
        }
        susi_paths::hex::encode(Sha256::digest(self.value.as_bytes())) == self.value_hash
    }
    /// Verify a received record against the local cluster key and its own
    /// internal consistency (tally crossed quorum, hash matches value).
    /// Rejects unsigned, forged, tampered, and self-inconsistent records.
    /// Accepts both current- and prior-epoch signatures — see
    /// `signature_epoch`; `append_to` further constrains prior-epoch
    /// records to chain-pinned history fills.
    pub fn verify(&self) -> bool {
        self.signature_epoch().is_some() && self.internally_consistent()
    }
    /// Signature epoch + consistency together — `None` rejects.
    pub fn verify_key_epoch(&self) -> Option<KeyEpoch> {
        self.signature_epoch()
            .filter(|_| self.internally_consistent())
    }
}
/// Which cluster-key epoch a record's signature verifies under.
/// `Ord` derives so `Prev < Current` — callers can require current-era
/// signatures with `>= KeyEpoch::Current` style checks if needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyEpoch {
    /// Signed under `cluster.key.prev` — genuine pre-rotation history.
    /// Never authoritative for new appends at the frontier.
    Prev,
    /// Signed under the live `cluster.key`.
    Current,
}
/// Shared env-mutation lock for tests: `cluster_key()` resolves through
/// XDG env vars, so any test that sets `XDG_CONFIG_HOME`/`HOME` must hold
/// this guard across the whole seal → verify → append sequence, or a
/// parallel test can swap the env mid-sequence and break signature
/// agreement. Public so consumer crates' own tests take the same lock.
#[doc(hidden)]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Next sequence number for `coordinator` in `records` — one past that
/// coordinator's highest seq. Max-based, not count-based: a ledger
/// holding {1,3} (a gap a peer later repairs) must seal seq 4, never
/// re-claim seq 3 — re-using a held slot is equivocation against the
/// coordinator's own history.
pub fn next_seq_for(records: &[CommitRecord], coordinator: &str) -> u64 {
    records
        .iter()
        .filter(|r| r.coordinator == coordinator)
        .map(|r| r.seq)
        .max()
        .unwrap_or(0)
        + 1
}
/// The epoch of `coordinator`'s current chain head — its highest-seq
/// record — or `None` when it has no records. New records seal this into
/// `prev_epoch`, giving receivers the Raft-style predecessor check.
fn chain_head_epoch(records: &[CommitRecord], coordinator: &str) -> Option<String> {
    records
        .iter()
        .filter(|r| r.coordinator == coordinator)
        .max_by_key(|r| r.seq)
        .map(|r| r.epoch.clone())
}
/// Sequence numbers from `coordinator` that a holder of `records` is
/// missing below `incoming_seq` — non-empty when a replicated record
/// arrives with gaps (dropped replication, or a forged seq that slipped
/// past nothing). `incoming_seq` is the seq of a record about to append.
pub fn missing_seqs(records: &[CommitRecord], coordinator: &str, incoming_seq: u64) -> Vec<u64> {
    missing_seqs_floored(records, coordinator, incoming_seq, 0)
}
/// `missing_seqs` bounded below by a snapshot floor: seqs at or under
/// the compaction high-water are archived, not missing — reporting them
/// would trigger repair pulls for history the snapshot already covers.
pub fn missing_seqs_floored(
    records: &[CommitRecord],
    coordinator: &str,
    incoming_seq: u64,
    floor: u64,
) -> Vec<u64> {
    let held: std::collections::BTreeSet<u64> = records
        .iter()
        .filter(|r| r.coordinator == coordinator)
        .map(|r| r.seq)
        .collect();
    (floor + 1..incoming_seq)
        .filter(|s| !held.contains(s))
        .collect()
}
/// The snapshot's last-included seq for `coordinator`, or 0 when no
/// compaction has run — the intake-side floor for `missing_seqs` and
/// the below-floor dedup check in `append_to`.
pub fn snapshot_floor(coordinator: &str) -> u64 {
    load_snapshot()
        .and_then(|s| s.high_water.get(coordinator).copied())
        .unwrap_or(0)
}
/// Parsed records of a ledger/archive file, cached by (mtime_ns, len):
/// Status and verification surfaces share parsed history. A stamp change
/// reloads; an absent file caches as empty.
fn cached_records(path: &Path) -> Vec<CommitRecord> {
    use std::sync::{Mutex, OnceLock};
    type Cache = Mutex<std::collections::HashMap<PathBuf, ((u128, u64), Vec<CommitRecord>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let stamp = fs::metadata(path)
        .ok()
        .and_then(|m| {
            m.modified().ok().map(|t| {
                (
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0),
                    m.len(),
                )
            })
        })
        .unwrap_or((0, 0));
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    {
        let map = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, records)) = map.get(path) {
            if *s == stamp {
                return records.clone();
            }
        }
    }
    let records: Vec<CommitRecord> = fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str::<CommitRecord>(l).ok())
                .collect()
        })
        .unwrap_or_default();
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_path_buf(), (stamp, records.clone()));
    records
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
fn parse_term_bytes(raw: &[u8]) -> TermState {
    let default = || TermState {
        term: 0,
        leader: String::new(),
        updated_at: 0,
    };
    let Ok(text) = std::str::from_utf8(raw) else {
        return default();
    };
    serde_json::from_str(text).unwrap_or_else(|_| default())
}
/// Test seam: load term state from an explicit path. Stamp-cached (`mtime`,
/// `len`, `(dev, ino)`) — the intake path consults term state on every record,
/// and an anti-entropy pull of hundreds of records should not re-parse the
/// file each time; `save_term_to`'s atomic rename replaces the inode/stamp.
pub fn load_term_from(path: &std::path::Path) -> TermState {
    let Some(raw) = crate::susi_config::cluster_key::cached_file_bytes(path) else {
        return parse_term_bytes(&[]);
    };
    parse_term_bytes(&raw)
}
/// Read persisted term state directly without consulting `cached_file_bytes`,
/// used inside `FileLock`-protected read-modify-write critical sections.
fn load_term_uncached(path: &Path) -> TermState {
    let Ok(raw) = crate::susi_config::read_atomic_bytes(path) else {
        return parse_term_bytes(&[]);
    };
    parse_term_bytes(&raw)
}
/// Persist term state atomically (temp + rename — a crash mid-write
/// must never corrupt the term file into a state a peer could adopt).
pub fn save_term(state: &TermState) -> EaiResult<()> {
    save_term_to(&term_path(), state)
}
/// Test seam: persist term state to an explicit path.
pub fn save_term_to(path: &Path, state: &TermState) -> EaiResult<()> {
    let body = serde_json::to_string(state)
        .map_err(|e| EaiError::internal(format!("serialize term state: {e}")))?;
    crate::susi_config::atomic_write_bytes(path, body.as_bytes())
        .map_err(|e| EaiError::filesystem(format!("write {}: {e}", path.display())))
}
/// Serializes term read-modify-write sequences (claim + adopt) within
/// the process — two threads claiming leadership concurrently must not
/// interleave a lost bump.
static TERM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
const TERM_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Cross-process mutual exclusion for file read-modify-write.
///
/// Cross-process lockfile; lives in `susi_config::file_lock` so crates that
/// do not mount this module (the audit chain) share the same primitive.
pub use crate::susi_config::file_lock::FileLock;
/// Called by a coordinator before sealing a commit: records the elected
/// leader and bumps the term when leadership changed, returning the
/// term to stamp on the record. Same-leader re-elections reuse the
/// current term — terms move only on real leadership transitions.
/// An evicted node cannot claim: its term freezes while the marker
/// stands, so exile can't be spent inflating a term the cluster would
/// adopt on re-admission.
pub fn claim_leadership(leader: &str) -> u64 {
    if SusiDirs::config_dir().join("cluster_evicted.json").exists() {
        return load_term().term;
    }
    claim_leadership_at(&term_path(), leader)
}
/// Test seam: claim leadership against an explicit term file.
pub fn claim_leadership_at(path: &Path, leader: &str) -> u64 {
    let _g = TERM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Serialize the read-modify-write across processes. On lock failure we
    // decline the claim entirely and return the persisted term — the
    // caller then seals under current leadership, which receivers flag as
    // a conflict, rather than minting a phantom term that was never
    // durably claimed.
    let Some(_file_lock) = path
        .parent()
        .and_then(|dir| FileLock::acquire_within(dir, "term", TERM_LOCK_TIMEOUT))
    else {
        return load_term_uncached(path).term;
    };
    let mut state = load_term_uncached(path);
    if state.leader != leader {
        state.term += 1;
        state.leader = leader.to_string();
        state.updated_at = susi_paths::unix_epoch_elapsed().as_secs();
        let _ = save_term_to(path, &state);
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
pub fn check_term_at(path: &Path, record: &CommitRecord) -> TermVerdict {
    let _g = TERM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Same cross-process serialization as claim_leadership_at — term
    // adoption is a read-modify-write and must not interleave a claim.
    let _file_lock = path
        .parent()
        .and_then(|dir| FileLock::acquire_within(dir, "term", TERM_LOCK_TIMEOUT));
    let state = load_term_uncached(path);
    if record.term > state.term {
        let next = TermState {
            term: record.term,
            leader: record.leader.clone(),
            updated_at: susi_paths::unix_epoch_elapsed().as_secs(),
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
    /// Committed membership records folded in append order — the count
    /// of roster deltas the ledger carries.
    pub memberships: usize,
    /// Derived committed roster (`node_id -> address`) — the members a
    /// replay of this ledger converges to. Nodes holding identical logs
    /// derive identical rosters.
    pub roster: std::collections::BTreeMap<String, String>,
    /// Members evicted by committed removals (`node_id`, `address`) —
    /// a committed `member_add` cannot resurrect a banned member.
    pub banned: std::collections::BTreeSet<(String, String)>,
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
    fold_records(
        ClusterState::default(),
        records,
        &std::collections::BTreeMap::new(),
    )
}
/// The fold behind `replay_records`/`replay`: `floors` carries the
/// snapshot's per-coordinator last-included seq. Records at or below
/// the floor are already represented in the seeded `state` — they are
/// signature-checked (corruption still reports) but skipped from the
/// fold and ordering checks: re-folding would double-apply deltas and
/// windowing them against the live anchors would fabricate
/// chain-breaks the snapshot boundary legitimately creates.
fn fold_records(
    mut state: ClusterState,
    records: &[CommitRecord],
    floors: &std::collections::BTreeMap<String, u64>,
) -> ClusterState {
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
        let floor = floors.get(&r.coordinator).copied().unwrap_or(0);
        if r.seq < floor {
            // Covered by the snapshot — already in the seeded state,
            // and excluded from ordering checks: windowing archived-era
            // strays against the live anchors would fabricate
            // chain-breaks the snapshot boundary legitimately creates.
            continue;
        }
        if r.seq > floor {
            state.decisions += 1;
            // Fold membership deltas in append order — the same order
            // `apply_member_delta` ran in when the records landed, so
            // the derived roster reproduces what the ledger applied.
            if let Some((kind, id, addr)) = r.member_delta() {
                state.memberships += 1;
                match kind {
                    KIND_MEMBER_ADD => {
                        if !state
                            .banned
                            .iter()
                            .any(|(bid, baddr)| bid == id || baddr == addr)
                        {
                            // Dedup on either field — a member re-added
                            // with a new id at the same address replaces
                            // the stale claim.
                            state.roster.retain(|nid, naddr| nid != id && naddr != addr);
                            state.roster.insert(id.to_string(), addr.to_string());
                        }
                    }
                    KIND_MEMBER_REMOVE => {
                        state.roster.retain(|nid, naddr| nid != id && naddr != addr);
                        state.banned.insert((id.to_string(), addr.to_string()));
                    }
                    KIND_MEMBER_UNBAN => {
                        state
                            .banned
                            .retain(|(bid, baddr)| bid != id && baddr != addr);
                    }
                    _ => {}
                }
            }
            if r.term > state.term {
                state.term = r.term;
                state.leader = r.leader.clone();
            }
        }
        // A record AT the floor (the retained anchor) doesn't fold —
        // its effects are in the snapshot — but stays in `by_coord` as
        // the boundary predecessor the first post-snapshot record's
        // prev_epoch is checked against.
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
            // Chain audit: a linked record must name its actual
            // predecessor's epoch — a mismatch is fork evidence that
            // survived append (gap filled with a divergent record).
            if !w[1].prev_epoch.is_empty() && w[1].prev_epoch != w[0].epoch {
                state.anomalies.push(format!(
                    "chain-break: {coord} seq {} links to {} but predecessor is {}",
                    w[1].seq,
                    &w[1].prev_epoch[..12.min(w[1].prev_epoch.len())],
                    &w[0].epoch[..12.min(w[0].epoch.len())]
                ));
            }
        }
        if let Some(high) = recs.iter().map(|r| r.seq).max() {
            let held: std::collections::BTreeSet<u64> = recs.iter().map(|r| r.seq).collect();
            // Gaps below the snapshot floor are archived, not missing.
            let floor = floors.get(&coord).copied().unwrap_or(0);
            let missing: Vec<u64> = (floor + 1..high).filter(|s| !held.contains(s)).collect();
            if !missing.is_empty() {
                state
                    .anomalies
                    .push(format!("sequence gap: {coord} missing seq {missing:?}"));
            }
            let prev_high = state.coordinators.get(&coord).copied().unwrap_or(0);
            state.coordinators.insert(coord, high.max(prev_high));
        }
    }
    state
}
/// Replay the local ledger into the cluster's consensus view. When a
/// compaction snapshot exists, its derived state seeds the replay and
/// the per-coordinator high-water marks bound gap detection — the
/// compacted and uncompacted views of the same history are identical.
pub fn replay() -> ClusterState {
    let records = load();
    let mut state = match load_snapshot() {
        Some(snap) => fold_records(
            ClusterState {
                term: snap.term,
                leader: snap.leader.clone(),
                coordinators: snap.high_water.clone(),
                decisions: snap.decisions,
                memberships: snap.memberships,
                roster: snap.roster.clone(),
                banned: snap.banned.clone(),
                anomalies: Vec::new(),
            },
            &records,
            &snap.high_water,
        ),
        None => replay_records(&records),
    };
    // PKI gates re-checked against today's bindings: a record that
    // would be refused at intake now — bound coordinator without a
    // valid member_sig, privileged record below bound-majority,
    // member_add with an unattested pubkey — is an anomaly wherever it
    // came from (pre-gate appends, a stolen cluster.key, or a bug),
    // and the reconstructed view is where operators look for it.
    // Signature-failing records are already flagged by the fold.
    let dir = SusiDirs::config_dir();
    for r in &records {
        if !r.verify() {
            continue;
        }
        if !attribution_valid_at(r, &dir) {
            state.anomalies.push(format!(
                "attribution failure: {} seq {} — key-bound coordinator with missing/invalid member_sig",
                r.coordinator, r.seq
            ));
        }
        if !endorsements_satisfied_at(r, &dir) {
            state.anomalies.push(format!(
                "endorsement shortfall: {} seq {} ({}) below bound-majority",
                r.coordinator, r.seq, r.kind
            ));
        }
        if !subject_attestation_valid(r) {
            state.anomalies.push(format!(
                "subject attestation missing: {} seq {} — member_add pubkey lacks the subject's binding signature",
                r.coordinator, r.seq
            ));
        }
    }
    state
}
/// Where the ledger lives: `~/.susi/commit_log.jsonl` — one JSON record
/// per line, append-only, shared with peers under the same cluster key.
pub fn ledger_path() -> PathBuf {
    SusiDirs::config_dir().join("commit_log.jsonl")
}
/// A log-compaction snapshot (Raft's InstallSnapshot analog): the
/// derived cluster state at the compaction point plus each
/// coordinator's last-included seq (`high_water`). After `compact()`
/// runs, the live ledger keeps only per-coordinator anchor records
/// (the frontier record at `high_water`) and post-snapshot arrivals —
/// everything older moves to `commit_log.archive.jsonl`.
///
/// Compaction is LOCAL storage management, not a consensus event: any
/// node may fold its own copy — the snapshot is a pure function of the
/// same records every member holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerSnapshot {
    /// Snapshot format version.
    pub version: u32,
    /// Unix seconds when the snapshot was taken.
    pub created_at: u64,
    /// Per-coordinator last-included seq — records at or below this are
    /// already represented in `state` and must not re-fold.
    pub high_water: std::collections::BTreeMap<String, u64>,
    /// Highest term observed through the snapshot point.
    pub term: u64,
    /// Leader stamped on the highest-term records.
    pub leader: String,
    /// Derived committed roster at the snapshot point.
    pub roster: std::collections::BTreeMap<String, String>,
    /// Derived banned members at the snapshot point.
    pub banned: std::collections::BTreeSet<(String, String)>,
    /// Verified-record count through the snapshot point.
    pub decisions: usize,
    /// Membership-delta count through the snapshot point.
    pub memberships: usize,
}
/// Where the snapshot lives.
pub fn snapshot_path() -> PathBuf {
    SusiDirs::config_dir().join("commit_snapshot.json")
}
/// Where pre-snapshot records are archived — still served by
/// `commit_log_fetch` so peers can repair gaps that span the boundary.
pub fn archive_path() -> PathBuf {
    SusiDirs::config_dir().join("commit_log.archive.jsonl")
}
/// Current snapshot; `None` when the file is absent or unreadable —
/// uncompacted nodes simply have no floor.
pub fn load_snapshot() -> Option<LedgerSnapshot> {
    load_snapshot_from(&snapshot_path())
}
/// Test seam: load a snapshot from an explicit path.
pub fn load_snapshot_from(path: &PathBuf) -> Option<LedgerSnapshot> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
/// Fold the live ledger into a snapshot and truncate. Keeps each
/// coordinator's frontier record as the chain/seq anchor so sealing,
/// `next_seq_for`, and anti-entropy `from_seq` need no snapshot
/// awareness. Durability order: archive first, then snapshot, then the
/// truncated live file — a crash mid-compact leaves a consistent
/// (possibly redundant) state rather than a ledger with no roster.
///
/// Returns the number of records moved to the archive.
pub fn compact() -> EaiResult<usize> {
    compact_at(&ledger_path(), &snapshot_path(), &archive_path())
}
/// Test seam: compact explicit paths.
pub fn compact_at(ledger: &Path, snapshot: &Path, archive: &Path) -> EaiResult<usize> {
    socket::compact(ledger, snapshot, archive)
}
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
#[allow(
    clippy::ptr_arg,
    reason = "retain the existing public function signature"
)]
pub fn append_to(path: &PathBuf, record: &CommitRecord) -> EaiResult<()> {
    socket::append(path, record)
}
/// Per-record outcome of a batch intake — `Skipped` is an idempotent
/// re-delivery (already held, identical), `Refused` a gate failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Applied,
    Skipped,
    Refused,
}
/// Batch intake for anti-entropy/gap repair: one lock + one ledger
/// load, then every record runs the full check sequence against the
/// growing `held` view — without it a 1000-record repair costs 1000
/// full-ledger parses and 1000 lock round-trips. A refused record
/// doesn't abort the batch (a forked record among good ones must not
/// starve the rest); outcomes align 1:1 with `records`.
pub fn append_many(records: &[CommitRecord]) -> Vec<AppendOutcome> {
    append_many_to(&ledger_path(), records)
}
/// Test seam + path-explicit form of `append_many`.
#[allow(
    clippy::ptr_arg,
    reason = "retain the existing public function signature"
)]
pub fn append_many_to(path: &PathBuf, records: &[CommitRecord]) -> Vec<AppendOutcome> {
    socket::append_many(path, records)
}
/// Whether this record changes cluster-wide security state — roster
/// deltas and key rotations alike. Decision records (empty kind) never
/// do.
fn privileged_kind(record: &CommitRecord) -> bool {
    record.member_delta().is_some() || record.rekey_fingerprint().is_some()
}
/// A privileged record (member delta or cluster rekey) only carries
/// authority when (a) its sealing coordinator is a current explicit
/// member of the roster — or this node itself (operator-initiated
/// local commits) — AND (b) the coordinator sealed under its own
/// leadership claim (`record.leader == record.coordinator`).
///
/// (a) alone is not enough: an evicted node still holds cluster.key,
/// so a valid HMAC cannot authorize roster or key-epoch changes — and
/// (b) is Raft's leader-proposed configuration-entry rule: membership
/// and epoch changes serialize through the elected leader, so two
/// members cannot race divergent deltas on the same subject. A stale
/// leader's self-claim still passes this check locally, but the term
/// gate (`check_term`) refuses its records wherever a newer term is
/// known — the same bound Raft gives a partitioned leader.
///
/// Intake paths (the `commit_record` tool, anti-entropy pulls,
/// `commits sync`) must check this BEFORE append — a refused record
/// stays missing and is retried once the coordinator is known, rather
/// than entering the ledger applied.
pub fn member_coordinator_known(record: &CommitRecord) -> bool {
    member_coordinator_known_at(record, &SusiDirs::config_dir())
}
/// Test seam: coordinator-authority check against an explicit roster dir.
pub fn member_coordinator_known_at(record: &CommitRecord, dir: &Path) -> bool {
    if !privileged_kind(record) {
        return true;
    }
    // Leader-proposed config changes only: the sealer must have
    // believed itself leader at seal time. Non-leader members route
    // `member_add` through the leader's `member_propose` tool instead.
    record.leader == record.coordinator && coordinator_known_at(record, dir)
}
/// Whether the record's coordinator holds commit authority on this
/// roster — a current explicit member or this node itself. Commit
/// authority = membership: records from non-members may still append as
/// history, but they must not bump `term.json` (an evicted node's
/// forged high term would otherwise freeze consensus) nor alter the
/// roster.
pub fn coordinator_known(record: &CommitRecord) -> bool {
    coordinator_known_at(record, &SusiDirs::config_dir())
}
/// Test seam: coordinator-authority check against an explicit roster dir.
pub fn coordinator_known_at(record: &CommitRecord, dir: &Path) -> bool {
    if record.coordinator == crate::susi_config::cluster_key::wire_node_id() {
        return true;
    }
    // Mtime-cached roster rows — intake runs this on every record.
    let peers = crate::susi_config::cluster_key::config_json_rows_at(dir, "peers.json");
    peers.iter().any(|p| {
        p.get("node_id").and_then(|v| v.as_str()) == Some(record.coordinator.as_str())
            && p.get("admission").and_then(|v| v.as_str()) == Some("explicit")
    })
}
/// The coordinator's bound signing key `(pubkey_hex, bound_at)` — the
/// roster half of member-signed consensus. `bound_at` is the
/// `committed_at` of the record (or handshake time) that established
/// the binding: only records sealed after it must carry `member_sig`,
/// so a member's pre-binding history always converges. This node is
/// always self-bound (bound_at = 0: every record we seal post-keygen
/// signs, so self-attributed forgeries die on arrival). `None` when
/// no key is bound — the shared-key ceiling that predates PKI.
/// The binding triple `(pubkey, bound_at, bound_seq)` — `bound_seq` is
/// `Some` for bindings written after the seq-floor hardening, `None`
/// for older roster rows that only recorded the timestamp.
fn bound_pubkey_at(coordinator: &str, dir: &Path) -> Option<(String, u64, Option<u64>)> {
    if coordinator == crate::susi_config::cluster_key::wire_node_id() {
        return crate::susi_config::cluster_key::node_pubkey_hex().map(|pk| (pk, 0, Some(0)));
    }
    let peers = crate::susi_config::cluster_key::config_json_rows_at(dir, "peers.json");
    peers.iter().find_map(|p| {
        if p.get("node_id").and_then(|v| v.as_str()) != Some(coordinator) {
            return None;
        }
        let pk = p.get("pubkey").and_then(|v| v.as_str())?.to_string();
        if pk.is_empty() {
            return None;
        }
        let at = p.get("key_bound_at").and_then(|v| v.as_u64()).unwrap_or(0);
        let seq = p.get("key_bound_seq").and_then(|v| v.as_u64());
        Some((pk, at, seq))
    })
}
/// Whether this record's coordinator attribution is genuine. When the
/// roster binds a pubkey for the coordinator, records sealed at or
/// after the binding must carry a valid `member_sig` — a stolen
/// cluster.key can still mint HMACs, but it can no longer sign as
/// that member. Unbound coordinators and pre-binding history pass on
/// the cluster-key proof alone (the ceiling this replaces — the
/// residual is documented in ARCHITECTURE.md).
pub fn attribution_valid(record: &CommitRecord) -> bool {
    attribution_valid_at(record, &SusiDirs::config_dir())
}
/// Test seam: attribution check against an explicit roster dir.
pub fn attribution_valid_at(record: &CommitRecord, dir: &Path) -> bool {
    let Some((pk, bound_at, bound_seq)) = bound_pubkey_at(&record.coordinator, dir) else {
        return true;
    };
    // With a recorded seq floor, only records that look exactly like
    // genuine pre-binding history — seq inside the bound frontier AND
    // a pre-binding timestamp — skip the signature. A forged record
    // must clear both: seq above the floor fails here, a post-binding
    // timestamp fails the other check — `committed_at` alone can no
    // longer launder a new record into the exempt window.
    let exempt = match bound_seq {
        Some(floor) => record.seq <= floor && record.committed_at < bound_at,
        None => record.committed_at < bound_at,
    };
    if exempt {
        return true;
    }
    !record.member_sig.is_empty()
        && crate::susi_config::cluster_key::member_verify(
            &pk,
            &record.signature,
            &record.member_sig,
        )
}
/// The payload a member signs to endorse a privileged record — bound to
/// the record's HMAC signature so an endorsement can never be transplanted
/// onto a different record.
pub fn endorsement_payload(signature: &str) -> String {
    format!("susi-endorse-v1:{signature}")
}
/// Whether a `member_add` carrying `member_pubkey` also carries the
/// subject's own attestation for it (`subject_sig` over
/// `susi-bind-v1:{id}:{pubkey}`, sourced from the v3 handshake). A
/// binding the subject never signed is refused — a malicious or
/// careless proposer can no longer bind a wrong key onto a member,
/// which was a record-starvation DoS on the victim. `member_add`s
/// without a pubkey (unbound legacy members) stay admissible.
/// Whether a `member_add` carrying `member_pubkey` also carries the
/// subject's own binding attestation — the same check intake runs,
/// exposed so `commits audit` can flag records that wouldn't be
/// admitted today. Non-member_add and keyless records always pass.
pub fn subject_attestation_valid(record: &CommitRecord) -> bool {
    let Some((KIND_MEMBER_ADD, id, _)) = record.member_delta() else {
        return true;
    };
    if record.member_pubkey.is_empty() {
        return true;
    }
    crate::susi_config::cluster_key::verify_bind_attestation(
        id,
        &record.member_pubkey,
        &record.subject_sig,
    )
}
/// Whether `kind` is a privileged roster/config delta — the record kinds
/// the endorsement gate applies to. Quorum decisions (empty kind) are
/// already electorate-tallied and stay ungated.
pub fn privileged_kind_name(kind: &str) -> bool {
    matches!(
        kind,
        KIND_MEMBER_ADD
            | KIND_MEMBER_REMOVE
            | KIND_MEMBER_UNBAN
            | KIND_CLUSTER_REKEY
            | KIND_CLUSTER_REKEY_ACTIVATE
    )
}
/// Whether a privileged record carries the endorsements its electorate
/// owes it — the receiver-side joint-consensus gate. The required count
/// is the majority of electorate members whose pubkey was bound *at the
/// record's `committed_at`*; when nothing was bound yet (the pre-PKI
/// window) the requirement is zero and the record passes on its HMAC +
/// attribution proof alone. Verified supporters are distinct: the
/// coordinator counts through its own `member_sig`, and every other
/// endorsement must verify under the key bound for its `node` at record
/// time — endorsements from unbound, later-bound, or non-electorate
/// signers cannot be counted (they're ignored, not penalized: a record
/// can carry extra signatures harmlessly).
pub fn endorsements_satisfied(record: &CommitRecord) -> bool {
    endorsements_satisfied_at(record, &SusiDirs::config_dir())
}
/// Test seam: endorsement check against an explicit roster dir.
pub fn endorsements_satisfied_at(record: &CommitRecord, dir: &Path) -> bool {
    if !privileged_kind_name(&record.kind) {
        return true;
    }
    let bound: Vec<(String, String)> = record
        .electorate
        .iter()
        .filter_map(|m| {
            let (pk, at, _) = bound_pubkey_at(m, dir)?;
            (at <= record.committed_at).then(|| (m.clone(), pk))
        })
        .collect();
    if bound.is_empty() {
        return true;
    }
    let required = bound.len() / 2 + 1;
    let mut supporters: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    // The coordinator's member_sig is its own endorsement — it signed
    // `signature`, the very payload an endorsement covers.
    if let Some((_, pk)) = bound.iter().find(|(m, _)| m == &record.coordinator) {
        if crate::susi_config::cluster_key::member_verify(pk, &record.signature, &record.member_sig)
        {
            supporters.insert(record.coordinator.as_str());
        }
    }
    let payload = endorsement_payload(&record.signature);
    for e in &record.endorsements {
        let Some((_, pk)) = bound.iter().find(|(m, _)| m == &e.node) else {
            continue;
        };
        if crate::susi_config::cluster_key::member_verify(pk, &payload, &e.sig) {
            supporters.insert(e.node.as_str());
        }
    }
    supporters.len() >= required
}
/// The intake half of the joint-consensus gate: the bound electorate is
/// read off the *committed ledger's applied roster* (this node's
/// `peers.json` at append time), never off the record's proposer-
/// attested `committed_at`. VC-200-001's residual closed here: a leader
/// can no longer backdate `committed_at` into the pre-binding window —
/// where `endorsements_satisfied_at` sees zero bound members and the
/// requirement collapses — and slip a roster/key binding through on its
/// own signature. Once any electorate member is bound in committed
/// state, a binding (or any privileged delta) is accepted only when a
/// majority of the *currently bound* electorate endorsed it, so
/// overlapping config changes serialize through the same quorum.
///
/// `endorsements_satisfied_at` stays the audit/history view: for a
/// record already committed, "was it valid then" is correctly measured
/// against its own time — an old record isn't an anomaly because later
/// bindings raised today's requirement.
pub fn endorsements_satisfied_at_tip(record: &CommitRecord, dir: &Path) -> bool {
    if !privileged_kind_name(&record.kind) {
        return true;
    }
    // Bound in committed state: the roster row carries a pubkey because
    // a committed member_add applied it (or the handshake bound it) —
    // either way the receiver holds proof on record, and `committed_at`
    // is never consulted.
    let bound: Vec<(String, String)> = record
        .electorate
        .iter()
        .filter_map(|m| {
            let (pk, _, _) = bound_pubkey_at(m, dir)?;
            Some((m.clone(), pk))
        })
        .collect();
    if bound.is_empty() {
        return true;
    }
    let required = bound.len() / 2 + 1;
    let mut supporters: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    if let Some((_, pk)) = bound.iter().find(|(m, _)| m == &record.coordinator) {
        if crate::susi_config::cluster_key::member_verify(pk, &record.signature, &record.member_sig)
        {
            supporters.insert(record.coordinator.as_str());
        }
    }
    let payload = endorsement_payload(&record.signature);
    for e in &record.endorsements {
        let Some((_, pk)) = bound.iter().find(|(m, _)| m == &e.node) else {
            continue;
        };
        if crate::susi_config::cluster_key::member_verify(pk, &payload, &e.sig) {
            supporters.insert(e.node.as_str());
        }
    }
    supporters.len() >= required
}
/// Ask each `(node_id, gmcp_addr)` member to endorse `record` via its
/// `member_endorse` tool — the leader-side half of the joint-consensus
/// gate. Runs the calls on scoped threads so one dead member stalls a
/// single worker, not the tally; members that don't answer simply
/// produce no endorsement. The coordinator's own signature already
/// counts as its vote, so callers pass the *other* electorate members.
pub fn collect_endorsements(
    record: &CommitRecord,
    targets: &[(String, String)],
) -> Vec<MemberEndorsement> {
    let bearer = crate::susi_config::cluster_key::peer_bearer();
    let args = serde_json::json!({"record": record});
    let collected = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for (_, addr) in targets {
            let args = args.clone();
            let bearer = bearer.clone();
            let collected = &collected;
            s.spawn(move || {
                let Ok(result) = crate::susi_core::mcp_client::call_tool(
                    addr,
                    "member_endorse",
                    &args,
                    bearer.as_deref(),
                ) else {
                    return;
                };
                let Some(text) = result.pointer("/content/0/text").and_then(|v| v.as_str()) else {
                    return;
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
                    return;
                };
                let (Some(node), Some(sig)) = (
                    v.get("node").and_then(|x| x.as_str()),
                    v.get("sig").and_then(|x| x.as_str()),
                ) else {
                    return;
                };
                collected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(MemberEndorsement {
                        node: node.to_string(),
                        sig: sig.to_string(),
                    });
            });
        }
    });
    collected.into_inner().unwrap_or_else(|e| e.into_inner())
}
/// Load every well-formed record, oldest first. Malformed lines are
/// skipped rather than fatal — a torn final line from a mid-append crash
/// must not hide the valid history before it.
pub fn load() -> Vec<CommitRecord> {
    load_from(&ledger_path())
}
/// Test seam: load from an explicit path. Mtime-cached — status and
/// audit surfaces re-parse the ledger per call in the daemon, while
/// intake appends invalidate by stamp.
pub fn load_from(path: &std::path::Path) -> Vec<CommitRecord> {
    cached_records(path)
}

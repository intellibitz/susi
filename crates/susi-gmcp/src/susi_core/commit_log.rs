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
            && hex::decode(member_pubkey)
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
        if hex::decode(fingerprint).ok()?.len() != 32 {
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
        if hex::decode(fingerprint).ok()?.len() != 32 {
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
        let committed_at = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut sorted = electorate;
        sorted.sort();
        let epoch = hex::encode(Sha256::digest(
            format!("{}|{}|{}", kind, sorted.join(","), committed_at).as_bytes(),
        ));
        let held = load();
        let mut rec = CommitRecord {
            epoch,
            coordinator: coordinator.to_string(),
            electorate: sorted,
            tally: 0,
            quorum_threshold: 0,
            value_hash: hex::encode(Sha256::digest(value.as_bytes())),
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
        if hex::decode(&self.value).ok()?.len() != 32 {
            return None;
        }
        Some(self.value.as_str())
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
        hex::encode(Sha256::digest(self.value.as_bytes())) == self.value_hash
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
/// agreement. `pub(crate)` so vendored copies share the lock with their
/// consumer crate's own tests.
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

/// Snapshot floor relative to an explicit ledger path — the
/// `append_to`/`commit_log_fetch` seam that can't assume `~/.susi`.
fn snapshot_floor_at(ledger: &Path, coordinator: &str) -> u64 {
    ledger
        .parent()
        .and_then(|dir| load_snapshot_from(&dir.join("commit_snapshot.json")))
        .and_then(|s| s.high_water.get(coordinator).copied())
        .unwrap_or(0)
}

/// Find a record by `(coordinator, seq)` in the compaction archive —
/// the dedup source for below-floor arrivals.
fn find_in_archive(archive: &Path, coordinator: &str, seq: u64) -> Option<CommitRecord> {
    let text = fs::read_to_string(archive).ok()?;
    text.lines()
        .filter_map(|line| serde_json::from_str::<CommitRecord>(line).ok())
        .find(|r| r.coordinator == coordinator && r.seq == seq)
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

/// Test seam: load term state from an explicit path. Mtime-cached —
/// the intake path consults term state on every record, and an
/// anti-entropy pull of hundreds of records should not re-parse the
/// file each time; `save_term_to`'s atomic rename changes the stamp.
pub fn load_term_from(path: &std::path::Path) -> TermState {
    let default = || TermState {
        term: 0,
        leader: String::new(),
        updated_at: 0,
    };
    let Some(raw) = crate::susi_config::cluster_key::cached_file_bytes(path) else {
        return default();
    };
    let Ok(text) = String::from_utf8(raw) else {
        return default();
    };
    serde_json::from_str(&text).unwrap_or_else(|_| default())
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
/// `pub` so sibling kernel modules (service_table) share the primitive —
/// every cross-process read-modify-write on `~/.susi` state needs it.
#[doc(hidden)]
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// ~3s of 10ms retries — the guarded sections are millisecond-scale,
    /// so a longer wait means a wedged holder, not contention.
    pub fn acquire(dir: &Path, name: &str) -> Option<Self> {
        // A bare filename's parent is the empty path — normalize it to
        // the current directory so lock placement is well-defined.
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
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
pub fn claim_leadership_at(path: &PathBuf, leader: &str) -> u64 {
    let _g = TERM_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Serialize the read-modify-write across processes. On lock failure we
    // decline the claim entirely and return the persisted term — the
    // caller then seals under current leadership, which receivers flag as
    // a conflict, rather than minting a phantom term that was never
    // durably claimed.
    let Some(_file_lock) = path.parent().and_then(|dir| FileLock::acquire(dir, "term")) else {
        return load_term_from(path).term;
    };
    let mut state = load_term_from(path);
    if state.leader != leader {
        state.term += 1;
        state.leader = leader.to_string();
        state.updated_at = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
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
    let Some(snap) = load_snapshot() else {
        return replay_records(&records);
    };
    let seed = ClusterState {
        term: snap.term,
        leader: snap.leader.clone(),
        coordinators: snap.high_water.clone(),
        decisions: snap.decisions,
        memberships: snap.memberships,
        roster: snap.roster.clone(),
        banned: snap.banned.clone(),
        anomalies: Vec::new(),
    };
    fold_records(seed, &records, &snap.high_water)
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
pub fn compact_at(ledger: &PathBuf, snapshot: &PathBuf, archive: &PathBuf) -> EaiResult<usize> {
    let _g = APPEND_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(_file_lock) = ledger
        .parent()
        .and_then(|dir| FileLock::acquire(dir, "commit_log"))
    else {
        return Err(EaiError::filesystem(
            "commit ledger lock unavailable — possible wedged holder or extreme contention",
        ));
    };
    let records = load_from(ledger);
    if records.is_empty() {
        return Ok(0);
    }
    let state = replay_records(&records);
    // Anchor = each coordinator's highest-seq record — the frontier
    // every post-snapshot seal/link/anti-entropy computation needs.
    let mut anchors: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut by_coord: std::collections::BTreeMap<&str, Vec<(usize, &CommitRecord)>> =
        std::collections::BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        by_coord
            .entry(r.coordinator.as_str())
            .or_default()
            .push((i, r));
    }
    for recs in by_coord.values() {
        if let Some((i, _)) = recs.iter().max_by_key(|(_, r)| r.seq) {
            anchors.insert(*i);
        }
    }
    let archived: Vec<&CommitRecord> = records
        .iter()
        .enumerate()
        .filter(|(i, _)| !anchors.contains(i))
        .map(|(_, r)| r)
        .collect();
    if archived.is_empty() {
        return Ok(0);
    }
    if let Some(dir) = archive.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| EaiError::filesystem(format!("create {}: {e}", dir.display())))?;
    }
    let mut out = String::new();
    for r in &archived {
        let line = serde_json::to_string(r)
            .map_err(|e| EaiError::internal(format!("serialize archived record: {e}")))?;
        out.push_str(&line);
        out.push('\n');
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(archive)
        .map_err(|e| EaiError::filesystem(format!("open {}: {e}", archive.display())))?;
    file.write_all(out.as_bytes())
        .map_err(|e| EaiError::filesystem(format!("write {}: {e}", archive.display())))?;
    let snap = LedgerSnapshot {
        version: 1,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        high_water: state.coordinators.clone(),
        term: state.term,
        leader: state.leader.clone(),
        roster: state.roster.clone(),
        banned: state.banned.clone(),
        decisions: state.decisions,
        memberships: state.memberships,
    };
    let snap_text = serde_json::to_string_pretty(&snap)
        .map_err(|e| EaiError::internal(format!("serialize ledger snapshot: {e}")))?;
    let snap_tmp = snapshot.with_extension("tmp");
    fs::write(&snap_tmp, snap_text)
        .map_err(|e| EaiError::filesystem(format!("write {}: {e}", snap_tmp.display())))?;
    fs::rename(&snap_tmp, snapshot)
        .map_err(|e| EaiError::filesystem(format!("rename {}: {e}", snap_tmp.display())))?;
    let live: String = records
        .iter()
        .enumerate()
        .filter(|(i, _)| anchors.contains(i))
        .filter_map(|(_, r)| serde_json::to_string(r).ok())
        .map(|l| format!("{l}\n"))
        .collect();
    let live_tmp = ledger.with_extension("tmp");
    fs::write(&live_tmp, live)
        .map_err(|e| EaiError::filesystem(format!("write {}: {e}", live_tmp.display())))?;
    fs::rename(&live_tmp, ledger)
        .map_err(|e| EaiError::filesystem(format!("rename {}: {e}", live_tmp.display())))?;
    Ok(archived.len())
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
    // equivocation check and both append. Failing closed here is correct:
    // >3s of contention on a millisecond-scale section means a wedged
    // holder, and surfacing the error beats silently forking history.
    let Some(_file_lock) = path
        .parent()
        .and_then(|dir| FileLock::acquire(dir, "commit_log"))
    else {
        return Err(EaiError::filesystem(
            "commit ledger lock unavailable — possible wedged holder or extreme contention",
        ));
    };
    let mut held = load_from(path);
    append_checked(path, record, &mut held).map(|_| ())
}

/// Intake checks + file append against a caller-maintained `held` —
/// the same gates `append_to` runs, minus the lock and the load, so a
/// batch can pay them once. Returns `true` when the record was written
/// (the caller's `held` is extended so follow-on records in the same
/// batch check against it — a batch containing both a record and its
/// successor still chains).
fn append_checked(
    path: &PathBuf,
    record: &CommitRecord,
    held: &mut Vec<CommitRecord>,
) -> EaiResult<bool> {
    if record.seq > 0 {
        if let Some(existing) = held
            .iter()
            .find(|r| r.coordinator == record.coordinator && r.seq == record.seq)
        {
            if existing == record {
                return Ok(false);
            }
            return Err(EaiError::protocol(format!(
                "commit equivocation: {} seq {} already held with different content",
                record.coordinator, record.seq
            )));
        }
    } else if held.iter().any(|r| r == record) {
        return Ok(false);
    }
    // Below-floor dedup: a record under the compaction high-water is
    // covered by the snapshot. The archive decides — identical content
    // is a redundant re-delivery (no-op), divergent content at an
    // archived seq is equivocation against committed history.
    let floor = snapshot_floor_at(path, &record.coordinator);
    if record.seq > 0 && record.seq < floor {
        if let Some(dir) = path.parent() {
            match find_in_archive(
                &dir.join("commit_log.archive.jsonl"),
                &record.coordinator,
                record.seq,
            ) {
                Some(archived) if &archived == record => return Ok(false),
                Some(_) => {
                    return Err(EaiError::protocol(format!(
                        "commit equivocation: {} seq {} diverges from archived record",
                        record.coordinator, record.seq
                    )));
                }
                // Not in the archive — pre-snapshot history this node
                // never received. Harmless to append: the fold skips
                // records under the floor.
                None => {}
            }
        }
    }
    // Epoch-aware intake: current-key records append normally. A record
    // signed only under the retired key (`cluster.key.prev`) is genuine
    // pre-rotation history — but a revoked member still holding that key
    // could forge it, so prior-epoch records may ONLY fill internal
    // chain-pinned gaps: a held successor at seq+1 must name this
    // record's epoch. The successor's signed prev_epoch pins exactly one
    // content hash — a forged fill cannot produce it — and any seq at
    // the frontier (no successor held) is post-rotation traffic signed
    // with a dead key: refused unconditionally.
    match record.verify_key_epoch() {
        Some(KeyEpoch::Current) => {}
        Some(KeyEpoch::Prev) => {
            let pinned = held.iter().any(|r| {
                r.coordinator == record.coordinator
                    && r.seq == record.seq + 1
                    && r.prev_epoch == record.epoch
            });
            if !pinned {
                return Err(EaiError::protocol(
                    "refusing prior-epoch record without a chain-pinned successor \
                     (revoked-key traffic cannot extend the ledger frontier)",
                ));
            }
        }
        None => {
            return Err(EaiError::protocol(
                "refusing to append a commit record that fails signature or consistency verification",
            ));
        }
    }
    // Non-repudiable attribution: once a member's signing key is bound
    // (handshake or committed member_add), records claiming its
    // coordination from the binding point forward must carry that key's
    // signature — membership proof alone no longer mints records as
    // them. Pre-binding history stays appendable or pre-PKI ledgers
    // could never converge through the gate.
    if let Some(dir) = path.parent() {
        if !attribution_valid_at(record, dir) {
            return Err(EaiError::protocol(format!(
                "refusing record: {} is key-bound but member_sig is missing or invalid",
                record.coordinator
            )));
        }
        // Joint-consensus gate: a privileged roster/config delta sealed
        // after a quorum of its electorate had bound keys must carry
        // that bound majority's endorsements — one leader's signature
        // alone no longer rewrites membership (see MemberEndorsement).
        if !endorsements_satisfied_at(record, dir) {
            return Err(EaiError::protocol(format!(
                "refusing {} record: electorate endorsements below bound quorum",
                record.kind
            )));
        }
    }
    // Subject-attested binding: a member_add claiming a pubkey must
    // carry the subject's own signature for it — bindings a subject
    // never claimed can't be committed (proposer-attested DoS closed).
    if !subject_attestation_valid(record) {
        return Err(EaiError::protocol(
            "refusing member_add: member_pubkey lacks a valid subject attestation",
        ));
    }
    // Chain check (Raft's prevLogIndex/prevLogTerm consistency): when the
    // record declares a predecessor link and we hold that predecessor
    // slot, the epochs must agree — a mismatch is proof the
    // coordinator's log diverged from ours (a fork), which is refused,
    // not silently interleaved. When the predecessor slot is empty the
    // link can't be evaluated yet — the record appends and gap repair
    // fills history; replay() flags any residual chain-break.
    if !record.prev_epoch.is_empty() && record.seq > 1 {
        if let Some(pred) = held
            .iter()
            .find(|r| r.coordinator == record.coordinator && r.seq == record.seq - 1)
        {
            if pred.epoch != record.prev_epoch {
                return Err(EaiError::protocol(format!(
                    "chain divergence: {} seq {} links to epoch {} but held seq {} is {}",
                    record.coordinator,
                    record.seq,
                    &record.prev_epoch[..12.min(record.prev_epoch.len())],
                    record.seq - 1,
                    &pred.epoch[..12.min(pred.epoch.len())]
                )));
            }
        }
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
        .map_err(|e| EaiError::filesystem(format!("append {}: {e}", path.display())))?;
    // Committed membership is applied state — the ledger IS the roster
    // history, so a member record landing here updates peers.json as
    // part of the same append. Best-effort: a failed apply leaves the
    // durable record (replay and gossip re-converge) rather than
    // rejecting a validly signed append.
    if record.member_delta().is_some() {
        apply_member_delta(record, path.parent().unwrap_or_else(|| Path::new(".")));
    }
    // Key rotation is two-phase: the `cluster_rekey` record only
    // commits WHICH key was agreed; `cluster_rekey_activate` is the
    // epoch boundary — applying it activates the staged next-epoch key.
    // The fingerprint match inside activation pins the staged file to
    // THIS record, so an activate can never swap in an arbitrary file.
    if record.kind == KIND_CLUSTER_REKEY_ACTIVATE {
        if let Some(fingerprint) = record.rekey_fingerprint() {
            let _ = crate::susi_config::cluster_key::activate_staged_key_at(
                fingerprint,
                path.parent().unwrap_or_else(|| Path::new(".")),
            );
        }
    }
    held.push(record.clone());
    Ok(true)
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
pub fn append_many_to(path: &PathBuf, records: &[CommitRecord]) -> Vec<AppendOutcome> {
    let _g = APPEND_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(_file_lock) = path
        .parent()
        .and_then(|dir| FileLock::acquire(dir, "commit_log"))
    else {
        return vec![AppendOutcome::Refused; records.len()];
    };
    let mut held = load_from(path);
    records
        .iter()
        .map(|record| match append_checked(path, record, &mut held) {
            Ok(true) => AppendOutcome::Applied,
            Ok(false) => AppendOutcome::Skipped,
            Err(_) => AppendOutcome::Refused,
        })
        .collect()
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
    let peers: Vec<serde_json::Value> = fs::read_to_string(dir.join("peers.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
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
    let peers: Vec<serde_json::Value> = fs::read_to_string(dir.join("peers.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
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
fn subject_attestation_valid(record: &CommitRecord) -> bool {
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

/// Bind the member's signing key into a roster row when the committed
/// add carries one — first write wins. A bound key is only ever
/// replaced by remove + re-add (leader-gated): letting a later record
/// or handshake overwrite it would let an attacker re-bind a victim's
/// identity to a key they hold and resume forging.
fn bind_member_key(row: &mut serde_json::Value, record: &CommitRecord, dir: &Path, id: &str) {
    if record.member_pubkey.is_empty() {
        return;
    }
    let unbound = row
        .get("pubkey")
        .and_then(|v| v.as_str())
        .is_none_or(|p| p.is_empty());
    if !unbound {
        return;
    }
    row["pubkey"] = serde_json::json!(record.member_pubkey);
    row["key_bound_at"] = serde_json::json!(record.committed_at);
    // The subject's attestation rides along — a later member_propose or
    // re-push can carry proof the binding was the subject's own claim.
    row["bind_sig"] = serde_json::json!(record.subject_sig);
    // Seq floor: the member's highest seq visible at bind time (live +
    // archive). Records beyond it are post-binding traffic that must
    // carry `member_sig` even with a forged `committed_at` — the
    // timestamp is attacker-controlled, the seq frontier is not.
    // Undercounting (e.g. compaction removed history) only tightens
    // the rule, never loosens it.
    let floor = [
        dir.join("commit_log.archive.jsonl"),
        dir.join("commit_log.jsonl"),
    ]
    .iter()
    .flat_map(load_from)
    .filter(|r| r.coordinator == id)
    .map(|r| r.seq)
    .max()
    .unwrap_or(0);
    row["key_bound_seq"] = serde_json::json!(floor);
}

/// Apply a committed membership delta to `peers.json` /
/// `peers_banned.json` beside the ledger — the roster half of "the
/// ledger is applied state". `dir` is the ledger's directory (not the
/// global config dir) so path-seamed test ledgers apply hermetically.
/// Structural JSON: this kernel module cannot depend on the swarm
/// plane's `ClusterPeerNode`, and the on-disk schema (snake_case field
/// names) is the stable wire format both sides already share.
fn apply_member_delta(record: &CommitRecord, dir: &Path) {
    let Some((kind, id, addr)) = record.member_delta() else {
        return;
    };
    // A committed delta naming our own node_id never touches the
    // roster — a node never lists, evicts, or bans itself (the same
    // self-edge rule the scout and `peers add` enforce). Membership
    // pushes fan out to every peer including the subject, and
    // anti-entropy eventually delivers every committed record, so
    // this case is routine, not adversarial. But a self-remove is not
    // a pure no-op: eviction means stand down — the marker file tells
    // the scout to go silent and dispatch to see an empty cluster
    // until a committed unban (or re-add) clears it. Only our node_id
    // stands us down — a loopback address in a crafted remove must not.
    if id == crate::susi_config::cluster_key::wire_node_id() {
        let marker = dir.join("cluster_evicted.json");
        match kind {
            KIND_MEMBER_REMOVE => {
                let _ = crate::susi_config::atomic_write_json_pretty(
                    &marker,
                    &serde_json::json!({
                        "evicted_at": record.committed_at,
                        "by": record.coordinator,
                    }),
                );
            }
            KIND_MEMBER_UNBAN | KIND_MEMBER_ADD => {
                let _ = fs::remove_file(&marker);
            }
            _ => {}
        }
        return;
    }
    // Loopback addresses are never remote members either — a pong from
    // 127.0.0.1 is always ourselves.
    let looped = addr
        .split(':')
        .next()
        .and_then(|h| h.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if looped {
        return;
    }
    // Serialize the roster read-modify-write across processes — same
    // lockfile discipline as the ledger append itself.
    let Some(_lock) = FileLock::acquire(dir, "peers") else {
        return;
    };
    let peers_path = dir.join("peers.json");
    let banned_path = dir.join("peers_banned.json");
    let mut peers: Vec<serde_json::Value> = fs::read_to_string(&peers_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut banned: Vec<serde_json::Value> = fs::read_to_string(&banned_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let member_matches = |n: &serde_json::Value| {
        n.get("node_id").and_then(|v| v.as_str()) == Some(id)
            || n.get("address").and_then(|v| v.as_str()) == Some(addr)
    };
    match kind {
        KIND_MEMBER_ADD => {
            // A ban is operator eviction — a committed add must never
            // silently resurrect an evicted member.
            if banned.iter().any(&member_matches) {
                return;
            }
            // Collapse every row matching the committed identity into
            // one — a synthetic `susi-peer-*` row or a stale pre-move
            // address is superseded by the attested (id, addr) pair.
            // Admission upgrades to explicit (the coordinator verified
            // this member) while liveness fields stay as probed:
            // membership grants standing, not liveness — the same
            // invariant the new-row path keeps below.
            let mut merged = false;
            peers.retain_mut(|n| {
                if !member_matches(n) {
                    return true;
                }
                if merged {
                    return false;
                }
                merged = true;
                n["node_id"] = serde_json::json!(id);
                n["address"] = serde_json::json!(addr);
                n["admission"] = serde_json::json!("explicit");
                bind_member_key(n, record, dir, id);
                true
            });
            if !merged {
                // Explicit admission — the coordinator cryptographically
                // verified this member — but `last_seen_secs: 0`:
                // committed membership grants roster standing, not
                // liveness. The member stays stale (no quorum weight,
                // no leadership) until it directly pongs this node.
                let mut row = serde_json::json!({
                    "node_id": id,
                    "address": addr,
                    "node_type": "PEER",
                    "is_active": true,
                    "capabilities": [],
                    "registry_checksum": 0,
                    "latency_ms": 0,
                    "uptime_secs": 0,
                    "trust_score": 0.8,
                    "capability_bloom": [0, 0, 0, 0],
                    "admission": "explicit",
                    "last_seen_secs": 0,
                });
                bind_member_key(&mut row, record, dir, id);
                peers.push(row);
            }
            let _ = crate::susi_config::atomic_write_json_pretty(&peers_path, &peers);
        }
        KIND_MEMBER_REMOVE => {
            peers.retain(|n| !member_matches(n));
            if !banned.iter().any(&member_matches) {
                banned.push(serde_json::json!({
                    "node_id": id,
                    "address": addr,
                    "banned_at": record.committed_at,
                }));
            }
            let _ = crate::susi_config::atomic_write_json_pretty(&peers_path, &peers);
            let _ = crate::susi_config::atomic_write_json_pretty(&banned_path, &banned);
        }
        KIND_MEMBER_UNBAN => {
            banned.retain(|b| !member_matches(b));
            let _ = crate::susi_config::atomic_write_json_pretty(&banned_path, &banned);
        }
        _ => {}
    }
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
            prev_epoch: String::new(),
            kind: String::new(),
            signature: String::new(),
            member_sig: String::new(),
            member_pubkey: String::new(),
            subject_sig: String::new(),
            endorsements: Vec::new(),
        };
        assert!(append_to(&path, &unsigned).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn member_records_seal_verify_and_apply_to_roster() {
        let _g = test_key_guard();
        let dir = std::env::temp_dir().join(format!("susi_member_{}", std::process::id()));
        let path = dir.join("commit_log.jsonl");
        let _ = fs::remove_dir_all(&dir);

        // seal_member assigns seq from the shared ledger path — the
        // path-seamed test ledger is empty, so re-seq + re-sign each
        // record into the test chain (same pattern as the gap tests).
        let mut seq = 0u64;
        let mut seal_into_test = |kind: &str, member: &str| -> Option<CommitRecord> {
            let mut rec =
                CommitRecord::seal_member("coord-a", "coord-a", kind, member, vec![], "")?;
            seq += 1;
            rec.seq = seq;
            if let Some(k) = crate::susi_config::cluster_key::cluster_key() {
                rec.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                    &k,
                    rec.signed_payload().as_bytes(),
                );
            }
            Some(rec)
        };

        let Some(add) = seal_into_test(KIND_MEMBER_ADD, "node-b@10.0.0.2:9090") else {
            eprintln!("skip: no cluster.key on this host");
            return;
        };
        assert!(add.verify(), "sealed member record must verify");
        assert_eq!(
            add.member_delta(),
            Some((KIND_MEMBER_ADD, "node-b", "10.0.0.2:9090"))
        );
        append_to(&path, &add).expect("append add");

        // Applied: peers.json beside the ledger gained the member as
        // explicit — but stale (last_seen 0): committed standing, not
        // liveness.
        let peers: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0]["node_id"], "node-b");
        assert_eq!(peers[0]["admission"], "explicit");
        assert_eq!(peers[0]["last_seen_secs"], 0);

        // Removal evicts + bans on append.
        let Some(rem) = seal_into_test(KIND_MEMBER_REMOVE, "node-b@10.0.0.2:9090") else {
            return;
        };
        append_to(&path, &rem).expect("append remove");
        let peers: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
        assert!(peers.is_empty(), "evicted member must leave the roster");
        let banned: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(dir.join("peers_banned.json")).unwrap())
                .unwrap();
        assert_eq!(banned.len(), 1);

        // A re-add while banned is refused — eviction wins over a stale
        // add arriving late.
        let Some(readd) = seal_into_test(KIND_MEMBER_ADD, "node-b@10.0.0.2:9090") else {
            return;
        };
        append_to(&path, &readd).expect("append re-add");
        let peers: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
        assert!(peers.is_empty(), "banned member must not be resurrected");

        // Unban lifts the ban so the member can re-verify naturally.
        let Some(unban) = seal_into_test(KIND_MEMBER_UNBAN, "node-b@10.0.0.2:9090") else {
            return;
        };
        append_to(&path, &unban).expect("append unban");
        let banned: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(dir.join("peers_banned.json")).unwrap())
                .unwrap();
        assert!(banned.is_empty(), "unban must clear the eviction");

        // Identity collapse: a committed add must supersede every row
        // matching its (id|addr) — a synthetic LAN-discovery id at the
        // same address, or the same node_id at a stale pre-move address.
        // The surviving row carries the attested pair + explicit
        // admission while keeping its probed liveness.
        fs::write(
            dir.join("peers.json"),
            serde_json::to_string(&serde_json::json!([
                { "node_id": "susi-peer-10.0.0.5", "address": "10.0.0.5:9090",
                  "admission": "discovered", "last_seen_secs": 999 },
                { "node_id": "node-d", "address": "10.0.0.44:9090",
                  "admission": "explicit", "last_seen_secs": 12345 },
            ]))
            .unwrap(),
        )
        .unwrap();
        let Some(add_d) = seal_into_test(KIND_MEMBER_ADD, "node-d@10.0.0.5:9090") else {
            return;
        };
        append_to(&path, &add_d).expect("append collapse add");
        let peers: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
        assert_eq!(peers.len(), 1, "add must collapse matching rows into one");
        assert_eq!(peers[0]["node_id"], "node-d");
        assert_eq!(peers[0]["address"], "10.0.0.5:9090");
        assert_eq!(peers[0]["admission"], "explicit");
        assert_eq!(
            peers[0]["last_seen_secs"], 999,
            "committed membership grants standing, not liveness"
        );

        // Self-edge guard: a committed add naming this node, or any
        // loopback address, never lands in peers.json — `peers add`
        // pushes the member record to the member itself, so this is a
        // routine delivery, not an adversarial edge.
        let self_id = crate::susi_config::cluster_key::wire_node_id();
        let Some(self_add) = seal_into_test(KIND_MEMBER_ADD, &format!("{self_id}@10.0.0.9:9090"))
        else {
            return;
        };
        append_to(&path, &self_add).expect("append self-add");
        let Some(loop_add) = seal_into_test(KIND_MEMBER_ADD, "node-c@127.0.0.1:9090") else {
            return;
        };
        append_to(&path, &loop_add).expect("append loop-add");
        let peers: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
        assert!(
            !peers.iter().any(|p| {
                p["node_id"].as_str() == Some(self_id.as_str())
                    || p["address"].as_str().is_some_and(|a| a.starts_with("127."))
            }),
            "self/loopback member_add must not create a self-edge"
        );

        // Self-subject remove: fan-out delivers a node its own eviction
        // record — it must not write a ban entry against itself, but it
        // does stand down: the eviction marker the scout checks lands
        // beside the ledger, and a committed unban clears it.
        let Some(self_rem) =
            seal_into_test(KIND_MEMBER_REMOVE, &format!("{self_id}@10.0.0.9:9090"))
        else {
            return;
        };
        append_to(&path, &self_rem).expect("append self-remove");
        let banned: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(dir.join("peers_banned.json")).unwrap())
                .unwrap();
        assert!(banned.is_empty(), "a node must never ban itself");
        let marker = dir.join("cluster_evicted.json");
        assert!(marker.exists(), "self-remove must land the eviction marker");
        let Some(self_unban) =
            seal_into_test(KIND_MEMBER_UNBAN, &format!("{self_id}@10.0.0.9:9090"))
        else {
            return;
        };
        append_to(&path, &self_unban).expect("append self-unban");
        assert!(
            !marker.exists(),
            "self-unban must clear the eviction marker"
        );

        // Replay is the pure cluster view — the deltas were committed,
        // so the derived roster reports them even though the local
        // roster correctly declined to apply them. The self-remove
        // evicts the self entry the derived roster had been carrying.
        let state = replay_records(&load_from(&path));
        assert_eq!(state.memberships, 9);
        assert_eq!(state.roster.len(), 2);
        assert!(state.banned.is_empty());

        // Malformed member specs can't seal, and a member record
        // carrying one fails verify even when validly signed.
        assert!(
            CommitRecord::seal_member("c", "c", KIND_MEMBER_ADD, "no-address", vec![], "")
                .is_none()
        );
        // An unknown kind can't seal either, and a signed record
        // carrying one fails verify — arbitrary kinds must not append.
        assert!(CommitRecord::seal_member(
            "c",
            "c",
            "bogus_kind",
            "node-x@10.0.0.5:9090",
            vec![],
            ""
        )
        .is_none());
        let Some(mut bad) = seal_into_test(KIND_MEMBER_ADD, "node-x@10.0.0.5:9090") else {
            return;
        };
        bad.value = "not-a-member-spec".into();
        if let Some(k) = crate::susi_config::cluster_key::cluster_key() {
            bad.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &k,
                bad.signed_payload().as_bytes(),
            );
        }
        assert!(
            !bad.verify(),
            "member record with malformed value must fail verify"
        );
        // Same for a signed record whose kind is arbitrary — verify
        // must refuse it before it ever appends.
        let Some(mut bogus) = seal_into_test(KIND_MEMBER_ADD, "node-x@10.0.0.5:9090") else {
            return;
        };
        bogus.kind = "bogus_kind".into();
        if let Some(k) = crate::susi_config::cluster_key::cluster_key() {
            bogus.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &k,
                bogus.signed_payload().as_bytes(),
            );
        }
        assert!(!bogus.verify(), "unknown record kind must fail verify");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn member_records_require_a_known_coordinator() {
        // Coordinator-authority gate: an evicted node still holds
        // cluster.key, so a member record sealed by a non-member (or
        // merely-discovered peer) must be refused at intake — otherwise
        // a rogue evicted member could evict the whole cluster.
        let dir = std::env::temp_dir().join(format!("susi_mauth_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("peers.json"),
            serde_json::to_string(&serde_json::json!([
                { "node_id": "coord-a", "admission": "explicit" },
                { "node_id": "gossip-only", "admission": "discovered" },
            ]))
            .unwrap(),
        )
        .unwrap();
        // Privileged records must be sealed under the coordinator's own
        // leadership claim — leader == coordinator.
        let rec = |coordinator: &str, kind: &str| CommitRecord {
            epoch: "e".into(),
            coordinator: coordinator.into(),
            electorate: vec![],
            tally: 1,
            quorum_threshold: 1,
            value_hash: "h".into(),
            value: "node-x@10.0.0.5:9090".into(),
            committed_at: 0,
            seq: 1,
            leader: coordinator.into(),
            term: 0,
            prev_epoch: String::new(),
            kind: kind.into(),
            signature: "s".into(),
            member_sig: String::new(),
            member_pubkey: String::new(),
            subject_sig: String::new(),
            endorsements: Vec::new(),
        };
        // An explicit member's delta is authorized; discovered peers and
        // unknown ids are not.
        assert!(member_coordinator_known_at(
            &rec("coord-a", KIND_MEMBER_ADD),
            &dir
        ));
        assert!(!member_coordinator_known_at(
            &rec("gossip-only", KIND_MEMBER_ADD),
            &dir
        ));
        assert!(!member_coordinator_known_at(
            &rec("ghost", KIND_MEMBER_REMOVE),
            &dir
        ));
        // Our own operator-sealed deltas always pass, and non-member
        // records are unaffected by the gate.
        let self_id = crate::susi_config::cluster_key::wire_node_id();
        assert!(member_coordinator_known_at(
            &rec(&self_id, KIND_MEMBER_REMOVE),
            &dir
        ));
        assert!(member_coordinator_known_at(&rec("ghost", ""), &dir));
        // Leader-proposed rule: a member record whose sealer observed a
        // DIFFERENT leader carries no authority — it must be proposed
        // through the leader, not self-sealed by a follower.
        let mut follower_sealed = rec("coord-a", KIND_MEMBER_ADD);
        follower_sealed.leader = "someone-else".into();
        assert!(!member_coordinator_known_at(&follower_sealed, &dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pre_membership_records_parse_as_decisions() {
        // A record serialized before `kind` existed must still load and
        // present as a decision — the ledger's history is permanent.
        let line = r#"{"epoch":"e","coordinator":"c","electorate":["A","B"],"tally":2,"quorum_threshold":2,"value_hash":"h","value":"v","committed_at":0,"seq":1,"leader":"l","term":1,"signature":"s"}"#;
        let rec: CommitRecord = serde_json::from_str(line).expect("legacy record parses");
        assert!(rec.kind.is_empty());
        assert!(rec.member_delta().is_none());
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

    #[test]
    fn chain_link_seals_previous_head_and_signature_covers_it() {
        let _g = test_key_guard();
        let dir = std::env::temp_dir().join(format!("susi_chain_{}", std::process::id()));
        let path = dir.join("commit_log.jsonl");
        let _ = fs::remove_dir_all(&dir);

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
        // Genesis: no predecessor → empty link.
        r1.seq = 1;
        r1.prev_epoch.clear();
        resign(&mut r1);
        append_to(&path, &r1).unwrap();

        // An honest successor links its predecessor's epoch.
        let mut r2 = r1.clone();
        r2.epoch = "epoch-two".into();
        r2.seq = 2;
        r2.value = "v2".into();
        r2.value_hash = hex::encode(Sha256::digest(b"v2"));
        r2.prev_epoch = r1.epoch.clone();
        resign(&mut r2);
        append_to(&path, &r2).unwrap();

        // Derivation: chain_head_epoch tracks the coordinator's tail.
        let held = load_from(&path);
        assert_eq!(
            chain_head_epoch(&held, "node-a").as_deref(),
            Some("epoch-two")
        );
        assert_eq!(chain_head_epoch(&held, "node-b"), None);

        // The link is signed — mutating it breaks verification.
        let mut forged = r2.clone();
        forged.prev_epoch = "deadbeef".into();
        assert!(!forged.verify(), "tampered prev_epoch must fail");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn divergent_predecessor_is_refused_and_replay_flags_chain_break() {
        let _g = test_key_guard();
        let dir = std::env::temp_dir().join(format!("susi_fork_{}", std::process::id()));
        let path = dir.join("commit_log.jsonl");
        let _ = fs::remove_dir_all(&dir);

        let Some(mut pred) = CommitRecord::seal(CommitInput {
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
        pred.seq = 1;
        pred.prev_epoch.clear();
        resign(&mut pred);
        append_to(&path, &pred).unwrap();

        // A successor claiming a different predecessor epoch is fork
        // evidence — refused at the boundary.
        let mut fork = pred.clone();
        fork.epoch = "epoch-fork".into();
        fork.seq = 2;
        fork.prev_epoch = "not-the-held-epoch".into();
        resign(&mut fork);
        let err = append_to(&path, &fork).unwrap_err();
        assert!(err.to_string().contains("chain divergence"), "{err}");

        // Out-of-order delivery: seq 2 lands before its predecessor —
        // the link can't be evaluated yet so it appends, but once the
        // divergent predecessor fills in, replay must flag the break.
        let mut ahead = pred.clone();
        ahead.epoch = "epoch-two".into();
        ahead.seq = 3;
        ahead.prev_epoch = "phantom-epoch".into();
        resign(&mut ahead);
        append_to(&path, &ahead).unwrap(); // seq 2 slot still empty
        let mut gap = pred.clone();
        gap.epoch = "epoch-real-two".into();
        gap.seq = 2;
        gap.prev_epoch = pred.epoch.clone();
        resign(&mut gap);
        append_to(&path, &gap).unwrap();
        let state = replay_records(&load_from(&path));
        assert!(
            state.anomalies.iter().any(|a| a.contains("chain-break")),
            "replay must flag the divergent link: {:?}",
            state.anomalies
        );
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

    /// Real multi-process serialization proof: spawn copies of this test
    /// binary as workers that race `claim_leadership_at` on one term file.
    /// Every claim names a distinct leader, so each is a genuine
    /// read-modify-write — the file lock must deliver exactly
    /// WORKERS × CLAIMS bumps. Without it, two workers can both read
    /// term N and each write N+1, losing bumps.
    #[test]
    fn cross_process_term_claims_serialize_through_file_lock() {
        const WORKERS: usize = 4;
        const CLAIMS: usize = 8;
        let dir = std::env::temp_dir().join(format!("susi-flock-e2e-{}", std::process::id()));
        if let Ok(worker) = std::env::var("SUSI_FLOCK_WORKER") {
            // Worker mode — dir arrives via env so all processes share it.
            let dir = PathBuf::from(std::env::var("SUSI_FLOCK_DIR").expect("worker dir"));
            let term = dir.join("term.json");
            for i in 0..CLAIMS {
                claim_leadership_at(&term, &format!("{worker}-{i}"));
            }
            return;
        }
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let exe = std::env::current_exe().expect("test binary path");
        let mut children = Vec::new();
        for w in 0..WORKERS {
            children.push(
                std::process::Command::new(&exe)
                    // Substring filter, not --exact: the test's full name
                    // differs between canonical (commit_log::tests::…) and
                    // vendored binaries (susi_core::commit_log::tests::…).
                    .args([
                        "cross_process_term_claims_serialize_through_file_lock",
                        "--nocapture",
                    ])
                    .env("SUSI_FLOCK_WORKER", format!("worker-{w}"))
                    .env("SUSI_FLOCK_DIR", &dir)
                    .spawn()
                    .expect("spawn worker"),
            );
        }
        for mut c in children {
            assert!(c.wait().expect("wait worker").success(), "worker failed");
        }
        let state = load_term_from(&dir.join("term.json"));
        assert_eq!(
            state.term,
            (WORKERS * CLAIMS) as u64,
            "file lock must serialize every cross-process claim"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

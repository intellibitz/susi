//! `susi commits` — audit view of the replicated quorum-commit ledger
//! (`~/.susi/commit_log.jsonl`). Each record is signature-verified against
//! the local cluster key on display: a forged or tampered entry that
//! somehow landed in the file shows `VERIFIED no`, so the audit output
//! never silently trusts the ledger's contents. `susi commits audit`
//! cross-checks the whole ledger for the protocol's defined anomalies:
//! invalid signatures, per-coordinator sequence gaps, duplicate or
//! equivocating `(coordinator, seq)` slots, and commits stamped by a
//! coordinator that wasn't the elected leader.

use anyhow::Result;
use clap::Subcommand;
use std::path::Path;
use susi_core::commit_log;

#[derive(Debug, Subcommand)]
pub enum CommitsCommands {
    /// List commit records, newest first (default)
    List {
        /// Maximum records to show
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
        /// Only records from this coordinator
        #[arg(long)]
        coordinator: Option<String>,
        /// Only records sealed under this consensus term
        #[arg(long)]
        term: Option<u64>,
        /// Only records of this kind (decision, member_add,
        /// member_remove, member_unban)
        #[arg(long)]
        kind: Option<String>,
        /// Emit machine-readable JSON instead of the table — the form
        /// agents and scripts should consume
        #[arg(long)]
        json: bool,
    },
    /// Print the full record (including committed value) for an epoch prefix
    Show {
        /// Epoch hex prefix, e.g. the first 12 chars shown by `susi commits`
        epoch: String,
    },
    /// Whole-ledger consistency check: signature failures, sequence gaps,
    /// equivocation, and commits from non-elected coordinators
    Audit {
        /// Exit nonzero when anomalies are found — lets CI and operators
        /// gate on ledger health instead of eyeballing output
        #[arg(long)]
        strict: bool,
    },
    /// Fold the ledger into the cluster's consensus view (term, leader,
    /// per-coordinator high-water marks) — the state machine is a pure
    /// function of the log
    Replay,
    /// Proactively pull records this node is missing from verified peers —
    /// the receive-path repair only fires when a *push* arrives, so a node
    /// that was offline during replication needs this to catch up
    Sync,
    /// Fold the ledger into a snapshot and move pre-snapshot records to
    /// commit_log.archive.jsonl — bounds the live file's growth (Raft's
    /// InstallSnapshot analog). Local storage management, not a
    /// consensus event: the compacted and uncompacted views of the same
    /// history are identical
    Compact,
}

pub fn execute(action: Option<CommitsCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(CommitsCommands::List {
        limit: 20,
        coordinator: None,
        term: None,
        kind: None,
        json: false,
    }) {
        CommitsCommands::List {
            limit,
            coordinator,
            term,
            kind,
            json,
        } => list(limit, coordinator.as_deref(), term, kind.as_deref(), json),
        CommitsCommands::Show { epoch } => show(&epoch),
        CommitsCommands::Audit { strict } => audit(strict),
        CommitsCommands::Replay => replay_view(),
        CommitsCommands::Sync => sync(),
        CommitsCommands::Compact => compact(),
    }
}

/// Fold the live ledger into `commit_snapshot.json` + archive
/// pre-snapshot records. The retained per-coordinator anchors keep
/// chain linkage and seq derivation intact; `commit_log_fetch` keeps
/// serving the archive so peers can still repair across the boundary.
fn compact() -> Result<()> {
    let before = commit_log::load().len();
    let archived =
        commit_log::compact().map_err(|e| anyhow::anyhow!("ledger compaction failed: {e}"))?;
    let after = commit_log::load().len();
    if archived == 0 {
        println!("ledger already minimal ({before} records) — nothing to compact");
    } else {
        println!(
            "compacted: {archived} records archived to {}, {after} live (anchors + post-snapshot)",
            commit_log::archive_path().display()
        );
    }
    Ok(())
}

/// Pull every record each verified peer holds, verify + append locally.
/// `commit_log::append` is idempotent — records already held are skipped
/// safely, and anything failing signature verification is rejected.
fn sync() -> Result<()> {
    let peers_path = susi_paths::SusiDirs::config_dir().join("peers.json");
    let Ok(text) = std::fs::read_to_string(&peers_path) else {
        println!("no verified peers — nothing to sync from");
        return Ok(());
    };
    let Ok(nodes) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
        println!("peers.json is unreadable — nothing to sync from");
        return Ok(());
    };
    // Member-to-member calls authenticate with the cluster-derived
    // peer bearer — the per-host api_token only authenticates locally.
    let bearer = susi_config::cluster_key::peer_bearer();

    // Snapshot held records once — append() is idempotent on duplicates,
    // so "new" is determined by membership before the write, not the
    // write's outcome. The compaction archive counts as held: archived
    // records are still ours for dedup purposes.
    let held: std::collections::HashSet<String> = commit_log::load()
        .iter()
        .chain(commit_log::load_from(&commit_log::archive_path()).iter())
        .filter_map(|r| serde_json::to_string(r).ok())
        .collect();

    let mut total_new = 0usize;
    let mut total_dup = 0usize;
    let mut total_bad = 0usize;
    let mut total_pushed = 0usize;
    let mut total_push_rejected = 0usize;
    for n in &nodes {
        if n.get("admission").and_then(|a| a.as_str()) != Some("explicit") {
            continue;
        }
        let Some(addr) = n.get("address").and_then(|a| a.as_str()) else {
            continue;
        };
        // A loopback explicit entry is a self-edge (only possible from a
        // pre-guard `peers add` or a hand-edited peers.json) — syncing
        // with ourselves is a no-op that doubles as an MCP recursion.
        if addr.starts_with("127.") || addr.starts_with("::1") || addr.starts_with("localhost") {
            continue;
        }
        let id = n.get("node_id").and_then(|v| v.as_str()).unwrap_or(addr);
        // Paginate until a short page — a ledger past the fetch cap must
        // still converge instead of truncating at the first 1000 records.
        // The full record set is retained for the push half of the
        // exchange (records we hold that the peer lacks).
        let mut offset = 0usize;
        let mut their_records: Vec<commit_log::CommitRecord> = Vec::new();
        let (mut new, mut dup, mut bad) = (0usize, 0usize, 0usize);
        let mut reachable = true;
        loop {
            match susi_core::mcp_client::call_tool(
                addr,
                "commit_log_fetch",
                &serde_json::json!({ "limit": 1000, "offset": offset }),
                bearer.as_deref(),
            ) {
                Ok(result) => {
                    let Some(text) = result.pointer("/content/0/text").and_then(|t| t.as_str())
                    else {
                        println!("{id} ({addr}): no tool output");
                        reachable = false;
                        break;
                    };
                    let Ok(records) = serde_json::from_str::<Vec<commit_log::CommitRecord>>(text)
                    else {
                        println!("{id} ({addr}): malformed fetch payload");
                        reachable = false;
                        break;
                    };
                    let page = records.len();
                    offset += page;
                    their_records.extend(records);
                    if page < 1000 {
                        break;
                    }
                }
                Err(e) => {
                    println!("{id} ({addr}): unreachable — {e}");
                    reachable = false;
                    break;
                }
            }
        }
        if !reachable {
            continue;
        }
        let theirs: std::collections::HashSet<String> = their_records
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .collect();
        // Descending seq order: prior-epoch records (signed under the
        // retired cluster key) may only append when their seq+1
        // successor is held and names their epoch — landing successors
        // first lets a pre-rotation tail gap fill in one pass.
        let mut ordered: Vec<&commit_log::CommitRecord> = their_records.iter().collect();
        ordered.sort_by_key(|r| std::cmp::Reverse(r.seq));
        let mut to_apply: Vec<commit_log::CommitRecord> = Vec::new();
        for r in ordered {
            if !r.verify() {
                bad += 1;
                continue;
            }
            // Raft's step-down on the pull path, but only from member
            // coordinators — a non-member's forged high term must not
            // adopt into term.json (it would freeze honest pushes).
            if commit_log::coordinator_known(r) {
                let _ = commit_log::check_term(r);
            }
            // Member records need coordinator authority — an evicted
            // node still holds cluster.key. Refused records stay
            // missing and converge once the coordinator is known.
            if !commit_log::member_coordinator_known(r) {
                bad += 1;
                continue;
            }
            let key = serde_json::to_string(&r).unwrap_or_default();
            if held.contains(&key) {
                dup += 1;
                continue;
            }
            to_apply.push((*r).clone());
        }
        // One lock + one ledger load for the whole pull — per-record
        // `append` re-read and re-locked the ledger every record.
        for outcome in commit_log::append_many(&to_apply) {
            match outcome {
                commit_log::AppendOutcome::Applied => new += 1,
                commit_log::AppendOutcome::Skipped => dup += 1,
                commit_log::AppendOutcome::Refused => bad += 1,
            }
        }

        // Symmetric repair: records we hold that the peer lacks are pushed
        // through `commit_record` — the receive path re-runs signature,
        // quorum, and term gates, so a rejected push is the protocol
        // working, not a sync failure. The archive is ours to serve: an
        // uncompacted peer missing below-floor history can only get it
        // from our cold storage.
        let mut pushed = 0usize;
        let mut push_rejected = 0usize;
        for r in commit_log::load()
            .into_iter()
            .chain(commit_log::load_from(&commit_log::archive_path()))
        {
            let key = serde_json::to_string(&r).unwrap_or_default();
            if theirs.contains(&key) {
                continue;
            }
            // The tool's args ARE the record — commit_record deserializes
            // the argument object directly into CommitRecord.
            let Ok(args) = serde_json::to_value(&r) else {
                continue;
            };
            match susi_core::mcp_client::call_tool(addr, "commit_record", &args, bearer.as_deref())
            {
                // A completed RPC can still carry a tool-level rejection
                // (isError) — a stale-term or consistency refusal is the
                // protocol's gate working, not a sync failure.
                Ok(result) if result.get("isError").and_then(|v| v.as_bool()) == Some(true) => {
                    push_rejected += 1;
                }
                Ok(_) => pushed += 1,
                Err(_) => push_rejected += 1,
            }
        }
        println!(
            "{id} ({addr}): +{new} pulled, {dup} already held, {bad} rejected; \
             {pushed} pushed, {push_rejected} push-rejected"
        );
        total_new += new;
        total_dup += dup;
        total_bad += bad;
        total_pushed += pushed;
        total_push_rejected += push_rejected;
    }
    println!(
        "sync: {total_new} pulled, {total_dup} already held, {total_bad} rejected; \
         {total_pushed} pushed, {total_push_rejected} push-rejected"
    );
    Ok(())
}

fn list(
    limit: usize,
    coordinator: Option<&str>,
    term: Option<u64>,
    kind: Option<&str>,
    json: bool,
) -> Result<()> {
    // "decision" is the display name for records with no kind field —
    // let the filter accept the same vocabulary the table prints.
    let records: Vec<_> = commit_log::load()
        .into_iter()
        .filter(|r| coordinator.is_none_or(|c| r.coordinator == c))
        .filter(|r| term.is_none_or(|t| r.term == t))
        .filter(|r| {
            kind.is_none_or(|k| {
                if r.kind.is_empty() {
                    k == "decision"
                } else {
                    r.kind == k
                }
            })
        })
        .collect();
    if json {
        let rows: Vec<serde_json::Value> = records
            .iter()
            .rev()
            .take(limit.min(500))
            .map(|r| {
                serde_json::json!({
                    "epoch": r.epoch,
                    "seq": r.seq,
                    "term": r.term,
                    "coordinator": r.coordinator,
                    "leader": r.leader,
                    "kind": if r.kind.is_empty() { "decision" } else { r.kind.as_str() },
                    "tally": r.tally,
                    "quorum": r.quorum_threshold,
                    "committed_at": r.committed_at,
                    "verified": match r.verify_key_epoch() {
                        Some(commit_log::KeyEpoch::Current) => "yes",
                        Some(commit_log::KeyEpoch::Prev) => "prev",
                        None => "no",
                    },
                    "subject": r
                        .member_delta()
                        .map(|(_, id, addr)| format!("{id}@{addr}"))
                        .or_else(|| {
                            r.rekey_fingerprint()
                                .map(|fp| format!("key:{}", &fp[..12.min(fp.len())]))
                        }),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!(
        "{:<14} {:<5} {:<5} {:<18} {:<24} {:<7} {:<7} {:<12} {:<8} SUBJECT",
        "EPOCH",
        "SEQ",
        "TERM",
        "COORDINATOR",
        "KIND",
        "TALLY",
        "QUORUM",
        "COMMITTED_AT",
        "VERIFIED"
    );
    for r in records.iter().rev().take(limit.min(500)) {
        // Member rows show the delta's target — an audit of roster
        // history needs the who, not just the kind. Rekey rows show
        // the rotated key's fingerprint prefix — the epoch boundary's
        // identity.
        let subject = r
            .member_delta()
            .map(|(_, id, addr)| format!("{id}@{addr}"))
            .or_else(|| {
                r.rekey_fingerprint()
                    .map(|fp| format!("key:{}", &fp[..12.min(fp.len())]))
            })
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<14} {:<5} {:<5} {:<18} {:<24} {:<7} {:<7} {:<12} {:<8} {}",
            &r.epoch[..12.min(r.epoch.len())],
            r.seq,
            r.term,
            r.coordinator,
            if r.kind.is_empty() {
                "decision"
            } else {
                r.kind.as_str()
            },
            r.tally,
            r.quorum_threshold,
            r.committed_at,
            match r.verify_key_epoch() {
                Some(commit_log::KeyEpoch::Current) => "yes",
                // Valid but signed under the retired key — pre-rotation
                // history, marked so an audit can tell the eras apart.
                Some(commit_log::KeyEpoch::Prev) => "prev",
                None => "NO",
            },
            subject
        );
    }
    Ok(())
}

fn show(epoch_prefix: &str) -> Result<()> {
    let mut records = commit_log::load();
    records.extend(commit_log::load_from(&commit_log::archive_path()));
    let matches: Vec<_> = records
        .iter()
        .filter(|r| r.epoch.starts_with(epoch_prefix))
        .collect();
    match matches.len() {
        0 => anyhow::bail!("no commit record with epoch prefix `{epoch_prefix}`"),
        1 => {}
        _ => anyhow::bail!("epoch prefix `{epoch_prefix}` is ambiguous"),
    }
    let r = matches[0];
    println!("{}", serde_json::to_string_pretty(r)?);
    println!("verified: {}", r.verify());
    // Coordinator-attribution state: absent is the pre-PKI form; when
    // the roster binds the coordinator's key the sig must verify under
    // it — an INVALID here means the record would now be refused at
    // intake (forgery or a re-bound key).
    let bound_pk = std::fs::read_to_string(susi_paths::SusiDirs::config_dir().join("peers.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<serde_json::Value>>(&t).ok())
        .unwrap_or_default()
        .iter()
        .find_map(|p| {
            (p.get("node_id").and_then(|v| v.as_str()) == Some(r.coordinator.as_str()))
                .then(|| {
                    p.get("pubkey")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
                .flatten()
        });
    let msig = if r.member_sig.is_empty() {
        "absent".to_string()
    } else if r.coordinator == susi_config::cluster_key::wire_node_id() {
        match susi_config::cluster_key::node_pubkey_hex() {
            Some(pk)
                if susi_config::cluster_key::member_verify(&pk, &r.signature, &r.member_sig) =>
            {
                "valid (self)".to_string()
            }
            _ => "INVALID (self)".to_string(),
        }
    } else {
        match bound_pk {
            Some(pk)
                if susi_config::cluster_key::member_verify(&pk, &r.signature, &r.member_sig) =>
            {
                "valid".to_string()
            }
            Some(_) => "INVALID".to_string(),
            None => "present (coordinator key unbound)".to_string(),
        }
    };
    println!("member_sig: {msig}");
    // Binding attestation: a member_add claiming a pubkey must carry
    // the subject's own signature — verify it here so an operator can
    // see whether a binding is subject-proven or would be refused now.
    if !r.member_pubkey.is_empty() {
        let member_id = r.member_delta().map(|(_, id, _)| id).unwrap_or_default();
        let att = if r.subject_sig.is_empty() {
            "ABSENT — record would be refused at intake today".to_string()
        } else if susi_config::cluster_key::verify_bind_attestation(
            member_id,
            &r.member_pubkey,
            &r.subject_sig,
        ) {
            "valid (subject-signed)".to_string()
        } else {
            "INVALID".to_string()
        };
        println!("subject_sig: {att}");
    }
    if !r.kind.is_empty() {
        let names: Vec<&str> = r.endorsements.iter().map(|e| e.node.as_str()).collect();
        let gate = if susi_core::commit_log::endorsements_satisfied(r) {
            "satisfied"
        } else {
            "BELOW bound quorum"
        };
        println!(
            "endorsements: {} signer(s) [{}] — {}",
            r.endorsements.len(),
            names.join(", "),
            gate
        );
    }
    Ok(())
}

/// Cross-checks the whole ledger for the anomalies the protocol defines.
/// Structural checks (signature failures, sequence gaps, duplicates,
/// equivocation, term regressions) come from `commit_log::replay` — the
/// same fold that reconstructs consensus state, so audit and replay can
/// never disagree. Adds the audit-only non-leader-commit check:
/// `coordinator != leader` is legitimate for per-node missions, so it is
/// reported as an anomaly, not a violation.
fn audit(strict: bool) -> Result<()> {
    // Full history = live ledger + compaction archive — an audit that
    // stops at the snapshot boundary would miss pre-snapshot
    // anomalies. `replay()` seeds from the snapshot so gap detection
    // stays honest on compacted ledgers.
    let mut records = commit_log::load();
    records.extend(commit_log::load_from(&commit_log::archive_path()));
    let state = commit_log::replay();
    if let Some(snap) = commit_log::load_snapshot() {
        println!(
            "snapshot: {} coordinators at/below high-water (created {})",
            snap.high_water.len(),
            snap.created_at
        );
    }
    let mut anomalies = state.anomalies.len();

    for a in &state.anomalies {
        println!("{}", a.to_uppercase());
    }
    for r in &records {
        if r.verify() && !r.leader.is_empty() && r.leader != r.coordinator {
            anomalies += 1;
            println!(
                "NON-LEADER COMMIT  {} seq {} — elected leader was {} ({})",
                r.coordinator,
                r.seq,
                r.leader,
                &r.epoch[..12.min(r.epoch.len())]
            );
        }
    }

    println!(
        "{} — {} record(s) checked, {} verified",
        if anomalies == 0 {
            "OK"
        } else {
            "ANOMALIES FOUND"
        },
        records.len(),
        state.decisions
    );
    if strict && anomalies > 0 {
        anyhow::bail!(
            "{anomalies} ledger anomal{} found",
            if anomalies == 1 { "y" } else { "ies" }
        );
    }
    Ok(())
}

/// Print the consensus view reconstructed from the ledger — current term,
/// leader, and each coordinator's high-water sequence mark. Two nodes
/// holding the same records derive the same view.
fn replay_view() -> Result<()> {
    let state = commit_log::replay();
    let persisted = commit_log::load_term();
    println!("term:          {}", state.term);
    println!(
        "leader:        {}",
        if state.leader.is_empty() {
            "(none)"
        } else {
            &state.leader
        }
    );
    println!(
        "persisted:     term {} / leader {}",
        persisted.term,
        if persisted.leader.is_empty() {
            "(none)".to_string()
        } else {
            persisted.leader.clone()
        }
    );
    println!("decisions:     {}", state.decisions);
    println!("memberships:   {}", state.memberships);
    if !state.roster.is_empty() {
        println!("committed roster:");
        for (id, addr) in &state.roster {
            println!("  {id:<24} {addr}");
        }
    }
    if !state.banned.is_empty() {
        println!("committed bans:");
        for (id, addr) in &state.banned {
            println!("  {id:<24} {addr}");
        }
    }
    println!("coordinators:");
    for (coord, high) in &state.coordinators {
        println!("  {coord:<20} high-water seq {high}");
    }
    if !state.anomalies.is_empty() {
        println!("anomalies:");
        for a in &state.anomalies {
            println!("  - {a}");
        }
    }
    Ok(())
}

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
}

pub fn execute(action: Option<CommitsCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(CommitsCommands::List {
        limit: 20,
        coordinator: None,
        term: None,
    }) {
        CommitsCommands::List {
            limit,
            coordinator,
            term,
        } => list(limit, coordinator.as_deref(), term),
        CommitsCommands::Show { epoch } => show(&epoch),
        CommitsCommands::Audit { strict } => audit(strict),
        CommitsCommands::Replay => replay_view(),
        CommitsCommands::Sync => sync(),
    }
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
    let token_path = susi_paths::SusiDirs::config_dir().join("api_token");
    let token = std::fs::read_to_string(token_path).unwrap_or_default();
    let bearer = (!token.trim().is_empty()).then(|| token.trim().to_string());

    // Snapshot held records once — append() is idempotent on duplicates,
    // so "new" is determined by membership before the write, not the
    // write's outcome.
    let mut held: std::collections::HashSet<String> = commit_log::load()
        .iter()
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
        for r in &their_records {
            if !r.verify() {
                bad += 1;
                continue;
            }
            let key = serde_json::to_string(&r).unwrap_or_default();
            if held.contains(&key) {
                dup += 1;
                continue;
            }
            match commit_log::append(r) {
                Ok(()) => {
                    held.insert(key);
                    new += 1;
                }
                Err(_) => bad += 1,
            }
        }

        // Symmetric repair: records we hold that the peer lacks are pushed
        // through `commit_record` — the receive path re-runs signature,
        // quorum, and term gates, so a rejected push is the protocol
        // working, not a sync failure.
        let mut pushed = 0usize;
        let mut push_rejected = 0usize;
        for r in commit_log::load() {
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

fn list(limit: usize, coordinator: Option<&str>, term: Option<u64>) -> Result<()> {
    let records: Vec<_> = commit_log::load()
        .into_iter()
        .filter(|r| coordinator.is_none_or(|c| r.coordinator == c))
        .filter(|r| term.is_none_or(|t| r.term == t))
        .collect();
    println!(
        "{:<14} {:<5} {:<5} {:<18} {:<13} {:<7} {:<7} {:<12} VERIFIED",
        "EPOCH", "SEQ", "TERM", "COORDINATOR", "KIND", "TALLY", "QUORUM", "COMMITTED_AT"
    );
    for r in records.iter().rev().take(limit.min(500)) {
        println!(
            "{:<14} {:<5} {:<5} {:<18} {:<13} {:<7} {:<7} {:<12} {}",
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
            if r.verify() { "yes" } else { "NO" }
        );
    }
    Ok(())
}

fn show(epoch_prefix: &str) -> Result<()> {
    let records = commit_log::load();
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
    let records = commit_log::load();
    let state = commit_log::replay_records(&records);
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

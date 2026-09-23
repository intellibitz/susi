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
}

pub fn execute(action: Option<CommitsCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(CommitsCommands::List { limit: 20 }) {
        CommitsCommands::List { limit } => list(limit),
        CommitsCommands::Show { epoch } => show(&epoch),
        CommitsCommands::Audit { strict } => audit(strict),
        CommitsCommands::Replay => replay_view(),
    }
}

fn list(limit: usize) -> Result<()> {
    let records = commit_log::load();
    println!(
        "{:<14} {:<5} {:<5} {:<18} {:<7} {:<7} {:<12} VERIFIED",
        "EPOCH", "SEQ", "TERM", "COORDINATOR", "TALLY", "QUORUM", "COMMITTED_AT"
    );
    for r in records.iter().rev().take(limit.min(500)) {
        println!(
            "{:<14} {:<5} {:<5} {:<18} {:<7} {:<7} {:<12} {}",
            &r.epoch[..12.min(r.epoch.len())],
            r.seq,
            r.term,
            r.coordinator,
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

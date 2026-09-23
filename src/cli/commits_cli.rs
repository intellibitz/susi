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
    Audit,
}

pub fn execute(action: Option<CommitsCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(CommitsCommands::List { limit: 20 }) {
        CommitsCommands::List { limit } => list(limit),
        CommitsCommands::Show { epoch } => show(&epoch),
        CommitsCommands::Audit => audit(),
    }
}

fn list(limit: usize) -> Result<()> {
    let records = commit_log::load();
    println!(
        "{:<14} {:<5} {:<18} {:<7} {:<7} {:<12} VERIFIED",
        "EPOCH", "SEQ", "COORDINATOR", "TALLY", "QUORUM", "COMMITTED_AT"
    );
    for r in records.iter().rev().take(limit.min(500)) {
        println!(
            "{:<14} {:<5} {:<18} {:<7} {:<7} {:<12} {}",
            &r.epoch[..12.min(r.epoch.len())],
            r.seq,
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

/// Cross-checks the whole ledger for the anomalies the protocol defines:
///
/// - **signature failure** — forged or tampered entry that landed in the file
/// - **sequence gap** — a coordinator's `seq` skips values (lost replication
///   that anti-entropy could not repair)
/// - **equivocation** — two different records claiming the same
///   `(coordinator, seq)` slot (refused at append since the idempotency fix,
///   but a ledger written earlier or corrupted out-of-band can still hold them)
/// - **non-leader commit** — `coordinator != leader`: per-node missions are
///   legitimately coordinated by their initiator, so this is reported as an
///   anomaly for audit, not a violation.
fn audit() -> Result<()> {
    let records = commit_log::load();
    let mut anomalies = 0usize;

    let mut by_coordinator: std::collections::BTreeMap<&str, Vec<&commit_log::CommitRecord>> =
        std::collections::BTreeMap::new();
    for r in &records {
        if !r.verify() {
            anomalies += 1;
            println!(
                "INVALID SIGNATURE  {} seq {} ({})",
                r.coordinator,
                r.seq,
                &r.epoch[..12.min(r.epoch.len())]
            );
        }
        if r.seq > 0 {
            by_coordinator.entry(&r.coordinator).or_default().push(r);
        }
        if !r.leader.is_empty() && r.leader != r.coordinator {
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

    for (coordinator, mut recs) in by_coordinator {
        recs.sort_by_key(|r| r.seq);
        // Same seq appearing twice: identical lines are pre-idempotency
        // duplicates; different content is equivocation.
        for w in recs.windows(2) {
            if w[0].seq == w[1].seq {
                anomalies += 1;
                if w[0] == w[1] {
                    println!(
                        "DUPLICATE  {coordinator} seq {} — identical record appended twice",
                        w[0].seq
                    );
                } else {
                    println!(
                        "EQUIVOCATION  {coordinator} seq {} — two different signed records",
                        w[0].seq
                    );
                }
            }
        }
        let held: Vec<u64> = recs.iter().map(|r| r.seq).collect();
        if let Some(&max) = held.iter().max() {
            let missing = commit_log::missing_seqs(&records, coordinator, max + 1);
            if !missing.is_empty() {
                anomalies += 1;
                println!("SEQUENCE GAP  {coordinator} — missing seq {missing:?}");
            }
        }
    }

    println!(
        "{} — {} record(s) checked",
        if anomalies == 0 {
            "OK"
        } else {
            "ANOMALIES FOUND"
        },
        records.len()
    );
    Ok(())
}

//! `susi commits` — audit view of the replicated quorum-commit ledger
//! (`~/.susi/commit_log.jsonl`). Each record is signature-verified against
//! the local cluster key on display: a forged or tampered entry that
//! somehow landed in the file shows `VERIFIED no`, so the audit output
//! never silently trusts the ledger's contents.

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
}

pub fn execute(action: Option<CommitsCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(CommitsCommands::List { limit: 20 }) {
        CommitsCommands::List { limit } => list(limit),
        CommitsCommands::Show { epoch } => show(&epoch),
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

//! Multi-agent transaction CLI.
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;

#[derive(Debug, Subcommand)]
pub enum TxCommands {
    /// Begin a transaction snapshotting the given files (comma-separated).
    Begin {
        #[arg(short, long)]
        description: String,
        #[arg(short, long, default_value = "")]
        files: String,
    },
    /// Commit an open transaction.
    Commit {
        #[arg(short, long)]
        id: String,
    },
    /// Abort and restore snapshotted files.
    Abort {
        #[arg(short, long)]
        id: String,
    },
    /// List open transactions.
    List,
}

pub fn execute(action: Option<TxCommands>, workspace: &Path) -> Result<()> {
    let mgr = susi_core::agent_tx::TxManager::global();
    match action {
        Some(TxCommands::Begin { description, files }) => {
            let file_rels: Vec<String> = files
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let tx = mgr.begin(workspace, &description, &file_rels, Default::default())?;
            println!("{}", serde_json::to_string_pretty(&tx)?);
        }
        Some(TxCommands::Commit { id }) => {
            let tx = mgr.commit(&id)?;
            println!("{}", serde_json::to_string_pretty(&tx)?);
        }
        Some(TxCommands::Abort { id }) => {
            let tx = mgr.abort(&id, workspace)?;
            println!("{}", serde_json::to_string_pretty(&tx)?);
        }
        Some(TxCommands::List) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"open": mgr.list_open()}))?
            );
        }
        None => bail!("tx subcommand required"),
    }
    Ok(())
}

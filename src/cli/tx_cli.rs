//! Multi-agent transaction CLI.
use anyhow::{bail, Result};
use std::path::Path;

pub use susi_cli_defs::tx::*;

pub fn execute(action: Option<TxCommands>, workspace: &Path) -> Result<()> {
    let mgr = susi_core::agent_tx::TxManager::global();
    // Every CLI call is a fresh process — pull the durable .susi/tx/
    // records in so commit/abort/list see earlier `begin`s.
    mgr.hydrate(workspace);
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
            let tx = mgr.commit(&id, workspace)?;
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

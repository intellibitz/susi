//! Extension pack control plane (list/create/load/unload/status).
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use susi_sandbox::extensions::{
    active_pack, create_pack, ensure_extensions_substrate, list_packs, load_pack, unload_pack,
};

#[derive(Debug, Subcommand)]
pub enum ExtensionCommands {
    /// Seed substrate + list packs (default when bare `susi extensions`)
    List,
    /// Show active pack and substrate root
    Status,
    /// Create an empty pack shell under ~/.susi/extensions/<id>/
    Create { id: String },
    /// Load (activate) a pack
    Load { id: String },
    /// Unload a pack (keeps files; falls back to default)
    Unload { id: String },
    /// Ensure default pack is seeded and print its path
    Seed,
}

pub fn execute(action: Option<ExtensionCommands>) -> Result<()> {
    match action.unwrap_or(ExtensionCommands::List) {
        ExtensionCommands::List => {
            let _ = ensure_extensions_substrate().map_err(|e| anyhow::anyhow!(e))?;
            print_json(&list_packs())?;
        }
        ExtensionCommands::Status => {
            let pack = ensure_extensions_substrate().map_err(|e| anyhow::anyhow!(e))?;
            print_json(&serde_json::json!({
                "active": pack.id,
                "root": pack.root,
                "packs": list_packs(),
            }))?;
        }
        ExtensionCommands::Create { id } => {
            let status = create_pack(&id).map_err(|e| anyhow::anyhow!(e))?;
            print_json(&status)?;
        }
        ExtensionCommands::Load { id } => {
            let status = load_pack(&id).map_err(|e| anyhow::anyhow!(e))?;
            print_json(&serde_json::json!({
                "loaded": status,
                "active": active_pack().id,
            }))?;
        }
        ExtensionCommands::Unload { id } => {
            let status = unload_pack(&id).map_err(|e| anyhow::anyhow!(e))?;
            print_json(&serde_json::json!({
                "unloaded": status.id,
                "active": active_pack().id,
            }))?;
        }
        ExtensionCommands::Seed => {
            let pack = ensure_extensions_substrate().map_err(|e| anyhow::anyhow!(e))?;
            if pack.id.is_empty() {
                bail!("failed to seed extension substrate");
            }
            print_json(&serde_json::json!({
                "seeded": true,
                "active": pack.id,
                "root": pack.root,
            }))?;
        }
    }
    Ok(())
}

//! Extension pack control plane (list/create/load/unload/status/validate).
//!
//! Core ships and seeds no pack: packs are installed under
//! `~/.susi/extensions/<id>/` by the user or the commercial desktop, and this
//! plane lists, verifies, activates and deactivates them.
use crate::cli_json::print_json;
use anyhow::Result;
use clap::Subcommand;
use susi_sandbox::extensions::{
    create_pack, list_packs, load_report, status_report, unload_report, validate_report,
};

#[derive(Debug, Subcommand)]
pub enum ExtensionCommands {
    /// List installed packs (default when bare `susi extensions`)
    List,
    /// Show active pack and substrate root
    Status,
    /// Create an empty pack shell under ~/.susi/extensions/<id>/
    Create { id: String },
    /// Load (activate) a pack
    Load { id: String },
    /// Unload a pack (keeps files; falls back to compiled defaults)
    Unload { id: String },
    /// Check a pack directory against the host's admission rules (for pack
    /// authors; reads and changes no host state)
    Validate { path: std::path::PathBuf },
}

pub fn execute(action: Option<ExtensionCommands>) -> Result<()> {
    match action.unwrap_or(ExtensionCommands::List) {
        ExtensionCommands::List => {
            print_json(&list_packs())?;
        }
        ExtensionCommands::Status => {
            print_json(&status_report())?;
        }
        ExtensionCommands::Create { id } => {
            let status = create_pack(&id).map_err(|e| anyhow::anyhow!(e))?;
            print_json(&status)?;
        }
        ExtensionCommands::Load { id } => {
            print_json(&load_report(&id).map_err(|e| anyhow::anyhow!(e))?)?;
        }
        ExtensionCommands::Unload { id } => {
            print_json(&unload_report(&id).map_err(|e| anyhow::anyhow!(e))?)?;
        }
        ExtensionCommands::Validate { path } => {
            print_json(&validate_report(&path).map_err(|e| anyhow::anyhow!(e))?)?;
        }
    }
    Ok(())
}

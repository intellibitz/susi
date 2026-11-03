//! Extension pack control plane (list/create/load/unload/status/validate).
//!
//! Core ships and seeds no pack: packs are installed under
//! `~/.susi/extensions/<id>/` by the user or the commercial desktop, and this
//! plane lists, verifies, activates and deactivates them.
use crate::cli_json::print_json;
use anyhow::Result;
use clap::Subcommand;
use susi_sandbox::extensions::{
    active_pack, create_pack, list_packs, load_pack, unload_pack, validate_pack_dir,
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
            let pack = active_pack();
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
        ExtensionCommands::Validate { path } => {
            let (manifest, artifacts) = validate_pack_dir(&path).map_err(|e| anyhow::anyhow!(e))?;
            print_json(&serde_json::json!({
                "valid": true,
                "id": manifest.id,
                "version": manifest.version,
                "apiVersion": manifest.api_version,
                "permissions": manifest.permissions,
                "signed": manifest.signer.is_some(),
                "artifacts_sha256": artifacts,
            }))?;
        }
    }
    Ok(())
}

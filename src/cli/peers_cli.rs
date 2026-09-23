//! `susi peers` — operator surface for the verified cluster roster
//! (`~/.susi/peers.json`). Only `explicit` (cluster-key-verified) peers
//! are persisted by the swarm; `remove` revokes a member's persisted
//! trust so a decommissioned or suspect node stops voting on the next
//! election/quorum round. The roster is read structurally — the root
//! crate takes no dependency on the swarm plane.

use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum PeersCommands {
    /// List verified peers (default)
    List,
    /// Revoke a peer's persisted trust by node_id or address prefix
    Remove {
        /// node_id or address prefix of the peer to remove
        peer: String,
    },
}

pub fn execute(action: Option<PeersCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(PeersCommands::List) {
        PeersCommands::List => list(),
        PeersCommands::Remove { peer } => remove(&peer),
    }
}

fn registry_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("peers.json")
}

fn load_registry() -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(registry_path()) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn list() -> Result<()> {
    let nodes = load_registry();
    if nodes.is_empty() {
        println!("no verified peers — this node runs standalone");
        return Ok(());
    }
    println!(
        "{:<22} {:<22} {:<7} {:<8} ACTIVE",
        "PEER", "ADDRESS", "TRUST", "ADMISSION"
    );
    for n in &nodes {
        println!(
            "{:<22} {:<22} {:<7.2} {:<8} {}",
            n.get("node_id").and_then(|v| v.as_str()).unwrap_or("?"),
            n.get("address").and_then(|v| v.as_str()).unwrap_or("?"),
            n.get("trust_score").and_then(|v| v.as_f64()).unwrap_or(0.0),
            n.get("admission").and_then(|v| v.as_str()).unwrap_or("?"),
            if n.get("is_active")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                "yes"
            } else {
                "no"
            }
        );
    }
    Ok(())
}

fn remove(peer: &str) -> Result<()> {
    let nodes = load_registry();
    let kept: Vec<_> = nodes
        .iter()
        .filter(|n| {
            let id = n.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
            let addr = n.get("address").and_then(|v| v.as_str()).unwrap_or("");
            !(id == peer || addr.starts_with(peer) || id.starts_with(peer))
        })
        .cloned()
        .collect();
    if kept.len() == nodes.len() {
        bail!("no verified peer matching `{peer}`");
    }
    let path = registry_path();
    let body = serde_json::to_string_pretty(&kept)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)?;
    println!(
        "removed {} peer(s); {} verified member(s) remain",
        nodes.len() - kept.len(),
        kept.len()
    );
    Ok(())
}

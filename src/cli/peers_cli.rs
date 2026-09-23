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

/// A persisted peer is only "live" if it ponged within the swarm's staleness
/// window — the on-disk `is_active` flag is last-sweep state and can be hours
/// stale when the daemon isn't running.
const PEER_STALE_SECS: u64 = 30;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Liveness for display: only a `last_seen_secs` within the staleness window
/// counts — anything older (or absent, from a pre-liveness registry) shows
/// `stale` until the peer re-verifies with a signed pong.
fn liveness(n: &serde_json::Value) -> &'static str {
    let now = now_secs();
    let last_seen = n
        .get("last_seen_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if last_seen != 0 && now.saturating_sub(last_seen) <= PEER_STALE_SECS {
        "yes"
    } else {
        "stale"
    }
}

fn list() -> Result<()> {
    let nodes = load_registry();
    if nodes.is_empty() {
        println!("no verified peers — this node runs standalone");
        return Ok(());
    }
    println!(
        "{:<22} {:<22} {:<7} {:<8} LIVE",
        "PEER", "ADDRESS", "TRUST", "ADMISSION"
    );
    for n in &nodes {
        println!(
            "{:<22} {:<22} {:<7.2} {:<8} {}",
            n.get("node_id").and_then(|v| v.as_str()).unwrap_or("?"),
            n.get("address").and_then(|v| v.as_str()).unwrap_or("?"),
            n.get("trust_score").and_then(|v| v.as_f64()).unwrap_or(0.0),
            n.get("admission").and_then(|v| v.as_str()).unwrap_or("?"),
            liveness(n),
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
    // Name what's being revoked — a broad prefix must not wipe members silently.
    let removed: Vec<String> = nodes
        .iter()
        .filter(|n| !kept.contains(n))
        .map(|n| {
            format!(
                "{} ({})",
                n.get("node_id").and_then(|v| v.as_str()).unwrap_or("?"),
                n.get("address").and_then(|v| v.as_str()).unwrap_or("?")
            )
        })
        .collect();
    let path = registry_path();
    let body = serde_json::to_string_pretty(&kept)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)?;
    for r in &removed {
        println!("revoked verified trust: {r}");
    }
    println!("{} verified member(s) remain", kept.len());
    Ok(())
}

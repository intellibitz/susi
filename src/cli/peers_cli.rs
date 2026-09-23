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
    /// List verified peers and banned members (default)
    List,
    /// Evict a peer: revoke persisted trust AND ban re-verification.
    /// The ban is enforced at the signed-pong handshake — the peer cannot
    /// rejoin until `susi peers unban` lifts it.
    Remove {
        /// node_id or address prefix of the peer to evict
        peer: String,
    },
    /// Lift a ban so a previously evicted peer can re-verify
    Unban {
        /// node_id or address of the banned peer to restore
        peer: String,
    },
}

pub fn execute(action: Option<PeersCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(PeersCommands::List) {
        PeersCommands::List => list(),
        PeersCommands::Remove { peer } => remove(&peer),
        PeersCommands::Unban { peer } => unban(&peer),
    }
}

fn registry_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("peers.json")
}

fn banned_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("peers_banned.json")
}

fn load_banned() -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(banned_path()) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
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
    let banned = load_banned();
    if nodes.is_empty() && banned.is_empty() {
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
    if !banned.is_empty() {
        println!();
        println!("banned members (blocked at handshake):");
        for b in &banned {
            println!(
                "  {} ({}) — evicted {}s ago",
                b.get("node_id").and_then(|v| v.as_str()).unwrap_or("?"),
                b.get("address").and_then(|v| v.as_str()).unwrap_or("?"),
                now_secs().saturating_sub(b.get("banned_at").and_then(|v| v.as_u64()).unwrap_or(0))
            );
        }
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
    let evicted: Vec<serde_json::Value> = nodes
        .iter()
        .filter(|n| !kept.contains(n))
        .cloned()
        .collect();
    let path = registry_path();
    let body = serde_json::to_string_pretty(&kept)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)?;
    // Record the ban — without it the evicted member re-verifies on its next
    // signed pong and silently rejoins. The swarm's signed-pong handler reads
    // peers_banned.json at admission.
    let mut banned = load_banned();
    let now = now_secs();
    for n in &evicted {
        let id = n.get("node_id").and_then(|v| v.as_str()).unwrap_or("?");
        let addr = n.get("address").and_then(|v| v.as_str()).unwrap_or("?");
        if !banned.iter().any(|b| {
            b.get("node_id").and_then(|v| v.as_str()) == Some(id)
                || b.get("address").and_then(|v| v.as_str()) == Some(addr)
        }) {
            banned.push(serde_json::json!({
                "node_id": id, "address": addr, "banned_at": now,
            }));
        }
        println!("evicted + banned: {id} ({addr})");
    }
    let bpath = banned_path();
    let btmp = bpath.with_extension("json.tmp");
    std::fs::write(&btmp, serde_json::to_string_pretty(&banned)?)?;
    std::fs::rename(&btmp, &bpath)?;
    println!(
        "{} verified member(s) remain; {} banned",
        kept.len(),
        banned.len()
    );
    Ok(())
}

fn unban(peer: &str) -> Result<()> {
    let banned = load_banned();
    let kept: Vec<_> = banned
        .iter()
        .filter(|b| {
            let id = b.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
            let addr = b.get("address").and_then(|v| v.as_str()).unwrap_or("");
            !(id == peer || addr == peer || id.starts_with(peer) || addr.starts_with(peer))
        })
        .cloned()
        .collect();
    if kept.len() == banned.len() {
        bail!("no banned peer matching `{peer}`");
    }
    let bpath = banned_path();
    let btmp = bpath.with_extension("json.tmp");
    std::fs::write(&btmp, serde_json::to_string_pretty(&kept)?)?;
    std::fs::rename(&btmp, &bpath)?;
    println!(
        "lifted ban on {} member(s); they may re-verify on next handshake",
        banned.len() - kept.len()
    );
    Ok(())
}

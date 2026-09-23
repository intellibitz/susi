//! `susi os` — the operating-system view of the substrate in one shot:
//! consensus state (term / leader / decisions from the replicated
//! ledger), the leaf-service process table, and the verified peer
//! roster. This is the operator's answer to "is the OS for agents
//! healthy right now?" — everything printed is derived from persisted
//! kernel state, so it is accurate even when the daemon is down.

use anyhow::Result;
use std::path::Path;
use susi_core::{commit_log, service_table};

#[derive(Debug, clap::Subcommand)]
pub enum OsCommands {
    /// Full substrate status: consensus, services, peers (default)
    Status,
}

pub fn execute(action: Option<OsCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(OsCommands::Status) {
        OsCommands::Status => status(),
    }
}

fn status() -> Result<()> {
    let state = commit_log::replay();
    let term = commit_log::load_term();
    let services = service_table::status();
    let peers = load_verified_peers();
    let up = services.iter().filter(|s| s.up).count();

    println!("SUSI OS — substrate status");
    println!(
        "consensus:   term {} / leader {} — {} decision(s), {} anomal{}",
        state.term.max(term.term),
        leader_display(&state, &term),
        state.decisions,
        state.anomalies.len(),
        if state.anomalies.len() == 1 {
            "y"
        } else {
            "ies"
        }
    );
    println!("services:    {}/{} leaf services up", up, services.len());
    println!("peers:       {} verified cluster member(s)", peers.len());
    println!();

    println!(
        "{:<14} {:<6} {:<8} {:<9} UP",
        "SERVICE", "PORT", "PID", "RESTARTS"
    );
    for s in &services {
        let pid = s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into());
        println!(
            "{:<14} {:<6} {:<8} {:<9} {}",
            s.name,
            s.port,
            pid,
            s.restarts,
            if s.up { "yes" } else { "no" }
        );
    }
    println!();

    if peers.is_empty() {
        println!("no verified peers (peers.json empty — this node runs standalone)");
    } else {
        println!("{:<22} {:<22} {:<7} ACTIVE", "PEER", "ADDRESS", "TRUST");
        for p in &peers {
            println!(
                "{:<22} {:<22} {:<7.2} {}",
                p.node_id,
                p.address,
                p.trust_score,
                if p.is_active { "yes" } else { "no" }
            );
        }
    }

    if !state.anomalies.is_empty() {
        println!();
        println!("ledger anomalies (see `susi commits audit`):");
        for a in &state.anomalies {
            println!("  - {a}");
        }
    }
    Ok(())
}

/// Prefer the persisted term's leader when it is ahead of what the
/// ledger alone shows — the persisted file is updated by elections even
/// when no commits followed.
fn leader_display<'a>(
    state: &'a commit_log::ClusterState,
    term: &'a commit_log::TermState,
) -> String {
    if term.term > state.term {
        if term.leader.is_empty() {
            "(none)".to_string()
        } else {
            term.leader.clone()
        }
    } else if state.leader.is_empty() {
        "(none)".to_string()
    } else {
        state.leader.clone()
    }
}

/// The persisted roster is `susi_gawd_swarm`'s type; the root crate reads
/// it structurally (node_id / address / trust_score / is_active) so the
/// OS view needs no dependency edge into the swarm plane.
struct PeerView {
    node_id: String,
    address: String,
    trust_score: f64,
    is_active: bool,
}

fn load_verified_peers() -> Vec<PeerView> {
    let path = susi_paths::SusiDirs::config_dir().join("peers.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(nodes) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
        return Vec::new();
    };
    nodes
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            Some(PeerView {
                node_id: n.get("node_id")?.as_str()?.to_string(),
                address: n.get("address")?.as_str()?.to_string(),
                trust_score: n.get("trust_score").and_then(|t| t.as_f64()).unwrap_or(0.0),
                is_active: n
                    .get("is_active")
                    .and_then(|a| a.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect()
}

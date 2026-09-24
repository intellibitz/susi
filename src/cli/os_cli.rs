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
    Status {
        /// Emit machine-readable JSON instead of the table view
        #[arg(long)]
        json: bool,
    },
}

pub fn execute(action: Option<OsCommands>, top_json: bool, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(OsCommands::Status { json: false }) {
        OsCommands::Status { json } => status(json || top_json),
    }
}

fn status(json: bool) -> Result<()> {
    let state = commit_log::replay();
    let term = commit_log::load_term();
    let services = service_table::status();
    let peers = load_verified_peers();
    let up = services.iter().filter(|s| s.up).count();

    if json {
        let daemon =
            susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir());
        let body = serde_json::json!({
            "node_id": susi_config::cluster_key::wire_node_id(),
            "consensus": {
                "term": state.term.max(term.term),
                "leader": leader_display(&state, &term),
                "decisions": state.decisions,
                "anomalies": state.anomalies,
                "coordinators": state.coordinators,
            },
            "daemon": daemon.as_ref().map(|d| serde_json::json!({
                "pid": d.pid, "substrate_home": d.substrate_home,
            })),
            "storage": disk_free(&susi_paths::SusiDirs::substrate_home())
                .map(|(avail, total)| serde_json::json!({
                    "available_bytes": avail, "total_bytes": total,
                })),
            "services": services.iter().map(|s| serde_json::json!({
                "name": s.name, "port": s.port, "pid": s.pid,
                "restarts": s.restarts, "up": s.up, "external": s.external,
                "stopped": s.stopped, "rss": s.rss(),
            })).collect::<Vec<_>>(),
            "peers": peers.iter().map(|p| serde_json::json!({
                "node_id": &p.node_id, "address": &p.address,
                "trust_score": p.trust_score, "fresh": p.fresh(),
                "reachable": p.probe(),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    println!("SUSI OS — substrate status");
    println!("node:        {}", susi_config::cluster_key::wire_node_id());
    let term_age = if term.term == 0 || term.updated_at == 0 {
        String::new()
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!(" ({}s ago)", now.saturating_sub(term.updated_at))
    };
    println!(
        "consensus:   term {} / leader {}{} — {} decision(s), {} anomal{}",
        state.term.max(term.term),
        leader_display(&state, &term),
        term_age,
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
    if susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists()
    {
        println!("cluster:     EVICTED — removed by a committed member_remove; standing down until a committed unban");
    }
    let daemon = susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir());
    println!(
        "daemon:      {}",
        daemon
            .as_ref()
            .map(|d| format!("running (pid {})", d.pid))
            .unwrap_or_else(|| "not running".to_string())
    );
    if let Some((avail, total)) = disk_free(&susi_paths::SusiDirs::substrate_home()) {
        println!(
            "storage:     {} free / {} total on substrate_home",
            human_bytes(avail),
            human_bytes(total)
        );
    }
    println!();

    println!(
        "{:<14} {:<6} {:<8} {:<9} {:<8} {:<10} UP",
        "SERVICE", "PORT", "PID", "RESTARTS", "UPTIME", "RSS"
    );
    let any_external = services.iter().any(|s| s.external);
    for s in &services {
        let pid = match (s.external, s.pid) {
            (true, Some(p)) => format!("{p}*"),
            (true, None) => "ext*".to_string(),
            (false, Some(p)) => p.to_string(),
            (false, None) => "-".to_string(),
        };
        println!(
            "{:<14} {:<6} {:<8} {:<9} {:<8} {:<10} {}",
            s.name,
            s.port,
            pid,
            s.restarts,
            s.uptime(),
            s.rss(),
            if s.stopped {
                "stopped"
            } else if s.up {
                "yes"
            } else {
                "no"
            }
        );
    }
    if any_external {
        println!("* external process — bound outside daemon supervision");
    }
    println!();

    if peers.is_empty() {
        println!("no verified peers (peers.json empty — this node runs standalone)");
    } else {
        let live = peers.iter().filter(|p| p.probe()).count();
        println!("peers live:  {}/{} reachable", live, peers.len());
        println!();
        println!(
            "{:<22} {:<22} {:<7} {:<7} REACHABLE",
            "PEER", "ADDRESS", "TRUST", "FRESH"
        );
        for p in &peers {
            println!(
                "{:<22} {:<22} {:<7.2} {:<7} {}",
                p.node_id,
                p.address,
                p.trust_score,
                if p.fresh() { "yes" } else { "stale" },
                if p.probe() { "yes" } else { "no" }
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

/// Free/total bytes on the filesystem holding `path` — a ledger or roster
/// write failing on a full disk is a substrate-level fault the OS view
/// should surface, not hide behind generic IO errors.
fn disk_free(path: &std::path::Path) -> Option<(u64, u64)> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    // Longest-mount-point-prefix match (e.g. /home on a separate fs).
    disks
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| (d.available_space(), d.total_space()))
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
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
/// it structurally (node_id / address / trust_score / last_seen_secs) so the
/// OS view needs no dependency edge into the swarm plane.
struct PeerView {
    node_id: String,
    address: String,
    trust_score: f64,
    last_seen_secs: u64,
}

/// Mirrors `susi_gawd_swarm::amas::PEER_STALE_SECS` — kept as a literal so
/// the root crate keeps zero dependency edges into the swarm plane.
const PEER_STALE_SECS: u64 = 30;

impl PeerView {
    /// Whether the peer's last signed pong is inside the staleness window —
    /// the swarm's definition of quorum-eligible liveness.
    fn fresh(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.last_seen_secs != 0 && now.saturating_sub(self.last_seen_secs) <= PEER_STALE_SECS
    }

    /// TCP liveness probe — the persisted roster only records who was
    /// verified, not who is reachable right now. 300ms budget: peers
    /// are LAN-adjacent, so a longer wait just stalls the status view.
    fn probe(&self) -> bool {
        use std::net::ToSocketAddrs;
        let Some(addr) = self
            .address
            .to_socket_addrs()
            .ok()
            .and_then(|mut i| i.next())
        else {
            return false;
        };
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
    }
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
                last_seen_secs: n
                    .get("last_seen_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            })
        })
        .collect()
}

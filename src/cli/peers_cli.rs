//! `susi peers` — operator surface for the verified cluster roster
//! (`~/.susi/peers.json`). Only `explicit` (cluster-key-verified) peers
//! are persisted by the swarm; `remove` revokes a member's persisted
//! trust so a decommissioned or suspect node stops voting on the next
//! election/quorum round. The roster is read structurally — the root
//! crate takes no dependency on the swarm plane.

use anyhow::Result;
use std::path::Path;
use susi_core::peer_admin::{self, Line};

/// Result lines to stdout, notes to stderr, as they happen.
fn emit(line: Line) {
    match line {
        Line::Info(text) => println!("{text}"),
        Line::Note(text) => eprintln!("{text}"),
    }
}

pub use susi_cli_defs::peers::*;

pub fn execute(action: Option<PeersCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(PeersCommands::List { json: false }) {
        PeersCommands::List { json } => list(json),
        // `susi_core::peer_admin` runs the membership changes; the MCP
        // `peers_*` tools call the same functions.
        PeersCommands::Add { host, port } => {
            peer_admin::add(&mut emit, &host, port).map_err(anyhow::Error::msg)
        }
        PeersCommands::Probe { host, port } => probe(&host, port),
        PeersCommands::Remove { peer } => {
            peer_admin::remove(&mut emit, &peer).map_err(anyhow::Error::msg)
        }
        PeersCommands::Unban { peer } => {
            peer_admin::unban(&mut emit, &peer).map_err(anyhow::Error::msg)
        }
        PeersCommands::Rekey { force } => {
            peer_admin::rekey(&mut emit, force).map_err(anyhow::Error::msg)
        }
        PeersCommands::Status { json } => status(json),
    }
}

/// `peers status` — query every rostered member's `cluster_status` and
/// print the consensus picture side by side. Member calls ride the
/// signed-request channel, so bound members answer on key possession
/// alone; unreachable members show as offline rather than blocking.
fn status(json_out: bool) -> Result<()> {
    // `susi_core::peer_roster::status_rows`: the same rows the MCP
    // `peers_status` tool returns.
    let rows = susi_core::peer_roster::status_rows().map_err(anyhow::Error::msg)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
        return Ok(());
    }
    println!(
        "{:<28} {:>5} {:<28} {:>7} {:>6} {:>6} {:<14} STATE",
        "NODE", "TERM", "LEADER", "RECORDS", "ROSTER", "BOUND", "KEY EPOCH"
    );
    for r in &rows {
        println!("{}", status_line(r));
    }
    // Divergence surface: differing terms, leaders or key epochs across
    // members is the operator-visible symptom of a partitioned or stale
    // cluster.
    for warning in susi_core::peer_roster::divergence_warnings(&rows) {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

/// Liveness for display: only a `last_seen_secs` within the staleness window
/// counts — anything older (or absent, from a pre-liveness registry) shows
/// `stale` until the peer re-verifies with a signed pong.
/// One `susi peers status` table row. A member discovery still sees is
/// shown as live even when its status call failed: only its term, leader,
/// records and key epoch are unknown, so those columns stay blank.
fn status_line(r: &serde_json::Value) -> String {
    let node = r.get("node").and_then(|v| v.as_str()).unwrap_or("?");
    if let Some(err) = r.get("error").and_then(|v| v.as_str()) {
        let state = if r.get("live_via_discovery").and_then(|v| v.as_bool()) == Some(true) {
            "live, status unavailable"
        } else {
            "offline"
        };
        return format!(
            "{node:<28} {:>5} {:<28} {:>7} {:>6} {:>6} {:<14} {state} ({err})",
            "-", "-", "-", "-", "-", "-"
        );
    }
    let term = r.get("term").and_then(|v| v.as_u64()).unwrap_or(0);
    let leader = r.get("leader").and_then(|v| v.as_str()).unwrap_or("");
    let leader_disp = if leader.is_empty() { "(none)" } else { leader };
    let records = r.get("records").and_then(|v| v.as_u64()).unwrap_or(0);
    let roster_n = r.get("roster").and_then(|v| v.as_u64()).unwrap_or(0);
    let bound = r.get("bound").and_then(|v| v.as_u64()).unwrap_or(0);
    let epoch = r.get("key_epoch").and_then(|v| v.as_str()).unwrap_or("-");
    let state = if r.get("self").and_then(|v| v.as_bool()) == Some(true) {
        "this node"
    } else {
        "member"
    };
    format!(
        "{node:<28} {term:>5} {leader_disp:<28} {records:>7} {roster_n:>6} {bound:>6} {epoch:<14} {state}"
    )
}

fn liveness(n: &serde_json::Value) -> &'static str {
    susi_core::peer_roster::liveness(n)
}

fn list(json: bool) -> Result<()> {
    let nodes = susi_core::peer_roster::load_registry().map_err(anyhow::Error::msg)?;
    let banned = susi_core::peer_roster::load_banned().map_err(anyhow::Error::msg)?;
    let evicted = susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists();
    // The elected coordination head from term.json — marks the roster row
    // so an operator can see who currently claims leadership.
    let term = susi_core::commit_log::load_term();
    let leader = term.leader.as_str();
    if json {
        // Machine surface for agents — same fields the table shows, and the
        // document the MCP `peers_list` tool returns.
        let body = susi_core::peer_roster::list_report().map_err(anyhow::Error::msg)?;
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    // This node's wire identity — an operator verifying a member joined
    // needs the local id to match against the remote roster.
    println!("node: {}", susi_config::cluster_key::wire_node_id());
    if !leader.is_empty() {
        println!("leader: {} (term {})", leader, term.term);
    }
    // A committed member_remove naming this node stands it down — the
    // scout is silent until a committed unban clears the marker.
    if evicted {
        println!("status: EVICTED — this node was removed from the cluster; cluster traffic is suspended until a committed unban");
    }
    if nodes.is_empty() && banned.is_empty() {
        println!("no verified peers — this node runs standalone");
        return Ok(());
    }
    println!(
        "{:<22} {:<22} {:<7} {:<8} LIVE  KEY",
        "PEER", "ADDRESS", "TRUST", "ADMISSION"
    );
    for n in &nodes {
        let id = n.get("node_id").and_then(|v| v.as_str()).unwrap_or("?");
        let id = if id == leader {
            format!("{} *", id)
        } else {
            id.to_string()
        };
        // Bound = the member's Ed25519 key is attested — its records
        // must carry member_sig or intake refuses them. `bound*` means
        // the subject signed the binding itself (v3 attestation);
        // plain `bound` is HMAC-only (pre-v3 peer or a committed
        // binding from before attestations existed).
        let key_state = if !n
            .get("pubkey")
            .and_then(|v| v.as_str())
            .is_some_and(|p| !p.is_empty())
        {
            "-"
        } else if n
            .get("bind_sig")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
        {
            "bound*"
        } else {
            "bound"
        };
        println!(
            "{:<22} {:<22} {:<7.2} {:<8} {:<5} {}",
            id,
            n.get("address").and_then(|v| v.as_str()).unwrap_or("?"),
            n.get("trust_score").and_then(|v| v.as_f64()).unwrap_or(0.0),
            n.get("admission").and_then(|v| v.as_str()).unwrap_or("?"),
            liveness(n),
            key_state,
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
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
                    .saturating_sub(b.get("banned_at").and_then(|v| v.as_u64()).unwrap_or(0))
            );
        }
    }
    Ok(())
}

/// `peers probe` — the same verified handshake `peers add` performs,
/// without persisting anything. Reports the responder's attested
/// identity and its standing in this node's roster — the
/// non-destructive way to test reachability and cluster.key
/// compatibility before (or instead of) `peers add`.
fn probe(host: &str, port: Option<u16>) -> Result<()> {
    let report = susi_core::peer_probe::probe_report(host, port).map_err(anyhow::Error::msg)?;
    let text = |key: &str| report[key].as_str().unwrap_or_default().to_string();
    println!("verified: {} ({})", text("node_id"), text("address"));
    println!("  registry checksum: {}", report["registry_checksum"]);
    println!("  capability bloom:  {}", text("capability_bloom"));
    println!(
        "  gossip roster:     {} member(s) advertised",
        report["gossip_roster_members"]
    );
    let key = &report["signing_key"];
    let prefix = key["prefix"].as_str().unwrap_or_default();
    println!(
        "  signing key:       {}",
        match key["attestation"].as_str().unwrap_or_default() {
            "subject-attested" => format!("{prefix}… (subject-attested)"),
            "unbound (pre-PKI peer)" => "unbound (pre-PKI peer)".to_string(),
            other => format!("{prefix}… ({other})"),
        }
    );
    let endpoint = &report["mcp_endpoint"];
    if endpoint["serving"].as_bool() == Some(true) {
        println!(
            "  mcp endpoint:      serving (state {}, term {})",
            endpoint["state"].as_str().unwrap_or("up"),
            endpoint["term"]
        );
    } else {
        println!(
            "  mcp endpoint:      UNREACHABLE/INVALID — {}",
            endpoint["error"].as_str().unwrap_or_default()
        );
    }
    println!("  local standing:    {}", text("local_standing"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::status_line;
    use serde_json::json;

    #[test]
    fn a_live_member_whose_status_call_failed_reads_live_not_offline() {
        let line = status_line(&json!({
            "node": "susi-node-peer",
            "error": "live by discovery; status unavailable: connection refused",
            "live_via_discovery": true,
            "self": false,
        }));
        assert!(line.contains("live, status unavailable"), "{line}");
        assert!(!line.contains("offline"), "{line}");
        assert!(line.contains("connection refused"), "{line}");
    }

    #[test]
    fn an_unreachable_member_discovery_does_not_see_reads_offline() {
        let line = status_line(&json!({"node": "susi-node-gone", "error": "timed out"}));
        assert!(line.contains("offline (timed out)"), "{line}");
    }

    #[test]
    fn a_reachable_member_shows_its_own_term_and_state() {
        let line = status_line(&json!({
            "node": "susi-node-a", "term": 4, "leader": "susi-node-a", "records": 9,
            "roster": 3, "bound": 2, "key_epoch": "e1", "self": false,
        }));
        assert!(line.contains(" 4 ") && line.ends_with("member"), "{line}");
        let me = status_line(&json!({"node": "susi-node-b", "self": true}));
        assert!(me.ends_with("this node"), "{me}");
    }
}

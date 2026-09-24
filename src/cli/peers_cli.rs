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
    /// Bootstrap cluster membership: send a cluster-key-signed discovery
    /// ping to a host and persist the verified responder as an explicit
    /// member. Fails closed when the peer doesn't hold our cluster.key.
    Add {
        /// IP or hostname of the peer daemon to join (may include :port)
        host: String,
        /// UDP discovery port (default: host-contract 9092)
        #[arg(long)]
        port: Option<u16>,
    },
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
        PeersCommands::Add { host, port } => add(&host, port),
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

/// Commit a roster change to the replicated ledger and push it to every
/// verified peer — Raft's committed configuration-entry analog. The
/// delta lands in `commit_log.jsonl` locally (applied to peers.json by
/// `commit_log::append` itself) and is pushed through the same
/// `commit_record` tool path `commits sync` uses; receivers verify the
/// signature/term/chain and apply on append, so one `peers add`
/// converges membership cluster-wide. Push failures only delay
/// convergence — `commits sync` and roster gossip carry the record.
fn commit_membership(kind: &str, node_id: &str, address: &str) {
    use susi_core::commit_log;
    // The roster as this node observed it at commit time — audit context
    // for who was a member when the delta was decided.
    let electorate: Vec<String> = load_registry()
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            n.get("node_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    let Some(record) = commit_log::CommitRecord::seal_member(
        &susi_config::cluster_key::wire_node_id(),
        &commit_log::load_term().leader,
        kind,
        &format!("{node_id}@{address}"),
        electorate,
    ) else {
        eprintln!("note: no cluster.key — membership change not committed to ledger");
        return;
    };
    if let Err(e) = commit_log::append(&record) {
        eprintln!("note: membership record not appended — {e}");
        return;
    }
    let token = std::fs::read_to_string(susi_paths::SusiDirs::config_dir().join("api_token"))
        .unwrap_or_default();
    let bearer = (!token.trim().is_empty()).then(|| token.trim().to_string());
    let Ok(args) = serde_json::to_value(&record) else {
        return;
    };
    let mut pushed = 0usize;
    for n in load_registry() {
        if n.get("admission").and_then(|a| a.as_str()) != Some("explicit") {
            continue;
        }
        let Some(addr) = n.get("address").and_then(|a| a.as_str()) else {
            continue;
        };
        // Same self-edge rule as `commits sync` — a loopback explicit
        // row is this node, and an MCP call to ourselves recurses.
        if addr.starts_with("127.") || addr.starts_with("::1") || addr.starts_with("localhost") {
            continue;
        }
        match susi_core::mcp_client::call_tool(addr, "commit_record", &args, bearer.as_deref()) {
            // A tool-level isError is a protocol refusal (stale term,
            // equivocation) — the gate working, not a push success.
            Ok(result) if result.get("isError").and_then(|v| v.as_bool()) != Some(true) => {
                pushed += 1;
            }
            _ => {}
        }
    }
    println!("membership delta committed to ledger; pushed to {pushed} peer(s)");
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
        // Commit the eviction — receivers drop the member and record the
        // ban on append, so the eviction takes effect cluster-wide, not
        // just where the operator ran the command.
        commit_membership(susi_core::commit_log::KIND_MEMBER_REMOVE, id, addr);
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

/// Join a peer by address: the same signed-ping → signed-pong handshake
/// the swarm scout uses, directed at one host instead of LAN broadcast.
/// A peer that can't produce a cluster-key-signed pong echoing our nonce
/// is never persisted — admission stays cryptographic, not asserted.
fn add(host: &str, port: Option<u16>) -> Result<()> {
    // `host` may carry an inline :port — split it so `add 10.0.0.4:9092`
    // works as naturally as `add 10.0.0.4 --port 9092`.
    let (host, inline_port) = match host.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() => match p.parse::<u16>() {
            Ok(n) => (h.to_string(), Some(n)),
            Err(_) => (host.to_string(), None),
        },
        _ => (host.to_string(), None),
    };
    let port = port
        .or(inline_port)
        .unwrap_or(susi_paths::ports::UDP_DISCOVERY);
    // Bloom field must be wire-safe (non-empty); a zero bloom is honest —
    // the CLI registers no tools, and the peer answers with its own bloom.
    // Identity-era ping first, then the pre-identity format — a daemon
    // that doesn't know the node_id field ignores the 7-field ping, so
    // `peers add` falls back instead of dead-ending on version skew.
    let mut attempts: Vec<(String, String)> = Vec::new();
    if let Some(p) = susi_config::cluster_key::signed_ping("CORE", 0, "0000000000000000") {
        attempts.push(p);
    }
    if let Some(p) = susi_config::cluster_key::signed_ping_legacy("CORE", 0, "0000000000000000") {
        attempts.push(p);
    }
    if attempts.is_empty() {
        bail!("no cluster key at ~/.susi/cluster.key — peers cannot be verified");
    }
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;

    // 4 KiB: a signed pong carrying a full gossip roster runs past 1 KiB.
    let mut buf = [0u8; 4096];
    for (ping, nonce) in attempts {
        socket.send_to(ping.as_bytes(), (host.as_str(), port))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let (amt, src) = match socket.recv_from(&mut buf) {
                Ok(r) => r,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            let msg = String::from_utf8_lossy(&buf[..amt]);
            let Some((node_id, checksum, bloom_hex, roster)) =
                susi_config::cluster_key::verify_signed_pong(&msg, &nonce)
            else {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                continue;
            };
            // A loopback responder is this host's own daemon — only one
            // process can bind the discovery port per host, so a signed
            // pong from 127.0.0.1/::1 is always ourselves. A self-edge
            // would let mission dispatch recurse into our own endpoint.
            if src.ip().is_loopback() {
                bail!(
                    "{node_id} answered from {} — that is this node's own daemon; \
                 `peers add` needs a remote host",
                    src.ip()
                );
            }

            let address = format!("{}:{}", src.ip(), susi_paths::ports::GMCP_HTTP);
            // ClusterPeerNode shape, written structurally — the root crate
            // takes no dependency on the swarm plane.
            let bloom_words: Vec<u64> = (0..bloom_hex.len() / 16)
                .filter_map(|i| u64::from_str_radix(&bloom_hex[i * 16..i * 16 + 16], 16).ok())
                .collect();
            let node = serde_json::json!({
                "node_id": node_id,
                "address": address,
                "node_type": "PEER",
                "is_active": true,
                "capabilities": ["CORE"],
                "registry_checksum": checksum,
                "latency_ms": 0,
                "uptime_secs": 0,
                "trust_score": 0.8,
                "capability_bloom": bloom_words,
                "admission": "explicit",
                "last_seen_secs": now_secs(),
            });

            let mut nodes = load_registry();
            nodes.retain(|n| {
                n.get("node_id").and_then(|v| v.as_str()) != Some(node_id.as_str())
                    && n.get("address").and_then(|v| v.as_str()) != Some(address.as_str())
            });
            nodes.push(node);
            // Roster gossip: the peer vouched for its own verified members.
            // Record them `discovered` — visible to the operator, never
            // load-bearing until the swarm's directed handshake upgrades
            // them to explicit (persisted members only ever carry
            // `explicit`; the swarm filters on read regardless).
            let banned = load_banned();
            let mut learned = 0usize;
            for (gid, gaddr) in &roster {
                let known = nodes.iter().any(|n| {
                    n.get("node_id").and_then(|v| v.as_str()) == Some(gid.as_str())
                        || n.get("address").and_then(|v| v.as_str()) == Some(gaddr.as_str())
                });
                let banned_hit = banned.iter().any(|b| {
                    b.get("node_id").and_then(|v| v.as_str()) == Some(gid.as_str())
                        || b.get("address").and_then(|v| v.as_str()) == Some(gaddr.as_str())
                });
                let looped = gaddr
                    .split(':')
                    .next()
                    .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback());
                if known || banned_hit || looped {
                    continue;
                }
                nodes.push(serde_json::json!({
                    "node_id": gid,
                    "address": gaddr,
                    "node_type": "PEER",
                    "is_active": true,
                    "capabilities": ["CORE"],
                    "registry_checksum": 0,
                    "latency_ms": 0,
                    "uptime_secs": 0,
                    "trust_score": 0.5,
                    "capability_bloom": [],
                    "admission": "discovered",
                    "last_seen_secs": now_secs(),
                }));
                learned += 1;
            }
            let path = registry_path();
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_string_pretty(&nodes)?)?;
            std::fs::rename(&tmp, &path)?;
            if learned > 0 {
                println!("verified + admitted: {node_id} ({address}); learned {learned} roster entr(ies) via gossip");
            } else {
                println!("verified + admitted: {node_id} ({address})");
            }
            // Commit the admission to the replicated ledger — every verified
            // peer applies the same roster delta on append, so membership
            // converges without a `peers add` on each node.
            commit_membership(susi_core::commit_log::KIND_MEMBER_ADD, &node_id, &address);
            return Ok(());
        }
    }
    bail!(
        "no signed pong from {host}:{port} — peer unreachable, not a susi daemon, \
         or doesn't share this cluster.key"
    )
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
    let lifted: Vec<serde_json::Value> = banned
        .iter()
        .filter(|b| !kept.contains(b))
        .cloned()
        .collect();
    let bpath = banned_path();
    let btmp = bpath.with_extension("json.tmp");
    std::fs::write(&btmp, serde_json::to_string_pretty(&kept)?)?;
    std::fs::rename(&btmp, &bpath)?;
    for b in &lifted {
        let id = b.get("node_id").and_then(|v| v.as_str()).unwrap_or("?");
        let addr = b.get("address").and_then(|v| v.as_str()).unwrap_or("?");
        // Replicate the unban — receivers banned this member via the
        // committed removal, so lifting it locally alone would leave
        // them refusing its handshakes forever.
        commit_membership(susi_core::commit_log::KIND_MEMBER_UNBAN, id, addr);
    }
    println!(
        "lifted ban on {} member(s); they may re-verify on next handshake",
        lifted.len()
    );
    Ok(())
}

//! `susi os status`: consensus, services, peers, endpoints and storage in
//! one view — the document `susi os status --json` prints and the table
//! the bare command prints. The MCP `os_status` tool returns the same JSON
//! over the `daemon.host` plane topic (zero-UI plan B4 group 9). Ledger and
//! roster are persisted kernel state, accurate even when the daemon is down.

use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;
use susi_core::{commit_log, service_table};

/// `susi os status --json`.
pub fn report() -> Value {
    let mut ignored = String::new();
    status_impl(true, &mut ignored).unwrap_or(Value::Null)
}

/// `susi os status`: the table view.
pub fn render() -> String {
    let mut out = String::new();
    let _ = status_impl(false, &mut out);
    out
}

/// Hygiene check: the credential files consensus depends on
/// (`cluster.key`, `node.key`, `api_token`) are created 0600, but an
/// operator `chmod` or a bad umask can loosen them silently — and a
/// world-readable cluster.key hands every local user full cluster
/// membership. Surfacing it in the status view turns an invisible
/// misconfiguration into an actionable warning.
#[cfg(unix)]
fn credential_warnings() -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = susi_paths::SusiDirs::config_dir();
    let mut warnings = Vec::new();
    for name in ["cluster.key", "node.key", "api_token"] {
        let path = dir.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue; // absent is a bootstrap state, not a perms issue
        };
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            warnings.push(format!(
                "{name} is group/world-readable (mode {mode:04o}) — run `chmod 600 {}`",
                path.display()
            ));
        }
    }
    warnings
}

#[cfg(not(unix))]
fn credential_warnings() -> Vec<String> {
    Vec::new()
}

fn status_impl(json: bool, out: &mut String) -> Option<Value> {
    let state = commit_log::replay();
    let term = commit_log::load_term();
    let services = service_table::status();
    let peers = load_verified_peers();
    let banned_count = load_banned_count();
    let up = services.iter().filter(|s| s.up).count();
    // Ledger flow: raw record count + the newest commit's age — an
    // operator watching replication health needs to see the log is
    // moving, not just that consensus state parses.
    let records = commit_log::load();
    let last_commit_age = records.iter().map(|r| r.committed_at).max().map(|newest| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(newest)
    });
    // A committed member_remove naming this node stands it down.
    let evicted = susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists();

    if json {
        let daemon = crate::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir());
        let swarm_host = crate::composition::swarm_host_snapshot();
        let body = serde_json::json!({
            "node_id": susi_config::cluster_key::wire_node_id(),
            "evicted": evicted,
            "swarm_host": swarm_host,
            "consensus": {
                "term": state.term.max(term.term),
                "leader": leader_display(&state, &term),
                "decisions": state.decisions,
                "anomalies": state.anomalies,
                "coordinators": state.coordinators,
            },
            "ledger": {
                "records": records.len(),
                "last_commit_age_secs": last_commit_age,
                "snapshot": commit_log::load_snapshot().map(|s| serde_json::json!({
                    "created_at": s.created_at,
                    "coordinators": s.high_water.len(),
                    "archived_records": s.decisions,
                })),
            },
            "daemon": daemon.as_ref().map(|d| serde_json::json!({
                "pid": d.pid, "substrate_home": d.substrate_home,
            })),
            "storage": disk_free(&susi_paths::SusiDirs::substrate_home())
                .map(|(avail, total)| serde_json::json!({
                    "available_bytes": avail, "total_bytes": total,
                })),
            "endpoints": endpoint_probes().iter().map(|(name, port, up)| {
                serde_json::json!({ "name": name, "port": port, "up": up })
            }).chain(std::iter::once(serde_json::json!({
                "name": "a2a-udp", "port": udp_port(),
                "up": null,
                "host_contract": true,
            }))).chain(std::iter::once(serde_json::json!({
                "name": "gossip-udp", "port": gossip_port(),
                "up": null,
                "host_contract": false,
                "note": "swarm-internal; not in ports::ALL (9092 is A2A discovery)",
            }))).collect::<Vec<_>>(),
            "substrate_usage": substrate_usage().iter().take(8).map(|(name, bytes)| {
                serde_json::json!({ "entry": name, "bytes": bytes })
            }).collect::<Vec<_>>(),
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
            "banned_peers": banned_count,
            "key_epoch": susi_config::cluster_key::cluster_key()
                .map(|k| susi_config::cluster_key::key_fingerprint(&k)[..12].to_string()),
            "staged_key_epoch": susi_config::cluster_key::staged_key()
                .map(|k| susi_config::cluster_key::key_fingerprint(&k)[..12].to_string()),
            "warnings": credential_warnings()
                .into_iter()
                .chain(stray_bin_warnings())
                .collect::<Vec<_>>(),
        });
        return Some(body);
    }

    let _ = writeln!(out, "SUSI OS — substrate status");
    let swarm_host = crate::composition::swarm_host_snapshot();
    let _ = writeln!(
        out,
        "swarm host:  {} — {} worker(s), {} busy, {} identit{}",
        swarm_host
            .get("orchestrator_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unwired"),
        swarm_host
            .get("workers")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        swarm_host.get("busy").and_then(|v| v.as_u64()).unwrap_or(0),
        swarm_host
            .get("identities")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        if swarm_host
            .get("identities")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            == 1
        {
            "y"
        } else {
            "ies"
        },
    );
    let _ = writeln!(
        out,
        "node:        {}",
        susi_config::cluster_key::wire_node_id()
    );
    // Key epoch: the current key's fingerprint prefix, plus any staged
    // next-epoch key awaiting its committed rekey record — operators
    // comparing `susi os` across nodes can see rotation drift at a
    // glance without the key itself ever being printed.
    let key_epoch = susi_config::cluster_key::cluster_key()
        .map(|k| susi_config::cluster_key::key_fingerprint(&k));
    let staged_epoch = susi_config::cluster_key::staged_key()
        .map(|k| susi_config::cluster_key::key_fingerprint(&k));
    if let Some(fp) = &key_epoch {
        let _ = writeln!(
            out,
            "key epoch:   {}{}",
            &fp[..12],
            staged_epoch
                .map(|s| format!(" — rotation to {} staged, awaiting commit", &s[..12]))
                .unwrap_or_default()
        );
    }
    let term_age = if term.term == 0 || term.updated_at == 0 {
        String::new()
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!(" (leader for {}s)", now.saturating_sub(term.updated_at))
    };
    let _ = writeln!(
        out,
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
    let _ = writeln!(
        out,
        "ledger:      {} record(s), last commit {}{}",
        records.len(),
        last_commit_age
            .map(|a| format!("{a}s ago"))
            .unwrap_or_else(|| "never".to_string()),
        commit_log::load_snapshot()
            .map(|s| format!(
                " — snapshot at {} ({} records archived)",
                s.created_at, s.decisions
            ))
            .unwrap_or_default()
    );
    let _ = writeln!(
        out,
        "services:    {}/{} leaf services up",
        up,
        services.len()
    );
    let _ = writeln!(
        out,
        "peers:       {} verified cluster member(s){}",
        peers.len(),
        if banned_count > 0 {
            format!(", {banned_count} banned")
        } else {
            String::new()
        }
    );
    if evicted {
        let _ = writeln!(
            out,
            "cluster:     EVICTED — removed by a committed member_remove; standing down until a committed unban"
        );
    }
    for warning in credential_warnings()
        .into_iter()
        .chain(stray_bin_warnings())
    {
        let _ = writeln!(out, "warning:     {warning}");
    }
    let daemon = crate::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir());
    let _ = writeln!(
        out,
        "daemon:      {}",
        daemon
            .as_ref()
            .map(|d| format!("running (pid {})", d.pid))
            .unwrap_or_else(|| "not running".to_string())
    );
    {
        let fields: Vec<String> = endpoint_probes()
            .iter()
            .map(|(name, port, up)| format!("{name} :{port} {}", if *up { "up" } else { "DOWN" }))
            .collect();
        let _ = writeln!(
            out,
            "endpoints:   {} (+a2a-udp :{})",
            fields.join(" · "),
            udp_port()
        );
    }
    if let Some((avail, total)) = disk_free(&susi_paths::SusiDirs::substrate_home()) {
        let _ = writeln!(
            out,
            "storage:     {} free / {} total on substrate_home",
            human_bytes(avail),
            human_bytes(total)
        );
    }
    let usage = substrate_usage();
    if !usage.is_empty() {
        let top: Vec<String> = usage
            .iter()
            .take(4)
            .map(|(name, bytes)| format!("{name} {}", human_bytes(*bytes)))
            .collect();
        let _ = writeln!(out, "footprint:   {}", top.join(" · "));
    }
    out.push('\n');

    let _ = writeln!(
        out,
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
        let _ = writeln!(
            out,
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
        let _ = writeln!(out, "* external process — bound outside daemon supervision");
    }
    out.push('\n');

    if peers.is_empty() {
        let _ = writeln!(
            out,
            "no verified peers (peers.json empty — this node runs standalone)"
        );
    } else {
        let live = peers.iter().filter(|p| p.probe()).count();
        let _ = writeln!(out, "peers live:  {}/{} reachable", live, peers.len());
        out.push('\n');
        let _ = writeln!(
            out,
            "{:<22} {:<22} {:<7} {:<7} REACHABLE",
            "PEER", "ADDRESS", "TRUST", "FRESH"
        );
        for p in &peers {
            let _ = writeln!(
                out,
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
        out.push('\n');
        let _ = writeln!(out, "ledger anomalies (see `susi commits audit`):");
        for a in &state.anomalies {
            let _ = writeln!(out, "  - {a}");
        }
    }
    None
}

/// Per-entry disk usage of `substrate_home`, largest first. Some
/// subsystems grow by design (model downloads, the cargo build cache,
/// rotated archives); the OS view should show where the substrate's
/// bytes actually live so an operator can spot unbounded growth rather
/// than discovering it via a full disk.
///
/// The recursive walk is bounded — an adversarial or pathological tree
/// must not stall a status command.
fn substrate_usage() -> Vec<(String, u64)> {
    const MAX_ENTRIES: usize = 250_000;
    let home = susi_paths::SusiDirs::substrate_home();
    let mut entries = 0usize;
    fn dir_size(path: &Path, entries: &mut usize) -> u64 {
        let mut total = 0u64;
        let Ok(read) = std::fs::read_dir(path) else {
            return 0;
        };
        for entry in read.flatten() {
            if *entries >= MAX_ENTRIES {
                return total;
            }
            *entries += 1;
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_file() {
                total += meta.len();
            } else if meta.is_dir() {
                total += dir_size(&entry.path(), entries);
            }
        }
        total
    }
    let mut usage: Vec<(String, u64)> = std::fs::read_dir(&home)
        .map(|read| {
            read.flatten()
                .map(|e| {
                    let size = e
                        .metadata()
                        .map(|m| {
                            if m.is_dir() {
                                dir_size(&e.path(), &mut entries)
                            } else {
                                m.len()
                            }
                        })
                        .unwrap_or(0);
                    (e.file_name().to_string_lossy().to_string(), size)
                })
                .collect()
        })
        .unwrap_or_default();
    usage.sort_by_key(|a| std::cmp::Reverse(a.1));
    usage
}

/// Stray heavyweight files inside `~/.susi/bin` — only the live binary
/// (`susi`/`susi.exe`) and an optional `lib/` dir belong there; an
/// orphaned `susi.rollback-*` from a manual backup can quietly hold
/// gigabytes. Flagged, never auto-deleted. One predicate with `clean`
/// (`susi_core::substrate_hygiene`, shared with the MCP `clean` tool).
fn stray_bin_warnings() -> Vec<String> {
    susi_core::substrate_hygiene::residue_warnings()
}

/// Free/total bytes on the filesystem holding `path` — a ledger or roster
/// write failing on a full disk is a substrate-level fault the OS view
/// should surface, not hide behind generic IO errors.
fn disk_free(path: &std::path::Path) -> Option<(u64, u64)> {
    // Longest-mount-point-prefix match (e.g. /home on a separate fs).
    susi_config::host_metrics::filesystem_of(path).map(|fs| (fs.available_bytes, fs.total_bytes))
}

fn human_bytes(n: u64) -> String {
    susi_core::substrate_hygiene::human_bytes(n)
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

/// Effective UDP discovery port (canonical base + `port_offset`).
fn udp_port() -> u16 {
    susi_config::SusiConfig::load_global()
        .map(|c| c.udp_discovery_port())
        .unwrap_or(susi_paths::ports::UDP_DISCOVERY)
}

fn gossip_port() -> u16 {
    susi_config::SusiConfig::load_global()
        .map(|c| c.gossip_port())
        .unwrap_or(susi_paths::ports::effective(susi_paths::ports::GOSSIP))
}

/// Live TCP probes of the public host-contract endpoints — shared by the
/// text view's `endpoints:` line and the `--json` payload. UDP discovery
/// has no TCP probe; callers surface its port statically.
fn endpoint_probes() -> Vec<(&'static str, u16, bool)> {
    let cfg = susi_config::SusiConfig::load_global().unwrap_or_default();
    let probes: [(&str, u16); 4] = [
        ("gmcp", cfg.gmcp_port()),
        ("gemi", cfg.gemi_port()),
        ("gmcp-sse", cfg.gmcp_http_port()),
        ("a2a", cfg.a2a_http_port()),
    ];
    // Concurrently: a refused loopback connect costs the whole timeout on
    // Windows, so four sequential probes of a stopped host took ~600 ms.
    std::thread::scope(|scope| {
        let handles: Vec<_> = probes
            .iter()
            .map(|&(name, port)| {
                scope.spawn(move || {
                    let up = std::net::TcpStream::connect_timeout(
                        &std::net::SocketAddr::new(
                            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                            port,
                        ),
                        std::time::Duration::from_millis(150),
                    )
                    .is_ok();
                    (name, port, up)
                })
            })
            .collect();
        handles
            .into_iter()
            .zip(probes)
            .map(|(handle, (name, port))| handle.join().unwrap_or((name, port, false)))
            .collect()
    })
}

/// Count of operator-evicted members — a nonzero value means
/// `peers_banned.json` holds members blocked at handshake.
fn load_banned_count() -> usize {
    let path = susi_paths::SusiDirs::config_dir().join("peers_banned.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<serde_json::Value>>(&t).ok())
        .map(|v| v.len())
        .unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_display_prefers_the_newer_term() {
        let state = commit_log::ClusterState {
            term: 1,
            leader: "a".into(),
            ..Default::default()
        };
        let term = commit_log::TermState {
            term: 2,
            leader: String::new(),
            updated_at: 0,
        };
        assert_eq!(leader_display(&state, &term), "(none)");
        let older = commit_log::TermState {
            term: 0,
            leader: String::new(),
            updated_at: 0,
        };
        assert_eq!(leader_display(&state, &older), "a");
    }

    #[test]
    fn leader_display_returns_non_empty_term_leader_when_term_is_ahead() {
        let state = commit_log::ClusterState {
            term: 3,
            leader: "node-old".into(),
            ..Default::default()
        };
        let newer = commit_log::TermState {
            term: 4,
            leader: "node-new".into(),
            updated_at: 100,
        };
        assert_eq!(leader_display(&state, &newer), "node-new");
    }

    #[test]
    fn leader_display_returns_none_placeholder_when_both_leaders_are_empty() {
        let state = commit_log::ClusterState {
            term: 2,
            leader: String::new(),
            ..Default::default()
        };
        let same_term = commit_log::TermState {
            term: 2,
            leader: String::new(),
            updated_at: 0,
        };
        assert_eq!(leader_display(&state, &same_term), "(none)");
    }

    #[test]
    fn peer_view_freshness_window_and_invalid_address_probe() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let fresh_peer = PeerView {
            node_id: "peer-1".into(),
            address: "invalid-address-without-port".into(),
            trust_score: 0.95,
            last_seen_secs: now,
        };
        assert!(fresh_peer.fresh());
        assert!(!fresh_peer.probe());

        let unseen_peer = PeerView {
            node_id: "peer-0".into(),
            address: "invalid-address".into(),
            trust_score: 0.5,
            last_seen_secs: 0,
        };
        assert!(!unseen_peer.fresh());
    }

    #[test]
    fn peer_view_marks_timestamp_beyond_staleness_window_as_stale() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(100);
        let stale_peer = PeerView {
            node_id: "peer-stale".into(),
            address: "invalid-address".into(),
            trust_score: 0.8,
            last_seen_secs: now.saturating_sub(PEER_STALE_SECS + 5),
        };
        assert!(!stale_peer.fresh());
    }
}

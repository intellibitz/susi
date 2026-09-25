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
    /// Remove stray heavyweight files flagged in `~/.susi/bin` and
    /// report reclaimed space
    Clean,
}

pub fn execute(action: Option<OsCommands>, top_json: bool, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(OsCommands::Status { json: false }) {
        OsCommands::Status { json } => status(json || top_json),
        OsCommands::Clean => clean(),
    }
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

fn status(json: bool) -> Result<()> {
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
        let daemon =
            susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir());
        let body = serde_json::json!({
            "node_id": susi_config::cluster_key::wire_node_id(),
            "evicted": evicted,
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
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    println!("SUSI OS — substrate status");
    println!("node:        {}", susi_config::cluster_key::wire_node_id());
    // Key epoch: the current key's fingerprint prefix, plus any staged
    // next-epoch key awaiting its committed rekey record — operators
    // comparing `susi os` across nodes can see rotation drift at a
    // glance without the key itself ever being printed.
    let key_epoch = susi_config::cluster_key::cluster_key()
        .map(|k| susi_config::cluster_key::key_fingerprint(&k));
    let staged_epoch = susi_config::cluster_key::staged_key()
        .map(|k| susi_config::cluster_key::key_fingerprint(&k));
    if let Some(fp) = &key_epoch {
        println!(
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
    println!(
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
    println!("services:    {}/{} leaf services up", up, services.len());
    println!(
        "peers:       {} verified cluster member(s){}",
        peers.len(),
        if banned_count > 0 {
            format!(", {banned_count} banned")
        } else {
            String::new()
        }
    );
    if evicted {
        println!("cluster:     EVICTED — removed by a committed member_remove; standing down until a committed unban");
    }
    for warning in credential_warnings()
        .into_iter()
        .chain(stray_bin_warnings())
    {
        println!("warning:     {warning}");
    }
    let daemon = susi_daemon::SusiDaemon::find_running_daemon(&susi_paths::SusiDirs::config_dir());
    println!(
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
        println!(
            "endpoints:   {} (+a2a-udp :{})",
            fields.join(" · "),
            udp_port()
        );
    }
    if let Some((avail, total)) = disk_free(&susi_paths::SusiDirs::substrate_home()) {
        println!(
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
        println!("footprint:   {}", top.join(" · "));
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
/// gigabytes. Flagged, never auto-deleted.
fn stray_bin_warnings() -> Vec<String> {
    const STRAY_WARN_BYTES: u64 = 256 * 1024 * 1024;
    let bin_dir = susi_paths::SusiDirs::substrate_home().join("bin");
    let mut warnings = Vec::new();
    let Ok(read) = std::fs::read_dir(&bin_dir) else {
        return warnings;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), "susi" | "susi.exe" | "lib") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() && meta.len() >= STRAY_WARN_BYTES {
            warnings.push(format!(
                "stray {name} ({}) in {} — `susi os clean` removes it",
                human_bytes(meta.len()),
                bin_dir.display()
            ));
        }
    }
    // Pre-cap rotated metrics residue: the sink rotates one generation
    // at 64MiB, so a `.1` larger than that can only be residue.
    if let Some((path, len)) = susi_error::oversized_rotated_metrics() {
        warnings.push(format!(
            "oversized rotated metrics {} ({}) — `susi os clean` removes it",
            path.display(),
            human_bytes(len)
        ));
    }
    // Same residue class: the flat audit.log is dead once dated rotation
    // files exist and are newer (the fallback sink would be newer instead).
    if let Some((path, len)) = susi_error::stale_flat_audit_log() {
        warnings.push(format!(
            "stale pre-rotation audit log {} ({}) — `susi os clean` removes it",
            path.display(),
            human_bytes(len)
        ));
    }
    warnings
}

/// Remove the same files `stray_bin_warnings` flags — stray binaries in
/// `~/.susi/bin` at or above the warn threshold. Only files the warning
/// predicate already names are touched; the live binary and `lib/` are
/// never removed.
fn clean() -> Result<()> {
    const STRAY_WARN_BYTES: u64 = 256 * 1024 * 1024;
    let bin_dir = susi_paths::SusiDirs::substrate_home().join("bin");
    let mut reclaimed = 0u64;
    let mut removed = 0usize;
    // Same predicate as the warning: oversized rotated metrics residue —
    // independent of the bin sweep so a missing bin dir cannot skip it.
    if let Some((path, len)) = susi_error::oversized_rotated_metrics() {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("removed {} ({})", path.display(), human_bytes(len));
                reclaimed += len;
                removed += 1;
            }
            Err(e) => eprintln!("could not remove {}: {e}", path.display()),
        }
    }
    if let Some((path, len)) = susi_error::stale_flat_audit_log() {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("removed {} ({})", path.display(), human_bytes(len));
                reclaimed += len;
                removed += 1;
            }
            Err(e) => eprintln!("could not remove {}: {e}", path.display()),
        }
    }
    let Ok(read) = std::fs::read_dir(&bin_dir) else {
        println!(
            "cleaned {removed} file(s), reclaimed {}",
            human_bytes(reclaimed)
        );
        return Ok(());
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), "susi" | "susi.exe" | "lib") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !(meta.is_file() && meta.len() >= STRAY_WARN_BYTES) {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("removed {} ({})", path.display(), human_bytes(meta.len()));
                reclaimed += meta.len();
                removed += 1;
            }
            Err(e) => eprintln!("could not remove {}: {e}", path.display()),
        }
    }
    println!(
        "cleaned {removed} file(s), reclaimed {}",
        human_bytes(reclaimed)
    );
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

/// Effective UDP discovery port (canonical base + `port_offset`).
fn udp_port() -> u16 {
    susi_config::SusiConfig::load_global()
        .map(|c| c.udp_discovery_port())
        .unwrap_or(susi_paths::ports::UDP_DISCOVERY)
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
    probes
        .iter()
        .map(|(name, port)| {
            let up = std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    *port,
                ),
                std::time::Duration::from_millis(150),
            )
            .is_ok();
            (*name, *port, up)
        })
        .collect()
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

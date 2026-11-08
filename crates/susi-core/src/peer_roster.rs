//! Read views of the verified cluster roster: the documents `susi peers
//! list --json` and `susi peers status --json` print and the MCP
//! `peers_list` / `peers_status` tools return (zero-UI plan B4 group 9).
//! One implementation for the CLI and the socket.

use crate::commit_log;
use serde_json::{json, Value};

/// A persisted peer is only "live" if it ponged within the swarm's staleness
/// window — the on-disk `is_active` flag is last-sweep state and can be hours
/// stale when the daemon isn't running.
pub const PEER_STALE_SECS: u64 = 30;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The verified-member registry (`peers.json`), strictly parsed.
pub fn load_registry() -> Result<Vec<Value>, String> {
    susi_config::cluster_key::read_json_rows_strict(
        &susi_paths::SusiDirs::config_dir().join("peers.json"),
    )
}

/// Members banned by a committed member_remove (`peers_banned.json`).
pub fn load_banned() -> Result<Vec<Value>, String> {
    susi_config::cluster_key::read_json_rows_strict(
        &susi_paths::SusiDirs::config_dir().join("peers_banned.json"),
    )
}

/// `yes` when the member was seen within [`PEER_STALE_SECS`], else `stale`.
pub fn liveness(n: &Value) -> &'static str {
    let last_seen = n
        .get("last_seen_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if last_seen != 0 && now_secs().saturating_sub(last_seen) <= PEER_STALE_SECS {
        "yes"
    } else {
        "stale"
    }
}

/// `susi peers list --json`: this node's identity, eviction state, the
/// elected leader, and every rostered and banned member.
pub fn list_report() -> Result<Value, String> {
    let nodes = load_registry()?;
    let banned = load_banned()?;
    let evicted = susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists();
    // The elected coordination head from term.json — marks who currently
    // claims leadership.
    let term = commit_log::load_term();
    let leader = term.leader.as_str();
    Ok(json!({
        "node_id": susi_config::cluster_key::wire_node_id(),
        "evicted": evicted,
        "leader": if leader.is_empty() { Value::Null } else { json!(leader) },
        "term": term.term,
        "peers": nodes.iter().map(|n| json!({
            "node_id": n.get("node_id"),
            "address": n.get("address"),
            "trust_score": n.get("trust_score"),
            "admission": n.get("admission"),
            "live": liveness(n) == "yes",
            "key_bound": n.get("pubkey").and_then(|v| v.as_str()).is_some_and(|p| !p.is_empty()),
            "key_attested": n.get("bind_sig").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
        })).collect::<Vec<_>>(),
        "banned": banned,
    }))
}

/// The local node's own consensus snapshot — same fields the
/// `cluster_status` tool returns, computed without a round trip.
pub fn local_cluster_status() -> Value {
    let term = commit_log::load_term();
    let records = commit_log::load();
    let roster = load_registry().unwrap_or_default();
    let bound = roster
        .iter()
        .filter(|p| {
            !p.get("pubkey")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .is_empty()
        })
        .count();
    let key_epoch = susi_config::cluster_key::cluster_key()
        .map(|k| susi_config::cluster_key::key_fingerprint(&k))
        .unwrap_or_default();
    json!({
        "node": susi_config::cluster_key::wire_node_id(),
        "term": term.term,
        "leader": term.leader,
        "records": records.len(),
        "key_epoch": key_epoch.get(..12).unwrap_or(&key_epoch),
        "roster": roster.len(),
        "bound": bound,
        "pubkey": susi_config::cluster_key::node_pubkey_hex()
            .map(|p| p.get(..12).unwrap_or(&p).to_string())
            .unwrap_or_default(),
        "self": true,
    })
}

/// `susi peers status --json`: every rostered member's `cluster_status`
/// plus this node's own row, sorted by node. Member calls ride the
/// signed-request channel; an unreachable member is an `error` row rather
/// than a blocked call.
pub fn status_rows() -> Result<Vec<Value>, String> {
    let roster = load_registry()?;
    let local = local_cluster_status();
    let targets: Vec<(String, String, bool, String, String)> = roster
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            let nid = n.get("node_id").and_then(|v| v.as_str())?.to_string();
            let addr = n.get("address").and_then(|v| v.as_str())?.to_string();
            let live = liveness(n) == "yes";
            let pubkey = n
                .get("pubkey")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let bind_sig = n
                .get("bind_sig")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((nid, addr, live, pubkey, bind_sig))
        })
        .collect();
    let bearer = susi_config::cluster_key::peer_bearer();
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for (nid, addr, live, pubkey, bind_sig) in &targets {
            // A loopback entry is this node itself; its row is added below.
            if addr.starts_with("127.") || addr.starts_with("::1") || addr.starts_with("localhost")
            {
                continue;
            }
            let bearer = bearer.clone();
            let results = &results;
            s.spawn(move || {
                let row = match crate::mcp_client::call_tool_with_timeout(
                    addr,
                    "cluster_status",
                    &json!({}),
                    bearer.as_deref(),
                    2,
                ) {
                    Ok(result) => result
                        .pointer("/content/0/text")
                        .and_then(|v| v.as_str())
                        .and_then(|t| serde_json::from_str::<Value>(t).ok())
                        .unwrap_or_else(|| json!({"node": nid, "error": "unparseable status"})),
                    Err(e) if *live && !pubkey.is_empty() && !bind_sig.is_empty() => {
                        // A key-bound member that discovery still sees (for
                        // example one whose OS firewall refuses inbound TCP)
                        // is live, but its own term, leader, ledger size and
                        // key epoch are unknown here. Report exactly that:
                        // copying this node's values into its row would
                        // claim agreement nobody measured and hide the
                        // divergence this view exists to show.
                        json!({
                            "node": nid,
                            "error": format!("live by discovery; status unavailable: {e}"),
                            "live_via_discovery": true,
                            "pubkey": pubkey.get(..12).unwrap_or(pubkey),
                            "self": false,
                        })
                    }
                    Err(e) => json!({"node": nid, "error": e}),
                };
                results.lock().unwrap_or_else(|e| e.into_inner()).push(row);
            });
        }
    });
    let mut rows = results.into_inner().unwrap_or_else(|e| e.into_inner());
    rows.push(local);
    rows.sort_by_key(|r| {
        r.get("node")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    });
    Ok(rows)
}

/// Operator warnings over `status_rows`: differing terms, leaders or key
/// epochs across reachable members.
pub fn divergence_warnings(rows: &[Value]) -> Vec<String> {
    let reachable = || rows.iter().filter(|r| r.get("error").is_none());
    let terms: std::collections::BTreeSet<u64> = reachable()
        .filter_map(|r| r.get("term").and_then(|v| v.as_u64()))
        .collect();
    let epochs: std::collections::BTreeSet<String> = reachable()
        .filter_map(|r| {
            r.get("key_epoch")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    // The split-brain signal: same term but different claimed leaders
    // means two nodes both believe they lead — term divergence alone
    // can't see it when the partition predates the next election.
    let leader_claims: std::collections::BTreeSet<String> = reachable()
        .filter_map(|r| {
            let l = r.get("leader").and_then(|v| v.as_str())?;
            (!l.is_empty()).then(|| l.to_string())
        })
        .collect();
    let mut warnings = Vec::new();
    if terms.len() > 1 {
        warnings.push(format!(
            "members report different terms ({terms:?}) — leadership may be partitioned"
        ));
    }
    if leader_claims.len() > 1 {
        warnings.push(format!(
            "members report different leaders ({leader_claims:?}) — possible split-brain; expect terms to diverge at the next election"
        ));
    }
    if epochs.len() > 1 {
        warnings.push(format!(
            "members report different key epochs ({epochs:?}) — a rekey is mid-flight or some members are stranded"
        ));
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_marks_recent_peers_live() {
        assert_eq!(liveness(&json!({"last_seen_secs": now_secs()})), "yes");
        assert_eq!(liveness(&json!({"last_seen_secs": 1})), "stale");
        assert_eq!(liveness(&json!({})), "stale");
    }

    #[test]
    fn divergence_warnings_name_split_brain_and_epochs() {
        let rows = vec![
            json!({"node": "a", "term": 3, "leader": "a", "key_epoch": "e1"}),
            json!({"node": "b", "term": 3, "leader": "b", "key_epoch": "e2"}),
            json!({"node": "c", "error": "offline", "term": 9}),
        ];
        let warnings = divergence_warnings(&rows);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("different leaders"));
        assert!(warnings[1].contains("key epochs"));
        assert!(divergence_warnings(&rows[..1]).is_empty());
    }

    #[test]
    fn standalone_node_lists_and_reports_only_itself() {
        let _lock = commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "susi-peer-roster-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);
        let listed = list_report().unwrap();
        assert_eq!(listed["peers"], json!([]));
        assert_eq!(listed["banned"], json!([]));
        let rows = status_rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["self"], true);
        drop(env);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn divergence_warnings_detects_term_divergence_and_ignores_unreachable_error_rows() {
        let rows = vec![
            json!({"node": "a", "term": 2, "leader": "a", "key_epoch": "e1"}),
            json!({"node": "b", "term": 5, "leader": "a", "key_epoch": "e1"}),
            json!({"node": "c", "error": "timeout", "term": 99, "leader": "z", "key_epoch": "e9"}),
        ];
        let warnings = divergence_warnings(&rows);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("different terms"));
    }

    #[test]
    fn local_cluster_status_reports_self_node_and_zero_bound_on_empty_roster() {
        let _lock = commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "susi-peer-roster-status-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);

        let status = local_cluster_status();
        assert_eq!(status["self"], true);
        assert_eq!(status["roster"], 0);
        assert_eq!(status["bound"], 0);
        assert!(!status["node"].as_str().unwrap().is_empty());

        drop(env);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn status_rows_reports_a_firewalled_live_member_without_inventing_its_state() {
        let _lock = commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "susi-peer-roster-fw-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);

        let peers = json!([
            {
                "node_id": "susi-node-peer-live",
                "address": "0.0.0.0:1",
                "admission": "explicit",
                "last_seen_secs": now_secs() + 3600,
                "pubkey": "aa".repeat(32),
                "bind_sig": "bb".repeat(64)
            },
            {
                "node_id": "susi-node-peer-stale",
                "address": "0.0.0.0:2",
                "admission": "explicit",
                "last_seen_secs": 1,
                "pubkey": "cc".repeat(32),
                "bind_sig": "dd".repeat(64)
            }
        ]);
        std::fs::write(
            home.join("peers.json"),
            serde_json::to_string_pretty(&peers).unwrap(),
        )
        .unwrap();

        let rows = status_rows().unwrap();
        let live_row = rows
            .iter()
            .find(|r| r["node"] == "susi-node-peer-live")
            .unwrap();
        // Live by discovery, but its status was not fetched: the row says so
        // and carries none of this node's term, leader, records or epoch.
        assert!(live_row.get("error").is_some(), "{live_row:?}");
        assert_eq!(live_row["live_via_discovery"], true);
        assert_eq!(live_row["self"], false);
        for field in ["term", "leader", "records", "key_epoch", "roster", "bound"] {
            assert!(
                live_row.get(field).is_none(),
                "{field} invented: {live_row:?}"
            );
        }

        let stale_row = rows
            .iter()
            .find(|r| r["node"] == "susi-node-peer-stale")
            .unwrap();
        assert!(stale_row.get("error").is_some(), "{stale_row:?}");
        assert!(
            stale_row.get("live_via_discovery").is_none(),
            "{stale_row:?}"
        );

        drop(env);
        let _ = std::fs::remove_dir_all(&home);
    }
}

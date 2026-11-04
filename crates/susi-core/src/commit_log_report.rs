//! Operator reports over the replicated quorum-commit ledger: the documents
//! `susi commits show|audit|replay|compact|sync` print (`--json` where the
//! command has it) and the MCP `commits_*` tools return (zero-UI plan B4
//! group 9). One implementation, so the CLI and the socket never disagree.

use crate::commit_log;
use serde_json::{json, Value};
use std::collections::HashSet;

/// Every record held: the live ledger plus the compaction archive.
fn all_records() -> Vec<commit_log::CommitRecord> {
    let mut records = commit_log::load();
    records.extend(commit_log::load_from(&commit_log::archive_path()));
    records
}

/// `susi commits audit --json`: cross-check the whole ledger (live +
/// archived) for the protocol's anomalies — the replay's own findings plus
/// verified commits stamped by a coordinator that was not the elected
/// leader. `ok` is true only when no anomaly was found.
pub fn audit_report() -> Value {
    // Full history = live ledger + compaction archive — an audit that
    // stops at the snapshot boundary would miss pre-snapshot
    // anomalies. `replay()` seeds from the snapshot so gap detection
    // stays honest on compacted ledgers.
    let records = all_records();
    let state = commit_log::replay();
    let snapshot = commit_log::load_snapshot();

    // Attribution/endorsement/subject-attestation anomalies are
    // folded by replay() itself — they land in state.anomalies.

    let mut anomalies: Vec<String> = state.anomalies.iter().map(|a| a.to_uppercase()).collect();
    for r in &records {
        // A verified record whose stamped leader differs from its own
        // coordinator was committed by a non-leader.
        if r.verify() && !r.leader.is_empty() && r.leader != r.coordinator {
            anomalies.push(format!(
                "NON-LEADER COMMIT  {} seq {} — elected leader was {} ({})",
                r.coordinator,
                r.seq,
                r.leader,
                r.epoch.get(..12).unwrap_or(&r.epoch)
            ));
        }
    }
    json!({
        "ok": anomalies.is_empty(),
        "records_checked": records.len(),
        "verified": state.decisions,
        "snapshot": snapshot.map(|s| json!({
            "coordinators": s.high_water.len(),
            "created_at": s.created_at,
        })),
        "anomalies": anomalies,
    })
}

/// `susi commits replay --json`: the cluster state the ledger replays to,
/// next to the persisted term record.
pub fn replay_report() -> Value {
    let state = commit_log::replay();
    let persisted = commit_log::load_term();
    json!({
        "term": state.term,
        "leader": state.leader,
        "persisted": { "term": persisted.term, "leader": persisted.leader },
        "decisions": state.decisions,
        "memberships": state.memberships,
        "roster": state.roster.iter().map(|(id, addr)| json!({
            "node_id": id, "address": addr,
        })).collect::<Vec<_>>(),
        "banned": state.banned.iter().map(|(id, addr)| json!({
            "node_id": id, "address": addr,
        })).collect::<Vec<_>>(),
        "coordinators": state.coordinators.iter().map(|(c, high)| json!({
            "coordinator": c, "high_water": high,
        })).collect::<Vec<_>>(),
        "anomalies": state.anomalies,
    })
}

/// `susi commits show <epoch> --json`: one record (live or archived) by
/// unique epoch prefix, with its signature, member-signature, subject
/// attestation and endorsement verdicts.
pub fn show_report(epoch_prefix: &str) -> Result<Value, String> {
    let records = all_records();
    let matches: Vec<_> = records
        .iter()
        .filter(|r| r.epoch.starts_with(epoch_prefix))
        .collect();
    let r = match matches.as_slice() {
        [] => {
            return Err(format!(
                "no commit record with epoch prefix `{epoch_prefix}`"
            ))
        }
        [one] => *one,
        _ => return Err(format!("epoch prefix `{epoch_prefix}` is ambiguous")),
    };
    // The coordinator's bound Ed25519 key, from the verified roster.
    let bound_pk = std::fs::read_to_string(susi_paths::SusiDirs::config_dir().join("peers.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
        .unwrap_or_default()
        .iter()
        .find_map(|p| {
            (p.get("node_id").and_then(|v| v.as_str()) == Some(r.coordinator.as_str()))
                .then(|| {
                    p.get("pubkey")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
                .flatten()
        });
    // Coordinator-attribution state: absent is the pre-PKI form; when
    // the roster binds the coordinator's key the sig must verify under
    // it — an INVALID here means the record would now be refused at
    // intake (forgery or a re-bound key).
    let member_sig = if r.member_sig.is_empty() {
        "absent".to_string()
    } else if r.coordinator == susi_config::cluster_key::wire_node_id() {
        match susi_config::cluster_key::node_pubkey_hex() {
            Some(pk)
                if susi_config::cluster_key::member_verify(&pk, &r.signature, &r.member_sig) =>
            {
                "valid (self)".to_string()
            }
            _ => "INVALID (self)".to_string(),
        }
    } else {
        match bound_pk {
            Some(pk)
                if susi_config::cluster_key::member_verify(&pk, &r.signature, &r.member_sig) =>
            {
                "valid".to_string()
            }
            Some(_) => "INVALID".to_string(),
            None => "present (coordinator key unbound)".to_string(),
        }
    };
    // Binding attestation: a member_add claiming a pubkey must carry
    // the subject's own signature — verify it here so an operator can
    // see whether a binding is subject-proven or would be refused now.
    let subject_sig = if r.member_pubkey.is_empty() {
        None
    } else {
        let member_id = r.member_delta().map(|(_, id, _)| id).unwrap_or_default();
        Some(if r.subject_sig.is_empty() {
            "ABSENT — record would be refused at intake today".to_string()
        } else if susi_config::cluster_key::verify_bind_attestation(
            member_id,
            &r.member_pubkey,
            &r.subject_sig,
        ) {
            "valid (subject-signed)".to_string()
        } else {
            "INVALID".to_string()
        })
    };
    let endorsements = if r.kind.is_empty() {
        None
    } else {
        let names: Vec<&str> = r.endorsements.iter().map(|e| e.node.as_str()).collect();
        let gate = if commit_log::endorsements_satisfied(r) {
            "satisfied"
        } else {
            "BELOW bound quorum"
        };
        Some(format!(
            "{} signer(s) [{}] — {}",
            r.endorsements.len(),
            names.join(", "),
            gate
        ))
    };
    Ok(json!({
        "record": r,
        "verified": r.verify(),
        "member_sig": member_sig,
        "subject_sig": subject_sig,
        "endorsements": endorsements,
    }))
}

/// `susi commits compact`: archive records the snapshot covers. Returns
/// the record counts before and after, the archived count and the archive
/// path.
pub fn compact_report() -> Result<Value, String> {
    let before = commit_log::load().len();
    let archived = commit_log::compact().map_err(|e| format!("ledger compaction failed: {e}"))?;
    let after = commit_log::load().len();
    Ok(json!({
        "before": before,
        "archived": archived,
        "live": after,
        "archive_path": commit_log::archive_path().display().to_string(),
    }))
}

/// One peer's anti-entropy outcome.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SyncOutcome {
    pub node_id: String,
    pub address: String,
    /// Human-readable one-line summary (what `susi commits sync` prints).
    pub line: String,
    pub pulled: usize,
    pub already_held: usize,
    pub rejected: usize,
    pub pushed: usize,
    pub push_rejected: usize,
}

/// `susi commits sync`: bidirectional anti-entropy with every verified
/// remote peer over its `commit_log_fetch` / `commit_records` tools.
/// Returns `{peers: [SyncOutcome], totals: {..}, note}`; `note` explains a
/// sync that had nothing to do.
pub fn sync_report() -> Value {
    let peers_path = susi_paths::SusiDirs::config_dir().join("peers.json");
    let Ok(text) = std::fs::read_to_string(&peers_path) else {
        return sync_summary(Vec::new(), Some("no verified peers — nothing to sync from"));
    };
    let Ok(nodes) = serde_json::from_str::<Vec<Value>>(&text) else {
        return sync_summary(
            Vec::new(),
            Some("peers.json is unreadable — nothing to sync from"),
        );
    };
    // Member-to-member calls authenticate with the cluster-derived
    // peer bearer — the per-host api_token only authenticates locally.
    let bearer = susi_config::cluster_key::peer_bearer();

    // Snapshot held records once — append() is idempotent on duplicates,
    // so "new" is determined by membership before the write, not the
    // write's outcome. The compaction archive counts as held: archived
    // records are still ours for dedup purposes.
    let held: HashSet<String> = commit_log::load()
        .iter()
        .chain(commit_log::load_from(&commit_log::archive_path()).iter())
        .filter_map(|r| serde_json::to_string(r).ok())
        .collect();

    // Explicit (cluster-key-verified) remote members only.
    let targets: Vec<(String, String)> = nodes
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            let addr = n.get("address").and_then(|a| a.as_str())?;
            // A loopback explicit entry is a self-edge (only possible from
            // a pre-guard `peers add` or a hand-edited peers.json) — syncing
            // with ourselves is a no-op that doubles as an MCP recursion.
            if addr.starts_with("127.") || addr.starts_with("::1") || addr.starts_with("localhost")
            {
                return None;
            }
            let id = n.get("node_id").and_then(|v| v.as_str()).unwrap_or(addr);
            Some((id.to_string(), addr.to_string()))
        })
        .collect();
    // Parallel per-peer sync — a dead member costs its connect timeout,
    // so a serial walk stalls the whole sync ~10s per unresponsive peer.
    let outcomes: Vec<SyncOutcome> = std::thread::scope(|s| {
        let handles: Vec<_> = targets
            .iter()
            .map(|(id, addr)| s.spawn(|| sync_peer(id, addr, &held, bearer.as_deref())))
            .collect();
        handles
            .into_iter()
            .zip(&targets)
            .map(|(h, (id, addr))| {
                h.join().unwrap_or_else(|_| SyncOutcome {
                    node_id: id.clone(),
                    address: addr.clone(),
                    line: "sync worker panicked".to_string(),
                    ..SyncOutcome::default()
                })
            })
            .collect()
    });
    sync_summary(outcomes, None)
}

fn sync_summary(peers: Vec<SyncOutcome>, note: Option<&str>) -> Value {
    let total = |f: fn(&SyncOutcome) -> usize| peers.iter().map(f).sum::<usize>();
    json!({
        "totals": {
            "pulled": total(|o| o.pulled),
            "already_held": total(|o| o.already_held),
            "rejected": total(|o| o.rejected),
            "pushed": total(|o| o.pushed),
            "push_rejected": total(|o| o.push_rejected),
        },
        "note": note,
        "peers": peers,
    })
}

/// One peer's sync: pull their records (paginated), verify + batch-append
/// locally, then push what they lack — batch intake first, per-record
/// `commit_record` as the pre-batch-peer fallback.
fn sync_peer(id: &str, addr: &str, held: &HashSet<String>, bearer: Option<&str>) -> SyncOutcome {
    let mut out = SyncOutcome {
        node_id: id.to_string(),
        address: addr.to_string(),
        ..SyncOutcome::default()
    };
    // Paginate until a short page — a ledger past the fetch cap must
    // still converge instead of truncating at the first 1000 records.
    // The full record set is retained for the push half of the
    // exchange (records we hold that the peer lacks).
    let mut offset = 0usize;
    let mut their_records: Vec<commit_log::CommitRecord> = Vec::new();
    let mut unreachable_note = String::new();
    loop {
        match crate::mcp_client::call_tool(
            addr,
            "commit_log_fetch",
            &json!({ "limit": 1000, "offset": offset }),
            bearer,
        ) {
            Ok(result) => {
                let Some(text) = result.pointer("/content/0/text").and_then(|t| t.as_str()) else {
                    unreachable_note = "no tool output".to_string();
                    break;
                };
                let Ok(records) = serde_json::from_str::<Vec<commit_log::CommitRecord>>(text)
                else {
                    unreachable_note = "malformed fetch payload".to_string();
                    break;
                };
                let page = records.len();
                offset += page;
                their_records.extend(records);
                if page < 1000 {
                    break;
                }
            }
            Err(e) => {
                unreachable_note = format!("unreachable — {e}");
                break;
            }
        }
    }
    if !unreachable_note.is_empty() {
        out.line = format!("{id} ({addr}): {unreachable_note}");
        return out;
    }
    let theirs: HashSet<String> = their_records
        .iter()
        .filter_map(|r| serde_json::to_string(r).ok())
        .collect();
    // Descending seq order: prior-epoch records (signed under the
    // retired cluster key) may only append when their seq+1
    // successor is held and names their epoch — landing successors
    // first lets a pre-rotation tail gap fill in one pass.
    let mut ordered: Vec<&commit_log::CommitRecord> = their_records.iter().collect();
    ordered.sort_by_key(|r| std::cmp::Reverse(r.seq));
    let mut to_apply: Vec<commit_log::CommitRecord> = Vec::new();
    for r in ordered {
        if !r.verify() {
            out.rejected += 1;
            continue;
        }
        // Raft's step-down on the pull path, but only from member
        // coordinators — a non-member's forged high term must not
        // adopt into term.json (it would freeze honest pushes).
        if commit_log::coordinator_known(r) {
            let _ = commit_log::check_term(r);
        }
        // Member records need coordinator authority — an evicted
        // node still holds cluster.key. Refused records stay
        // missing and converge once the coordinator is known.
        if !commit_log::member_coordinator_known(r) {
            out.rejected += 1;
            continue;
        }
        let key = serde_json::to_string(&r).unwrap_or_default();
        if held.contains(&key) {
            out.already_held += 1;
            continue;
        }
        to_apply.push((*r).clone());
    }
    // One lock + one ledger load for the whole pull — per-record
    // `append` re-read and re-locked the ledger every record.
    for outcome in commit_log::append_many(&to_apply) {
        match outcome {
            commit_log::AppendOutcome::Applied => out.pulled += 1,
            commit_log::AppendOutcome::Skipped => out.already_held += 1,
            commit_log::AppendOutcome::Refused => out.rejected += 1,
        }
    }

    // Symmetric repair: records we hold that the peer lacks are pushed
    // through `commit_record` — the receive path re-runs signature,
    // quorum, and term gates, so a rejected push is the protocol
    // working, not a sync failure. The archive is ours to serve: an
    // uncompacted peer missing below-floor history can only get it
    // from our cold storage.
    let to_push: Vec<commit_log::CommitRecord> = commit_log::load()
        .into_iter()
        .chain(commit_log::load_from(&commit_log::archive_path()))
        .filter(|r| {
            let key = serde_json::to_string(r).unwrap_or_default();
            !theirs.contains(&key)
        })
        .collect();
    // Batch intake first — one RPC for the whole repair on peers
    // running commit_records (the per-record loop below costs one
    // TCP+HTTP round trip per record). Pre-batch peers get the
    // fallback loop; the batch call failing for any other reason is
    // covered by the same path — refused records are the protocol's
    // gate working, not a sync failure.
    // The tool caps a batch at 1000 — a bigger delta takes the
    // per-record path (a peer that far behind is rare, and today's
    // per-record behavior is no worse than before).
    let batch_ok = if to_push.is_empty() || to_push.len() > 1000 {
        None
    } else {
        crate::mcp_client::call_tool(
            addr,
            "commit_records",
            &json!({ "records": to_push }),
            bearer,
        )
        .ok()
        .filter(|r| r.get("isError").and_then(|v| v.as_bool()) != Some(true))
    };
    if let Some(result) = &batch_ok {
        // The summary is JSON text: {applied, skipped, refused, repaired}.
        let counts = result
            .pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .and_then(|t| serde_json::from_str::<Value>(t).ok());
        let count = |key: &str| {
            counts
                .as_ref()
                .and_then(|c| c.get(key).and_then(|v| v.as_u64()))
                .unwrap_or(0) as usize
        };
        out.pushed += count("applied") + count("skipped");
        out.push_rejected += count("refused");
    }
    if batch_ok.is_none() {
        for r in &to_push {
            // The tool's args ARE the record — commit_record
            // deserializes the argument object directly.
            let Ok(args) = serde_json::to_value(r) else {
                continue;
            };
            match crate::mcp_client::call_tool(addr, "commit_record", &args, bearer) {
                // A completed RPC can still carry a tool-level rejection
                // (isError) — a stale-term or consistency refusal is the
                // protocol's gate working, not a sync failure.
                Ok(result) if result.get("isError").and_then(|v| v.as_bool()) == Some(true) => {
                    out.push_rejected += 1;
                }
                Ok(_) => out.pushed += 1,
                Err(_) => out.push_rejected += 1,
            }
        }
    }
    out.line = format!(
        "{id} ({addr}): +{} pulled, {} already held, {} rejected; \
         {} pushed, {} push-rejected",
        out.pulled, out.already_held, out.rejected, out.pushed, out.push_rejected
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_home(f: impl FnOnce()) {
        let _lock = commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "susi-commit-report-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);
        f();
        drop(env);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn empty_ledger_reports_are_clean() {
        with_home(|| {
            let audit = audit_report();
            assert_eq!(audit["ok"], true);
            assert_eq!(audit["records_checked"], 0);
            assert!(audit["anomalies"].as_array().unwrap().is_empty());

            let replay = replay_report();
            assert_eq!(replay["decisions"], 0);
            assert!(replay["roster"].as_array().unwrap().is_empty());

            let missing = show_report("deadbeef").unwrap_err();
            assert!(missing.contains("no commit record"), "{missing}");

            let compact = compact_report().unwrap();
            assert_eq!(compact["archived"], 0);

            let sync = sync_report();
            assert_eq!(sync["totals"]["pulled"], 0);
            assert!(sync["note"].as_str().unwrap().contains("nothing to sync"));
        });
    }

    #[test]
    fn sync_report_handles_unreadable_peers_json_and_filters_loopback_peers() {
        with_home(|| {
            let peers_path = susi_paths::SusiDirs::config_dir().join("peers.json");
            std::fs::create_dir_all(peers_path.parent().unwrap()).unwrap();
            std::fs::write(&peers_path, "{broken-json").unwrap();
            assert_eq!(
                sync_report()["note"],
                "peers.json is unreadable — nothing to sync from"
            );

            std::fs::write(
                &peers_path,
                serde_json::to_string(&vec![json!({
                    "admission": "explicit",
                    "address": "127.0.0.1:9090",
                    "node_id": "self-node"
                })])
                .unwrap(),
            )
            .unwrap();
            assert!(sync_report()["peers"].as_array().unwrap().is_empty());
        });
    }

    #[test]
    fn show_report_returns_record_for_unique_epoch_and_rejects_ambiguous_empty_prefix() {
        with_home(|| {
            let r1 = commit_log::CommitRecord::seal(commit_log::CommitInput {
                coordinator: "n1",
                leader: "n1",
                electorate: vec!["n1".into()],
                tally: 1,
                quorum_threshold: 1,
                value: "v1",
            })
            .unwrap();
            commit_log::append(&r1).unwrap();

            let r2 = commit_log::CommitRecord::seal(commit_log::CommitInput {
                coordinator: "n1",
                leader: "n1",
                electorate: vec!["n1".into(), "n2".into()],
                tally: 2,
                quorum_threshold: 2,
                value: "v2",
            })
            .unwrap();
            commit_log::append(&r2).unwrap();

            let shown = show_report(&r1.epoch).unwrap();
            assert_eq!(shown["verified"], true);
            assert!(show_report("").unwrap_err().contains("ambiguous"));
        });
    }

    #[test]
    fn audit_and_replay_reports_reflect_committed_records() {
        with_home(|| {
            let rec = commit_log::CommitRecord::seal(commit_log::CommitInput {
                coordinator: "n1",
                leader: "n1",
                electorate: vec!["n1".into()],
                tally: 1,
                quorum_threshold: 1,
                value: "decision-1",
            })
            .unwrap();
            commit_log::append(&rec).unwrap();

            let audit = audit_report();
            assert!(audit["records_checked"].as_u64().unwrap() >= 1);
            let replay = replay_report();
            assert!(replay["decisions"].as_u64().unwrap() >= 1);
        });
    }
}

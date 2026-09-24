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
use susi_core::commit_log;

#[derive(Debug, Subcommand)]
pub enum PeersCommands {
    /// List verified peers and banned members (default)
    List {
        /// Emit machine-readable JSON instead of the table view
        #[arg(long)]
        json: bool,
    },
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
    /// Probe a peer without persisting anything: the same signed-ping →
    /// signed-pong handshake `peers add` performs, reporting only the
    /// responder's attested identity and its standing in this node's
    /// roster. The non-destructive way to test reachability and
    /// cluster.key compatibility before (or instead of) `peers add`.
    Probe {
        /// IP or hostname of the peer daemon to probe (may include :port)
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
    /// Rotate the cluster key across every verified member — the only
    /// way to truly revoke an evicted member's cluster.key. Two-phase:
    /// a `cluster_rekey` record commits WHICH key members stage; a
    /// `cluster_rekey_activate` record then triggers rotation. If any
    /// member fails to stage, the rotation aborts safely (nothing
    /// activates anywhere) unless `--force` is given. Members that
    /// don't acknowledge activation are cryptographically stranded.
    Rekey {
        /// Proceed to activation even when some members failed to stage
        /// — those members will be stranded and must be re-provisioned
        /// with the new key manually.
        #[arg(long)]
        force: bool,
    },
}

pub fn execute(action: Option<PeersCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(PeersCommands::List { json: false }) {
        PeersCommands::List { json } => list(json),
        PeersCommands::Add { host, port } => add(&host, port),
        PeersCommands::Probe { host, port } => probe(&host, port),
        PeersCommands::Remove { peer } => remove(&peer),
        PeersCommands::Unban { peer } => unban(&peer),
        PeersCommands::Rekey { force } => rekey(force),
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
    // An evicted node can still seal member records locally, but no
    // member will accept them — coordinator authority requires current
    // explicit membership. Say so instead of reporting false pushes.
    if susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists()
    {
        eprintln!(
            "note: this node is evicted — member records it seals carry no cluster authority"
        );
    }
    let self_id = susi_config::cluster_key::wire_node_id();
    let term = commit_log::load_term();
    let roster = load_registry();
    // Raft's leader-proposed configuration-entry rule: only the claimed
    // leader seals roster deltas. A standalone node (empty roster, no
    // leader yet) leads itself — `peers add` is the bootstrap path and
    // must not deadlock waiting for an election that needs a peer.
    let we_lead = term.leader == self_id || (term.leader.is_empty() && roster.is_empty());
    if !we_lead {
        if term.leader.is_empty() {
            eprintln!(
                "note: no elected leader yet — the swarm elects on a ~10s cadence; retry shortly"
            );
            return;
        }
        // Follower delegation: the elected leader seals every committed
        // roster delta — ask it to via member_propose rather than
        // sealing an unauthorized record locally.
        propose_member(&term.leader, kind, node_id, address, &roster);
        return;
    }
    // The roster as this node observed it at commit time — audit context
    // for who was a member when the delta was decided.
    let electorate: Vec<String> = roster
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            n.get("node_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    // Seal under our own leadership claim — the intake gate
    // (member_coordinator_known) refuses privileged records whose
    // coordinator observed a different leader.
    let Some(record) = commit_log::CommitRecord::seal_member(
        &self_id,
        &self_id,
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
    // Peer pushes carry the cluster-derived bearer — the per-host
    // api_token cannot authenticate on a remote node's NetGuard.
    let bearer = susi_config::cluster_key::peer_bearer();
    let Ok(args) = serde_json::to_value(&record) else {
        return;
    };
    // Push targets: every explicit peer — plus the subject itself for
    // remove/unban. The subject just left our roster, but it must learn
    // its own eviction promptly (the stand-down marker) instead of
    // waiting for its next pull sweep. A re-add's subject is already
    // back in the roster and covered by the peer list.
    let mut targets: Vec<String> = load_registry()
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            n.get("address")
                .and_then(|a| a.as_str())
                .map(str::to_string)
        })
        .collect();
    if matches!(
        kind,
        commit_log::KIND_MEMBER_REMOVE | commit_log::KIND_MEMBER_UNBAN
    ) && !targets.iter().any(|a| a == address)
    {
        targets.push(address.to_string());
    }
    // Parallel pushes: each call carries a ~10s connect timeout, so a
    // serial loop over a roster with dead members stalls N×10s. The
    // swarm's broadcast paths parallelize for the same reason.
    use rayon::prelude::*;
    let pushed = targets
        .par_iter()
        .filter(|addr| {
            // Same self-edge rule as `commits sync` — a loopback explicit
            // row is this node, and an MCP call to ourselves recurses.
            !(addr.starts_with("127.") || addr.starts_with("::1") || addr.starts_with("localhost"))
        })
        .filter(|addr| {
            match susi_core::mcp_client::call_tool(addr, "commit_record", &args, bearer.as_deref())
            {
                // A tool-level isError is a protocol refusal (stale term,
                // equivocation) — the gate working, not a push success.
                Ok(result) => result.get("isError").and_then(|v| v.as_bool()) != Some(true),
                Err(_) => false,
            }
        })
        .count();
    println!("membership delta committed to ledger; pushed to {pushed} peer(s)");
}

/// Follower-side `member_add` delegation: the elected leader seals
/// config changes, so a non-leader asks it via `member_propose`. The
/// subject was verified locally already — the proposal carries only
/// the attested `node_id@address`, never key material.
/// Follower-side delegation: the elected leader seals every committed
/// roster delta, so a non-leader asks it via `member_propose`. The
/// subject was resolved locally already — the proposal carries only
/// the attested `node_id@address` and the delta kind.
fn propose_member(
    leader: &str,
    kind: &str,
    node_id: &str,
    address: &str,
    roster: &[serde_json::Value],
) {
    let Some(leader_addr) = roster.iter().find_map(|n| {
        (n.get("node_id").and_then(|v| v.as_str()) == Some(leader))
            .then(|| {
                n.get("address")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .flatten()
    }) else {
        eprintln!(
            "note: this node is not the elected leader (leader: {leader}) and the leader \
             is not in the local roster — membership not committed; retry on the leader"
        );
        return;
    };
    let bearer = susi_config::cluster_key::peer_bearer();
    let args = serde_json::json!({
        "member": format!("{node_id}@{address}"),
        "kind": kind,
    });
    match susi_core::mcp_client::call_tool(&leader_addr, "member_propose", &args, bearer.as_deref())
    {
        Ok(result) if result.get("isError").and_then(|v| v.as_bool()) != Some(true) => {
            println!("{kind} committed by leader {leader}");
        }
        Ok(result) => {
            let detail = result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("refused")
                .to_string();
            eprintln!("note: leader {leader} refused the proposal — {detail}");
        }
        Err(e) => {
            eprintln!(
                "note: leader {leader} unreachable ({e}) — membership not committed; \
                 retry after the swarm re-elects (~30s staleness + election)"
            );
        }
    }
}

/// `susi peers rekey` — rotate cluster.key cluster-wide, two-phase.
///
/// Prepare: generate → fingerprint → seal `cluster_rekey` (signed
/// under the CURRENT key) → stage locally → append locally → push
/// {record, key} to each member's `cluster_rekey_stage` tool (each
/// stages + appends; nothing activates yet). If any member fails, the
/// rotation ABORTS here — no key anywhere has changed — unless
/// `--force`.
///
/// Commit: seal `cluster_rekey_activate` → push to each member's
/// `cluster_rekey_commit` tool (each appends → apply activates) →
/// append locally LAST so all pushes still authenticate under the old
/// derived bearer. Members unreachable at commit-time are stranded;
/// members that never staged are stranded by design.
fn rekey(force: bool) -> Result<()> {
    use susi_core::commit_log;
    if susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists()
    {
        bail!("this node is evicted — a rekey it seals carries no cluster authority");
    }
    // Epoch changes are leader-proposed like every other config change:
    // the intake gate refuses rekey records whose sealer observed a
    // different leader, so a follower-sealed rotation could never
    // commit — refuse here instead of stranding members on a key the
    // ledger never authorized.
    let self_id = susi_config::cluster_key::wire_node_id();
    let term = commit_log::load_term();
    let roster_now = load_registry();
    let we_lead = term.leader == self_id || (term.leader.is_empty() && roster_now.is_empty());
    if !we_lead {
        bail!(
            "this node is not the elected leader (leader: {}) — run `susi peers rekey` on the leader",
            if term.leader.is_empty() {
                "none yet — retry after the swarm elects"
            } else {
                &term.leader
            }
        );
    }
    let Some(new_key) = susi_config::cluster_key::generate_key() else {
        bail!("could not generate a new cluster key (getrandom failed)");
    };
    let fingerprint = susi_config::cluster_key::key_fingerprint(&new_key);
    let electorate: Vec<String> = load_registry()
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            n.get("node_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    // Seal under our own leadership claim — the intake gate refuses
    // privileged records whose coordinator observed a different leader.
    let Some(record) =
        commit_log::CommitRecord::seal_rekey(&self_id, &self_id, &fingerprint, electorate.clone())
    else {
        bail!("no cluster.key — cannot seal a rekey record");
    };
    // Stage locally, then commit the prepare-phase record to our own
    // ledger — it documents which key the cluster agreed to stage and
    // chains the activate record that follows. No activation yet.
    if !susi_config::cluster_key::stage_key(&new_key) {
        bail!("failed to stage cluster.key.next — refusing to rekey");
    }
    if let Err(e) = commit_log::append(&record) {
        let _ = std::fs::remove_file(susi_paths::SusiDirs::config_dir().join("cluster.key.next"));
        bail!("local rekey record not appended — {e}");
    }
    let stage_args = serde_json::json!({
        "record": &record,
        "key_hex": hex::encode(new_key),
    });
    let bearer = susi_config::cluster_key::peer_bearer();
    let targets: Vec<String> = load_registry()
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            n.get("address")
                .and_then(|a| a.as_str())
                .map(str::to_string)
        })
        .filter(|a| !(a.starts_with("127.") || a.starts_with("::1") || a.starts_with("localhost")))
        .collect();
    // Parallel staging: each call carries a ~10s connect timeout —
    // serial staging over dead members stalls N×10s inside a
    // timing-sensitive rotation.
    use rayon::prelude::*;
    let outcomes: Vec<Result<(), String>> = targets
        .par_iter()
        .map(|addr| {
            match susi_core::mcp_client::call_tool(
                addr,
                "cluster_rekey_stage",
                &stage_args,
                bearer.as_deref(),
            ) {
                Ok(result) if result.get("isError").and_then(|v| v.as_bool()) != Some(true) => {
                    Ok(())
                }
                Ok(result) => {
                    let detail = result
                        .get("content")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                        .and_then(|c| c.get("text"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("refused")
                        .to_string();
                    Err(format!("{addr}: {detail}"))
                }
                Err(e) => Err(format!("{addr}: {e}")),
            }
        })
        .collect();
    let acked = outcomes.iter().filter(|o| o.is_ok()).count();
    let failed: Vec<String> = outcomes.into_iter().filter_map(|o| o.err()).collect();
    if !failed.is_empty() && !force {
        // Abort: staged files sit inert — the fingerprint match in
        // activation means they can never be swapped in by another
        // record, and a later rekey overwrites them. The cluster stays
        // on the current epoch everywhere.
        eprintln!(
            "rekey ABORTED — {acked}/{} member(s) staged; no key rotated anywhere",
            targets.len()
        );
        for f in &failed {
            eprintln!("  not staged: {f}");
        }
        eprintln!("retry when members are reachable, or re-run with --force to strand them");
        bail!("rekey aborted: incomplete member staging");
    }
    // Commit phase: the activate record is sealed under the CURRENT
    // (old) key — members verify it pre-rotation, then activate on
    // apply. Push it to members FIRST, then append locally: after our
    // own activation the derived bearer changes and pushes to
    // still-old members would stop authenticating.
    let Some(activate) =
        commit_log::CommitRecord::seal_rekey_activate(&self_id, &self_id, &fingerprint, electorate)
    else {
        bail!("could not seal the activate record");
    };
    let activate_args = serde_json::json!({ "record": &activate });
    let activate_results: Vec<bool> = targets
        .par_iter()
        .map(|addr| {
            match susi_core::mcp_client::call_tool(
                addr,
                "cluster_rekey_commit",
                &activate_args,
                bearer.as_deref(),
            ) {
                Ok(result) => result.get("isError").and_then(|v| v.as_bool()) != Some(true),
                Err(_) => false,
            }
        })
        .collect();
    let activated = activate_results.iter().filter(|ok| **ok).count();
    let stranded: Vec<String> = targets
        .iter()
        .zip(activate_results.iter())
        .filter(|(_, ok)| !**ok)
        .map(|(a, _)| a.clone())
        .collect();
    // Local activation LAST.
    if let Err(e) = commit_log::append(&activate) {
        eprintln!("warning: local activate record not appended — {e}; members already rotated");
        return Err(e.into());
    }
    println!(
        "cluster key rotated — epoch {} active; {activated}/{} member(s) rotated",
        &fingerprint[..16],
        targets.len()
    );
    for f in &failed {
        eprintln!("  not staged (forced): {f}");
    }
    for s in &stranded {
        eprintln!("  stranded-risk: {s}");
    }
    if !failed.is_empty() || !stranded.is_empty() {
        eprintln!(
            "recovery: unacknowledged members are cryptographically stranded — \
             copy the NEW ~/.susi/cluster.key to them (0600) to rejoin"
        );
    }
    Ok(())
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

fn list(json: bool) -> Result<()> {
    let nodes = load_registry();
    let banned = load_banned();
    let evicted = susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists();
    // The elected coordination head from term.json — marks the roster row
    // so an operator can see who currently claims leadership.
    let term = susi_core::commit_log::load_term();
    let leader = term.leader.as_str();
    if json {
        // Machine surface for agents — same fields the table shows.
        let body = serde_json::json!({
            "node_id": susi_config::cluster_key::wire_node_id(),
            "evicted": evicted,
            "leader": if leader.is_empty() { serde_json::Value::Null } else { serde_json::json!(leader) },
            "term": term.term,
            "peers": nodes.iter().map(|n| serde_json::json!({
                "node_id": n.get("node_id"),
                "address": n.get("address"),
                "trust_score": n.get("trust_score"),
                "admission": n.get("admission"),
                "live": liveness(n) == "yes",
            })).collect::<Vec<_>>(),
            "banned": banned,
        });
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
        "{:<22} {:<22} {:<7} {:<8} LIVE",
        "PEER", "ADDRESS", "TRUST", "ADMISSION"
    );
    for n in &nodes {
        let id = n.get("node_id").and_then(|v| v.as_str()).unwrap_or("?");
        let id = if id == leader {
            format!("{} *", id)
        } else {
            id.to_string()
        };
        println!(
            "{:<22} {:<22} {:<7.2} {:<8} {}",
            id,
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

/// Config changes serialize through the elected leader (Raft's
/// leader-proposed config-entry rule). A node with a known leader that
/// isn't itself must not mutate `peers.json`/`peers_banned.json`
/// locally — the ledger would never carry the delta and this node's
/// roster would silently diverge. Standalone nodes lead themselves.
/// Follower delegation check: returns `true` when the delta was handed
/// to the elected leader (or no leader exists yet) and the caller must
/// return WITHOUT mutating local roster/ban files — the committed
/// record's apply is what updates them, on every node including this
/// one. `false` means this node leads (or stands alone) and proceeds
/// with the local commit path.
fn delegate_membership(kind: &str, node_id: &str, address: &str) -> bool {
    let self_id = susi_config::cluster_key::wire_node_id();
    let term = commit_log::load_term();
    let we_lead = term.leader == self_id || (term.leader.is_empty() && load_registry().is_empty());
    if we_lead {
        return false;
    }
    if term.leader.is_empty() {
        eprintln!(
            "note: no elected leader yet — the swarm elects on a ~10s cadence; retry shortly"
        );
        return true;
    }
    propose_member(&term.leader, kind, node_id, address, &load_registry());
    true
}

fn remove(peer: &str) -> Result<()> {
    // Held from the initial read through both writes — the lock drops
    // before commit_membership pushes (its receiver-side apply takes
    // the same lock).
    let _peers_lock =
        susi_core::commit_log::FileLock::acquire(&susi_paths::SusiDirs::config_dir(), "peers");
    let nodes = load_registry();
    // An eviction is replicated cluster-wide — a broad prefix must not
    // wipe the membership in one shot. Exact id/address always works; a
    // prefix only resolves when it names exactly one member.
    let matches: Vec<&serde_json::Value> = nodes
        .iter()
        .filter(|n| {
            let id = n.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
            let addr = n.get("address").and_then(|v| v.as_str()).unwrap_or("");
            id == peer || addr.starts_with(peer) || id.starts_with(peer)
        })
        .collect();
    if matches.is_empty() {
        bail!("no verified peer matching `{peer}`");
    }
    if matches.len() > 1 {
        let names: Vec<String> = matches
            .iter()
            .map(|n| {
                format!(
                    "{} ({})",
                    n.get("node_id").and_then(|v| v.as_str()).unwrap_or("?"),
                    n.get("address").and_then(|v| v.as_str()).unwrap_or("?")
                )
            })
            .collect();
        bail!(
            "`{peer}` matches {} members — refuse to mass-evict: {}",
            matches.len(),
            names.join(", ")
        );
    }
    // Follower path: the elected leader seals every committed roster
    // delta — delegate before any local mutation, or this node's files
    // would diverge from the ledger's applied state.
    let (subject_id, subject_addr) = {
        let m = matches[0];
        (
            m.get("node_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            m.get("address")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
        )
    };
    if delegate_membership(commit_log::KIND_MEMBER_REMOVE, &subject_id, &subject_addr) {
        return Ok(());
    }
    let evicted: Vec<serde_json::Value> = vec![matches[0].clone()];
    let kept: Vec<_> = nodes
        .iter()
        .filter(|n| **n != evicted[0])
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
    }
    let bpath = banned_path();
    let btmp = bpath.with_extension("json.tmp");
    std::fs::write(&btmp, serde_json::to_string_pretty(&banned)?)?;
    std::fs::rename(&btmp, &bpath)?;
    drop(_peers_lock);
    for n in &evicted {
        let id = n.get("node_id").and_then(|v| v.as_str()).unwrap_or("?");
        let addr = n.get("address").and_then(|v| v.as_str()).unwrap_or("?");
        println!("evicted + banned: {id} ({addr})");
        // Commit the eviction — receivers drop the member and record the
        // ban on append, so the eviction takes effect cluster-wide, not
        // just where the operator ran the command.
        commit_membership(susi_core::commit_log::KIND_MEMBER_REMOVE, id, addr);
    }
    println!(
        "{} verified member(s) remain; {} banned",
        kept.len(),
        banned.len()
    );
    Ok(())
}

/// The signed-ping → signed-pong handshake `peers add` and `peers
/// probe` share, directed at one host instead of LAN broadcast. A peer
/// that can't produce a cluster-key-signed pong echoing our nonce is
/// never trusted — admission stays cryptographic, not asserted.
/// Returns the verified pong fields plus the responder's source IP.
fn handshake(
    host: &str,
    port: Option<u16>,
) -> Result<(susi_config::cluster_key::VerifiedPong, std::net::IpAddr)> {
    // `host` may carry an inline :port — split it so `10.0.0.4:9092`
    // works as naturally as `--port 9092`.
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
    // the handshake falls back instead of dead-ending on version skew.
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
            let Some(pong) = susi_config::cluster_key::verify_signed_pong(&msg, &nonce) else {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                continue;
            };
            // Self-edge guards — adding or probing ourselves is never a
            // remote peer. Three routes, one refusal: a loopback responder
            // is this host's own daemon (only one process binds the
            // discovery port per host); a responder at any of our own
            // interface addresses is likewise ourselves — UDP to our LAN
            // IP loops back (the bind check can only succeed on local
            // addresses); and on the identity-era wire format the
            // attested node_id is the strongest check of all.
            if src.ip().is_loopback()
                || std::net::TcpListener::bind((src.ip(), 0)).is_ok()
                || pong.0 == susi_config::cluster_key::wire_node_id()
            {
                bail!(
                    "{} answered from {} — that is this node's own daemon; \
                 this command needs a remote host",
                    pong.0,
                    src.ip()
                );
            }
            return Ok((pong, src.ip()));
        }
    }
    bail!(
        "no signed pong from {host}:{port} — peer unreachable, not a susi daemon, \
         or doesn't share this cluster.key"
    )
}

/// Join a peer by address: verify the responder cryptographically via
/// `handshake`, then persist it as an explicit member and commit the
/// admission to the replicated ledger.
fn add(host: &str, port: Option<u16>) -> Result<()> {
    let ((node_id, checksum, bloom_hex, roster), ip) = handshake(host, port)?;
    {
        let address = format!("{}:{}", ip, susi_paths::ports::GMCP_HTTP);
        // A banned member was operator-evicted — re-adding must be a
        // deliberate `peers unban` first, not an accidental re-add.
        let banned = load_banned();
        if banned.iter().any(|b| {
            b.get("node_id").and_then(|v| v.as_str()) == Some(node_id.as_str())
                || b.get("address").and_then(|v| v.as_str()) == Some(address.as_str())
        }) {
            bail!("{node_id} ({address}) is banned — `susi peers unban` it before re-adding");
        }
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

        // Serialize the roster RMW with the daemon's apply/scout
        // writers — released before commit_membership pushes over
        // the network (the push path takes the same lock on the
        // receiver side, and locally via append's apply).
        let _peers_lock =
            susi_core::commit_log::FileLock::acquire(&susi_paths::SusiDirs::config_dir(), "peers");
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
        drop(_peers_lock);
        // Commit the admission to the replicated ledger — every verified
        // peer applies the same roster delta on append, so membership
        // converges without a `peers add` on each node.
        commit_membership(susi_core::commit_log::KIND_MEMBER_ADD, &node_id, &address);
    }
    Ok(())
}

/// `peers probe` — the same verified handshake `peers add` performs,
/// without persisting anything. Reports the responder's attested
/// identity and its standing in this node's roster — the
/// non-destructive way to test reachability and cluster.key
/// compatibility before (or instead of) `peers add`.
fn probe(host: &str, port: Option<u16>) -> Result<()> {
    let ((node_id, checksum, bloom_hex, roster), ip) = handshake(host, port)?;
    let address = format!("{}:{}", ip, susi_paths::ports::GMCP_HTTP);
    let banned = load_banned().iter().any(|b| {
        b.get("node_id").and_then(|v| v.as_str()) == Some(node_id.as_str())
            || b.get("address").and_then(|v| v.as_str()) == Some(address.as_str())
    });
    let known = load_registry().iter().any(|n| {
        n.get("node_id").and_then(|v| v.as_str()) == Some(node_id.as_str())
            || n.get("address").and_then(|v| v.as_str()) == Some(address.as_str())
    });
    let standing = if banned {
        "BANNED — `susi peers unban` before `peers add`"
    } else if known {
        "explicit member"
    } else {
        "not a member — `susi peers add` to admit"
    };
    let bloom = if bloom_hex.is_empty() || bloom_hex.chars().all(|c| c == '0') {
        "none".to_string()
    } else {
        bloom_hex
    };
    println!("verified: {node_id} ({address})");
    println!("  registry checksum: {checksum}");
    println!("  capability bloom:  {bloom}");
    println!("  gossip roster:     {} member(s) advertised", roster.len());
    println!("  local standing:    {standing}");
    Ok(())
}

fn unban(peer: &str) -> Result<()> {
    let _peers_lock =
        susi_core::commit_log::FileLock::acquire(&susi_paths::SusiDirs::config_dir(), "peers");
    let banned = load_banned();
    // Same unambiguous-prefix rule as `peers remove` — each lifted ban
    // is replicated cluster-wide, so a broad prefix must not sweep
    // multiple evictions open in one shot.
    let matches: Vec<&serde_json::Value> = banned
        .iter()
        .filter(|b| {
            let id = b.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
            let addr = b.get("address").and_then(|v| v.as_str()).unwrap_or("");
            id == peer || addr == peer || id.starts_with(peer) || addr.starts_with(peer)
        })
        .collect();
    if matches.is_empty() {
        bail!("no banned peer matching `{peer}`");
    }
    if matches.len() > 1 {
        let names: Vec<String> = matches
            .iter()
            .map(|b| {
                format!(
                    "{} ({})",
                    b.get("node_id").and_then(|v| v.as_str()).unwrap_or("?"),
                    b.get("address").and_then(|v| v.as_str()).unwrap_or("?")
                )
            })
            .collect();
        bail!(
            "`{peer}` matches {} banned members — refuse to mass-unban: {}",
            matches.len(),
            names.join(", ")
        );
    }
    // Follower path — same early-delegation rule as `peers remove`:
    // no local mutation until the leader commits the delta.
    let (subject_id, subject_addr) = {
        let m = matches[0];
        (
            m.get("node_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            m.get("address")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
        )
    };
    if delegate_membership(commit_log::KIND_MEMBER_UNBAN, &subject_id, &subject_addr) {
        return Ok(());
    }
    let lifted: Vec<serde_json::Value> = vec![matches[0].clone()];
    let kept: Vec<_> = banned
        .iter()
        .filter(|b| **b != lifted[0])
        .cloned()
        .collect();
    let bpath = banned_path();
    let btmp = bpath.with_extension("json.tmp");
    std::fs::write(&btmp, serde_json::to_string_pretty(&kept)?)?;
    std::fs::rename(&btmp, &bpath)?;
    drop(_peers_lock);
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

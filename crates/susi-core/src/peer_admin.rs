//! Cluster membership administration: what `susi peers add|remove|unban|
//! rekey` do, and what the MCP `peers_add`, `peers_remove`, `peers_unban`
//! and `peers_rekey` tools do (zero-UI plan B4 group 9). One
//! implementation for both: the CLI prints the lines these functions emit
//! (`Info` to stdout, `Note` to stderr), the tools return them.
//!
//! Every change is a privileged roster delta: the elected leader seals it,
//! a bound quorum endorses it, and it is pushed to every verified member
//! over the signed channel. No key material is ever emitted as a line.

use crate::commit_log;
use crate::susi_config;
use std::path::PathBuf;

/// One line a membership command emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A result line (the CLI's stdout).
    Info(String),
    /// A note, warning or recovery hint (the CLI's stderr).
    Note(String),
}

/// Where a membership command's lines go as they happen.
pub type Out<'a> = &'a mut dyn FnMut(Line);

macro_rules! say {
    ($out:expr, $($arg:tt)*) => {
        $out(Line::Info(format!($($arg)*)))
    };
}

macro_rules! note {
    ($out:expr, $($arg:tt)*) => {
        $out(Line::Note(format!($($arg)*)))
    };
}

/// Run `f` over `items` on scoped threads (each call is a network round
/// trip with its own timeout), keeping the input order. A worker that panics yields `failed` for its item.
fn par_map<T: Sync, R: Send + Clone>(items: &[T], failed: R, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let f = &f;
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .iter()
            .map(|item| scope.spawn(move || f(item)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| failed.clone()))
            .collect()
    })
}

/// Collect a command's lines: `{lines, notes}` for a socket result.
pub fn collect<T>(
    run: impl FnOnce(Out<'_>) -> Result<T, String>,
) -> (Result<T, String>, Vec<String>, Vec<String>) {
    let mut info = Vec::new();
    let mut notes = Vec::new();
    let result = run(&mut |line| match line {
        Line::Info(text) => info.push(text),
        Line::Note(text) => notes.push(text),
    });
    (result, info, notes)
}

fn registry_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("peers.json")
}

fn banned_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("peers_banned.json")
}

/// Ban list and roster through the one strict reader the swarm uses:
/// absent is empty, damaged or unreadable is an error. The lenient copies
/// here read damage as empty, so `peers remove` rewrote the ban list with
/// only the new ban and `peers add` admitted banned members and rewrote
/// the roster from nothing.
fn load_banned() -> Result<Vec<serde_json::Value>, String> {
    crate::peer_roster::load_banned()
}

fn load_registry() -> Result<Vec<serde_json::Value>, String> {
    crate::peer_roster::load_registry()
}

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
#[allow(clippy::too_many_arguments)] // the flat member-delta fields plus the line sink
fn commit_membership(
    out: Out<'_>,
    kind: &str,
    node_id: &str,
    address: &str,
    member_pubkey: &str,
    subject_sig: &str,
) {
    // An evicted node can still seal member records locally, but no
    // member will accept them — coordinator authority requires current
    // explicit membership. Say so instead of reporting false pushes.
    if susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists()
    {
        note!(
            out,
            "note: this node is evicted — member records it seals carry no cluster authority"
        );
    }
    let self_id = susi_config::cluster_key::wire_node_id();
    let term = commit_log::load_term();
    let roster = match load_registry() {
        Ok(roster) => roster,
        Err(e) => {
            note!(out, "note: membership record not pushed — {e}");
            return;
        }
    };
    // Raft's leader-proposed configuration-entry rule: only the claimed
    // leader seals roster deltas. A standalone node (empty roster, no
    // leader yet) leads itself — `peers add` is the bootstrap path and
    // must not deadlock waiting for an election that needs a peer.
    let we_lead = term.leader == self_id || (term.leader.is_empty() && roster.is_empty());
    if !we_lead {
        if term.leader.is_empty() {
            note!(
                out,
                "note: no elected leader yet — the swarm elects on a ~10s cadence; retry shortly"
            );
            return;
        }
        // Follower delegation: the elected leader seals every committed
        // roster delta — ask it to via member_propose rather than
        // sealing an unauthorized record locally.
        propose_member(
            out,
            &term.leader,
            kind,
            node_id,
            address,
            &roster,
            member_pubkey,
            subject_sig,
        );
        return;
    }
    // The roster as this node observed it at commit time — audit context
    // for who was a member when the delta was decided. The coordinator
    // counts as an electorate member (it leads its own roster view) so a
    // bound-quorum endorsement gate doesn't need the removal subject's
    // own signature to evict it.
    let mut electorate: Vec<String> = roster
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            n.get("node_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    electorate.push(self_id.clone());
    // Seal under our own leadership claim — the intake gate
    // (member_coordinator_known) refuses privileged records whose
    // coordinator observed a different leader.
    let Some(mut record) = commit_log::CommitRecord::seal_member(
        &self_id,
        &self_id,
        kind,
        &format!("{node_id}@{address}"),
        electorate,
        member_pubkey,
    ) else {
        note!(
            out,
            "note: no cluster.key — membership change not committed to ledger"
        );
        return;
    };
    // The subject's own binding attestation from the v3 handshake —
    // required by intake for any member_add claiming a pubkey.
    record.subject_sig = subject_sig.to_string();
    // Joint-consensus: a bound-quorum of the electorate must endorse a
    // privileged record or every receiver refuses it. Collect the other
    // members' signatures (our member_sig is our own vote) before
    // committing — sealing short of quorum produces a record that can
    // never land, so fail here instead.
    let targets = endorse_targets(&roster, &self_id);
    record.endorsements = commit_log::collect_endorsements(&record, &targets);
    if !commit_log::endorsements_satisfied(&record) {
        // Name the members that didn't sign — "insufficient" alone
        // leaves the operator guessing who's offline vs refusing.
        let signed: std::collections::HashSet<&str> = record
            .endorsements
            .iter()
            .map(|e| e.node.as_str())
            .collect();
        let missing: Vec<&str> = targets
            .iter()
            .map(|(nid, _)| nid.as_str())
            .filter(|nid| !signed.contains(nid))
            .collect();
        note!(out,
            "note: insufficient endorsements for {kind} — a majority of the bound electorate must sign; non-signers: {}; the change was NOT committed",
            if missing.is_empty() {
                "none (signatures collected but quorum still short)".to_string()
            } else {
                missing.join(", ")
            }
        );
        return;
    }
    if let Err(e) = commit_log::append(&record) {
        note!(out, "note: membership record not appended — {e}");
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
    // Re-read: sealing applied the record to peers.json locally.
    let current = match load_registry() {
        Ok(current) => current,
        Err(e) => {
            note!(out, "note: membership record not pushed — {e}");
            return;
        }
    };
    let mut targets: Vec<String> = current
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
    // Same self-edge rule as `commits sync` — a loopback explicit row is
    // this node, and an MCP call to ourselves recurses.
    targets.retain(|addr| {
        !(addr.starts_with("127.") || addr.starts_with("::1") || addr.starts_with("localhost"))
    });
    let pushed = par_map(&targets, false, |addr| {
        match crate::mcp_client::call_tool(addr, "commit_record", &args, bearer.as_deref()) {
            // A tool-level isError is a protocol refusal (stale term,
            // equivocation) — the gate working, not a push success.
            Ok(result) => result.get("isError").and_then(|v| v.as_bool()) != Some(true),
            Err(_) => false,
        }
    })
    .into_iter()
    .filter(|ok| *ok)
    .count();
    say!(
        out,
        "membership delta committed to ledger; pushed to {pushed} peer(s)"
    );
}

/// `(node_id, gmcp_addr)` pairs of the roster's explicit members minus
/// `self_id` — the endorsement targets for a privileged record (the
/// coordinator endorses through its own member_sig; loopback rows are
/// self and unreachable by design).
fn endorse_targets(roster: &[serde_json::Value], self_id: &str) -> Vec<(String, String)> {
    roster
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            let nid = n.get("node_id").and_then(|v| v.as_str())?;
            let addr = n.get("address").and_then(|v| v.as_str())?;
            (nid != self_id
                && !addr.starts_with("127.")
                && !addr.starts_with("::1")
                && !addr.starts_with("localhost"))
            .then(|| (nid.to_string(), addr.to_string()))
        })
        .collect()
}

/// Follower-side delegation: the elected leader seals every committed
/// roster delta, so a non-leader asks it via `member_propose`. The
/// subject was resolved locally already — the proposal carries only
/// the attested `node_id@address` and the delta kind.
#[allow(clippy::too_many_arguments)]
// Same flat-delta shape as CommitRecord::seal_member — the proposal
// forwards exactly what the leader seals, plus the line sink.
fn propose_member(
    out: Out<'_>,
    leader: &str,
    kind: &str,
    node_id: &str,
    address: &str,
    roster: &[serde_json::Value],
    member_pubkey: &str,
    subject_sig: &str,
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
        note!(
            out,
            "note: this node is not the elected leader (leader: {leader}) and the leader \
             is not in the local roster — membership not committed; retry on the leader"
        );
        return;
    };
    let bearer = susi_config::cluster_key::peer_bearer();
    let args = serde_json::json!({
        "member": format!("{node_id}@{address}"),
        "kind": kind,
        "member_pubkey": member_pubkey,
        "subject_sig": subject_sig,
    });
    match crate::mcp_client::call_tool(&leader_addr, "member_propose", &args, bearer.as_deref()) {
        Ok(result) if result.get("isError").and_then(|v| v.as_bool()) != Some(true) => {
            say!(out, "{kind} committed by leader {leader}");
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
            note!(out, "note: leader {leader} refused the proposal — {detail}");
        }
        Err(e) => {
            note!(
                out,
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
pub fn rekey(out: Out<'_>, force: bool) -> Result<(), String> {
    if susi_paths::SusiDirs::config_dir()
        .join("cluster_evicted.json")
        .exists()
    {
        return Err(
            "this node is evicted — a rekey it seals carries no cluster authority".to_string(),
        );
    }
    // Epoch changes are leader-proposed like every other config change:
    // the intake gate refuses rekey records whose sealer observed a
    // different leader, so a follower-sealed rotation could never
    // commit — refuse here instead of stranding members on a key the
    // ledger never authorized.
    let self_id = susi_config::cluster_key::wire_node_id();
    let term = commit_log::load_term();
    let roster_now = load_registry()?;
    let we_lead = term.leader == self_id || (term.leader.is_empty() && roster_now.is_empty());
    if !we_lead {
        return Err(format!(
            "this node is not the elected leader (leader: {}) — run `susi peers rekey` on the leader",
            if term.leader.is_empty() {
                "none yet — retry after the swarm elects"
            } else {
                &term.leader
            }
        ));
    }
    let Some(new_key) = susi_config::cluster_key::generate_key() else {
        return Err("could not generate a new cluster key (getrandom failed)".to_string());
    };
    let fingerprint = susi_config::cluster_key::key_fingerprint(&new_key);
    let mut electorate: Vec<String> = load_registry()?
        .iter()
        .filter(|n| n.get("admission").and_then(|a| a.as_str()) == Some("explicit"))
        .filter_map(|n| {
            n.get("node_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    electorate.push(self_id.clone());
    // Seal under our own leadership claim — the intake gate refuses
    // privileged records whose coordinator observed a different leader.
    let Some(mut record) =
        commit_log::CommitRecord::seal_rekey(&self_id, &self_id, &fingerprint, electorate.clone())
    else {
        return Err("no cluster.key — cannot seal a rekey record".to_string());
    };
    // Joint-consensus: collect the bound electorate's endorsements for
    // the prepare record — the receivers' append gate requires the same
    // quorum, so a short-sealed record would be refused everywhere.
    let targets = endorse_targets(&roster_now, &self_id);
    record.endorsements = commit_log::collect_endorsements(&record, &targets);
    if !commit_log::endorsements_satisfied(&record) {
        let signed: std::collections::HashSet<&str> = record
            .endorsements
            .iter()
            .map(|e| e.node.as_str())
            .collect();
        let missing: Vec<&str> = targets
            .iter()
            .map(|(nid, _)| nid.as_str())
            .filter(|nid| !signed.contains(nid))
            .collect();
        return Err(format!(
            "insufficient endorsements for cluster_rekey — the bound electorate must agree to rotate; non-signers: {}",
            if missing.is_empty() {
                "none (signatures collected but quorum still short)".to_string()
            } else {
                missing.join(", ")
            }
        ));
    }
    // Stage locally, then commit the prepare-phase record to our own
    // ledger — it documents which key the cluster agreed to stage and
    // chains the activate record that follows. No activation yet.
    if !susi_config::cluster_key::stage_key(&new_key) {
        return Err("failed to stage cluster.key.next — refusing to rekey".to_string());
    }
    if let Err(e) = commit_log::append(&record) {
        let _ = std::fs::remove_file(susi_paths::SusiDirs::config_dir().join("cluster.key.next"));
        return Err(format!("local rekey record not appended — {e}"));
    }
    let stage_args = serde_json::json!({
        "record": &record,
        "key_hex": hex::encode(new_key),
    });
    let bearer = susi_config::cluster_key::peer_bearer();
    let targets: Vec<String> = load_registry()?
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
    let outcomes: Vec<Result<(), String>> =
        par_map(&targets, Err("staging worker failed".to_string()), |addr| {
            match crate::mcp_client::call_tool(
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
        });
    let acked = outcomes.iter().filter(|o| o.is_ok()).count();
    let failed: Vec<String> = outcomes.into_iter().filter_map(|o| o.err()).collect();
    if !failed.is_empty() && !force {
        // Abort: staged files sit inert — the fingerprint match in
        // activation means they can never be swapped in by another
        // record, and a later rekey overwrites them. The cluster stays
        // on the current epoch everywhere.
        note!(
            out,
            "rekey ABORTED — {acked}/{} member(s) staged; no key rotated anywhere",
            targets.len()
        );
        for f in &failed {
            note!(out, "  not staged: {f}");
        }
        note!(
            out,
            "retry when members are reachable, or re-run with --force to strand them"
        );
        return Err("rekey aborted: incomplete member staging".to_string());
    }
    // Commit phase: the activate record is sealed under the CURRENT
    // (old) key — members verify it pre-rotation, then activate on
    // apply. Push it to members FIRST, then append locally: after our
    // own activation the derived bearer changes and pushes to
    // still-old members would stop authenticating.
    let Some(mut activate) =
        commit_log::CommitRecord::seal_rekey_activate(&self_id, &self_id, &fingerprint, electorate)
    else {
        return Err("could not seal the activate record".to_string());
    };
    // The activate record is a privileged delta too — collect the bound
    // electorate's endorsements before pushing it anywhere.
    let act_targets = endorse_targets(&roster_now, &self_id);
    activate.endorsements = commit_log::collect_endorsements(&activate, &act_targets);
    if !commit_log::endorsements_satisfied(&activate) {
        let signed: std::collections::HashSet<&str> = activate
            .endorsements
            .iter()
            .map(|e| e.node.as_str())
            .collect();
        let missing: Vec<&str> = act_targets
            .iter()
            .map(|(nid, _)| nid.as_str())
            .filter(|nid| !signed.contains(nid))
            .collect();
        return Err(format!(
            "insufficient endorsements for cluster_rekey_activate — members stay on the current epoch; non-signers: {}",
            if missing.is_empty() {
                "none (signatures collected but quorum still short)".to_string()
            } else {
                missing.join(", ")
            }
        ));
    }
    let activate_args = serde_json::json!({ "record": &activate });
    let activate_results: Vec<bool> = par_map(&targets, false, |addr| {
        match crate::mcp_client::call_tool(
            addr,
            "cluster_rekey_commit",
            &activate_args,
            bearer.as_deref(),
        ) {
            Ok(result) => result.get("isError").and_then(|v| v.as_bool()) != Some(true),
            Err(_) => false,
        }
    });
    let activated = activate_results.iter().filter(|ok| **ok).count();
    let stranded: Vec<String> = targets
        .iter()
        .zip(activate_results.iter())
        .filter(|(_, ok)| !**ok)
        .map(|(a, _)| a.clone())
        .collect();
    // Local activation LAST.
    if let Err(e) = commit_log::append(&activate) {
        note!(
            out,
            "warning: local activate record not appended — {e}; members already rotated"
        );
        return Err(e.to_string());
    }
    say!(
        out,
        "cluster key rotated — epoch {} active; {activated}/{} member(s) rotated",
        &fingerprint[..16],
        targets.len()
    );
    for f in &failed {
        note!(out, "  not staged (forced): {f}");
    }
    for s in &stranded {
        note!(out, "  stranded-risk: {s}");
    }
    if !failed.is_empty() || !stranded.is_empty() {
        note!(
            out,
            "recovery: unacknowledged members are cryptographically stranded — \
             copy the NEW ~/.susi/cluster.key to them (0600) to rejoin"
        );
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
fn delegate_membership(out: Out<'_>, kind: &str, node_id: &str, address: &str) -> bool {
    let self_id = susi_config::cluster_key::wire_node_id();
    let term = commit_log::load_term();
    // A damaged roster cannot say who leads: stop before any local mutation.
    let roster = match load_registry() {
        Ok(roster) => roster,
        Err(e) => {
            note!(out, "error: {e} — repair it before changing membership");
            return true;
        }
    };
    let we_lead = term.leader == self_id || (term.leader.is_empty() && roster.is_empty());
    if we_lead {
        return false;
    }
    if term.leader.is_empty() {
        note!(
            out,
            "note: no elected leader yet — the swarm elects on a ~10s cadence; retry shortly"
        );
        return true;
    }
    propose_member(out, &term.leader, kind, node_id, address, &roster, "", "");
    true
}

pub fn remove(out: Out<'_>, peer: &str) -> Result<(), String> {
    // Held from the initial read through both writes — the lock drops
    // before commit_membership pushes (its receiver-side apply takes
    // the same lock).
    let _peers_lock =
        crate::commit_log::FileLock::acquire(&susi_paths::SusiDirs::config_dir(), "peers");
    let nodes = load_registry()?;
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
        return Err(format!("no verified peer matching `{peer}`"));
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
        return Err(format!(
            "`{peer}` matches {} members — refuse to mass-evict: {}",
            matches.len(),
            names.join(", ")
        ));
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
    if delegate_membership(
        out,
        commit_log::KIND_MEMBER_REMOVE,
        &subject_id,
        &subject_addr,
    ) {
        return Ok(());
    }
    let evicted: Vec<serde_json::Value> = vec![matches[0].clone()];
    let kept: Vec<_> = nodes
        .iter()
        .filter(|n| **n != evicted[0])
        .cloned()
        .collect();
    let path = registry_path();
    let body = serde_json::to_string_pretty(&kept).map_err(|e| e.to_string())?;
    susi_config::atomic_write_bytes(&path, body.as_bytes()).map_err(|e| e.to_string())?;
    // Record the ban — without it the evicted member re-verifies on its next
    // signed pong and silently rejoins. The swarm's signed-pong handler reads
    // peers_banned.json at admission.
    let mut banned = load_banned()?;
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
    susi_config::atomic_write_bytes(
        &banned_path(),
        serde_json::to_string_pretty(&banned)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    drop(_peers_lock);
    for n in &evicted {
        let id = n.get("node_id").and_then(|v| v.as_str()).unwrap_or("?");
        let addr = n.get("address").and_then(|v| v.as_str()).unwrap_or("?");
        say!(out, "evicted + banned: {id} ({addr})");
        // Commit the eviction — receivers drop the member and record the
        // ban on append, so the eviction takes effect cluster-wide, not
        // just where the operator ran the command.
        commit_membership(out, crate::commit_log::KIND_MEMBER_REMOVE, id, addr, "", "");
    }
    say!(
        out,
        "{} verified member(s) remain; {} banned",
        kept.len(),
        banned.len()
    );
    Ok(())
}

/// The signed-ping → signed-pong handshake `peers add` and `peers
/// probe` share (`crate::peer_probe::handshake`). A peer that can't
/// produce a cluster-key-signed pong echoing our nonce is never trusted —
/// admission stays cryptographic, not asserted.
fn handshake(
    host: &str,
    port: Option<u16>,
) -> Result<(susi_config::cluster_key::VerifiedPong, std::net::IpAddr), String> {
    crate::peer_probe::handshake(host, port)
}

/// Post-handshake endpoint proof (`crate::peer_probe::verify_endpoint`):
/// the attested node must serve MCP inside the sealed peer channel under
/// the same identity. Returns the peer's reported status.
fn verify_endpoint(
    node_id: &str,
    pubkey: &str,
    address: &str,
    direct: Vec<String>,
) -> Result<serde_json::Value, String> {
    crate::peer_probe::verify_endpoint(node_id, pubkey, address, direct)
}

/// Join a peer by address: verify the responder cryptographically via
/// `handshake`, then persist it as an explicit member and commit the
/// admission to the replicated ledger.
/// Best guess at a peer's GMCP-HTTP port when the handshake didn't announce
/// one: same-config clusters share `port_offset`, so our effective port is
/// the right default; canonical 9093 is the fallback.
fn peer_http_port() -> u16 {
    crate::peer_probe::peer_http_port()
}

pub fn add(out: Out<'_>, host: &str, port: Option<u16>) -> Result<(), String> {
    // The sealed peer channel first: it is how a member on another network
    // (a hub behind a forwarded port, a laptop elsewhere) joins, and it
    // proves the responder's identity whatever address its traffic comes
    // from. LAN discovery is the zero-config path on one network.
    let endpoint = crate::peer_probe::channel_endpoint(host, port);
    match crate::peer_probe::channel_identity(&endpoint) {
        Ok(peer) => add_over_channel(out, &endpoint, &peer),
        Err(channel_err) => add_over_lan(out, host, port).map_err(|lan_err| {
            format!(
                "{host} answered neither the sealed peer channel at {endpoint} ({channel_err}) nor LAN discovery ({lan_err})"
            )
        }),
    }
}

/// Admit the node a channel handshake proved at `endpoint`: prove it serves
/// under that identity, bind its key in the roster with `endpoint` as its
/// address hint, keep an uplink to it when this node cannot be dialed,
/// commit the admission, and ask it to admit this node in turn.
fn add_over_channel(
    out: Out<'_>,
    endpoint: &str,
    peer: &susi_config::peer_channel::PeerIdentity,
) -> Result<(), String> {
    let banned = load_banned()?;
    if banned.iter().any(|b| {
        b.get("node_id").and_then(|v| v.as_str()) == Some(peer.node_id.as_str())
            || b.get("pubkey")
                .and_then(|v| v.as_str())
                .is_some_and(|k| !k.is_empty() && k.eq_ignore_ascii_case(&peer.pubkey))
    }) {
        return Err(format!(
            "{} is banned — `susi peers unban` it before re-adding",
            peer.node_id
        ));
    }
    verify_endpoint(
        &peer.node_id,
        &peer.pubkey,
        endpoint,
        vec![endpoint.to_string()],
    )?;
    let node = serde_json::json!({
        "node_id": peer.node_id,
        "address": endpoint,
        "channel_endpoint": endpoint,
        "node_type": "PEER",
        "is_active": true,
        "capabilities": ["CORE"],
        "registry_checksum": 0,
        "latency_ms": 0,
        "uptime_secs": 0,
        "trust_score": 0.8,
        "capability_bloom": [],
        "admission": "explicit",
        "last_seen_secs": now_secs(),
        "pubkey": peer.pubkey,
        "bind_sig": peer.bind_sig,
    });
    {
        let _peers_lock =
            crate::commit_log::FileLock::acquire(&susi_paths::SusiDirs::config_dir(), "peers");
        let mut nodes = load_registry()?;
        nodes.retain(|n| {
            n.get("node_id").and_then(|v| v.as_str()) != Some(peer.node_id.as_str())
                && n.get("address").and_then(|v| v.as_str()) != Some(endpoint)
                && !n
                    .get("pubkey")
                    .and_then(|v| v.as_str())
                    .is_some_and(|k| k.eq_ignore_ascii_case(&peer.pubkey))
        });
        nodes.push(node);
        susi_config::atomic_write_bytes(
            &registry_path(),
            serde_json::to_string_pretty(&nodes)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    }
    say!(
        out,
        "verified + admitted over the sealed peer channel: {} ({endpoint}), node key {}…",
        peer.node_id,
        peer.pubkey.get(..16).unwrap_or(&peer.pubkey)
    );
    // A node with no channel listener of its own (a laptop behind NAT) is
    // reachable only over a link it keeps open: its daemon dials this peer
    // and holds the link, with keepalive and reconnect.
    if susi_config::peer_channel_link::local_listen_addr().is_none() {
        susi_config::peer_channel_link::remember_hub(&peer.node_id, &peer.pubkey, endpoint)?;
        say!(
            out,
            "uplink: this node's daemon keeps a sealed link open to {} so the cluster can reach it",
            peer.node_id
        );
    }
    commit_membership(
        out,
        crate::commit_log::KIND_MEMBER_ADD,
        &peer.node_id,
        endpoint,
        &peer.pubkey,
        &peer.bind_sig,
    );
    request_admission(out, peer, endpoint);
    Ok(())
}

/// The address hint this node gives when it asks to be admitted: its
/// public channel endpoint when it has one (`peer_channel_public`), else a
/// routing name under `.invalid` that never resolves — such a node is
/// reached by its identity over the link it keeps, never by dialing.
fn own_routing_address() -> String {
    let cfg = susi_config::SusiConfig::load_global().ok();
    let public = cfg
        .as_ref()
        .and_then(|c| c.get::<serde_json::Value>("peer_channel_public"))
        .and_then(|v| match v {
            serde_json::Value::String(one) => Some(one),
            serde_json::Value::Array(many) => many
                .into_iter()
                .find_map(|v| v.as_str().map(str::to_string)),
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Object(_) => None,
        })
        .filter(|a| !a.trim().is_empty());
    public.unwrap_or_else(|| {
        format!(
            "{}.invalid:{}",
            susi_config::cluster_key::wire_node_id(),
            peer_http_port()
        )
    })
}

/// Ask the peer just admitted to admit this node too (`member_propose`
/// with this node's own key and binding attestation). The peer seals the
/// change only as its cluster's leader, so this can be refused; the result
/// is reported either way and nothing local depends on it.
fn request_admission(out: Out<'_>, peer: &susi_config::peer_channel::PeerIdentity, endpoint: &str) {
    let self_id = susi_config::cluster_key::wire_node_id();
    let (Some(pubkey), Some(sig)) = (
        susi_config::cluster_key::node_pubkey_hex(),
        susi_config::cluster_key::bind_attestation(),
    ) else {
        note!(
            out,
            "note: no node.key here — cannot ask {} to admit this node",
            peer.node_id
        );
        return;
    };
    let self_addr = own_routing_address();
    let target = susi_config::peer_channel_link::Target {
        node_id: peer.node_id.clone(),
        pubkey: peer.pubkey.clone(),
        direct: vec![endpoint.to_string()],
    };
    let args = serde_json::json!({
        "name": "member_propose",
        "arguments": {
            "member": format!("{self_id}@{self_addr}"),
            "kind": crate::commit_log::KIND_MEMBER_ADD,
            "member_pubkey": pubkey,
            "subject_sig": sig,
        }
    });
    match crate::mcp_client::session_call_to(&target, endpoint, "tools/call", &args, 30) {
        Ok(result) if result.get("isError").and_then(|v| v.as_bool()) != Some(true) => {
            say!(
                out,
                "{} admitted this node ({self_id}@{self_addr})",
                peer.node_id
            );
        }
        Ok(result) => {
            let detail = result
                .pointer("/content/0/text")
                .and_then(|t| t.as_str())
                .unwrap_or("refused")
                .to_string();
            note!(
                out,
                "note: {} has not admitted this node yet — {detail}; run `susi peers add` for this node on {} (its leader), or retry once it has elected one",
                peer.node_id,
                peer.node_id
            );
        }
        Err(e) => {
            note!(
                out,
                "note: could not ask {} to admit this node — {e}",
                peer.node_id
            );
        }
    }
}

/// Admission over LAN discovery: the bound `SUSI_PING_SIG3` handshake
/// proves the responder holds the cluster key at its source address, then
/// the endpoint proof runs inside the sealed peer channel to the key the
/// pong attested (on the default channel port of that host, or through a
/// hub this node already uses).
fn add_over_lan(out: Out<'_>, host: &str, port: Option<u16>) -> Result<(), String> {
    let ((node_id, checksum, bloom_hex, roster, pubkey, bind_sig), ip) = handshake(host, port)?;
    {
        let address = format!("{}:{}", ip, peer_http_port());
        let channel_endpoint = format!("{ip}:{}", susi_config::peer_channel_link::DEFAULT_PORT);
        // The pong attested an id — now prove the claimed endpoint is
        // live, serving, and owned by that same id before any local
        // state changes.
        verify_endpoint(&node_id, &pubkey, &address, vec![channel_endpoint.clone()])?;
        // A banned member was operator-evicted — re-adding must be a
        // deliberate `peers unban` first, not an accidental re-add.
        let banned = load_banned()?;
        if banned.iter().any(|b| {
            b.get("node_id").and_then(|v| v.as_str()) == Some(node_id.as_str())
                || b.get("address").and_then(|v| v.as_str()) == Some(address.as_str())
        }) {
            return Err(format!(
                "{node_id} ({address}) is banned — `susi peers unban` it before re-adding"
            ));
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
            // The subject's own binding attestation (v3 pong) — persisted
            // so later `member_add` records and re-pushes carry proof the
            // key is the subject's claim, not ours.
            "pubkey": pubkey,
            "bind_sig": bind_sig,
            "channel_endpoint": channel_endpoint,
        });

        // Serialize the roster RMW with the daemon's apply/scout
        // writers — released before commit_membership pushes over
        // the network (the push path takes the same lock on the
        // receiver side, and locally via append's apply).
        let _peers_lock =
            crate::commit_log::FileLock::acquire(&susi_paths::SusiDirs::config_dir(), "peers");
        let mut nodes = load_registry()?;
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
        susi_config::atomic_write_bytes(
            &registry_path(),
            serde_json::to_string_pretty(&nodes)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        if learned > 0 {
            say!(out, "verified + admitted: {node_id} ({address}); learned {learned} roster entr(ies) via gossip");
        } else {
            say!(out, "verified + admitted: {node_id} ({address})");
        }
        drop(_peers_lock);
        // Commit the admission to the replicated ledger — every verified
        // peer applies the same roster delta on append, so membership
        // converges without a `peers add` on each node. The key binding
        // is propagated only when the subject attested it (v3 pong): a
        // pre-v3 peer joins unbound and binds on each member's own
        // handshake — a binding without subject_sig is refused at
        // intake, so claiming one here would just drop the record.
        let (commit_pubkey, commit_sig) = if bind_sig.is_empty() {
            ("", "")
        } else {
            (pubkey.as_str(), bind_sig.as_str())
        };
        commit_membership(
            out,
            crate::commit_log::KIND_MEMBER_ADD,
            &node_id,
            &address,
            commit_pubkey,
            commit_sig,
        );
    }
    Ok(())
}

pub fn unban(out: Out<'_>, peer: &str) -> Result<(), String> {
    let _peers_lock =
        crate::commit_log::FileLock::acquire(&susi_paths::SusiDirs::config_dir(), "peers");
    let banned = load_banned()?;
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
        return Err(format!("no banned peer matching `{peer}`"));
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
        return Err(format!(
            "`{peer}` matches {} banned members — refuse to mass-unban: {}",
            matches.len(),
            names.join(", ")
        ));
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
    if delegate_membership(
        out,
        commit_log::KIND_MEMBER_UNBAN,
        &subject_id,
        &subject_addr,
    ) {
        return Ok(());
    }
    let lifted: Vec<serde_json::Value> = vec![matches[0].clone()];
    let kept: Vec<_> = banned
        .iter()
        .filter(|b| **b != lifted[0])
        .cloned()
        .collect();
    susi_config::atomic_write_bytes(
        &banned_path(),
        serde_json::to_string_pretty(&kept)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    drop(_peers_lock);
    for b in &lifted {
        let id = b.get("node_id").and_then(|v| v.as_str()).unwrap_or("?");
        let addr = b.get("address").and_then(|v| v.as_str()).unwrap_or("?");
        // Replicate the unban — receivers banned this member via the
        // committed removal, so lifting it locally alone would leave
        // them refusing its handshakes forever.
        commit_membership(out, crate::commit_log::KIND_MEMBER_UNBAN, id, addr, "", "");
    }
    say!(
        out,
        "lifted ban on {} member(s); they may re-verify on next handshake",
        lifted.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_splits_results_from_notes() {
        let (result, info, notes) = collect(|out| {
            say!(out, "evicted + banned: {}", "n1");
            note!(out, "note: {}", "pushed to 0 peer(s)");
            Ok(7)
        });
        assert_eq!(result, Ok(7));
        assert_eq!(info, vec!["evicted + banned: n1".to_string()]);
        assert_eq!(notes, vec!["note: pushed to 0 peer(s)".to_string()]);
    }

    #[test]
    fn par_map_keeps_order_and_maps_every_item() {
        let doubled = par_map(&[1, 2, 3], 0, |n| n * 2);
        assert_eq!(doubled, vec![2, 4, 6]);
        assert!(par_map(&[] as &[u8], 0, |n| *n).is_empty());
    }

    #[test]
    fn a_standalone_node_refuses_unknown_peers_without_changing_the_roster() {
        let _lock = commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "susi-peer-admin-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);
        let (removed, lines, _) = collect(|out| remove(out, "no-such-node"));
        assert!(removed.unwrap_err().contains("no verified peer matching"));
        assert!(lines.is_empty());
        let (unbanned, _, _) = collect(|out| unban(out, "no-such-node"));
        assert!(unbanned.unwrap_err().contains("no banned peer matching"));
        assert!(!registry_path().exists(), "a refusal writes no roster");
        drop(env);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn endorse_targets_filters_self_loopback_and_non_explicit_rows() {
        let roster = vec![
            serde_json::json!({
                "admission": "explicit",
                "node_id": "self-node",
                "address": "10.0.0.1:9093"
            }),
            serde_json::json!({
                "admission": "explicit",
                "node_id": "loop-v4",
                "address": "127.0.0.1:9093"
            }),
            serde_json::json!({
                "admission": "explicit",
                "node_id": "loop-host",
                "address": "localhost:9093"
            }),
            serde_json::json!({
                "admission": "discovered",
                "node_id": "disc-1",
                "address": "10.0.0.2:9093"
            }),
            serde_json::json!({
                "admission": "explicit",
                "node_id": "peer-ok",
                "address": "10.0.0.3:9093"
            }),
        ];
        assert_eq!(
            endorse_targets(&roster, "self-node"),
            vec![("peer-ok".to_string(), "10.0.0.3:9093".to_string())]
        );
    }

    #[test]
    fn remove_and_unban_refuse_ambiguous_prefix_matching_multiple_members() {
        let _lock = commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "susi-peer-admin-ambig-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);

        let entries = serde_json::json!([
            {"node_id": "node-alpha", "address": "10.0.0.10:9093", "admission": "explicit"},
            {"node_id": "node-beta", "address": "10.0.0.11:9093", "admission": "explicit"}
        ]);
        std::fs::create_dir_all(registry_path().parent().unwrap()).unwrap();
        std::fs::write(registry_path(), serde_json::to_string(&entries).unwrap()).unwrap();
        std::fs::write(banned_path(), serde_json::to_string(&entries).unwrap()).unwrap();

        let (rem_res, _, _) = collect(|out| remove(out, "node-"));
        assert!(rem_res.unwrap_err().contains("refuse to mass-evict"));

        let (unban_res, _, _) = collect(|out| unban(out, "node-"));
        assert!(unban_res.unwrap_err().contains("refuse to mass-unban"));

        drop(env);
        let _ = std::fs::remove_dir_all(&home);
    }
}

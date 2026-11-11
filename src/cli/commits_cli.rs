//! `susi commits` — audit view of the replicated quorum-commit ledger
//! (`~/.susi/commit_log.jsonl`). Each record is signature-verified against
//! the local cluster key on display: a forged or tampered entry that
//! somehow landed in the file shows `VERIFIED no`, so the audit output
//! never silently trusts the ledger's contents. `susi commits audit`
//! cross-checks the whole ledger for the protocol's defined anomalies:
//! invalid signatures, per-coordinator sequence gaps, duplicate or
//! equivocating `(coordinator, seq)` slots, and commits stamped by a
//! coordinator that wasn't the elected leader.

use crate::anyhow::Result;
use std::path::Path;
pub use susi_cli_defs::commits::CommitsCommands;
use susi_core::commit_log;

pub fn execute(action: Option<CommitsCommands>, _workspace: &Path) -> Result<()> {
    match action.unwrap_or(CommitsCommands::List {
        limit: 20,
        coordinator: None,
        term: None,
        kind: None,
        json: false,
    }) {
        CommitsCommands::List {
            limit,
            coordinator,
            term,
            kind,
            json,
        } => list(limit, coordinator.as_deref(), term, kind.as_deref(), json),
        CommitsCommands::Show { epoch, json } => show(&epoch, json),
        CommitsCommands::Audit { strict, json } => audit(strict, json),
        CommitsCommands::Replay { json } => replay_view(json),
        CommitsCommands::Sync => sync(),
        CommitsCommands::Compact => compact(),
    }
}

/// Fold the live ledger into `commit_snapshot.json` + archive
/// pre-snapshot records. The retained per-coordinator anchors keep
/// chain linkage and seq derivation intact; `commit_log_fetch` keeps
/// serving the archive so peers can still repair across the boundary.
fn compact() -> Result<()> {
    let report =
        susi_core::commit_log_report::compact_report().map_err(crate::anyhow::Error::msg)?;
    if report["archived"].as_u64() == Some(0) {
        println!(
            "ledger already minimal ({} records) — nothing to compact",
            report["before"]
        );
    } else {
        println!(
            "compacted: {} records archived to {}, {} live (anchors + post-snapshot)",
            report["archived"],
            report["archive_path"].as_str().unwrap_or_default(),
            report["live"]
        );
    }
    Ok(())
}

/// Pull every record each verified peer holds, verify + append locally.
/// `commit_log::append` is idempotent — records already held are skipped
/// safely, and anything failing signature verification is rejected. The
/// report is `susi_core::commit_log_report`'s, the same document the MCP
/// `commits_sync` tool returns.
fn sync() -> Result<()> {
    let report = susi_core::commit_log_report::sync_report();
    if let Some(note) = report["note"].as_str() {
        println!("{note}");
        return Ok(());
    }
    for peer in report["peers"].as_array().into_iter().flatten() {
        println!("{}", peer["line"].as_str().unwrap_or_default());
    }
    let t = &report["totals"];
    println!(
        "sync: {} pulled, {} already held, {} rejected; \
         {} pushed, {} push-rejected",
        t["pulled"], t["already_held"], t["rejected"], t["pushed"], t["push_rejected"]
    );
    Ok(())
}

fn list(
    limit: usize,
    coordinator: Option<&str>,
    term: Option<u64>,
    kind: Option<&str>,
    json: bool,
) -> Result<()> {
    // "decision" is the display name for records with no kind field —
    // let the filter accept the same vocabulary the table prints.
    let records: Vec<_> = commit_log::load()
        .into_iter()
        .filter(|r| coordinator.is_none_or(|c| r.coordinator == c))
        .filter(|r| term.is_none_or(|t| r.term == t))
        .filter(|r| {
            kind.is_none_or(|k| {
                if r.kind.is_empty() {
                    k == "decision"
                } else {
                    r.kind == k
                }
            })
        })
        .collect();
    if json {
        let rows: Vec<serde_json::Value> = records
            .iter()
            .rev()
            .take(limit.min(500))
            .map(|r| {
                serde_json::json!({
                    "epoch": r.epoch,
                    "seq": r.seq,
                    "term": r.term,
                    "coordinator": r.coordinator,
                    "leader": r.leader,
                    "kind": if r.kind.is_empty() { "decision" } else { r.kind.as_str() },
                    "tally": r.tally,
                    "quorum": r.quorum_threshold,
                    "committed_at": r.committed_at,
                    "verified": match r.verify_key_epoch() {
                        Some(commit_log::KeyEpoch::Current) => "yes",
                        Some(commit_log::KeyEpoch::Prev) => "prev",
                        None => "no",
                    },
                    "subject": r
                        .member_delta()
                        .map(|(_, id, addr)| format!("{id}@{addr}"))
                        .or_else(|| {
                            r.rekey_fingerprint()
                                .map(|fp| format!("key:{}", fp.get(..12).unwrap_or(fp)))
                        }),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!(
        "{:<14} {:<5} {:<5} {:<18} {:<24} {:<7} {:<7} {:<12} {:<8} SUBJECT",
        "EPOCH",
        "SEQ",
        "TERM",
        "COORDINATOR",
        "KIND",
        "TALLY",
        "QUORUM",
        "COMMITTED_AT",
        "VERIFIED"
    );
    for r in records.iter().rev().take(limit.min(500)) {
        // Member rows show the delta's target — an audit of roster
        // history needs the who, not just the kind. Rekey rows show
        // the rotated key's fingerprint prefix — the epoch boundary's
        // identity.
        let subject = r
            .member_delta()
            .map(|(_, id, addr)| format!("{id}@{addr}"))
            .or_else(|| {
                r.rekey_fingerprint()
                    .map(|fp| format!("key:{}", fp.get(..12).unwrap_or(fp)))
            })
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<14} {:<5} {:<5} {:<18} {:<24} {:<7} {:<7} {:<12} {:<8} {}",
            r.epoch.get(..12).unwrap_or(&r.epoch),
            r.seq,
            r.term,
            r.coordinator,
            if r.kind.is_empty() {
                "decision"
            } else {
                r.kind.as_str()
            },
            r.tally,
            r.quorum_threshold,
            r.committed_at,
            match r.verify_key_epoch() {
                Some(commit_log::KeyEpoch::Current) => "yes",
                // Valid but signed under the retired key — pre-rotation
                // history, marked so an audit can tell the eras apart.
                Some(commit_log::KeyEpoch::Prev) => "prev",
                None => "NO",
            },
            subject
        );
    }
    Ok(())
}

fn show(epoch_prefix: &str, json: bool) -> Result<()> {
    let body = susi_core::commit_log_report::show_report(epoch_prefix)
        .map_err(crate::anyhow::Error::msg)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&body["record"])?);
    println!("verified: {}", body["verified"]);
    println!(
        "member_sig: {}",
        body["member_sig"].as_str().unwrap_or_default()
    );
    if let Some(att) = body["subject_sig"].as_str() {
        println!("subject_sig: {att}");
    }
    if let Some(e) = body["endorsements"].as_str() {
        println!("endorsements: {e}");
    }
    Ok(())
}

/// Cross-checks the whole ledger for the anomalies the protocol defines.
/// Structural checks (signature failures, sequence gaps, duplicates,
/// equivocation, term regressions) come from `commit_log::replay` — the
/// same fold that reconstructs consensus state, so audit and replay can
/// never disagree. Adds the audit-only non-leader-commit check:
/// `coordinator != leader` is legitimate for per-node missions, so it is
/// reported as an anomaly, not a violation.
fn audit(strict: bool, json: bool) -> Result<()> {
    let body = susi_core::commit_log_report::audit_report();
    let anomalies: Vec<&str> = body["anomalies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str())
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        if let Some(snap) = body["snapshot"].as_object() {
            println!(
                "snapshot: {} coordinators at/below high-water (created {})",
                snap.get("coordinators").cloned().unwrap_or_default(),
                snap.get("created_at").cloned().unwrap_or_default()
            );
        }
        for line in &anomalies {
            println!("{line}");
        }
        println!(
            "{} — {} record(s) checked, {} verified",
            if anomalies.is_empty() {
                "OK"
            } else {
                "ANOMALIES FOUND"
            },
            body["records_checked"],
            body["verified"]
        );
    }
    if strict && !anomalies.is_empty() {
        crate::anyhow::bail!(
            "{} ledger anomal{} found",
            anomalies.len(),
            if anomalies.len() == 1 { "y" } else { "ies" }
        );
    }
    Ok(())
}

/// Print the consensus view reconstructed from the ledger — current term,
/// leader, and each coordinator's high-water sequence mark. Two nodes
/// holding the same records derive the same view.
fn replay_view(json: bool) -> Result<()> {
    if json {
        let body = susi_core::commit_log_report::replay_report();
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let state = commit_log::replay();
    let persisted = commit_log::load_term();
    println!("term:          {}", state.term);
    println!(
        "leader:        {}",
        if state.leader.is_empty() {
            "(none)"
        } else {
            &state.leader
        }
    );
    println!(
        "persisted:     term {} / leader {}",
        persisted.term,
        if persisted.leader.is_empty() {
            "(none)".to_string()
        } else {
            persisted.leader.clone()
        }
    );
    println!("decisions:     {}", state.decisions);
    println!("memberships:   {}", state.memberships);
    if !state.roster.is_empty() {
        println!("committed roster:");
        for (id, addr) in &state.roster {
            println!("  {id:<24} {addr}");
        }
    }
    if !state.banned.is_empty() {
        println!("committed bans:");
        for (id, addr) in &state.banned {
            println!("  {id:<24} {addr}");
        }
    }
    println!("coordinators:");
    for (coord, high) in &state.coordinators {
        println!("  {coord:<20} high-water seq {high}");
    }
    if !state.anomalies.is_empty() {
        println!("anomalies:");
        for a in &state.anomalies {
            println!("  - {a}");
        }
    }
    Ok(())
}

//! Local-only, cooperative worktree and scope reservations for parallel agents.
//! Reservations prevent overlaps between users of this launcher; they are not a
//! filesystem sandbox or a substitute for testing the integrated tree.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(in crate::local_workflow) use crate::{
    affected, common, compiler_env, distill_compose, gate_receipt, native_provenance,
    storage_guard, test_binary_evidence, test_runner, trusted_sysroot, workspace_test_count,
    workspace_test_proof, worktree_admin,
};

mod claims;
mod gates;
mod ship;
mod sync_commit;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
use self::{claims::*, gates::*, ship::*, sync_commit::*};

pub(in crate::local_workflow) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Resolve worktree-local Git paths together. Reject incomplete or ambiguous
/// output rather than silently omitting any operation guard.
pub(in crate::local_workflow) fn git_paths(root: &Path, names: &[&str]) -> Result<Vec<PathBuf>> {
    let mut args = vec!["rev-parse", "--path-format=absolute"];
    for name in names {
        args.extend(["--git-path", *name]);
    }
    let output = git(root, &args)?;
    parse_git_paths(&output, names.len())
}

pub(in crate::local_workflow) fn parse_git_paths(
    output: &str,
    expected: usize,
) -> Result<Vec<PathBuf>> {
    let paths: Vec<_> = output.lines().map(PathBuf::from).collect();
    if paths.len() != expected || paths.iter().any(|p| !p.is_absolute()) {
        return Err("Git returned incomplete or non-absolute operation paths".into());
    }
    Ok(paths)
}

pub(in crate::local_workflow) fn git(dir: &Path, args: &[&str]) -> Result<String> {
    common::git_trimmed(dir, args)
}

pub(in crate::local_workflow) fn text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "path must be valid UTF-8".into())
}

/// A verification slot that a ship currently holds.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::local_workflow) struct BusySlot {
    pub(in crate::local_workflow) slot: usize,
    pub(in crate::local_workflow) owner: String,
}

/// A point-in-time view of the local workflow. It never takes the workflow
/// lock or waits on a slot, so it answers while other agents start, commit,
/// integrate or ship.
#[derive(Debug)]
pub(in crate::local_workflow) struct WorkflowStatus {
    pub(in crate::local_workflow) integration_head: Option<String>,
    pub(in crate::local_workflow) claims: Vec<Claim>,
    pub(in crate::local_workflow) busy_slots: Vec<BusySlot>,
}

pub(in crate::local_workflow) fn workflow_status(root: &Path) -> Result<WorkflowStatus> {
    let state = state(root)?;
    Ok(WorkflowStatus {
        integration_head: integration_head(root)?,
        claims: claims(&state)?,
        busy_slots: busy_slots(&state.join("slots"))?,
    })
}

/// Probe each `<n>.lock` with a non-blocking try-lock: a held lock is a busy
/// slot; a free one is released at once when the probe handle drops.
pub(in crate::local_workflow) fn busy_slots(slots: &Path) -> Result<Vec<BusySlot>> {
    let entries = match fs::read_dir(slots) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut busy = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(slot) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".lock"))
            .and_then(|index| index.parse::<usize>().ok())
        else {
            continue;
        };
        let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        match file.try_lock() {
            Ok(()) => release_probe(file),
            Err(fs::TryLockError::WouldBlock) => busy.push(BusySlot {
                slot,
                owner: fs::read_to_string(slots.join(slot.to_string()).join("owner"))
                    .map(|owner| owner.trim().to_owned())
                    .unwrap_or_default(),
            }),
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    busy.sort_by_key(|busy| busy.slot);
    Ok(busy)
}

pub(in crate::local_workflow) fn status_json(status: &WorkflowStatus) -> serde_json::Value {
    serde_json::json!({
        "schema": "susi-local-workflow-status-v1",
        "integration_head": status.integration_head,
        "claims": status.claims.iter().map(|claim| serde_json::json!({
            "name": claim.name,
            "worktree": claim.worktree.display().to_string(),
            "local": claim.is_local(),
            "base": claim.base,
            "scopes": claim.scopes,
        })).collect::<Vec<_>>(),
        "busy_slots": status.busy_slots.iter().map(|busy| serde_json::json!({
            "slot": busy.slot,
            "owner": busy.owner,
        })).collect::<Vec<_>>(),
    })
}

pub(in crate::local_workflow) fn status_text(status: &WorkflowStatus) -> String {
    let mut out = format!(
        "integration: {}\nclaims: {}\n",
        status.integration_head.as_deref().unwrap_or("none"),
        status.claims.len()
    );
    for claim in &status.claims {
        out.push_str(&format!(
            "  {}\t{}{}\tbase {}\t{}\n",
            claim.name,
            claim.worktree.display(),
            claim.host_label(),
            claim.base.get(..12).unwrap_or(&claim.base),
            claim.scopes.join(", ")
        ));
    }
    out.push_str(&format!("verifying: {}\n", status.busy_slots.len()));
    for busy in &status.busy_slots {
        let owner = if busy.owner.is_empty() {
            "unknown owner"
        } else {
            &busy.owner
        };
        out.push_str(&format!("  slot {}\t{owner}\n", busy.slot));
    }
    out
}

pub(in crate::local_workflow) fn run(args: &[String]) -> Result<()> {
    let root = env::current_dir()?;
    match args.first().map(String::as_str) {
        Some("provenance-policy") if args.len() == 1 => {
            println!("{}", serde_json::json!({"schema":"susi-native-provenance-policy-v1",
                "policy_sha256":native_provenance::policy_digest(),"policy":native_provenance::POLICY}));
        },
        Some("verify-provenance") => {
            let mut flags = std::collections::BTreeMap::new();
            let mut rest = args[1..].iter();
            while let Some(flag) = rest.next() {
                if !["--root","--permit","--permit-sha256","--verifier-sha256","--policy-sha256","--head"].contains(&flag.as_str())
                    || flags.insert(flag.as_str(), rest.next().ok_or("missing provenance flag value")?.as_str()).is_some() {
                    return Err("unknown or duplicate provenance argument".into());
                }
            }
            let get = |key| flags.get(key).copied().ok_or("missing required provenance argument");
            let report = native_provenance::verify(Path::new(get("--root")?), Path::new(get("--permit")?),
                &native_provenance::TrustPins {permit_sha256:get("--permit-sha256")?,
                    verifier_sha256:get("--verifier-sha256")?,policy_sha256:get("--policy-sha256")?}, get("--head")?)?;
            println!("{}",serde_json::to_string(&report)?);
        },
        Some("start" | "extend" | "shrink") => {
            let claimant = args.get(1).ok_or("start requires an agent name")?;
            let mut scopes = Vec::new();
            let mut rest = args[2..].iter();
            while let Some(flag) = rest.next() {
                if flag != "--scope" { return Err(format!("unknown argument: {flag}").into()); }
                scopes.push(rest.next().ok_or("--scope requires a path")?.clone());
            }
            if args[0] == "extend" { update_scopes(&root, claimant, &scopes, ScopeChange::Extend)?; return Ok(()); }
            if args[0] == "shrink" { update_scopes(&root, claimant, &scopes, ScopeChange::Shrink)?; return Ok(()); }
            let worktree = start(&root, claimant, &scopes)?;
            cli_board_check(&worktree, claimant)?;
            cli_board_post(
                &worktree,
                claimant,
                "claim",
                &format!("local-workflow start {claimant}"),
                &format!("Claimed isolated worktree `{}` for scopes: {}", worktree.display(), scopes.join(", ")),
            )?;
            println!("Worktree: {}\nTarget: {}\nUse the local-build entrypoint to override inherited target directories. Reservations cover cooperating launcher users only.", worktree.display(), worktree.join("target").display());
        }
        Some("release") if args.len() == 2 => {
            cli_board_check(&root, &args[1])?;
            release(&root, &args[1])?;
            cli_board_post(
                &root,
                &args[1],
                "note",
                &format!("local-workflow release {}", args[1]),
                &format!("Released claim `{}` via local-workflow.", args[1]),
            )?;
        }
        Some("sync" | "step") if (2..=3).contains(&args.len()) => {
            let json_out = if args.len() == 3 {
                if args[2] != "--json" {
                    return Err("unknown sync flag; expected --json".into());
                }
                true
            } else {
                false
            };
            let report = sync_claim(&root, &args[1])?;
            if json_out {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema": "susi-local-workflow-sync-v1",
                        "claimant": report.claimant,
                        "mode": report.mode.as_str(),
                        "previous_head": report.previous_head,
                        "head": report.head,
                        "upstream": report.upstream,
                        "claim_base": report.claim_base,
                    }))?
                );
            } else {
                println!(
                    "Synced {} ({}): {} -> {} (base {})",
                    report.claimant,
                    report.mode.as_str(),
                    report.previous_head,
                    report.head,
                    report.claim_base
                );
            }
        }
        Some("commit") if args.len() == 3 => {
            cli_board_check(&root, &args[1])?;
            commit(&root, &args[1], &args[2], None)?;
            cli_board_post(
                &root,
                &args[1],
                "note",
                &format!("local-workflow commit {}", args[1]),
                &format!("Committed in isolated worktree for `{}`: {}", args[1], args[2]),
            )?;
        }
        Some("finish-merge") if args.len() == 4 => {
            cli_board_check(&root, &args[1])?;
            scoped_commit(&root, &args[1], &args[3], None, Some(&args[2]))?;
            cli_board_post(
                &root,
                &args[1],
                "note",
                &format!("local-workflow finish-merge {}", args[1]),
                &format!("Completed reconciled merge for `{}`: {}", args[1], args[3]),
            )?;
        }
        Some("finish-merge") if args.iter().any(|arg| arg == "--sign") => return Err("finish-merge does not support signing".into()),
        Some("commit") if args.len() == 5 && args[3] == "--sign" => {
            cli_board_check(&root, &args[1])?;
            commit(&root, &args[1], &args[2], Some(&args[4]))?;
            cli_board_post(
                &root,
                &args[1],
                "note",
                &format!("local-workflow signed commit {}", args[1]),
                &format!("Signed commit in isolated worktree for `{}`: {}", args[1], args[2]),
            )?;
        }
        Some("distill") if args.len() >= 2 => {
            println!("{}", workflow_distill(&root, &args[1..])?);
        }
        Some("integrate") if args.len() >= 2 => {
            let started = std::time::Instant::now();
            let names: Vec<_> = args[1..].iter().map(String::as_str).collect();
            if let Some(first) = names.first() {
                cli_board_check(&root, first)?;
            }
            let result = integrate_with(&root, &names, verify_candidate);
            finish_phases(&root, "integrate", if result.is_ok() { "integrated" } else { "failed" }, started.elapsed());
            let ok = result?;
            if let Some(first) = names.first() {
                cli_board_post(
                    &root,
                    first,
                    "note",
                    &format!("local-workflow integrate {}", names.join(", ")),
                    &format!("Integrated {:?} in candidate.", ok),
                )?;
            }
        }
        Some("list") if args.len() == 1 => {
            let state = state(&root)?;
            let _lock = Lock::acquire(&state)?;
            for claim in claims(&state)? { println!("{}\t{}{}\t{}", claim.name, claim.worktree.display(), claim.host_label(), claim.scopes.join(", ")); }
        }
        Some("status") if args.len() == 1 => print!("{}", status_text(&workflow_status(&root)?)),
        Some("status") if args.len() == 2 && args[1] == "--json" => {
            println!("{}", serde_json::to_string_pretty(&status_json(&workflow_status(&root)?))?);
        }
        Some("check") if args.len() == 1 => check(&root)?,
        Some("affected") if (2..=3).contains(&args.len()) => {
            let head = args.get(2).map_or("HEAD", String::as_str);
            let changed = affected::changed(&root, &args[1], head)?;
            let (selection, why) = affected::select_explained(&affected::load(&root)?, &changed);
            match selection {
                affected::Selection::Packages(packages) => {
                    println!("{} changed paths; gate covers {} packages:", changed.len(), packages.len());
                    for package in packages {
                        let reason = why.get(&package).map_or("", String::as_str);
                        println!("  {package}  ({reason})");
                    }
                }
                affected::Selection::Workspace(reason) => {
                    println!("{} changed paths; gate covers the whole workspace: {reason}", changed.len());
                }
            }
        }
        Some("ship") if args.get(1).map(String::as_str) == Some("--retry") && args.len() == 2 => {
            let result = ship_retry(&root, &mut GitRemote);
            witness_storage(&root);
            print_ship(&result?);
        }
        Some("ship") if args.len() >= 2 => {
            let started = std::time::Instant::now();
            let names: Vec<_> = args[1..].iter().map(String::as_str).collect();
            let mut remote = GitRemote;
            let result = repo_kind(&root).and_then(|kind| match kind {
                RepoKind::Susi => ship_with_board_policy(&root, &names, &mut remote, true, verify_candidate),
                RepoKind::Commercial => ship_with_board_policy(&root, &names, &mut remote, true, commercial_gate),
            });
            finish_phases(&root, "ship", if result.is_ok() { "shipped" } else { "failed" }, started.elapsed());
            witness_storage(&root);
            let report = result?;
            if let Some(first) = names.first() {
                cli_board_post(
                    &root,
                    first,
                    "landed",
                    &format!("local-workflow ship {}", names.join(", ")),
                    &format!("Shipped {} (merge {}) to origin/main.", report.head, report.merge),
                )?;
            }
            print_ship(&report);
        }
        _ => return Err("usage: local-workflow start <name> --scope <path> [--scope <path> ...] | extend <name> --scope <path> | shrink <name> --scope <path> | sync <name> [--json] | step <name> [--json] | list | status [--json] | commit <name> <message> [--sign <fingerprint>] | finish-merge <name> <parent-oid> <message> | distill <args...> | integrate <name> [<name> ...] | release <name> | ship <name> [<name> ...] | ship --retry | check | affected <base> [<head>]".into()),
    }
    Ok(())
}

/// After a ship, whatever its result, this clone witnesses the storage remote
/// (`storage_guard`), in Susi core and in Susi Commercial Apps alike: lost
/// history is reported on stderr and never changes the ship's result.
pub(in crate::local_workflow) fn witness_storage(root: &Path) {
    if let Some(advisory) = storage_guard::witness(root, "origin") {
        eprintln!("local-workflow: {advisory}");
    }
}

/// CLI entrypoint for `local-workflow`.
pub fn main_entry() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("local-workflow") {
        args.remove(0);
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("local-workflow: {error}");
            ExitCode::FAILURE
        }
    }
}

use super::*;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub(in crate::distill_compose) fn store_durable_receipt(
    path: &Path,
    expected_sha256: &str,
    bytes: &[u8],
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| "receipt parent directory missing".to_owned())?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create receipt dir: {e}"))?;
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes)
                .map_err(|e| format!("cannot write distillation receipt: {e}"))?;
            file.sync_all()
                .map_err(|e| format!("cannot sync distillation receipt: {e}"))?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(format!("cannot create distillation receipt: {err}")),
    }
    let stored = read_and_sync_receipt(path)?;
    let stored_sha = stored
        .get("receipt_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| "stored distillation receipt missing receipt_sha256".to_owned())?;
    if stored_sha != expected_sha256 {
        return Err(
            "distillation name already reserved for different inputs/evidence; choose a new --name"
                .to_owned(),
        );
    }
    Ok(())
}

pub(in crate::distill_compose) fn read_and_sync_receipt(path: &Path) -> Result<Value> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("cannot open distillation receipt {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read distillation receipt: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("cannot sync distillation receipt: {e}"))?;
    parse_verified_receipt(&bytes)
}

pub(in crate::distill_compose) fn read_receipt_readonly(path: &Path) -> Result<Value> {
    let bytes = fs::read(path)
        .map_err(|e| format!("cannot read distillation receipt {}: {e}", path.display()))?;
    parse_verified_receipt(&bytes)
}

pub(in crate::distill_compose) fn parse_verified_receipt(bytes: &[u8]) -> Result<Value> {
    let mut val: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("corrupt distillation receipt JSON: {e}"))?;
    if val.get("schema").and_then(Value::as_str) != Some(RECEIPT_SCHEMA) {
        return Err("unknown distillation receipt schema".to_owned());
    }
    let recorded_sha = val
        .get("receipt_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| "distillation receipt missing receipt_sha256".to_owned())?
        .to_owned();
    valid_oid(&recorded_sha)?;
    if let Some(map) = val.as_object_mut() {
        map.remove("receipt_sha256");
    }
    let canonical = serde_json::to_vec(&val).map_err(|e| e.to_string())?;
    if sha256_hex(&canonical) != recorded_sha {
        return Err("distillation receipt digest mismatch; refusing tampered record".to_owned());
    }
    if let Some(map) = val.as_object_mut() {
        map.insert("receipt_sha256".to_owned(), json!(recorded_sha));
    }
    Ok(val)
}

pub(in crate::distill_compose) fn format_summary(
    outcome: &DistillOutcome,
    json_output: bool,
) -> String {
    if json_output {
        return String::from_utf8_lossy(&outcome.receipt_bytes)
            .trim_end()
            .to_owned();
    }
    let inc = outcome.receipt["included"].as_array().map_or(0, Vec::len);
    let base_sat = outcome.receipt["satisfied_in_base"]
        .as_array()
        .map_or(0, Vec::len);
    let def = outcome.receipt["deferred"].as_array().map_or(0, Vec::len);
    let rej = outcome.receipt["rejected"].as_array().map_or(0, Vec::len);
    let skipped_suffix = if outcome.receipt["functional_only"].as_bool() == Some(true) {
        let skipped = outcome.receipt["skipped_non_functional_tasks"]
            .as_array()
            .map_or(0, Vec::len);
        format!(" skipped_non_functional={skipped}")
    } else {
        String::new()
    };
    format!(
        "Distillation {}: included={inc} satisfied_in_base={base_sat}{skipped_suffix} deferred={def} rejected={rej} commit={} tree={} branch={} receipt_sha256={}\nUnqualified candidate only; full exact-tree release qualification remains required.",
        outcome.receipt["name"].as_str().unwrap_or(""),
        outcome.final_commit,
        outcome.final_tree,
        outcome.branch_ref,
        outcome.receipt_sha256
    )
}

pub(in crate::distill_compose) fn discover_tasks_in_range(
    git: &Git<'_>,
    range_spec: &str,
    functional_only: bool,
) -> Result<Vec<String>> {
    let (left_raw, right_raw) = range_spec
        .split_once("..")
        .ok_or_else(|| "invalid --tasks-from-range; expected <base-rev>..<from-rev>".to_owned())?;
    if left_raw.trim().is_empty() || right_raw.trim().is_empty() || right_raw.starts_with('.') {
        return Err(
            "invalid --tasks-from-range; expected two-dot <base-rev>..<from-rev>".to_owned(),
        );
    }
    let left_oid = git.resolve_commit(left_raw.trim())?;
    let right_oid = git.resolve_commit(right_raw.trim())?;
    let commits = git.non_merge_commits(&left_oid, &right_oid)?;
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for commit in commits {
        for trailer in commit.task_trailers {
            if functional_only && trailer.starts_with("LOCAL-") {
                continue;
            }
            if seen.insert(trailer.clone()) {
                ordered.push(trailer);
            }
        }
        if functional_only {
            for path in &commit.changed_paths {
                if let Some(id) = functional::task_id_from_manifest_path(path) {
                    if seen.insert(id.clone()) {
                        ordered.push(id);
                    }
                }
            }
        }
    }
    Ok(ordered)
}

pub(in crate::distill_compose) fn run_distill_command(
    root: &Path,
    args: &DistillArgs,
) -> Result<String> {
    let git = Git { root };
    let outcome = execute_distill(&git, root, args)?;
    if let Some(out_path) = &args.receipt_out {
        if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create output dir: {e}"))?;
        }
        fs::write(out_path, &outcome.receipt_bytes)
            .map_err(|e| format!("cannot write --receipt-out: {e}"))?;
    }

    if args.strict && outcome.has_deferred_or_rejected {
        return Err(format!(
            "strict distillation refused deferred/rejected tasks: {}",
            format_summary(&outcome, args.json_output)
        ));
    }

    if matches!(args.mode, CommandMode::Compose | CommandMode::Orchestrate) {
        let receipt_path = git.state_path("receipts", &args.name)?;
        store_durable_receipt(
            &receipt_path,
            &outcome.receipt_sha256,
            &outcome.receipt_bytes,
        )?;
        git.ensure_cas_ref(
            &format!("refs/susi-distill/candidates/{}", args.name),
            &outcome.final_commit,
        )?;
        let branch_tip = if let Some(rel_name) =
            outcome.branch_ref.strip_prefix("refs/heads/release/")
        {
            let rel_path = format!("releases/distill/{rel_name}.json");
            let env_msg = format!(
                "evidence(distill): release candidate receipt {rel_name}\n\nDistilled-Commit: {}\nReceipt-Sha256: {}\n",
                outcome.final_commit, outcome.receipt_sha256
            );
            git.commit_single_blob_envelope(
                &outcome.final_commit,
                &rel_path,
                &outcome.receipt_bytes,
                &env_msg,
            )?
        } else {
            outcome.final_commit.clone()
        };
        git.ensure_cas_ref(&outcome.branch_ref, &branch_tip)?;
        if let Some(rel_name) = outcome.branch_ref.strip_prefix("refs/heads/release/") {
            git.ensure_cas_ref(&format!("refs/candidates/release/{rel_name}"), &branch_tip)?;
        }
        if let (Some(tag), Some(record_out)) = (&args.release_tag, &args.release_record_out) {
            let rendered = render_distilled_release_record(
                &git,
                &outcome.receipt,
                &ReleaseRecordArgs {
                    name: args.name.clone(),
                    tag: tag.clone(),
                    commit: Some(branch_tip.clone()),
                    tagged: "2026-11-11T00:00:00Z".to_owned(),
                    toolchain: None,
                    host: None,
                    assets: Vec::new(),
                    out: Some(record_out.clone()),
                    json_output: false,
                },
            )?;
            if let Some(parent) = record_out.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create release record dir: {e}"))?;
            }
            fs::write(record_out, rendered.as_bytes())
                .map_err(|e| format!("cannot write --release-record-out: {e}"))?;
        }

        let mut extra_summaries = Vec::new();
        if args.mode == CommandMode::Orchestrate && args.publish_snapshot {
            extra_summaries.push(run_inspect_command(
                root,
                &InspectArgs {
                    mode: CommandMode::Publish,
                    name: args.name.clone(),
                    json_output: false,
                },
            )?);
        }
        if !extra_summaries.is_empty() && !args.json_output {
            return Ok(format!(
                "{}\n{}",
                format_summary(&outcome, false),
                extra_summaries.join("\n")
            ));
        }
    }

    Ok(format_summary(&outcome, args.json_output))
}

pub(in crate::distill_compose) fn verify_local_release_envelope(
    git: &Git<'_>,
    name: &str,
    branch_oid: &str,
    expected_parent: &str,
    expected_sha256: &str,
) -> Result<()> {
    let lineage = git.text(&["rev-list", "--parents", "-n", "1", branch_oid])?;
    let parents: Vec<&str> = lineage.split_whitespace().skip(1).collect();
    if parents != [expected_parent] {
        return Err("release branch envelope parent does not match distilled commit".to_owned());
    }
    let rel_path = format!("releases/distill/{name}.json");
    let changed = git.text(&[
        "diff-tree",
        "--no-commit-id",
        "--no-renames",
        "--name-only",
        "-r",
        expected_parent,
        branch_oid,
    ])?;
    if changed != rel_path {
        return Err(format!(
            "release branch envelope must change only {rel_path}, found: {changed}"
        ));
    }
    let raw = git.bytes(
        &["cat-file", "blob", &format!("{branch_oid}:{rel_path}")],
        None,
    )?;
    let parsed = parse_verified_receipt(&raw)?;
    if parsed.get("receipt_sha256").and_then(Value::as_str) != Some(expected_sha256) {
        return Err(
            "release branch envelope receipt digest does not match stored receipt".to_owned(),
        );
    }
    Ok(())
}

pub(in crate::distill_compose) fn run_inspect_command(
    root: &Path,
    args: &InspectArgs,
) -> Result<String> {
    let git = Git { root };
    git.require_original_history()?;
    let receipt_path = git.state_path("receipts", &args.name)?;
    let receipt = if args.mode == CommandMode::Status {
        read_receipt_readonly(&receipt_path)?
    } else {
        read_and_sync_receipt(&receipt_path)?
    };
    let final_commit = receipt["final_commit"]
        .as_str()
        .ok_or_else(|| "receipt missing final_commit".to_owned())?;
    let final_tree = receipt["final_tree"]
        .as_str()
        .ok_or_else(|| "receipt missing final_tree".to_owned())?;
    let branch_ref = receipt["branch_ref"]
        .as_str()
        .ok_or_else(|| "receipt missing branch_ref".to_owned())?;
    let tag_ref = receipt["snapshot_tag_ref"]
        .as_str()
        .ok_or_else(|| "receipt missing snapshot_tag_ref".to_owned())?;
    if git.tree(final_commit)? != final_tree {
        return Err("distilled commit/tree evidence does not match stored receipt".to_owned());
    }

    if args.mode == CommandMode::Status {
        return report_status(&git, &receipt, branch_ref, tag_ref, args.json_output);
    }

    let endpoint = git
        .endpoint_optional()?
        .ok_or_else(|| "origin remote is not configured; cannot publish".to_owned())?;
    if let Some(expected_ep) = receipt.get("endpoint_digest").and_then(Value::as_str) {
        if expected_ep != endpoint.digest {
            return Err(
                "origin destination changed since distillation compose; refusing to publish"
                    .to_owned(),
            );
        }
    }

    if let Some(remote_oid) = git.remote_ref(&endpoint, tag_ref)? {
        if remote_oid == final_commit {
            return Ok(format!(
                "Published distilled snapshot verified: commit={final_commit} tree={final_tree} tag={tag_ref}"
            ));
        }
        return Err(format!(
            "remote snapshot tag {tag_ref} already points to {remote_oid}; left unchanged"
        ));
    }

    let refspec = format!("{final_commit}:{tag_ref}");
    let _ = git.output(
        &[
            "push",
            "--porcelain",
            "--no-follow-tags",
            "--recurse-submodules=no",
            "--",
            &endpoint.address,
            &refspec,
        ],
        None,
    );

    match git.remote_ref(&endpoint, tag_ref) {
        Ok(Some(remote_oid)) if remote_oid == final_commit => Ok(format!(
            "Published distilled snapshot verified: commit={final_commit} tree={final_tree} tag={tag_ref}\nQualification remains separate before any production release tag."
        )),
        Ok(Some(other)) => Err(format!(
            "remote snapshot tag {tag_ref} points to {other}; left unchanged"
        )),
        Ok(None) => Err(format!(
            "distilled snapshot tag {tag_ref} was not published; safe to retry publish {}",
            args.name
        )),
        Err(_) => Err(format!(
            "publication outcome for {tag_ref} is unknown; run status {} before retrying",
            args.name
        )),
    }
}

pub(in crate::distill_compose) fn report_status(
    git: &Git<'_>,
    receipt: &Value,
    branch_ref: &str,
    tag_ref: &str,
    json_output: bool,
) -> Result<String> {
    let final_commit = receipt["final_commit"]
        .as_str()
        .ok_or_else(|| "receipt missing final_commit".to_owned())?;
    let receipt_sha256 = receipt["receipt_sha256"]
        .as_str()
        .ok_or_else(|| "receipt missing receipt_sha256".to_owned())?;
    let local_only_branch = branch_ref.starts_with("refs/heads/release/");
    let local_branch_oid = git
        .text(&["show-ref", "--verify", "--hash", branch_ref])
        .ok();
    let branch_state = match local_branch_oid.as_deref() {
        Some(oid) if oid == final_commit => "present_exact",
        Some(oid)
            if branch_ref
                .strip_prefix("refs/heads/release/")
                .is_some_and(|rel| {
                    verify_local_release_envelope(git, rel, oid, final_commit, receipt_sha256)
                        .is_ok()
                }) =>
        {
            "present_exact"
        }
        Some(_) => "present_diverged",
        None => "absent",
    };
    let (remote_state, remote_branch_state) = match git.endpoint_optional()? {
        None => (
            "no_remote",
            if local_only_branch {
                "local_only"
            } else {
                "no_remote"
            },
        ),
        Some(endpoint) => {
            if receipt
                .get("endpoint_digest")
                .and_then(Value::as_str)
                .is_some_and(|exp| exp != endpoint.digest)
            {
                (
                    "endpoint_changed",
                    if local_only_branch {
                        "local_only"
                    } else {
                        "endpoint_changed"
                    },
                )
            } else {
                let tag_st = match git.remote_ref(&endpoint, tag_ref) {
                    Ok(Some(oid)) if oid == final_commit => "published_exact",
                    Ok(Some(_)) => "conflict",
                    Ok(None) => "absent",
                    Err(_) => "unreadable",
                };
                let br_st = if local_only_branch {
                    "local_only"
                } else {
                    match (
                        local_branch_oid.as_deref(),
                        git.remote_ref(&endpoint, branch_ref),
                    ) {
                        (Some(expected), Ok(Some(oid)))
                            if branch_state == "present_exact" && oid == expected =>
                        {
                            "published_exact"
                        }
                        (_, Ok(Some(_))) => "diverged",
                        (_, Ok(None)) => "absent",
                        (_, Err(_)) => "unreadable",
                    }
                };
                (tag_st, br_st)
            }
        }
    };
    if json_output {
        let out = json!({
            "name": receipt["name"],
            "final_commit": final_commit,
            "final_tree": receipt["final_tree"],
            "branch_ref": branch_ref,
            "branch_state": branch_state,
            "remote_branch_state": remote_branch_state,
            "snapshot_tag_ref": tag_ref,
            "remote_state": remote_state,
            "receipt_sha256": receipt_sha256,
            "included_count": receipt["included"].as_array().map_or(0, Vec::len),
            "deferred_count": receipt["deferred"].as_array().map_or(0, Vec::len),
            "rejected_count": receipt["rejected"].as_array().map_or(0, Vec::len),
        });
        return serde_json::to_string_pretty(&out).map_err(|e| e.to_string());
    }
    Ok(format!(
        "Distillation status {}: commit={final_commit} branch={branch_ref} ({branch_state}, remote={remote_branch_state}) tag={tag_ref} ({remote_state})",
        receipt["name"].as_str().unwrap_or("")
    ))
}

pub(in crate::distill_compose) fn audit_candidate_commits_for_main(
    commits: &[CommitInfo],
) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut tasks = Vec::new();
    for commit in commits {
        if commit.task_trailers.is_empty() {
            return Err(format!(
                "missing_task_trailer: commit {} ('{}') lacks a 'Task: T-...' trailer required for main distillation",
                commit.oid, commit.subject
            ));
        }
        for path in &commit.changed_paths {
            if is_forbidden_confidential_path(path) {
                return Err(format!(
                    "confidentiality_violation: commit {} touches forbidden credential/key path '{path}'",
                    commit.oid
                ));
            }
        }
        for trailer in &commit.task_trailers {
            if seen.insert(trailer.clone()) {
                tasks.push(trailer.clone());
            }
        }
    }
    Ok(tasks)
}

pub(in crate::distill_compose) fn run_submit_main_command(
    root: &Path,
    args: &SubmitMainArgs,
) -> Result<String> {
    let git = Git { root };
    git.require_original_history()?;
    let candidate = git.resolve_commit(args.rev.as_deref().unwrap_or("HEAD"))?;
    let endpoint = git
        .endpoint_optional()?
        .ok_or_else(|| "origin remote is not configured; cannot submit to main".to_owned())?;

    for attempt in 1..=args.max_attempts {
        let remote_main = git.remote_ref(&endpoint, "refs/heads/main")?;
        if let Some(ref remote_oid) = remote_main {
            let fetch_out = git.output(
                &[
                    "fetch",
                    "--no-tags",
                    "--recurse-submodules=no",
                    "--",
                    &endpoint.address,
                    "refs/heads/main",
                ],
                None,
            )?;
            if !fetch_out.status.success() {
                return Err(format!(
                    "cannot fetch origin refs/heads/main ({})",
                    fetch_out.status
                ));
            }
            if remote_oid == &candidate || git.ancestor(&candidate, remote_oid)? {
                let payload = json!({
                    "schema": "susi-main-submission-receipt-v1",
                    "status": "already_landed",
                    "candidate_commit": candidate,
                    "published_commit": remote_oid,
                    "remote_main": remote_oid,
                    "reconciled": false,
                    "attempts": attempt,
                    "endpoint_digest": endpoint.digest,
                    "tasks": Vec::<String>::new(),
                });
                return format_submit_main_result(&payload, args.json_output);
            }
        }

        let (publish_commit, reconciled, tasks) = match remote_main {
            None => {
                let head_info = git.inspect_commit_allow_root(&candidate)?;
                let tasks = audit_candidate_commits_for_main(&[head_info])?;
                (candidate.clone(), false, tasks)
            }
            Some(ref remote_oid) if git.ancestor(remote_oid, &candidate)? => {
                let delta = git.non_merge_commits(remote_oid, &candidate)?;
                let tasks = audit_candidate_commits_for_main(&delta)?;
                (candidate.clone(), false, tasks)
            }
            Some(ref remote_oid) => {
                let merge_base = git.text(&["merge-base", remote_oid, &candidate])?;
                valid_oid(&merge_base)?;
                let delta = git.non_merge_commits(&merge_base, &candidate)?;
                let tasks = audit_candidate_commits_for_main(&delta)?;
                let merged_tree = match git.merge_three_way(&merge_base, remote_oid, &candidate)? {
                    ThreeWayMerge::Clean(tree) => tree,
                    ThreeWayMerge::Conflict(conflict_msg) => {
                        return Err(format!(
                            "reconcile_conflict against concurrent origin/main {remote_oid}: {conflict_msg}"
                        ));
                    }
                };
                let mut msg = format!(
                    "Reconcile autonomous contribution {} onto development main\n\nNon-destructive object-store reconciliation preserving peer commits.\n",
                    &candidate[..12.min(candidate.len())]
                );
                for task_id in &tasks {
                    msg.push_str(&format!("Task: {task_id}\n"));
                }
                let merge_commit = git.commit_tree_dated_now(
                    &merged_tree,
                    &[remote_oid.as_str(), candidate.as_str()],
                    &msg,
                )?;
                (merge_commit, true, tasks)
            }
        };

        let refspec = format!("{publish_commit}:refs/heads/main");
        let push_out = git.output(
            &[
                "push",
                "--porcelain",
                "--no-follow-tags",
                "--recurse-submodules=no",
                "--",
                &endpoint.address,
                &refspec,
            ],
            None,
        )?;

        let post_remote = git.remote_ref(&endpoint, "refs/heads/main")?;
        if let Some(ref landed) = post_remote {
            if landed == &publish_commit {
                let payload = json!({
                    "schema": "susi-main-submission-receipt-v1",
                    "status": if reconciled { "published_reconciled" } else { "published_fast_forward" },
                    "candidate_commit": candidate,
                    "published_commit": publish_commit,
                    "remote_main": landed,
                    "reconciled": reconciled,
                    "attempts": attempt,
                    "endpoint_digest": endpoint.digest,
                    "tasks": tasks,
                });
                return format_submit_main_result(&payload, args.json_output);
            }
        }

        if attempt == args.max_attempts {
            let stderr = String::from_utf8_lossy(&push_out.stderr);
            return Err(format!(
                "submit-main exhausted {} attempt(s) without fast-forwarding refs/heads/main (remote={:?}): {}",
                args.max_attempts,
                post_remote,
                stderr.trim()
            ));
        }
    }
    Err("submit-main failed".to_owned())
}

pub(in crate::distill_compose) fn format_submit_main_result(
    payload: &Value,
    json_output: bool,
) -> Result<String> {
    if json_output {
        return serde_json::to_string_pretty(payload).map_err(|e| e.to_string());
    }
    Ok(format!(
        "Main submission {}: candidate={} published={} remote_main={} reconciled={} attempts={}",
        payload["status"].as_str().unwrap_or(""),
        payload["candidate_commit"].as_str().unwrap_or(""),
        payload["published_commit"].as_str().unwrap_or(""),
        payload["remote_main"].as_str().unwrap_or(""),
        payload["reconciled"].as_bool().unwrap_or(false),
        payload["attempts"].as_u64().unwrap_or(1),
    ))
}

pub(in crate::distill_compose) fn run_verify_harvest_command(
    root: &Path,
    args: &VerifyHarvestArgs,
) -> Result<String> {
    let git = Git { root };
    git.require_original_history()?;
    let resolved_rev = match args.rev.as_deref() {
        Some(r) => Some(git.resolve_commit(r)?),
        None => None,
    };
    let report = mandate74::verify_harvest_tree_and_range(
        root,
        resolved_rev.as_deref(),
        args.range.as_deref(),
    )?;
    if args.strict && !report.passed {
        return Err(format!(
            "harvest audit failed ({} finding(s)): {}",
            report.findings.len(),
            report.findings.join("; ")
        ));
    }
    if args.json_output {
        return serde_json::to_string_pretty(&report.to_json()).map_err(|e| e.to_string());
    }
    if report.passed {
        Ok(format!(
            "Harvest audit passed: manifests_verified={} harvest_commits_checked={}",
            report.manifests_verified, report.harvest_commits_checked
        ))
    } else {
        Ok(format!(
            "Harvest audit FAILED ({} finding(s)): {}",
            report.findings.len(),
            report.findings.join("; ")
        ))
    }
}

pub(in crate::distill_compose) fn detect_toolchain_semver_at_commit(
    git: &Git<'_>,
    commit: &str,
) -> String {
    if let Ok(raw) = git.bytes(
        &["cat-file", "blob", &format!("{commit}:rust-toolchain.toml")],
        None,
    ) {
        let text = String::from_utf8_lossy(&raw);
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("channel") {
                if let Some((_, val)) = rest.split_once('=') {
                    let clean = val.trim().trim_matches('"');
                    if valid_semver_triplet(clean) {
                        return clean.to_owned();
                    }
                }
            }
        }
    }
    "1.99.0".to_owned()
}

pub(in crate::distill_compose) fn json_escape_plain(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Emits a deterministic `susi-release-record/v1` document (`releases/<tag>.json`)
/// strictly preserving the line-oriented layout required by `tests/release_records.rs`
/// (`LINE_FIELDS` each alone on its line, single-line `"env": [...]`, and single-line
/// asset objects with `built != owed` coverage across core's supported targets).
pub(in crate::distill_compose) fn render_distilled_release_record(
    git: &Git<'_>,
    receipt: &Value,
    args: &ReleaseRecordArgs,
) -> Result<String> {
    let version = args
        .tag
        .strip_prefix('v')
        .filter(|v| valid_semver_triplet(v))
        .ok_or_else(|| format!("invalid release tag '{}'", args.tag))?;
    let branch_ref = receipt["branch_ref"]
        .as_str()
        .unwrap_or("refs/heads/release/unknown");
    let final_commit = receipt["final_commit"]
        .as_str()
        .ok_or_else(|| "distillation receipt missing final_commit".to_owned())?;
    let final_tree = receipt["final_tree"]
        .as_str()
        .ok_or_else(|| "distillation receipt missing final_tree".to_owned())?;
    let receipt_sha256 = receipt["receipt_sha256"]
        .as_str()
        .ok_or_else(|| "distillation receipt missing receipt_sha256".to_owned())?;

    let commit_raw = match &args.commit {
        Some(explicit) => git.resolve_commit(explicit)?,
        None => git
            .text(&["show-ref", "--verify", "--hash", branch_ref])
            .unwrap_or_else(|_| final_commit.to_owned()),
    };
    let commit = commit_raw.to_ascii_lowercase();
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(format!(
            "release record requires a 40-char lowercase SHA-1 commit id, got '{commit}'"
        ));
    }

    let toolchain = match &args.toolchain {
        Some(tc) => {
            if !valid_semver_triplet(tc) {
                return Err(format!("--toolchain '{tc}' must be X.Y.Z semver"));
            }
            tc.clone()
        }
        None => detect_toolchain_semver_at_commit(git, &commit),
    };
    let default_host = if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let host = args.host.as_deref().unwrap_or(default_host);
    if !matches!(host, "x86_64-pc-windows-msvc" | "x86_64-unknown-linux-gnu") {
        return Err(format!("unsupported release build.host '{host}'"));
    }

    let mut seen_files = BTreeSet::new();
    for asset in &args.assets {
        if !seen_files.insert(asset.file.clone()) {
            return Err(format!("duplicate release asset '{}'", asset.file));
        }
    }

    const CORE_TARGETS: [&str; 2] = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"];
    let mut missing_lines = Vec::new();
    for target in CORE_TARGETS {
        let has_susi_asset = args
            .assets
            .iter()
            .any(|a| a.target == target && a.bin == "susi");
        if !has_susi_asset {
            let reason = if args.assets.is_empty() {
                format!(
                    "distilled candidate {} prepared on {}; reference binary build and smoke qualification for {target} are owed via release-qualify",
                    args.name, host
                )
            } else {
                format!(
                    "qualified on {host}; reference binary build and qualification for {target} are owed"
                )
            };
            missing_lines.push(format!(
                "    {{ \"target\": \"{target}\", \"reason\": \"{}\" }}",
                json_escape_plain(&reason)
            ));
        }
    }

    let included_tasks: Vec<&str> = receipt["included"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item["task_id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let qual_status = receipt["qualification_status"]
        .as_str()
        .unwrap_or("unqualified_candidate");
    let locally_qualified = qual_status == "accept_verified_candidate" && !args.assets.is_empty();

    let assets_block = if args.assets.is_empty() {
        "  \"assets\": [],".to_owned()
    } else {
        let rows: Vec<String> = args
            .assets
            .iter()
            .map(|a| {
                format!(
                    "    {{ \"file\": \"{}\", \"target\": \"{}\", \"package\": \"{}\", \"bin\": \"{}\", \"sha256\": \"{}\" }}",
                    a.file, a.target, a.package, a.bin, a.sha256
                )
            })
            .collect();
        format!("  \"assets\": [\n{}\n  ],", rows.join(",\n"))
    };

    let missing_block = if missing_lines.is_empty() {
        "  \"missing_targets\": [],".to_owned()
    } else {
        format!(
            "  \"missing_targets\": [\n{}\n  ],",
            missing_lines.join(",\n")
        )
    };

    let selected_list = if included_tasks.is_empty() {
        "base-satisfied".to_owned()
    } else {
        included_tasks.join(", ")
    };

    let build_source = format!(
        "distilled candidate {} (tree {}, receipt {})",
        args.name, final_tree, receipt_sha256
    );
    let prov_distill = format!(
        "source: distilled candidate {} = {} (tree {}), receipt {}, selecting {}",
        branch_ref,
        final_commit,
        final_tree,
        receipt_sha256,
        json_escape_plain(&selected_list)
    );

    Ok(format!(
        "{{\n\
  \"schema\": \"susi-release-record/v1\",\n\
  \"product\": \"susi\",\n\
  \"version\": \"{version}\",\n\
  \"tag\": \"{}\",\n\
  \"commit\": \"{commit}\",\n\
  \"tagged\": \"{}\",\n\
  \"build\": {{\n\
    \"toolchain\": \"{toolchain}\",\n\
    \"host\": \"{host}\",\n\
    \"source\": \"{}\",\n\
    \"profile\": \"dist\",\n\
    \"features\": \"susi/local-engine\",\n\
    \"env\": [\"CARGO_PROFILE_DIST_LTO=false\", \"CARGO_PROFILE_DIST_CODEGEN_UNITS=16\"],\n\
    \"command\": \"cargo build --offline --locked --profile dist -p susi-harness -p susi --features susi/local-engine\"\n\
  }},\n\
  \"reproducible\": false,\n\
{assets_block}\n\
{missing_block}\n\
  \"checks\": {{\n\
    \"distill_receipt\": \"releases/distill/{}.json\",\n\
    \"distill_status\": \"{}\",\n\
    \"locally_qualified\": {locally_qualified},\n\
    \"qualification_note\": \"distilled deterministically via local-distill from Task-labelled main commits ({})\"\n\
  }},\n\
  \"provenance\": [\n\
    \"{}\"\n\
  ]\n\
}}\n",
        args.tag,
        json_escape_plain(&args.tagged),
        json_escape_plain(&build_source),
        args.name,
        json_escape_plain(qual_status),
        json_escape_plain(&selected_list),
        prov_distill,
    ))
}

pub(in crate::distill_compose) fn run_release_record_command(
    root: &Path,
    args: &ReleaseRecordArgs,
) -> Result<String> {
    let git = Git { root };
    git.require_original_history()?;
    let receipt_path = git.state_path("receipts", &args.name)?;
    let receipt = read_receipt_readonly(&receipt_path)?;
    let rendered = render_distilled_release_record(&git, &receipt, args)?;
    if let Some(out_path) = &args.out {
        if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create release record output dir: {e}"))?;
        }
        fs::write(out_path, rendered.as_bytes())
            .map_err(|e| format!("cannot write release record {}: {e}", out_path.display()))?;
    }
    if args.json_output {
        return Ok(rendered.trim_end().to_owned());
    }
    Ok(format!(
        "Release record {} generated for distilled candidate {} (receipt_sha256={})",
        args.tag,
        args.name,
        receipt["receipt_sha256"].as_str().unwrap_or("")
    ))
}

pub fn run(root: &Path, args: &[String]) -> Result<String> {
    match parse_cli(args)? {
        ParsedCli::Help => Ok(USAGE.to_owned()),
        ParsedCli::Distill(distill_args) => run_distill_command(root, &distill_args),
        ParsedCli::Inspect(inspect_args) => run_inspect_command(root, &inspect_args),
        ParsedCli::ReleaseRecord(rr_args) => run_release_record_command(root, &rr_args),
        ParsedCli::SubmitMain(submit_args) => run_submit_main_command(root, &submit_args),
        ParsedCli::VerifyHarvest(vh_args) => run_verify_harvest_command(root, &vh_args),
        ParsedCli::QuarantineFetch(qf_args) => mandate74::quarantine_fetch(
            root,
            &qf_args.url,
            &qf_args.sha,
            &qf_args.slug,
            qf_args.json_output,
        ),
    }
}

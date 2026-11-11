//! Provider-neutral Git transport safety. Release tags are admitted only with
//! the exact-tree qualification record (`release_qualification.rs`).
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

#[path = "release_qualification.rs"]
mod release_qualification;

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(root)
        .arg("--no-replace-objects")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run Git: {e}"))?;
    if !out.status.success() {
        return Err(format!("Git validation failed: {}", args.join(" ")));
    }
    String::from_utf8(out.stdout)
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}
fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}
fn zero(value: &str) -> bool {
    value.bytes().all(|c| c == b'0')
}

/// Validate the exact transaction Git supplies, never HEAD or a tracking branch.
/// No network, builds, config writes, receipt fabrication or environment bypasses.
/// The receiving Git server remains responsible for atomic compare-and-swap.
pub fn validate(root: &Path, input: &str) -> Result<(), String> {
    // Ancestry must describe the actual objects being sent, not a local rewrite.
    // Git replacement refs are disabled on every command above. Legacy grafts
    // have no supported per-command ignore option, so reject their presence.
    let grafts = git(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/grafts",
        ],
    )?;
    match std::fs::symlink_metadata(&grafts) {
        Ok(_) => {
            return Err("legacy Git graft file present; cannot verify actual push ancestry".into())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect Git graft file: {error}")),
    }
    // Older Git versions may not incorporate the override into --git-path.
    if std::env::var_os("GIT_GRAFT_FILE").is_some() {
        return Err("GIT_GRAFT_FILE override prevents actual ancestry validation".into());
    }
    let mut destinations = BTreeSet::new();
    for line in input.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 4 {
            return Err("malformed pre-push record".into());
        }
        let (local, new, remote, old) = (fields[0], fields[1], fields[2], fields[3]);
        if !oid(new) || !oid(old) || new.len() != old.len() {
            return Err("invalid object ID".into());
        }
        git(root, &["check-ref-format", remote])?;
        if !destinations.insert(remote) {
            return Err(format!("duplicate destination {remote}"));
        }
        let deleting = zero(new);
        if deleting {
            if local != "(delete)" || zero(old) {
                return Err("invalid deletion record".into());
            }
        } else {
            if local.starts_with('-') {
                return Err("invalid local revision".into());
            }
            let actual = git(root, &["rev-parse", "--verify", "--end-of-options", local])?;
            if actual != new {
                return Err(format!("stale local ref {local}"));
            }
        }
        if remote == "refs/heads/release" {
            return Err(format!(
                "release branch {remote} requires exact native qualification (use refs/heads/release/<name>)"
            ));
        }
        if remote.starts_with("refs/heads/") {
            if deleting {
                return Err(format!("branch deletion prohibited: {remote}"));
            }
            if git(root, &["cat-file", "-t", new])? != "commit" {
                return Err("branch must name a commit".into());
            }
            if !zero(old) {
                git(root, &["merge-base", "--is-ancestor", old, new])?;
            }
            if let Some(name) = remote.strip_prefix("refs/heads/release/") {
                verify_release_branch(root, name, new).map_err(|e| {
                    format!(
                        "release branch {remote} requires exact native qualification or an accept-verified distillation receipt: {e}"
                    )
                })?;
            }
        } else if let Some(tag) = remote.strip_prefix("refs/tags/") {
            if deleting || !zero(old) {
                return Err(format!("tags are immutable: {remote}"));
            }
            // Unqualified snapshots (`susi-snapshot/*` and distilled candidates
            // `susi-distill/*`) are explicitly distinguished from production
            // releases. A `vX.Y.Z` release tag must point at the commit that adds
            // its qualification record to the exact qualified tree; anything
            // else, and every other tag name, fails closed. No caller flag, cache
            // file or hosted CI assertion is consulted.
            let is_unqualified_snapshot = tag
                .strip_prefix("susi-snapshot/")
                .or_else(|| tag.strip_prefix("susi-distill/"))
                .is_some_and(|name| !name.is_empty());
            if !is_unqualified_snapshot {
                if release_qualification::release_version(tag).is_none() {
                    return Err(format!("release tag {remote} requires exact native qualification (vX.Y.Z with its record)"));
                }
                release_qualification::verify_tag(root, tag, new)
                    .map_err(|e| format!("release tag {remote} is not qualified: {e}"))?;
            }
            git(
                root,
                &["rev-parse", "--verify", &format!("{new}^{{commit}}")],
            )?;
        } else if [
            "refs/claims/",
            "refs/claim-agents/",
            "refs/claim-generation/",
            "refs/closed/",
        ]
        .iter()
        .any(|prefix| remote.starts_with(prefix))
        {
            // Task leases, the claim generation counter and close receipts are
            // blobs, not source branches. `susi tasks claim` moves
            // refs/claim-generation/current in the same atomic push as the
            // claim; refusing it refused every claim. Their coordinator owns
            // authorization and leases; transport must not build.
            if !deleting && git(root, &["cat-file", "-t", new])? != "blob" {
                return Err("task metadata must name a blob".into());
            }
        } else {
            return Err(format!("unsupported ref namespace: {remote}"));
        }
    }
    Ok(())
}

const DISTILL_RECEIPT_SCHEMA: &str = "susi-distillation-receipt-v1";

fn valid_release_branch_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let device = matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    !name.is_empty()
        && name.len() <= 48
        && !device
        && !name.starts_with(['-', '.'])
        && !name.ends_with(['-', '.'])
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
}

fn verify_release_branch(root: &Path, name: &str, new: &str) -> Result<(), String> {
    if !valid_release_branch_name(name) {
        return Err(format!("invalid release branch name `{name}`"));
    }
    if release_qualification::release_version(name).is_some()
        && release_qualification::verify_tag(root, name, new).is_ok()
    {
        return Ok(());
    }

    let commit = git(
        root,
        &["rev-parse", "--verify", &format!("{new}^{{commit}}")],
    )?;
    let lineage = git(root, &["rev-list", "--parents", "-n", "1", &commit])?;
    let parents: Vec<&str> = lineage.split_whitespace().skip(1).collect();
    let [parent] = parents.as_slice() else {
        return Err(
            "the release branch commit must be a single-parent receipt envelope commit".into(),
        );
    };
    let path = format!("releases/distill/{name}.json");
    let changed = git(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--no-renames",
            "--name-only",
            "-r",
            parent,
            &commit,
        ],
    )?;
    if changed != path {
        return Err(format!(
            "the release branch commit must change only {path}, not: {changed}"
        ));
    }
    let out = Command::new("git")
        .current_dir(root)
        .arg("--no-replace-objects")
        .args(["cat-file", "blob", &format!("{commit}:{path}")])
        .output()
        .map_err(|e| format!("cannot run Git: {e}"))?;
    if !out.status.success() {
        return Err(format!("cannot read {path} from {commit}"));
    }
    let mut record: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("{path} is not JSON: {e}"))?;

    let recorded_sha = record["receipt_sha256"]
        .as_str()
        .ok_or("receipt missing `receipt_sha256`")?
        .to_owned();
    if recorded_sha.len() != 64 || !recorded_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("distillation receipt `receipt_sha256` is invalid".into());
    }
    if let Some(map) = record.as_object_mut() {
        map.remove("receipt_sha256");
    }
    let canonical =
        serde_json::to_vec(&record).map_err(|e| format!("cannot canonicalize receipt: {e}"))?;
    if release_qualification::sha256_hex(&canonical) != recorded_sha {
        return Err("distillation receipt digest mismatch".into());
    }

    let expect = |field: &str, wanted: &str| -> Result<(), String> {
        let found = record[field]
            .as_str()
            .ok_or_else(|| format!("receipt `{field}` missing"))?;
        if found == wanted {
            Ok(())
        } else {
            Err(format!("receipt `{field}` is {found}, expected {wanted}"))
        }
    };
    expect("schema", DISTILL_RECEIPT_SCHEMA)?;
    expect("name", name)?;
    expect("branch_ref", &format!("refs/heads/release/{name}"))?;
    expect("qualification_status", "accept_verified_candidate")?;
    expect("final_commit", parent)?;
    let tree = git(root, &["rev-parse", &format!("{parent}^{{tree}}")])?;
    expect("final_tree", &tree)?;

    let rejected = record["rejected"]
        .as_array()
        .ok_or("receipt missing `rejected` array")?;
    if !rejected.is_empty() {
        return Err("release branch receipt contains rejected tasks".into());
    }
    let included = record["included"]
        .as_array()
        .ok_or("receipt missing `included` array")?;
    let base_sat = record["satisfied_in_base"]
        .as_array()
        .ok_or("receipt missing `satisfied_in_base` array")?;
    if included.is_empty() && base_sat.is_empty() {
        return Err("release branch receipt includes no functional tasks".into());
    }

    let tasks = record["accept_verification"]
        .as_array()
        .ok_or("release branch missing accept_verification array")?;
    if !included.is_empty() && tasks.len() != included.len() {
        return Err(
            "release branch accept_verification count does not match included tasks".into(),
        );
    }
    for task in tasks {
        if task["exit_code"].as_i64() != Some(0) || task["passed"].as_bool() != Some(true) {
            return Err("an acceptance task in the release branch receipt did not pass".into());
        }
    }
    Ok(())
}

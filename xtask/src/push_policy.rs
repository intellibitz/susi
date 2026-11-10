//! Provider-neutral Git transport safety. Release tags are admitted only with
//! the exact-tree qualification record (`release_qualification.rs`). Release
//! candidate branches are local evidence, never a remote merge surface.
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
        if remote.starts_with("refs/heads/") {
            let allowed_head = remote == "refs/heads/main"
                || remote
                    .strip_prefix("refs/heads/local-agent/")
                    .is_some_and(|agent| !agent.is_empty() && !agent.contains('/'));
            if !allowed_head {
                return Err(format!(
                    "remote branch {remote} is forbidden: GitHub is storage only; allowed remote branches are refs/heads/main and refs/heads/local-agent/<agent>; store candidates under refs/candidates/release/<name> or refs/candidates/distill/<name>"
                ));
            }
            if deleting {
                return Err(format!("branch deletion prohibited: {remote}"));
            }
            if git(root, &["cat-file", "-t", new])? != "commit" {
                return Err("branch must name a commit".into());
            }
            if !zero(old) {
                git(root, &["merge-base", "--is-ancestor", old, new])?;
            }
        } else if let Some(candidate) = remote.strip_prefix("refs/candidates/") {
            let valid_candidate = candidate
                .strip_prefix("release/")
                .or_else(|| candidate.strip_prefix("distill/"))
                .is_some_and(|name| !name.is_empty());
            if !valid_candidate {
                return Err(format!(
                    "candidate ref {remote} must use refs/candidates/release/<name> or refs/candidates/distill/<name>"
                ));
            }
            if deleting {
                return Err(format!("candidate ref deletion prohibited: {remote}"));
            }
            if git(root, &["cat-file", "-t", new])? != "commit" {
                return Err("candidate ref must name a commit".into());
            }
            if !zero(old) {
                git(root, &["merge-base", "--is-ancestor", old, new])?;
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

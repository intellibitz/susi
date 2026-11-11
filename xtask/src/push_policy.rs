//! Provider-neutral Git transport safety. Release tags are admitted only with
//! the exact-tree qualification record (`release_qualification.rs`). Release
//! candidate branches are local evidence, never a remote merge surface. Agents
//! publish to the non-branch ref `refs/agents/<agent>` (docs/agent-refs.md), so
//! `refs/heads/main` is the only branch the host ever offers a pull request on.
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
/// An agent name is one non-empty ref component (the rule `local-agent/<agent>`
/// branches used); `git check-ref-format` has already vetted its characters.
fn valid_agent_name(agent: &str) -> bool {
    !agent.is_empty() && !agent.contains('/')
}

fn is_preservation_destination(remote: &str) -> bool {
    remote == "refs/heads/main"
        || remote
            .strip_prefix("refs/agents/")
            .is_some_and(valid_agent_name)
        || remote
            .strip_prefix("refs/candidates/release/")
            .or_else(|| remote.strip_prefix("refs/candidates/distill/"))
            .or_else(|| remote.strip_prefix("refs/tags/susi-distill/"))
            .or_else(|| remote.strip_prefix("refs/tags/susi-snapshot/"))
            .is_some_and(|name| !name.is_empty())
}

fn is_preserved_for_retirement(root: &Path, old: &str, co_preserved: &[&str]) -> bool {
    if git(
        root,
        &[
            "for-each-ref",
            &format!("--contains={old}"),
            "--count=1",
            "--format=ok",
            "refs/tags/susi-distill",
            "refs/tags/susi-snapshot",
            "refs/candidates",
            // Agent refs, as fetched (docs/agent-refs.md) or kept locally.
            "refs/agents",
            "refs/remotes/*-agents/*",
        ],
    )
    .is_ok_and(|s| !s.is_empty())
    {
        return true;
    }
    co_preserved
        .iter()
        .any(|co_new| git(root, &["merge-base", "--is-ancestor", old, co_new]).is_ok())
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
    let co_preserved: Vec<&str> = input
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() == 4
                && oid(fields[1])
                && !zero(fields[1])
                && is_preservation_destination(fields[2])
            {
                Some(fields[1])
            } else {
                None
            }
        })
        .collect();
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
        if let Some(agent) = remote.strip_prefix("refs/agents/") {
            // An agent's inbox (docs/agent-refs.md). Not a branch, so the Git
            // host offers no pull request for it; `main` stays the only branch.
            if !valid_agent_name(agent) {
                return Err(format!(
                    "agent ref {remote} must be refs/agents/<agent> (one component)"
                ));
            }
            if !deleting {
                if git(root, &["cat-file", "-t", new])? != "commit" {
                    return Err("agent ref must name a commit".into());
                }
                if !zero(old) {
                    git(root, &["merge-base", "--is-ancestor", old, new])
                        .map_err(|_| format!("non-fast-forward update of {remote} refused"))?;
                }
            }
            // A deletion names the tip the remote advertised (checked above);
            // the receiving server compares and swaps it.
        } else if remote.starts_with("refs/heads/") {
            if remote != "refs/heads/main" {
                let agent = remote
                    .strip_prefix("refs/heads/local-agent/")
                    .filter(|agent| valid_agent_name(agent));
                if deleting {
                    // Retiring a branch GitHub must no longer carry (old
                    // release/*, release-staging/*, distill/*, wip/* and other
                    // names outside the allowlist). The deletion record was
                    // validated above (named old object, no force). An agent
                    // branch's tip must be preserved first: under
                    // refs/agents/<agent> (docs/agent-refs.md), a susi-distill
                    // or susi-snapshot tag, or a candidate ref, locally or in
                    // the same push.
                    if agent.is_none() || is_preserved_for_retirement(root, old, &co_preserved) {
                        continue;
                    }
                    return Err(format!(
                        "branch deletion prohibited: {remote}: preserve {old} under refs/agents/<agent>, refs/tags/susi-distill/* or refs/candidates/* first"
                    ));
                }
                if let Some(agent) = agent {
                    return Err(format!(
                        "remote branch {remote} is retired: agents publish to refs/agents/{agent} (git push origin local-agent/{agent}:refs/agents/{agent}); see docs/agent-refs.md"
                    ));
                }
                return Err(format!(
                    "remote branch {remote} is forbidden: GitHub is storage only; the only remote branch is refs/heads/main; agents publish to refs/agents/<agent>; store candidates under refs/candidates/release/<name> or refs/candidates/distill/<name>"
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

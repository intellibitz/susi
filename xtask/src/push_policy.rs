//! Provider-neutral Git transport safety, separate from release qualification.
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(root)
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
            if deleting {
                return Err(format!("branch deletion prohibited: {remote}"));
            }
            if git(root, &["cat-file", "-t", new])? != "commit" {
                return Err("branch must name a commit".into());
            }
            if !zero(old) {
                git(root, &["merge-base", "--is-ancestor", old, new])?;
            }
        } else if let Some(tag) = remote.strip_prefix("refs/tags/") {
            if deleting || !zero(old) {
                return Err(format!("tags are immutable: {remote}"));
            }
            // Unqualified snapshots are explicitly distinguished from production releases.
            // No native exact-tag qualification adapter exists yet. Fail closed rather
            // than trust a caller flag, old cache file, or hosted CI assertion.
            if !tag.starts_with("susi-snapshot/") {
                return Err(format!("release tag {remote} requires exact native qualification; adapter not yet available"));
            }
            git(
                root,
                &["rev-parse", "--verify", &format!("{new}^{{commit}}")],
            )?;
        } else if ["refs/claims/", "refs/claim-agents/", "refs/closed/"]
            .iter()
            .any(|prefix| remote.starts_with(prefix))
        {
            // Task leases and close receipts are blobs, not source branches. Their
            // coordinator owns authorization and leases; transport must not build.
            if !deleting && git(root, &["cat-file", "-t", new])? != "blob" {
                return Err("task metadata must name a blob".into());
            }
        } else {
            return Err(format!("unsupported ref namespace: {remote}"));
        }
    }
    Ok(())
}

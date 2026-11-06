//! The queue's compare-and-swap push.
//!
//! A claim, a renewal, a release and a close receipt are compare-and-swap
//! pushes of blobs to `refs/claims/*`, `refs/claim-agents/*`,
//! `refs/claim-generation/current`, `refs/closed/*` and `refs/abandoned/*`
//! (see [`super::tasks`]), and a release cut reserves its version under
//! `refs/release-cuts/*` (see `super::release`). Each is one atomic push under
//! `--force-with-lease`.
//!
//! A push the remote refused while every lease still held used to be relayed
//! through a request branch that a GitHub Actions workflow replayed. GitHub
//! Actions workflows were removed (CEO decision 2026-11-06), so the remote's
//! refusal is now returned as it is; a lost lease is still reported in the
//! caller's own words, so a caller's retry matcher still sees it.
use crate::susi_error::{EaiError, EaiResult};
use std::collections::HashMap;
use std::path::Path;

/// A ref the transaction expects on the remote: `(name, sha)`; the empty sha
/// means "absent".
pub(super) type Lease = (String, String);
/// A ref the transaction writes: a blob's sha, or `None` to delete the ref.
pub(super) type Update = (String, Option<String>);

/// Push `updates` to `remote` under `leases`, atomically. `raced` is what the
/// caller says when a lease was lost; it is returned verbatim. Any other
/// refusal is the remote's own error.
pub(super) fn push(
    ws: &Path,
    remote: &str,
    leases: &[Lease],
    updates: &[Update],
    raced: &str,
) -> EaiResult<()> {
    if updates.is_empty() {
        return Ok(());
    }
    let refused = match direct(ws, remote, leases, updates) {
        Ok(()) => return Ok(()),
        Err(err) => err,
    };
    // A lease that no longer holds is a lost race, whatever the refusal said.
    if !leases_hold(ws, remote, leases)? {
        return Err(EaiError::config(raced.to_string()));
    }
    Err(refused)
}

fn direct(ws: &Path, remote: &str, leases: &[Lease], updates: &[Update]) -> EaiResult<()> {
    let mut args: Vec<String> = vec!["push".into(), "--quiet".into(), "--atomic".into()];
    args.extend(
        leases
            .iter()
            .map(|(name, old)| format!("--force-with-lease={name}:{old}")),
    );
    args.push(remote.to_string());
    args.extend(updates.iter().map(|(name, blob)| match blob {
        Some(sha) => format!("{sha}:{name}"),
        None => format!(":{name}"),
    }));
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    git(ws, &argv).map(|_| ())
}

fn leases_hold(ws: &Path, remote: &str, leases: &[Lease]) -> EaiResult<bool> {
    // One round trip for every lease.
    let names: Vec<String> = leases.iter().map(|(name, _)| name.clone()).collect();
    let present = remote_map(ws, remote, &names)?;
    Ok(leases
        .iter()
        .all(|(name, old)| present.get(name).map(String::as_str).unwrap_or("") == old.as_str()))
}

/// `git ls-remote` of every name in one round trip. A name it omits is absent.
fn remote_map(ws: &Path, remote: &str, names: &[String]) -> EaiResult<HashMap<String, String>> {
    let mut args = Vec::with_capacity(names.len() + 2);
    args.push("ls-remote".to_string());
    args.push(remote.to_string());
    args.extend(names.iter().cloned());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let listed = git(ws, &argv)?;
    let mut present = HashMap::with_capacity(names.len());
    for line in listed.lines() {
        let mut parts = line.split_whitespace();
        let Some(sha) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        if sha.is_empty() || name.is_empty() {
            continue;
        }
        present.insert(name.to_string(), sha.to_string());
    }
    Ok(present)
}

fn git(ws: &Path, args: &[&str]) -> EaiResult<String> {
    let out = super::tasks::git_command()
        .args(args)
        .current_dir(ws)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !out.status.success() {
        return Err(EaiError::process(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
// Test code: a failed fixture step is a failed test, so unwrap is the assertion.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Stdio;

    struct Fixture {
        root: PathBuf,
        bare: PathBuf,
        clone: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn run(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap()
    }

    fn fixture(tag: &str) -> Fixture {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "susi-queue-push-{tag}-{}-{unique}",
            std::process::id()
        ));
        let bare = root.join("server.git");
        let clone = root.join("clone");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&clone).unwrap();
        run(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        run(&clone, &["init", "--quiet", "-b", "main"]);
        run(&clone, &["config", "user.name", "AGENTONE"]);
        run(&clone, &["config", "user.email", "agentone@localhost"]);
        run(&clone, &["config", "commit.gpgsign", "false"]);
        run(&clone, &["remote", "add", "origin", bare.to_str().unwrap()]);
        std::fs::write(clone.join("README"), "fixture\n").unwrap();
        run(&clone, &["add", "-A"]);
        run(&clone, &["commit", "--quiet", "-m", "init"]);
        run(&clone, &["push", "--quiet", "origin", "HEAD:main"]);
        Fixture { root, bare, clone }
    }

    fn blob(ws: &Path, text: &str) -> String {
        let mut child = super::super::tasks::git_command()
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(ws)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A proxy that carries nothing outside `refs/heads/*`.
    fn refuse_queue_pushes(bare: &Path) {
        let hook = bare.join("hooks").join("pre-receive");
        std::fs::write(
            &hook,
            "#!/bin/sh\nwhile read -r _o _n ref; do case \"$ref\" in refs/heads/*) ;; *) \
             echo \"HTTP 403 ($ref)\" >&2; exit 1 ;; esac; done\n",
        )
        .unwrap();
        // Git for Windows runs a hook without an executable bit.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn branches(bare: &Path) -> String {
        run(
            bare,
            &["for-each-ref", "--format=%(refname)", "refs/heads/"],
        )
    }

    #[test]
    fn a_push_lands_and_a_deletion_lands_under_its_lease() {
        let f = fixture("direct");
        let sha = blob(&f.clone, r#"{"task":"T-AGENTONE-1"}"#);
        let name = "refs/claims/T-AGENTONE-1".to_string();
        push(
            &f.clone,
            "origin",
            &[(name.clone(), String::new())],
            &[(name.clone(), Some(sha.clone()))],
            "raced",
        )
        .unwrap();
        assert_eq!(run(&f.bare, &["rev-parse", &name]), sha);
        push(
            &f.clone,
            "origin",
            &[(name.clone(), sha)],
            &[(name.clone(), None)],
            "raced",
        )
        .unwrap();
        assert!(run(&f.bare, &["for-each-ref", "refs/claims/"]).is_empty());
        assert_eq!(branches(&f.bare), "refs/heads/main");
    }

    #[test]
    fn a_lost_lease_is_reported_as_the_race() {
        let f = fixture("race");
        let name = "refs/claims/T-AGENTONE-2".to_string();
        let theirs = blob(&f.clone, r#"{"agent":"OTHER"}"#);
        run(
            &f.clone,
            &["push", "--quiet", "origin", &format!("{theirs}:{name}")],
        );
        let mine = blob(&f.clone, r#"{"agent":"AGENTONE"}"#);
        // Expecting the ref absent, finding it taken: the caller's word, verbatim.
        let err = push(
            &f.clone,
            "origin",
            &[(name.clone(), String::new())],
            &[(name.clone(), Some(mine))],
            "T-AGENTONE-2 was claimed concurrently with another agent; refresh the queue and retry",
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("claimed concurrently with another agent"),
            "{err}"
        );
        assert_eq!(run(&f.bare, &["rev-parse", &name]), theirs);
    }

    #[test]
    fn a_refused_push_returns_the_remotes_refusal_and_publishes_no_request() {
        let f = fixture("refused");
        refuse_queue_pushes(&f.bare);
        let name = "refs/claims/T-AGENTONE-3".to_string();
        let mine = blob(&f.clone, r#"{"agent":"AGENTONE"}"#);
        let err = push(
            &f.clone,
            "origin",
            &[(name.clone(), String::new())],
            &[(name.clone(), Some(mine))],
            "raced",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("HTTP 403"), "{err}");
        assert!(!err.contains("raced"), "{err}");
        assert!(run(&f.bare, &["for-each-ref", "refs/claims/"]).is_empty());
        assert_eq!(branches(&f.bare), "refs/heads/main");
    }
}

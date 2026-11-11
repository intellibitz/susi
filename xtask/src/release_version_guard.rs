//! One release version, one cut.
//!
//! `release-qualify` used to look for the tag in the local checkout only, so
//! two agents could qualify two different trees as the same `vX.Y.Z` (it
//! happened for v0.21.62: one cut came from a `release-lanes` line, the other
//! from a distilled candidate, and the second to finish could not publish).
//! Before any gate runs, the cut is now checked against the storage remote and
//! reserved there:
//!
//! - a tag that is already published is refused;
//! - an open release line `refs/candidates/release/<tag>` must be contained in
//!   the tree being qualified, so a cut never bypasses a line other agents are
//!   advancing;
//! - the version is reserved with a create-only blob
//!   `refs/claims/release-version/<tag>`. Git refuses to replace an existing
//!   ref without force, so of two cuts exactly one wins. A reservation is
//!   renewed by the same agent for the same commit, taken over only when it is
//!   older than [`STALE_SECS`], and dropped when the gates fail.
//!
//! Everything is plain Git against the remote: no hosting API.
#![forbid(unsafe_code)]

use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Where a version reservation lives on the remote.
pub const CLAIM_PREFIX: &str = "refs/claims/release-version/";
/// Where `release-lanes` keeps a release line.
pub const LINE_PREFIX: &str = "refs/candidates/release/";
/// A reservation older than this may be taken over (a full qualification on
/// the slowest supported host takes under one hour).
pub const STALE_SECS: u64 = 4 * 60 * 60;

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("cannot run Git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// What the remote holds for one version.
#[derive(Debug, Default, PartialEq, Eq)]
struct RemoteState {
    tag: Option<String>,
    line: Option<String>,
    claim: Option<String>,
}

fn remote_state(root: &Path, remote: &str, tag: &str) -> Result<RemoteState, String> {
    let tag_ref = format!("refs/tags/{tag}");
    let line_ref = format!("{LINE_PREFIX}{tag}");
    let claim_ref = format!("{CLAIM_PREFIX}{tag}");
    let listing = git(
        root,
        &[
            "ls-remote",
            "--refs",
            remote,
            &tag_ref,
            &line_ref,
            &claim_ref,
        ],
    )
    .map_err(|e| {
        format!(
            "cannot ask `{remote}` whether {tag} is free ({e}); \
             pass --skip-remote-check only when the remote cannot be reached"
        )
    })?;
    let mut state = RemoteState::default();
    for line in listing.lines() {
        let mut fields = line.split_whitespace();
        let (Some(sha), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        let slot = if name == tag_ref {
            &mut state.tag
        } else if name == line_ref {
            &mut state.line
        } else if name == claim_ref {
            &mut state.claim
        } else {
            continue;
        };
        *slot = Some(sha.to_owned());
    }
    Ok(state)
}

/// Refuse a cut of `tag` at `head` that the remote already contradicts.
/// Returns the remote's reservation for the version, if any. Writes nothing.
pub fn check(root: &Path, remote: &str, tag: &str, head: &str) -> Result<Option<String>, String> {
    let state = remote_state(root, remote, tag)?;
    if let Some(published) = state.tag {
        return Err(format!(
            "{tag} is already published on `{remote}` ({published}); \
             release tags are immutable, so cut the next version"
        ));
    }
    if let Some(line) = state.line {
        let line_ref = format!("{LINE_PREFIX}{tag}");
        if git(root, &["cat-file", "-e", &format!("{line}^{{commit}}")]).is_err() {
            git(root, &["fetch", "--quiet", remote, &line_ref])?;
        }
        if git(root, &["merge-base", "--is-ancestor", &line, head]).is_err() {
            return Err(format!(
                "a release line for {tag} is open on `{remote}` ({line_ref} at {line}) \
                 and this tree does not contain it; qualify a tree built on that line, \
                 or cut the next version"
            ));
        }
    }
    Ok(state.claim)
}

fn read_claim(root: &Path, remote: &str, tag: &str, sha: &str) -> Option<Value> {
    if git(root, &["cat-file", "-e", sha]).is_err() {
        let claim_ref = format!("{CLAIM_PREFIX}{tag}");
        git(root, &["fetch", "--quiet", remote, &claim_ref]).ok()?;
    }
    serde_json::from_str(&git(root, &["cat-file", "blob", sha]).ok()?).ok()
}

fn write_blob(root: &Path, bytes: &[u8]) -> Result<String, String> {
    let mut child = Command::new("git")
        .current_dir(root)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run Git: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("git hash-object has no stdin")?
        .write_all(bytes)
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn delete_claim(root: &Path, remote: &str, tag: &str, old: &str) -> Result<(), String> {
    let claim_ref = format!("{CLAIM_PREFIX}{tag}");
    let lease = format!("--force-with-lease={claim_ref}:{old}");
    git(
        root,
        &["push", "--quiet", &lease, remote, &format!(":{claim_ref}")],
    )
    .map(|_| ())
}

/// Who is cutting a release, on which host, and when.
#[derive(Debug, Clone, Copy)]
pub struct Holder<'a> {
    pub agent: &'a str,
    pub host: &'a str,
    pub now: u64,
}

/// [`check`], then reserve `tag` for `who.agent` cutting `head`. Exactly one of
/// two concurrent cuts gets the reservation.
pub fn reserve(
    root: &Path,
    remote: &str,
    tag: &str,
    head: &str,
    who: &Holder<'_>,
) -> Result<(), String> {
    let Holder { agent, host, now } = *who;
    if let Some(old) = check(root, remote, tag, head)? {
        let held = read_claim(root, remote, tag, &old).unwrap_or(Value::Null);
        let holder = held["agent"].as_str().unwrap_or("an unknown agent");
        if holder == agent && held["commit"].as_str() == Some(head) {
            return Ok(());
        }
        let claimed = held["claimed_unix"].as_u64().unwrap_or(0);
        if now.saturating_sub(claimed) < STALE_SECS {
            return Err(format!(
                "{tag} is being cut by {holder} on {} from commit {} (reserved at unix {claimed}); \
                 let that cut finish, or cut the next version",
                held["host"].as_str().unwrap_or("an unknown host"),
                held["commit"].as_str().unwrap_or("unknown"),
            ));
        }
        delete_claim(root, remote, tag, &old)
            .map_err(|e| format!("another agent took over the stale reservation of {tag}: {e}"))?;
    }
    let record = json!({
        "schema": "susi-release-version-claim-v1",
        "tag": tag,
        "agent": agent,
        "host": host,
        "commit": head,
        "claimed_unix": now,
    });
    let blob = write_blob(root, record.to_string().as_bytes())?;
    let claim_ref = format!("{CLAIM_PREFIX}{tag}");
    git(
        root,
        &["push", "--quiet", remote, &format!("{blob}:{claim_ref}")],
    )
    .map(|_| ())
    .map_err(|e| format!("another cut reserved {tag} first: {e}"))
}

/// Drop this agent's reservation of `tag` (its gates failed). Best effort: a
/// reservation that cannot be dropped expires after [`STALE_SECS`].
pub fn release(root: &Path, remote: &str, tag: &str, head: &str, agent: &str) {
    let Ok(RemoteState {
        claim: Some(old), ..
    }) = remote_state(root, remote, tag)
    else {
        return;
    };
    let Some(held) = read_claim(root, remote, tag, &old) else {
        return;
    };
    if held["agent"].as_str() == Some(agent) && held["commit"].as_str() == Some(head) {
        let _ = delete_claim(root, remote, tag, &old);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::path::PathBuf;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "susi-version-guard-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let remote = dir.join("remote.git");
            run(
                &dir,
                &["init", "--quiet", "--bare", remote.to_str().unwrap()],
            );
            Self { dir }
        }

        /// A clone with one commit on `main`, pushed.
        fn clone(&self, who: &str) -> (PathBuf, String) {
            let work = self.dir.join(who);
            let remote = self.dir.join("remote.git");
            run(
                &self.dir,
                &[
                    "clone",
                    "--quiet",
                    remote.to_str().unwrap(),
                    work.to_str().unwrap(),
                ],
            );
            for (key, value) in [
                ("user.name", who),
                ("user.email", "agent@localhost"),
                ("core.hooksPath", "no-hooks"),
                ("commit.gpgsign", "false"),
            ] {
                run(&work, &["config", key, value]);
            }
            let head = commit(&work, who);
            (work, head)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn run(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap()
    }

    fn commit(work: &Path, text: &str) -> String {
        std::fs::write(work.join(format!("{text}.txt")), text).unwrap();
        run(work, &["add", "-A"]);
        run(work, &["commit", "--quiet", "-m", text]);
        run(work, &["rev-parse", "HEAD"])
    }

    #[test]
    fn of_two_cuts_of_one_version_exactly_one_reserves_it() {
        let fx = Fixture::new("race");
        let (alice, a_head) = fx.clone("alice");
        let (bob, b_head) = fx.clone("bob");
        reserve(
            &alice,
            "origin",
            "v1.2.3",
            &a_head,
            &Holder {
                agent: "alice",
                host: "linux",
                now: 1_000,
            },
        )
        .unwrap();
        let refused = reserve(
            &bob,
            "origin",
            "v1.2.3",
            &b_head,
            &Holder {
                agent: "bob",
                host: "windows",
                now: 1_500,
            },
        )
        .unwrap_err();
        assert!(refused.contains("being cut by alice on linux"), "{refused}");
        // The holder renews; another version is free.
        reserve(
            &alice,
            "origin",
            "v1.2.3",
            &a_head,
            &Holder {
                agent: "alice",
                host: "linux",
                now: 2_000,
            },
        )
        .unwrap();
        reserve(
            &bob,
            "origin",
            "v1.2.4",
            &b_head,
            &Holder {
                agent: "bob",
                host: "windows",
                now: 1_500,
            },
        )
        .unwrap();
        // A failed cut gives the version back, but only its own reservation.
        release(&bob, "origin", "v1.2.3", &b_head, "bob");
        assert!(check(&bob, "origin", "v1.2.3", &b_head).unwrap().is_some());
        release(&alice, "origin", "v1.2.3", &a_head, "alice");
        assert!(check(&bob, "origin", "v1.2.3", &b_head).unwrap().is_none());
        reserve(
            &bob,
            "origin",
            "v1.2.3",
            &b_head,
            &Holder {
                agent: "bob",
                host: "windows",
                now: 3_000,
            },
        )
        .unwrap();
    }

    #[test]
    fn a_stale_reservation_is_taken_over_and_a_fresh_one_is_not() {
        let fx = Fixture::new("stale");
        let (alice, a_head) = fx.clone("alice");
        let (bob, b_head) = fx.clone("bob");
        reserve(
            &alice,
            "origin",
            "v2.0.0",
            &a_head,
            &Holder {
                agent: "alice",
                host: "linux",
                now: 10,
            },
        )
        .unwrap();
        let fresh = 10 + STALE_SECS - 1;
        assert!(reserve(
            &bob,
            "origin",
            "v2.0.0",
            &b_head,
            &Holder {
                agent: "bob",
                host: "windows",
                now: fresh
            }
        )
        .is_err());
        let stale = 10 + STALE_SECS;
        reserve(
            &bob,
            "origin",
            "v2.0.0",
            &b_head,
            &Holder {
                agent: "bob",
                host: "windows",
                now: stale,
            },
        )
        .unwrap();
        let again = reserve(
            &alice,
            "origin",
            "v2.0.0",
            &a_head,
            &Holder {
                agent: "alice",
                host: "linux",
                now: stale + 1,
            },
        )
        .unwrap_err();
        assert!(again.contains("being cut by bob"), "{again}");
    }

    #[test]
    fn a_published_tag_and_a_bypassed_release_line_are_refused() {
        let fx = Fixture::new("line");
        let (alice, _) = fx.clone("alice");
        run(
            &alice,
            &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
        );
        let (bob, _) = fx.clone("bob");
        let b_head = run(&bob, &["rev-parse", "HEAD"]);

        // Alice opens a release line; Bob's unrelated tree must not cut it.
        let line = commit(&alice, "line");
        run(
            &alice,
            &[
                "push",
                "--quiet",
                "origin",
                &format!("{line}:{LINE_PREFIX}v3.0.0"),
            ],
        );
        let bypass = check(&bob, "origin", "v3.0.0", &b_head).unwrap_err();
        assert!(
            bypass.contains("release line for v3.0.0 is open"),
            "{bypass}"
        );
        // A tree built on the line may.
        let on_line = commit(&alice, "bump");
        assert_eq!(check(&alice, "origin", "v3.0.0", &on_line), Ok(None));

        // Once the tag is published nobody cuts that version again.
        run(&alice, &["tag", "v3.0.0", &on_line]);
        run(&alice, &["push", "--quiet", "origin", "refs/tags/v3.0.0"]);
        let published = check(&alice, "origin", "v3.0.0", &on_line).unwrap_err();
        assert!(published.contains("already published"), "{published}");
        assert!(reserve(
            &bob,
            "origin",
            "v3.0.0",
            &b_head,
            &Holder {
                agent: "bob",
                host: "windows",
                now: 5
            }
        )
        .is_err());
    }

    #[test]
    fn an_unreachable_remote_is_an_error_not_a_free_version() {
        let fx = Fixture::new("offline");
        let (alice, a_head) = fx.clone("alice");
        let error = check(&alice, "nowhere", "v4.0.0", &a_head).unwrap_err();
        assert!(error.contains("--skip-remote-check"), "{error}");
    }
}

//! Commits the Git host created instead of Susi (T-FURY-1).
//!
//! The CEO's rule: GitHub (or any later host) is storage only, and every merge
//! happens in Susi, governed by the agents or the harness. A commit object the
//! hosting service wrote itself (a web pull-request merge, a squash or rebase
//! merge, a web edit, or a commit made through the host's REST or GraphQL API)
//! records the host's service identity as its committer. Local Git, the Susi
//! publisher (`local-snapshot submit`) and the distiller never do: they commit
//! with the agent's own identity.
//!
//! This module only observes. It reads committer identities through
//! `git log` and never contacts the host. `distill lint` reports such commits
//! as findings; `distill plan` and `local-distill` refuse a task whose commits
//! the host created unless the planning agent adopts each commit by id, and
//! that adoption is written into the manifest or receipt.
//!
//! The built-in identity is the one the current host stamps. Other hosts add
//! theirs without a code change: `git config --add susi.hostCommitter <email>`.
//! A host whose web merges reuse the clicking user's identity cannot be
//! detected this way; the docs say so.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Stdio};

/// Committer e-mail addresses the Git host uses for objects it writes itself.
pub const BUILTIN: [(&str, &str); 1] = [(
    "noreply@github.com",
    "web pull-request merges, web edits and REST/GraphQL commits on the current host",
)];

/// Multi-valued Git configuration key for further host identities.
pub const CONFIG_KEY: &str = "susi.hostCommitter";

/// Upper bound on commits read in one scan.
pub const MAX_SCAN: usize = 200_000;

const US: char = '\u{1f}';
const RS: char = '\u{1e}';

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCommit {
    pub oid: String,
    pub merge: bool,
    pub committer: String,
    pub email: String,
    pub subject: String,
}

fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Normalise an identity for comparison: trimmed, angle brackets removed,
/// ASCII-lowercased.
pub fn normalise(email: &str) -> String {
    email
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_ascii_lowercase()
}

/// The built-in identities plus any configured ones.
pub fn identities_with(configured: &[String]) -> BTreeSet<String> {
    BUILTIN
        .iter()
        .map(|(e, _)| normalise(e))
        .chain(configured.iter().map(|e| normalise(e)))
        .filter(|e| !e.is_empty())
        .collect()
}

fn git(root: &Path, args: &[&str]) -> Result<(i32, Vec<u8>), String> {
    let out = Command::new("git")
        .current_dir(root)
        .arg("--no-replace-objects")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    Ok((out.status.code().unwrap_or(-1), out.stdout))
}

/// Identities from the repository's Git configuration (exit 1 = none set).
pub fn identities(root: &Path) -> Result<BTreeSet<String>, String> {
    let (code, out) = git(root, &["config", "--get-all", CONFIG_KEY])?;
    let configured: Vec<String> = match code {
        0 => String::from_utf8_lossy(&out)
            .lines()
            .map(str::to_owned)
            .collect(),
        1 => Vec::new(),
        other => return Err(format!("git config --get-all {CONFIG_KEY} exited {other}")),
    };
    Ok(identities_with(&configured))
}

/// Parse `--format=%x1e%H%x1f%P%x1f%cn%x1f%ce%x1f%s` output and keep the
/// commits whose committer is a host identity.
pub fn parse(text: &str, hosts: &BTreeSet<String>) -> Vec<HostCommit> {
    let mut found = Vec::new();
    for chunk in text.split(RS) {
        let chunk = chunk.trim_matches(['\n', '\r']);
        if chunk.is_empty() {
            continue;
        }
        let mut p = chunk.splitn(5, US);
        let oid = p.next().unwrap_or_default().trim();
        let parents = p.next().unwrap_or_default();
        let name = p.next().unwrap_or_default();
        let email = p.next().unwrap_or_default();
        let subject = p.next().unwrap_or_default();
        if !is_oid(oid) || !hosts.contains(&normalise(email)) {
            continue;
        }
        found.push(HostCommit {
            oid: oid.to_owned(),
            merge: parents.split_whitespace().count() > 1,
            committer: name.to_owned(),
            email: normalise(email),
            subject: subject.to_owned(),
        });
    }
    found
}

const FORMAT: &str = "--format=%x1e%H%x1f%P%x1f%cn%x1f%ce%x1f%s";

/// Host-created commits, merges included, in a revision range
/// (`<from>..<to>` or a single revision for its whole history).
pub fn scan_range(
    root: &Path,
    range: &str,
    hosts: &BTreeSet<String>,
) -> Result<Vec<HostCommit>, String> {
    if range.starts_with('-') {
        return Err(format!("refusing revision range {range:?}"));
    }
    let (code, count) = git(root, &["rev-list", "--count", range, "--"])?;
    if code != 0 {
        return Err(format!("git rev-list --count {range} exited {code}"));
    }
    let count: usize = String::from_utf8_lossy(&count)
        .trim()
        .parse()
        .map_err(|_| "bad rev-list count".to_owned())?;
    if count > MAX_SCAN {
        return Err(format!(
            "{count} commits in {range} exceed the {MAX_SCAN} scan limit; narrow the range"
        ));
    }
    let (code, out) = git(root, &["log", "--topo-order", FORMAT, range, "--"])?;
    if code != 0 {
        return Err(format!("git log {range} exited {code}"));
    }
    Ok(parse(&String::from_utf8_lossy(&out), hosts))
}

/// Of the named commits, those the host created (one `git log --no-walk`).
pub fn among(
    root: &Path,
    oids: &BTreeSet<String>,
    hosts: &BTreeSet<String>,
) -> Result<BTreeMap<String, HostCommit>, String> {
    let wanted: Vec<&str> = oids
        .iter()
        .map(String::as_str)
        .filter(|o| is_oid(o))
        .collect();
    let mut found = BTreeMap::new();
    for chunk in wanted.chunks(500) {
        let mut args = vec!["log", "--no-walk=unsorted", FORMAT];
        args.extend_from_slice(chunk);
        args.push("--");
        let (code, out) = git(root, &args)?;
        if code != 0 {
            return Err(format!(
                "git log --no-walk over {} commit(s) exited {code}",
                chunk.len()
            ));
        }
        for c in parse(&String::from_utf8_lossy(&out), hosts) {
            found.insert(c.oid.clone(), c);
        }
    }
    Ok(found)
}

/// Adoption ids given on the command line: full object ids only, so an
/// abbreviation can never adopt more than the agent meant.
pub fn parse_adoptions(values: &[String]) -> Result<BTreeSet<String>, String> {
    values
        .iter()
        .flat_map(|v| v.split([',', ' ']))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| {
            if is_oid(v) {
                Ok(v.to_ascii_lowercase())
            } else {
                Err(format!(
                    "--adopt-host-commit needs a full commit id, got {v:?}"
                ))
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn line(oid: &str, parents: &str, name: &str, email: &str, subject: &str) -> String {
        format!("{RS}{oid}{US}{parents}{US}{name}{US}{email}{US}{subject}\n")
    }

    #[test]
    fn only_host_identities_match_and_merges_are_marked() {
        let hosts = identities_with(&[]);
        let text = [
            line(
                A,
                &format!("{B} {C}"),
                "GitHub",
                "noreply@github.com",
                "Merge pull request #1",
            ),
            line(B, C, "Antigravity", "NoReply@GitHub.com", "api commit"),
            // A user's private noreply address is the user's own local commit.
            line(
                C,
                A,
                "dev",
                "123+dev@users.noreply.github.com",
                "local work",
            ),
        ]
        .concat();
        let found = parse(&text, &hosts);
        assert_eq!(found.len(), 2);
        assert!(found[0].merge && found[0].oid == A);
        assert!(!found[1].merge && found[1].email == "noreply@github.com");
    }

    #[test]
    fn configured_identities_extend_the_builtin_set() {
        let hosts = identities_with(&["<Web@Git.Example>".to_owned()]);
        assert!(hosts.contains("web@git.example"));
        assert!(hosts.contains("noreply@github.com"));
        let found = parse(&line(A, B, "host", "web@git.example", "s"), &hosts);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn adoptions_need_full_ids() {
        let ok = parse_adoptions(&[format!("{A},{}", B.to_ascii_uppercase())]).unwrap();
        assert_eq!(ok.len(), 2);
        assert!(ok.contains(B));
        assert!(parse_adoptions(&["abc123".to_owned()]).is_err());
    }
}

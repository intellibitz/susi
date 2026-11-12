//! Provider-neutral storage guard: nothing published is lost unnoticed.
//!
//! A storage remote is a bucket (CEO Mandate 74): it may not offer, and Susi
//! must not depend on, host-side branch or tag protection. `push-policy` stops
//! a cooperating clone from rewriting history, but it is a client hook: a push
//! with `--no-verify`, or from a clone without the hooks, is not checked.
//!
//! The guard closes that gap with Git alone. Every clone that runs it is a
//! witness: it keeps the last observed value of each protected remote ref as a
//! local ref under `refs/susi-guard/<remote>/`, which also keeps those objects
//! alive in this clone. Each run compares the remote with that record:
//!
//! - `refs/heads/main` and `refs/candidates/*` are append-only: the remote
//!   value must descend from the recorded one;
//! - `refs/tags/*` are immutable: the remote value must equal the recorded one;
//! - a recorded ref must still exist.
//!
//! A violation is reported with the retained objects and a recovery that needs
//! no force. The record of a violated ref is kept, so the loss stays reported
//! until it is repaired. It works with any Git remote: a hosted service, a
//! self-hosted server or a bare repository on a disk.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output, Stdio};

type Result<T> = std::result::Result<T, String>;

const LEDGER: &str = "refs/susi-guard";
const INCOMING: &str = "refs/susi-guard-incoming";
/// Refspecs per fetch: bounded so a command line never grows without limit.
const FETCH_BATCH: usize = 200;
/// A remote advertising more refs than this is refused rather than half read.
const MAX_REFS: usize = 200_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    AppendOnly,
    Immutable,
}

fn rule_for(reference: &str) -> Option<Rule> {
    if reference == "refs/heads/main"
        || reference
            .strip_prefix("refs/candidates/")
            .is_some_and(|name| !name.is_empty())
    {
        Some(Rule::AppendOnly)
    } else if reference
        .strip_prefix("refs/tags/")
        .is_some_and(|name| !name.is_empty())
    {
        Some(Rule::Immutable)
    } else {
        None
    }
}

/// What one run observed. `findings` empty means nothing was lost.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Protected refs whose remote value equals the record.
    pub unchanged: usize,
    /// Protected refs seen for the first time and recorded.
    pub recorded: usize,
    /// Append-only refs that moved forward; their record advanced.
    pub advanced: usize,
    /// One line per rewound, moved or deleted ref, with its recovery.
    pub findings: Vec<String>,
}

impl Report {
    /// One line of counts for `remote`.
    pub fn summary(&self, remote: &str) -> String {
        format!(
            "storage guard ({remote}): {} unchanged, {} moved forward, {} newly recorded, {} finding(s)",
            self.unchanged,
            self.advanced,
            self.recorded,
            self.findings.len()
        )
    }
}

fn git(root: &Path, args: &[&str]) -> Result<Output> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .arg("--no-replace-objects")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_GRAFT_FILE",
    ] {
        command.env_remove(key);
    }
    #[cfg(test)]
    {
        // Fixtures never read a real home, Git configuration or credentials.
        command.env_clear();
        for key in [
            "PATH",
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "PATHEXT",
            "TEMP",
            "TMP",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("HOME", root)
            .env("USERPROFILE", root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join(".absent-fixture-config"))
            .env("GIT_TERMINAL_PROMPT", "0");
    }
    command
        .output()
        .map_err(|error| format!("cannot run Git: {error}"))
}

fn text(root: &Path, args: &[&str]) -> Result<String> {
    let output = git(root, args)?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| error.to_string())
}

fn valid_remote(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}

/// `<oid><separator><refname>` lines into a map, keeping only wanted names.
fn parse_refs(
    listing: &str,
    separator: char,
    mut keep: impl FnMut(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>> {
    let mut refs = BTreeMap::new();
    for line in listing.lines().filter(|line| !line.is_empty()) {
        let (oid, name) = line
            .split_once(separator)
            .ok_or_else(|| format!("unreadable ref line: {line}"))?;
        if !valid_oid(oid) {
            return Err(format!("unreadable object id for {name}"));
        }
        if let Some(key) = keep(name) {
            if refs.len() >= MAX_REFS {
                return Err("remote advertises more refs than the guard reads".to_owned());
            }
            refs.insert(key, oid.to_owned());
        }
    }
    Ok(refs)
}

fn ledger_ref(remote: &str, reference: &str) -> String {
    let tail = reference.strip_prefix("refs/").unwrap_or(reference);
    format!("{LEDGER}/{remote}/{tail}")
}

fn incoming_ref(remote: &str, reference: &str) -> String {
    let tail = reference.strip_prefix("refs/").unwrap_or(reference);
    format!("{INCOMING}/{remote}/{tail}")
}

fn is_ancestor(root: &Path, old: &str, new: &str) -> Result<bool> {
    let output = git(root, &["merge-base", "--is-ancestor", old, new])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(_) | None => Err(format!(
            "cannot compare history: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

fn clear_incoming(root: &Path, remote: &str) -> Result<()> {
    let prefix = format!("{INCOMING}/{remote}/");
    let listed = text(root, &["for-each-ref", "--format=%(refname)", &prefix])?;
    for name in listed.lines().filter(|line| !line.is_empty()) {
        text(root, &["update-ref", "-d", name])?;
    }
    Ok(())
}

/// Compare `remote` with this clone's record and advance the record where the
/// remote only moved forward.
pub fn audit(root: &Path, remote: &str) -> Result<Report> {
    if !valid_remote(remote) {
        return Err(format!("not a remote name: {remote}"));
    }
    text(root, &["remote", "get-url", remote])
        .map_err(|_| format!("no configured remote named {remote}"))?;

    let advertised = text(root, &["ls-remote", "--refs", remote])?;
    let observed = parse_refs(&advertised, '\t', |name| {
        rule_for(name).map(|_| name.to_owned())
    })?;
    let prefix = format!("{LEDGER}/{remote}/");
    let recorded_listing = text(
        root,
        &["for-each-ref", "--format=%(objectname) %(refname)", &prefix],
    )?;
    let recorded = parse_refs(&recorded_listing, ' ', |name| {
        name.strip_prefix(&prefix)
            .map(|tail| format!("refs/{tail}"))
    })?;

    clear_incoming(root, remote)?;
    let moved: Vec<&String> = observed
        .iter()
        .filter(|(name, oid)| recorded.get(*name) != Some(*oid))
        .map(|(name, _)| name)
        .collect();
    for batch in moved.chunks(FETCH_BATCH) {
        let specs: Vec<String> = batch
            .iter()
            .map(|name| format!("+{name}:{}", incoming_ref(remote, name)))
            .collect();
        let mut args = vec![
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            remote,
        ];
        args.extend(specs.iter().map(String::as_str));
        text(root, &args)?;
    }

    let mut report = Report::default();
    let outcome = compare(root, remote, &observed, &recorded, &mut report);
    let cleared = clear_incoming(root, remote);
    outcome?;
    cleared?;
    Ok(report)
}

fn compare(
    root: &Path,
    remote: &str,
    observed: &BTreeMap<String, String>,
    recorded: &BTreeMap<String, String>,
    report: &mut Report,
) -> Result<()> {
    for (name, new) in observed {
        let kept = ledger_ref(remote, name);
        let old = recorded.get(name);
        if old == Some(new) {
            report.unchanged += 1;
            continue;
        }
        let fetched = text(
            root,
            &["rev-parse", "--verify", &incoming_ref(remote, name)],
        )?;
        if &fetched != new {
            return Err(format!(
                "{name} moved on {remote} while the guard was reading it; run the guard again"
            ));
        }
        let Some(old) = old else {
            text(root, &["update-ref", &kept, new, ""])?;
            report.recorded += 1;
            continue;
        };
        match rule_for(name) {
            Some(Rule::AppendOnly) => {
                if is_ancestor(root, old, new)? {
                    text(root, &["update-ref", &kept, new, old])?;
                    report.advanced += 1;
                } else {
                    let lost = text(root, &["rev-list", "--count", &format!("{new}..{old}")])?;
                    report.findings.push(format!(
                        "REWOUND {name}: {lost} published commit(s) are no longer in its history (was {old}, now {new}). They are retained in this clone at {kept}. Recover without force: merge {kept} into the current {name} in your own worktree and publish that as a normal fast-forward."
                    ));
                }
            }
            Some(Rule::Immutable) => report.findings.push(format!(
                "MOVED {name}: an immutable tag changed (was {old}, now {new}). The original is retained in this clone at {kept}. Treat the moved tag as unqualified until its owner explains it."
            )),
            None => {}
        }
    }
    for (name, old) in recorded {
        if !observed.contains_key(name) {
            let kept = ledger_ref(remote, name);
            report.findings.push(format!(
                "DELETED {name}: it is gone from {remote} (was {old}). It is retained in this clone at {kept}. Restore without force: git push {remote} {kept}:{name}"
            ));
        }
    }
    Ok(())
}

/// Run after a publishing command: every dump to main makes the publishing
/// clone a witness, so the guard needs no scheduler. Advisory only (main has
/// no gates): it returns a note when history was lost or the remote could not
/// be read, never changes the command's result, and is `None` when nothing is
/// wrong. `SUSI_NO_STORAGE_GUARD=1` skips it.
pub fn witness(root: &Path, remote: &str) -> Option<String> {
    if std::env::var_os("SUSI_NO_STORAGE_GUARD").is_some_and(|value| value == "1") {
        return None;
    }
    match audit(root, remote) {
        Ok(report) if report.findings.is_empty() => None,
        Ok(report) => Some(format!(
            "{}\n{}\nPublished history was lost on the storage remote. This did not block your command; repair it as printed above.",
            report.summary(remote),
            report.findings.join("\n")
        )),
        Err(error) => Some(format!("storage guard could not check {remote}: {error}")),
    }
}

#[cfg(test)]
#[path = "tests/storage_guard.rs"]
mod tests;

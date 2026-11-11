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

pub const USAGE: &str = "usage: cargo run --offline --locked -p xtask --bin local-snapshot -- guard [<remote>]\nChecks that the remote's main and release lines only moved forward and that no tag moved or vanished since this clone last looked. Reads the remote and writes only local refs under refs/susi-guard/.";

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
    pub unchanged: usize,
    pub recorded: usize,
    pub advanced: usize,
    pub findings: Vec<String>,
}

impl Report {
    fn summary(&self, remote: &str) -> String {
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

/// Command entry: `guard [<remote>]`. A finding is an error exit.
pub fn run(root: &Path, args: &[String]) -> Result<String> {
    let remote = match args {
        [] => "origin",
        [remote] if remote != "--help" => remote.as_str(),
        _ => return Err(USAGE.to_owned()),
    };
    let report = audit(root, remote)?;
    let summary = report.summary(remote);
    if report.findings.is_empty() {
        Ok(summary)
    } else {
        Err(format!("{summary}\n{}", report.findings.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    // Test assertions intentionally panic on a failed invariant.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Repo {
        root: PathBuf,
        work: PathBuf,
    }

    impl Repo {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "susi-storage-guard-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir(&root).unwrap();
            let work = root.join("work");
            fs::create_dir(&work).unwrap();
            text(&root, &["init", "--bare", "remote.git"]).unwrap();
            let repo = Self { root, work };
            repo.git(&["init", "-b", "main"]);
            repo.git(&["config", "user.name", "Guard Test"]);
            repo.git(&["config", "user.email", "guard@test.invalid"]);
            repo.git(&["config", "commit.gpgsign", "false"]);
            repo.git(&["config", "core.hooksPath", ".no-test-hooks"]);
            let remote = repo.root.join("remote.git");
            repo.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
            repo.commit("one");
            repo.git(&["push", "origin", "main"]);
            repo
        }
        fn git(&self, args: &[&str]) -> String {
            text(&self.work, args).unwrap()
        }
        fn commit(&self, body: &str) -> String {
            fs::write(self.work.join("source.txt"), body).unwrap();
            self.git(&["add", "source.txt"]);
            self.git(&["commit", "-m", body]);
            self.git(&["rev-parse", "HEAD"])
        }
        fn audit(&self) -> Report {
            audit(&self.work, "origin").unwrap()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn protected_namespaces_are_main_release_lines_and_tags_only() {
        assert_eq!(rule_for("refs/heads/main"), Some(Rule::AppendOnly));
        assert_eq!(
            rule_for("refs/candidates/release/v1"),
            Some(Rule::AppendOnly)
        );
        assert_eq!(rule_for("refs/tags/v1.0.0"), Some(Rule::Immutable));
        for free in [
            "refs/heads/other",
            "refs/agents/claude",
            "refs/claims/release-lanes/v1/a",
            "refs/tags/",
            "refs/candidates/",
        ] {
            assert_eq!(rule_for(free), None, "{free}");
        }
    }

    #[test]
    fn a_first_run_records_and_forward_movement_advances_the_record() {
        let repo = Repo::new();
        repo.git(&["tag", "v1"]);
        repo.git(&["push", "origin", "v1", "HEAD:refs/candidates/release/r1"]);
        let first = repo.audit();
        assert_eq!((first.recorded, first.findings.len()), (3, 0));
        assert_eq!(repo.audit().unchanged, 3);

        let two = repo.commit("two");
        repo.git(&["push", "origin", "main", "HEAD:refs/candidates/release/r1"]);
        let moved = repo.audit();
        assert_eq!(
            (moved.advanced, moved.unchanged, moved.findings.len()),
            (2, 1, 0)
        );
        assert_eq!(
            repo.git(&["rev-parse", "refs/susi-guard/origin/heads/main"]),
            two
        );
        assert!(run(&repo.work, &[]).unwrap().contains("0 finding(s)"));
        // Nothing but the record is written: no incoming ref is left behind.
        assert_eq!(repo.git(&["for-each-ref", INCOMING]), "");
    }

    #[test]
    fn a_rewound_main_is_reported_retained_and_recoverable_without_force() {
        let repo = Repo::new();
        let one = repo.git(&["rev-parse", "HEAD"]);
        let two = repo.commit("two");
        repo.git(&["push", "origin", "main"]);
        repo.audit();

        // Another client, with no hook, rewrites main and drops `two`.
        repo.git(&[
            "push",
            "--force",
            "origin",
            &format!("{one}:refs/heads/main"),
        ]);
        let report = repo.audit();
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].starts_with("REWOUND refs/heads/main: 1 published"));
        // The record is kept, so the loss stays reported until it is repaired.
        assert_eq!(
            repo.git(&["rev-parse", "refs/susi-guard/origin/heads/main"]),
            two
        );
        assert!(run(&repo.work, &[]).unwrap_err().contains("REWOUND"));

        // Recovery is an ordinary fast-forward of the retained history.
        repo.git(&[
            "push",
            "origin",
            "refs/susi-guard/origin/heads/main:refs/heads/main",
        ]);
        let healed = repo.audit();
        assert_eq!((healed.findings.len(), healed.unchanged), (0, 1));
    }

    #[test]
    fn a_moved_tag_and_a_deleted_tag_are_reported_and_the_deleted_one_restorable() {
        let repo = Repo::new();
        repo.git(&["tag", "v1"]);
        repo.git(&["tag", "v2"]);
        repo.git(&["push", "origin", "v1", "v2"]);
        let original = repo.git(&["rev-parse", "v1"]);
        repo.audit();

        repo.commit("two");
        repo.git(&["tag", "--force", "v1"]);
        repo.git(&["push", "--force", "origin", "v1"]);
        repo.git(&["push", "origin", ":refs/tags/v2"]);
        let report = repo.audit();
        assert_eq!(report.findings.len(), 2, "{:?}", report.findings);
        assert!(report
            .findings
            .iter()
            .any(|f| f.starts_with("MOVED refs/tags/v1")));
        assert!(report
            .findings
            .iter()
            .any(|f| f.starts_with("DELETED refs/tags/v2")));
        assert_eq!(
            repo.git(&["rev-parse", "refs/susi-guard/origin/tags/v1"]),
            original
        );

        // The printed restore is a plain create: no force is involved.
        repo.git(&[
            "push",
            "origin",
            "refs/susi-guard/origin/tags/v2:refs/tags/v2",
        ]);
        let after = repo.audit();
        assert_eq!(after.findings.len(), 1);
        assert!(after.findings[0].starts_with("MOVED refs/tags/v1"));
    }

    #[test]
    fn unprotected_refs_and_bad_remote_names_are_not_guarded() {
        let repo = Repo::new();
        repo.git(&["push", "origin", "HEAD:refs/agents/claude"]);
        assert_eq!(repo.audit().recorded, 1);
        repo.git(&["push", "origin", ":refs/agents/claude"]);
        assert!(repo.audit().findings.is_empty());
        for bad in ["", "-upload-pack=x", "a b", "absent"] {
            assert!(audit(&repo.work, bad).is_err(), "{bad}");
        }
        assert_eq!(
            run(&repo.work, &["a".into(), "b".into()]).unwrap_err(),
            USAGE
        );
    }
}

//! Portable, dependency-free snapshot publication. No main push or qualification.
//!
//! Git's ordinary tag push is create-only (without a force option). Local branch
//! creation uses update-ref's old-object compare-and-swap. An immutable intent
//! binds retry to the original commit, tree and configured Git destination.
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

type Result<T> = std::result::Result<T, String>;
const SCHEMA: &str = "susi-git-snapshot-v1";

struct Git<'a> {
    root: &'a Path,
}

#[derive(Debug)]
struct Endpoint {
    address: String,
    digest: String,
}

impl Git<'_> {
    fn output(&self, args: &[&str], input: Option<&[u8]>) -> Result<Output> {
        let mut command = Command::new("git");
        command
            .current_dir(self.root)
            .arg("--no-replace-objects")
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        // The supplied repository, not an inherited alternate index/worktree,
        // owns all objects and state. Normal credential helpers remain Git's job.
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            command.env_remove(key);
        }
        #[cfg(test)]
        {
            // Local fixtures never read real homes, Git configuration or keys,
            // even when invoked from a normally configured developer shell.
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
                .env("HOME", self.root)
                .env("USERPROFILE", self.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env(
                    "GIT_CONFIG_GLOBAL",
                    self.root.join(".absent-fixture-config"),
                )
                .env("GIT_TERMINAL_PROMPT", "0");
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start Git: {e}"))?;
        if let Some(bytes) = input {
            let written = child
                .stdin
                .take()
                .ok_or("Git stdin unavailable")?
                .write_all(bytes);
            if let Err(error) = written {
                let _ = child.wait();
                return Err(format!("cannot write Git input: {error}"));
            }
        }
        child
            .wait_with_output()
            .map_err(|e| format!("cannot wait for Git: {e}"))
    }

    fn text(&self, args: &[&str]) -> Result<String> {
        let output = self.output(args, None)?;
        if !output.status.success() {
            // Do not print remote URLs or credential-helper output.
            return Err(format!("Git {} failed ({})", args[0], output.status));
        }
        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_owned())
            .map_err(|_| "Git returned non-UTF-8 output".to_owned())
    }

    fn digest(&self, bytes: &[u8]) -> Result<String> {
        let output = self.output(&["hash-object", "--stdin"], Some(bytes))?;
        if !output.status.success() {
            return Err("cannot bind the Git destination".to_owned());
        }
        let oid = String::from_utf8(output.stdout).map_err(|_| "invalid Git digest")?;
        valid_oid(oid.trim())?;
        Ok(oid.trim().to_owned())
    }

    fn endpoint(&self) -> Result<Endpoint> {
        let fetch = self.text(&["remote", "get-url", "--all", "origin"])?;
        let push = self.text(&["remote", "get-url", "--push", "--all", "origin"])?;
        if fetch.is_empty() || fetch.lines().count() != 1 || fetch != push {
            return Err("origin must have exactly one matching fetch/push destination".to_owned());
        }
        let address = normalize_endpoint(self.root, &fetch)?;
        // get-url already expands URL rewrites. Passing its result directly
        // must not apply a second rewrite and silently select another repository.
        let effective = self.text(&["ls-remote", "--get-url", "--", &address])?;
        if effective != address {
            return Err(
                "Git URL rewrites re-apply to the resolved endpoint; refusing ambiguous routing"
                    .to_owned(),
            );
        }
        let rewrites = self.output(
            &[
                "config",
                "--null",
                "--get-regexp",
                "^url\\..*\\.pushinsteadof$",
            ],
            None,
        )?;
        if !rewrites.status.success() && rewrites.status.code() != Some(1) {
            return Err("cannot inspect Git push URL routing".to_owned());
        }
        let rules = String::from_utf8(rewrites.stdout).map_err(|_| "invalid Git URL routing")?;
        if matching_push_rewrite(&rules, &address)? {
            return Err(
                "Git push URL rewrite matches the resolved endpoint; refusing ambiguous routing"
                    .to_owned(),
            );
        }
        Ok(Endpoint {
            digest: self.digest(address.as_bytes())?,
            address,
        })
    }

    fn remote_ref(&self, endpoint: &Endpoint, reference: &str) -> Result<Option<String>> {
        let output = self.text(&["ls-remote", "--refs", "--", &endpoint.address, reference])?;
        parse_remote_ref(&output, reference)
    }

    fn tree(&self, commit: &str) -> Result<String> {
        valid_oid(commit)?;
        if self.text(&["cat-file", "-t", commit])? != "commit" {
            return Err("snapshot source is not a commit".to_owned());
        }
        let tree = self.text(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])?;
        valid_oid(&tree)?;
        Ok(tree)
    }

    fn intent_path(&self, name: &str) -> Result<PathBuf> {
        valid_name(name)?;
        let common = self.text(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let common = PathBuf::from(common);
        if !common.is_absolute() {
            return Err("Git common directory must be absolute".to_owned());
        }
        Ok(common
            .join("susi-publisher")
            .join("snapshots")
            .join(format!("{name}.intent")))
    }

    fn ensure_branch(&self, intent: &Intent) -> Result<()> {
        let reference = intent.branch_ref();
        let output = self.output(&["show-ref", "--verify", "--hash", &reference], None)?;
        if output.status.success() {
            let oid = String::from_utf8(output.stdout).map_err(|_| "invalid local branch")?;
            if oid.trim() == intent.commit {
                return Ok(());
            }
            return Err(
                "local release branch exists at a different commit; left unchanged".to_owned(),
            );
        }
        if output.status.code() != Some(1) && output.status.code() != Some(128) {
            return Err("cannot inspect local release branch".to_owned());
        }
        // No force: another writer wins instead of having its branch overwritten.
        let zero = "0".repeat(intent.commit.len());
        let result = self.output(&["update-ref", &reference, &intent.commit, &zero], None)?;
        if result.status.success() {
            return Ok(());
        }
        if self.text(&["show-ref", "--verify", "--hash", &reference])? == intent.commit {
            return Ok(());
        }
        Err("local release branch creation raced; no branch was overwritten".to_owned())
    }
}

fn matching_push_rewrite(rules: &str, address: &str) -> Result<bool> {
    if !rules.is_empty() && !rules.ends_with('\0') {
        return Err("incomplete Git push URL routing".to_owned());
    }
    let mut matches = false;
    for record in rules.split_terminator('\0') {
        let (_, prefix) = record
            .split_once('\n')
            .ok_or("invalid Git push URL routing")?;
        matches |= address.starts_with(prefix);
    }
    Ok(matches)
}

fn normalize_endpoint(root: &Path, value: &str) -> Result<String> {
    if value.chars().any(char::is_control) || value.starts_with('-') {
        return Err("invalid Git endpoint".to_owned());
    }
    if let Some((scheme, remainder)) = value.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git") {
            return Err(
                "unsupported endpoint scheme; use a Git URL or an absolute filesystem path"
                    .to_owned(),
            );
        }
        let authority = remainder.split('/').next().unwrap_or("");
        if authority.is_empty()
            || authority
                .split_once('@')
                .is_some_and(|(user, _)| scheme != "ssh" || user.contains(':'))
        {
            return Err("embedded endpoint credentials are not supported; use Git's credential or SSH helper".to_owned());
        }
        return Ok(value.to_owned());
    }
    if !Path::new(value).is_absolute() && value.contains(':') {
        // Standard scp-like SSH syntax only, never a command-bearing remote helper.
        let (host, path) = value.split_once(':').ok_or("invalid SSH endpoint")?;
        if host.is_empty() || path.is_empty() || path.starts_with(':') || host.contains(['/', '\\'])
        {
            return Err("unsupported Git remote helper endpoint".to_owned());
        }
        return Ok(value.to_owned());
    }
    let path = root
        .join(value)
        .canonicalize()
        .map_err(|e| format!("cannot resolve filesystem endpoint: {e}"))?;
    let value = path.to_str().ok_or("Git endpoint is not UTF-8")?;
    // Git for Windows accepts ordinary absolute paths, not every extended-path
    // spelling returned by canonicalize. Preserve UNC semantics explicitly.
    Ok(if let Some(unc) = value.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        value.strip_prefix("\\\\?\\").unwrap_or(value).to_owned()
    })
}

fn valid_oid(oid: &str) -> Result<()> {
    if matches!(oid.len(), 40 | 64)
        && oid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err("expected a full lowercase Git object ID".to_owned())
    }
}

fn valid_name(name: &str) -> Result<()> {
    let device = matches!(name, "con" | "prn" | "aux" | "nul")
        || (name.len() == 4
            && (name.starts_with("com") || name.starts_with("lpt"))
            && matches!(name.as_bytes()[3], b'1'..=b'9'));
    if name.is_empty()
        || name.len() > 48
        || device
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
    {
        return Err("snapshot name needs 1-48 lowercase letters/digits/interior hyphens, not a Windows device name".to_owned());
    }
    Ok(())
}

fn parse_remote_ref(output: &str, reference: &str) -> Result<Option<String>> {
    if output.is_empty() {
        return Ok(None);
    }
    let mut lines = output.lines();
    let line = lines.next().ok_or("missing remote ref")?;
    let (oid, name) = line.split_once('\t').ok_or("malformed remote ref")?;
    valid_oid(oid)?;
    if name != reference || lines.next().is_some() {
        return Err("ambiguous or unexpected remote ref".to_owned());
    }
    Ok(Some(oid.to_owned()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Intent {
    name: String,
    commit: String,
    tree: String,
    endpoint: String,
}

impl Intent {
    fn render(&self) -> String {
        format!(
            "{SCHEMA}\n{}\n{}\n{}\n{}\n",
            self.name, self.commit, self.tree, self.endpoint
        )
    }

    fn parse(text: &str, name: &str) -> Result<Self> {
        let fields: Vec<_> = text.lines().collect();
        if fields.len() != 5 || fields[0] != SCHEMA || fields[1] != name || !text.ends_with('\n') {
            return Err("incomplete or invalid snapshot intent; refusing to guess".to_owned());
        }
        valid_name(name)?;
        for oid in &fields[2..] {
            valid_oid(oid)?;
        }
        Ok(Self {
            name: name.to_owned(),
            commit: fields[2].to_owned(),
            tree: fields[3].to_owned(),
            endpoint: fields[4].to_owned(),
        })
    }

    fn branch_ref(&self) -> String {
        format!("refs/heads/release/{}", self.name)
    }
    fn tag_ref(&self) -> String {
        format!("refs/tags/susi-snapshot/{}", self.name)
    }

    fn validate(&self, git: &Git<'_>) -> Result<Endpoint> {
        let endpoint = git.endpoint()?;
        if endpoint.digest != self.endpoint {
            return Err(
                "origin destination changed since snapshot creation; refusing to publish"
                    .to_owned(),
            );
        }
        if git.tree(&self.commit)? != self.tree {
            return Err("snapshot commit/tree evidence does not match".to_owned());
        }
        Ok(endpoint)
    }
}

fn read_intent(path: &Path, name: &str) -> Result<Intent> {
    let text = fs::read_to_string(path).map_err(|e| format!("cannot read snapshot intent: {e}"))?;
    Intent::parse(&text, name)
}

fn ready_intent(path: &Path, name: &str) -> Result<Intent> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|e| e.to_string())?;
    let intent = Intent::parse(&text, name)?;
    // A concurrent creator may have completed write_all but not sync_all yet.
    // This reader establishes durability itself before it can publish.
    file.sync_all()
        .map_err(|e| format!("cannot sync snapshot intent: {e}"))?;
    Ok(intent)
}

fn store_intent(path: &Path, intent: &Intent) -> Result<()> {
    fs::create_dir_all(path.parent().ok_or("intent parent missing")?).map_err(|e| e.to_string())?;
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(intent.render().as_bytes())
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("cannot create snapshot intent: {error}")),
    }
    if ready_intent(path, &intent.name)? != *intent {
        return Err(
            "snapshot name already reserved for different evidence; use its retry command"
                .to_owned(),
        );
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Publication {
    Published,
    Absent,
    Conflict,
    Unknown,
}

trait Transport {
    fn observe(&mut self) -> Result<Option<String>>;
    fn push(&mut self) -> Result<()>;
}

struct SnapshotTransport<'a, 'b> {
    git: &'a Git<'b>,
    intent: &'a Intent,
}

impl Transport for SnapshotTransport<'_, '_> {
    fn observe(&mut self) -> Result<Option<String>> {
        let endpoint = self.intent.validate(self.git)?;
        self.git.remote_ref(&endpoint, &self.intent.tag_ref())
    }

    fn push(&mut self) -> Result<()> {
        let endpoint = self.intent.validate(self.git)?;
        let refspec = format!("{}:{}", self.intent.commit, self.intent.tag_ref());
        // Ordinary tag pushes reject replacement. Never use force, leases,
        // deletion refspecs, shell commands, API calls or a branch here.
        self.git
            .text(&[
                "push",
                "--porcelain",
                "--no-follow-tags",
                "--recurse-submodules=no",
                "--",
                &endpoint.address,
                &refspec,
            ])
            .map(|_| ())
    }
}

fn classify(observed: Result<Option<String>>, expected: &str) -> Publication {
    match observed {
        Ok(Some(oid)) if oid == expected => Publication::Published,
        Ok(Some(_)) => Publication::Conflict,
        Ok(None) => Publication::Absent,
        Err(_) => Publication::Unknown,
    }
}

fn publish(transport: &mut impl Transport, expected: &str) -> Publication {
    match classify(transport.observe(), expected) {
        Publication::Absent => {}
        status @ (Publication::Published | Publication::Conflict | Publication::Unknown) => {
            return status
        }
    }
    // Even a failed wait/transport response may have accepted the ref. Never
    // infer remote state from exit status and never automatically push twice.
    let _ = transport.push();
    classify(transport.observe(), expected)
}

pub fn run(root: &Path, args: &[String]) -> Result<String> {
    if args.len() != 2 || !matches!(args[0].as_str(), "cut" | "adopt" | "retry" | "status") {
        return Err("usage: cargo run --offline --locked -p xtask --bin local-snapshot -- <cut|adopt|retry|status> <name>".to_owned());
    }
    let name = &args[1];
    valid_name(name)?;
    let git = Git { root };
    let path = git.intent_path(name)?;
    let intent = if matches!(args[0].as_str(), "cut" | "adopt") {
        if path.try_exists().map_err(|e| e.to_string())? {
            return Err(
                "snapshot already has an intent; use retry or status to keep its original commit"
                    .to_owned(),
            );
        }
        let endpoint = git.endpoint()?;
        let source = if args[0] == "adopt" {
            format!("refs/tags/susi-snapshot/{name}")
        } else {
            "refs/heads/main".to_owned()
        };
        let commit = git
            .remote_ref(&endpoint, &source)?
            .ok_or("remote snapshot source does not exist")?;
        git.text(&[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--recurse-submodules=no",
            "--",
            &endpoint.address,
            &commit,
        ])?;
        let intent = Intent {
            name: name.to_owned(),
            tree: git.tree(&commit)?,
            commit,
            endpoint: endpoint.digest,
        };
        intent.validate(&git)?;
        store_intent(&path, &intent)?;
        intent
    } else {
        if args[0] == "status" {
            read_intent(&path, name)?
        } else {
            ready_intent(&path, name)?
        }
    };
    let endpoint = intent.validate(&git)?;
    let status = if args[0] == "status" {
        classify(git.remote_ref(&endpoint, &intent.tag_ref()), &intent.commit)
    } else {
        git.ensure_branch(&intent)?;
        if args[0] == "adopt" {
            classify(git.remote_ref(&endpoint, &intent.tag_ref()), &intent.commit)
        } else {
            publish(
                &mut SnapshotTransport {
                    git: &git,
                    intent: &intent,
                },
                &intent.commit,
            )
        }
    };
    let evidence = format!(
        "commit={} tree={} snapshot={}",
        intent.commit,
        intent.tree,
        intent.tag_ref()
    );
    match status {
        Publication::Published => Ok(format!("Published snapshot verified: {evidence}\nQualification is pending. No production artifact or remote release branch was created.")),
        Publication::Absent => Err(format!("snapshot not published: {evidence}; retry is safe and preserves this exact source")),
        Publication::Conflict => Err(format!("remote snapshot name points elsewhere; left unchanged: {evidence}")),
        Publication::Unknown => Err(format!("publication outcome is unknown: {evidence}; check status before retrying, never select a new source")),
    }
}

#[cfg(test)]
mod tests {
    // Test assertions intentionally panic on a failed invariant.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    struct Fake {
        observations: VecDeque<Result<Option<String>>>,
        pushes: usize,
        push_error: bool,
    }
    impl Transport for Fake {
        fn observe(&mut self) -> Result<Option<String>> {
            self.observations.pop_front().unwrap()
        }
        fn push(&mut self) -> Result<()> {
            self.pushes += 1;
            if self.push_error {
                Err("connection lost".to_owned())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn push_error_after_acceptance_is_reconciled() {
        let mut transport = Fake {
            observations: [Ok(None), Ok(Some(A.into()))].into(),
            pushes: 0,
            push_error: true,
        };
        assert_eq!(publish(&mut transport, A), Publication::Published);
        assert_eq!(transport.pushes, 1);
    }

    #[test]
    fn concurrent_different_snapshot_is_not_overwritten_or_retried() {
        let mut transport = Fake {
            observations: [Ok(None), Ok(Some(B.into()))].into(),
            pushes: 0,
            push_error: true,
        };
        assert_eq!(publish(&mut transport, A), Publication::Conflict);
        assert_eq!(transport.pushes, 1);
    }

    #[test]
    fn unknown_outcome_and_missing_ref_never_blindly_retry() {
        for observation in [Err("offline".into()), Ok(None)] {
            let expected = if observation.is_err() {
                Publication::Unknown
            } else {
                Publication::Absent
            };
            let mut transport = Fake {
                observations: [Ok(None), observation].into(),
                pushes: 0,
                push_error: true,
            };
            assert_eq!(publish(&mut transport, A), expected);
            assert_eq!(transport.pushes, 1);
        }
    }

    #[test]
    fn retry_observes_first_and_sends_nothing_after_success_conflict_or_error() {
        for observation in [
            Ok(Some(A.into())),
            Ok(Some(B.into())),
            Err("offline".into()),
        ] {
            let mut transport = Fake {
                observations: [observation].into(),
                pushes: 0,
                push_error: false,
            };
            let _ = publish(&mut transport, A);
            assert_eq!(transport.pushes, 0);
        }
    }

    #[test]
    fn names_oids_and_remote_responses_fail_closed() {
        for name in [
            "", "../main", "main/x", "-f", "x-", "a\nb", "CON", "con", "com1", "lpt9",
        ] {
            assert!(valid_name(name).is_err());
        }
        assert!(valid_name("2026-11-10-rc1").is_ok());
        assert!(valid_oid("main").is_err());
        assert!(valid_oid(&"A".repeat(40)).is_err());
        assert!(valid_oid(&"a".repeat(64)).is_ok());
        assert!(parse_remote_ref(&format!("{A}\trefs/heads/main"), "refs/heads/main").is_ok());
        for text in [
            format!("{A} refs/heads/main"),
            format!("{A}\trefs/heads/other"),
            format!("{A}\trefs/heads/main\n{B}\trefs/heads/main"),
        ] {
            assert!(parse_remote_ref(&text, "refs/heads/main").is_err());
        }
    }

    #[test]
    fn intent_requires_complete_exact_evidence() {
        let intent = Intent {
            name: "rc1".into(),
            commit: A.into(),
            tree: B.into(),
            endpoint: A.into(),
        };
        assert_eq!(Intent::parse(&intent.render(), "rc1").unwrap(), intent);
        for text in [
            intent.render().trim_end().to_owned(),
            intent.render() + "extra\n",
            "susi-git-snapshot-v1\nrc1\n".to_owned(),
        ] {
            assert!(Intent::parse(&text, "rc1").is_err());
        }
        assert!(Intent::parse(&intent.render(), "rc2").is_err());
    }

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Repo {
        root: PathBuf,
        work: PathBuf,
    }
    impl Repo {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "susi-git-snapshot-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let work = root.join("work");
            fs::create_dir(&work).unwrap();
            let git = Git { root: &root };
            git.text(&["init", "--bare", "remote.git"]).unwrap();
            let git = Git { root: &work };
            git.text(&["init", "-b", "main"]).unwrap();
            git.text(&["config", "user.name", "Snapshot Test"]).unwrap();
            git.text(&["config", "user.email", "snapshot@test.invalid"])
                .unwrap();
            git.text(&["config", "commit.gpgsign", "false"]).unwrap();
            git.text(&["config", "core.hooksPath", ".no-test-hooks"])
                .unwrap();
            git.text(&[
                "remote",
                "add",
                "origin",
                root.join("remote.git").to_str().unwrap(),
            ])
            .unwrap();
            let repo = Self { root, work };
            repo.commit("one");
            repo.git().text(&["push", "origin", "main"]).unwrap();
            repo
        }
        fn git(&self) -> Git<'_> {
            Git { root: &self.work }
        }
        fn commit(&self, text: &str) -> String {
            fs::write(self.work.join("source.txt"), text).unwrap();
            self.git().text(&["add", "source.txt"]).unwrap();
            self.git().text(&["commit", "-m", text]).unwrap();
            self.git().text(&["rev-parse", "HEAD"]).unwrap()
        }
        fn run(&self, command: &str, name: &str) -> Result<String> {
            run(&self.work, &[command.into(), name.into()])
        }
    }
    impl Drop for Repo {
        fn drop(&mut self) {
            // This unique test directory is created above; no external path is accepted.
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn real_git_snapshot_and_retry_keep_exact_tree_when_main_moves() {
        let repo = Repo::new();
        let first = repo.git().text(&["rev-parse", "HEAD"]).unwrap();
        assert!(repo
            .run("cut", "rc1")
            .unwrap()
            .contains("Qualification is pending"));
        let intent = read_intent(&repo.git().intent_path("rc1").unwrap(), "rc1").unwrap();
        assert_eq!(intent.commit, first);
        let second = repo.commit("two");
        repo.git().text(&["push", "origin", "main"]).unwrap();
        assert!(repo.run("retry", "rc1").is_ok());
        let endpoint = repo.git().endpoint().unwrap();
        assert_eq!(
            repo.git().remote_ref(&endpoint, &intent.tag_ref()).unwrap(),
            Some(first)
        );
        assert_eq!(
            repo.git().remote_ref(&endpoint, "refs/heads/main").unwrap(),
            Some(second)
        );
        assert!(repo
            .git()
            .remote_ref(&endpoint, &intent.branch_ref())
            .unwrap()
            .is_none());
        assert!(repo.run("cut", "rc1").is_err());
    }

    #[test]
    fn real_git_tag_rejects_even_fast_forward_replacement() {
        let repo = Repo::new();
        repo.run("cut", "rc1").unwrap();
        let intent = read_intent(&repo.git().intent_path("rc1").unwrap(), "rc1").unwrap();
        let second = repo.commit("two");
        let refspec = format!("{second}:{}", intent.tag_ref());
        assert!(repo.git().text(&["push", "origin", &refspec]).is_err());
        assert_eq!(
            repo.git()
                .remote_ref(&repo.git().endpoint().unwrap(), &intent.tag_ref())
                .unwrap(),
            Some(intent.commit)
        );
    }

    #[test]
    fn published_snapshot_can_be_adopted_from_a_fresh_clone() {
        let repo = Repo::new();
        repo.run("cut", "rc1").unwrap();
        let original = read_intent(&repo.git().intent_path("rc1").unwrap(), "rc1").unwrap();
        let moved = repo.root.join("moved");
        repo.git()
            .text(&[
                "clone",
                "--no-checkout",
                repo.root.join("remote.git").to_str().unwrap(),
                moved.to_str().unwrap(),
            ])
            .unwrap();
        run(&moved, &["adopt".into(), "rc1".into()]).unwrap();
        let git = Git { root: &moved };
        assert_eq!(
            read_intent(&git.intent_path("rc1").unwrap(), "rc1").unwrap(),
            original
        );
        assert_eq!(
            git.text(&["rev-parse", "refs/heads/release/rc1"]).unwrap(),
            original.commit
        );
    }

    #[test]
    fn changed_endpoint_and_local_branch_block_without_remote_publication() {
        let repo = Repo::new();
        let first = repo.git().text(&["rev-parse", "HEAD"]).unwrap();
        let second = repo.commit("two");
        repo.git()
            .text(&["update-ref", "refs/heads/release/rc1", &second])
            .unwrap();
        assert!(repo
            .run("cut", "rc1")
            .unwrap_err()
            .contains("left unchanged"));
        assert_eq!(
            repo.git()
                .remote_ref(
                    &repo.git().endpoint().unwrap(),
                    "refs/tags/susi-snapshot/rc1"
                )
                .unwrap(),
            None
        );
        assert_eq!(
            repo.git()
                .text(&["rev-parse", "refs/heads/release/rc1"])
                .unwrap(),
            second
        );
        let path = repo.git().intent_path("rc1").unwrap();
        assert_eq!(read_intent(&path, "rc1").unwrap().commit, first);
        repo.git()
            .text(&[
                "remote",
                "set-url",
                "--push",
                "origin",
                "different-destination",
            ])
            .unwrap();
        assert!(repo.run("retry", "rc1").is_err());
    }

    #[test]
    fn competing_intents_cannot_replace_or_truncate_the_winner() {
        let repo = Repo::new();
        let path = repo.git().intent_path("rc1").unwrap();
        let original = Intent {
            name: "rc1".into(),
            commit: A.into(),
            tree: B.into(),
            endpoint: A.into(),
        };
        store_intent(&path, &original).unwrap();
        store_intent(&path, &original).unwrap();
        let different = Intent {
            commit: B.into(),
            ..original.clone()
        };
        assert!(store_intent(&path, &different).is_err());
        assert_eq!(read_intent(&path, "rc1").unwrap(), original);
    }

    #[test]
    fn real_git_concurrent_publishers_have_one_winner_without_forcing() {
        let repo = Repo::new();
        let git = repo.git();
        let first = git.text(&["rev-parse", "HEAD"]).unwrap();
        let second = repo.commit("two");
        let endpoint = git.endpoint().unwrap();
        let intents = [first, second].map(|commit| Intent {
            name: "race".into(),
            tree: git.tree(&commit).unwrap(),
            commit,
            endpoint: endpoint.digest.clone(),
        });
        struct Racing<'a, 'b> {
            inner: SnapshotTransport<'a, 'b>,
            barrier: &'a std::sync::Barrier,
            first: bool,
        }
        impl Transport for Racing<'_, '_> {
            fn observe(&mut self) -> Result<Option<String>> {
                let result = self.inner.observe();
                if self.first {
                    self.first = false;
                    self.barrier.wait();
                }
                result
            }
            fn push(&mut self) -> Result<()> {
                self.inner.push()
            }
        }
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let git = &git;
            let barrier = &barrier;
            let handles: Vec<_> = intents
                .iter()
                .map(|intent| {
                    scope.spawn(move || {
                        publish(
                            &mut Racing {
                                inner: SnapshotTransport { git, intent },
                                barrier,
                                first: true,
                            },
                            &intent.commit,
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == Publication::Published)
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == Publication::Conflict)
                .count(),
            1
        );
        let winner = git
            .remote_ref(&endpoint, &intents[0].tag_ref())
            .unwrap()
            .unwrap();
        assert!(intents.iter().any(|i| i.commit == winner));
    }

    #[test]
    fn resolved_endpoint_is_absolute_and_not_redirected_by_an_origin_change() {
        let repo = Repo::new();
        repo.git()
            .text(&["remote", "set-url", "origin", "../remote.git"])
            .unwrap();
        let endpoint = repo.git().endpoint().unwrap();
        assert!(Path::new(&endpoint.address).is_absolute());
        let expected = repo.git().remote_ref(&endpoint, "refs/heads/main").unwrap();
        repo.git()
            .text(&["remote", "set-url", "origin", "../absent-remote.git"])
            .unwrap();
        assert_eq!(
            repo.git().remote_ref(&endpoint, "refs/heads/main").unwrap(),
            expected
        );
        for value in [
            "https://token@example.invalid/repo",
            "ssh://user:password@example.invalid/repo",
            "ext::run arbitrary",
            "file:///ambiguous",
        ] {
            assert!(normalize_endpoint(&repo.work, value).is_err());
        }
    }

    #[test]
    fn ready_reader_syncs_a_complete_intent_and_rejects_a_partial_one() {
        let repo = Repo::new();
        let path = repo.git().intent_path("rc1").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let intent = Intent {
            name: "rc1".into(),
            commit: A.into(),
            tree: B.into(),
            endpoint: A.into(),
        };
        let mut creator = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap();
        creator.write_all(b"susi-git-snapshot-v1\n").unwrap();
        assert!(ready_intent(&path, "rc1").is_err());
        creator
            .write_all(
                intent
                    .render()
                    .strip_prefix("susi-git-snapshot-v1\n")
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        // Creator deliberately has not synced; the competing reader must do it.
        assert_eq!(ready_intent(&path, "rc1").unwrap(), intent);
    }

    #[test]
    fn push_rewrite_with_spaces_cannot_redirect_a_pinned_endpoint() {
        let repo = Repo::new();
        let endpoint = repo.git().endpoint().unwrap();
        repo.git()
            .text(&["config", "remote.origin.pushurl", &endpoint.address])
            .unwrap();
        let key = format!(
            "url.{}.pushInsteadOf",
            repo.root.join("other remote.git").display()
        );
        repo.git()
            .text(&["config", &key, &endpoint.address])
            .unwrap();
        assert!(repo
            .git()
            .endpoint()
            .unwrap_err()
            .contains("push URL rewrite"));
        assert!(matching_push_rewrite("key with spaces\nprefix\0", "prefix/suffix").unwrap());
        assert!(matching_push_rewrite("incomplete record", "prefix").is_err());
    }
}

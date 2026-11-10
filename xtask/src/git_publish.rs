//! Portable Git development submissions and frozen qualification candidates.
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
const MAIN_SCHEMA: &str = "susi-git-main-submission-v1";

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
            "GIT_GRAFT_FILE",
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
        if args.first() == Some(&"push") {
            let executable = std::env::current_exe().map_err(|e| e.to_string())?;
            if let Some(path) = adjacent_helper_path(&executable, std::env::var_os("PATH"))? {
                // The configured Git hook still runs normally. Only this child
                // process can discover the native helper built beside us.
                command.env("PATH", path);
            }
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
        self.state_path("snapshots", name)
    }

    fn state_path(&self, area: &str, name: &str) -> Result<PathBuf> {
        valid_name(name)?;
        let common = self.text(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let common = PathBuf::from(common);
        if !common.is_absolute() {
            return Err("Git common directory must be absolute".to_owned());
        }
        Ok(common
            .join("susi-publisher")
            .join(area)
            .join(format!("{name}.intent")))
    }

    fn ensure_branch(&self, intent: &Intent) -> Result<()> {
        let reference = intent.branch_ref();
        self.ensure_ref(&reference, &intent.commit)
    }

    fn ensure_ref(&self, reference: &str, commit: &str) -> Result<()> {
        valid_oid(commit)?;
        let output = self.output(&["show-ref", "--verify", "--hash", reference], None)?;
        if output.status.success() {
            let oid = String::from_utf8(output.stdout).map_err(|_| "invalid local branch")?;
            if oid.trim() == commit {
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
        let zero = "0".repeat(commit.len());
        let result = self.output(&["update-ref", reference, commit, &zero], None)?;
        if result.status.success() {
            return Ok(());
        }
        if self.text(&["show-ref", "--verify", "--hash", reference])? == commit {
            return Ok(());
        }
        Err("local release branch creation raced; no branch was overwritten".to_owned())
    }

    fn fetch_commit(&self, endpoint: &Endpoint, commit: &str) -> Result<()> {
        valid_oid(commit)?;
        self.text(&[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--recurse-submodules=no",
            "--",
            &endpoint.address,
            commit,
        ])?;
        self.tree(commit)?;
        Ok(())
    }

    fn ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        self.require_original_history()?;
        valid_oid(ancestor)?;
        valid_oid(descendant)?;
        let output = self.output(&["merge-base", "--is-ancestor", ancestor, descendant], None)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err("cannot verify Git ancestry".to_owned()),
        }
    }

    fn require_original_history(&self) -> Result<()> {
        let path = PathBuf::from(self.text(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/grafts",
        ])?);
        if !path.is_absolute() || path.try_exists().map_err(|e| e.to_string())? {
            return Err("legacy Git grafts can alter history; publication requires the original commit graph".to_owned());
        }
        Ok(())
    }

    fn compose(&self, base: &str, source: &str, name: &str) -> Result<String> {
        if self.ancestor(base, source)? {
            return Ok(source.to_owned());
        }
        let tree = self.text(&["merge-tree", "--write-tree", base, source])
            .map_err(|_| "source conflicts with current main or has unrelated history; resolve in the agent worktree and submit a new name".to_owned())?;
        valid_oid(&tree)?;
        let message = format!("Susi development submission {name}\n\nUnqualified development main. Release qualification remains separate.\nSource: {source}\nMain-Base: {base}\nTask: LOCAL-{name}\n");
        let output = self.output(
            &["commit-tree", &tree, "-p", base, "-p", source, "-F", "-"],
            Some(message.as_bytes()),
        )?;
        if !output.status.success() {
            return Err(
                "cannot create composed development commit; check Git author/signing configuration"
                    .to_owned(),
            );
        }
        let commit = String::from_utf8(output.stdout).map_err(|_| "invalid composed commit")?;
        let commit = commit.trim().to_owned();
        valid_oid(&commit)?;
        if !self.ancestor(base, &commit)? || !self.ancestor(source, &commit)? {
            return Err("composed candidate does not preserve both histories".to_owned());
        }
        Ok(commit)
    }
}

fn adjacent_helper_path(
    executable: &Path,
    inherited: Option<std::ffi::OsString>,
) -> Result<Option<std::ffi::OsString>> {
    let directory = executable
        .parent()
        .ok_or("publisher executable has no directory")?;
    let helper = directory.join(if cfg!(windows) {
        "push-policy.exe"
    } else {
        "push-policy"
    });
    match helper.try_exists() {
        Ok(false) => return Ok(None),
        Ok(true) if helper.is_file() => {}
        Ok(true) => return Err("adjacent push-policy is not a file".to_owned()),
        Err(error) => return Err(format!("cannot inspect adjacent push-policy: {error}")),
    }
    let mut paths = vec![directory.to_path_buf()];
    if let Some(value) = inherited {
        paths.extend(std::env::split_paths(&value));
    }
    std::env::join_paths(paths)
        .map(Some)
        .map_err(|e| format!("cannot prepare native hook search path: {e}"))
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
    schema: &'static str,
    name: String,
    commit: String,
    tree: String,
    endpoint: String,
}

impl Intent {
    fn render(&self) -> String {
        format!(
            "{}\n{}\n{}\n{}\n{}\n",
            self.schema, self.name, self.commit, self.tree, self.endpoint
        )
    }

    fn parse(text: &str, name: &str) -> Result<Self> {
        let fields: Vec<_> = text.lines().collect();
        if fields.len() != 5 || fields[1] != name || !text.ends_with('\n') {
            return Err("incomplete or invalid snapshot intent; refusing to guess".to_owned());
        }
        valid_name(name)?;
        for oid in &fields[2..] {
            valid_oid(oid)?;
        }
        Ok(Self {
            schema: match fields[0] {
                SCHEMA => SCHEMA,
                MAIN_SCHEMA => MAIN_SCHEMA,
                _ => return Err("unknown publication intent schema".to_owned()),
            },
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
    const USAGE: &str = "usage: cargo run --offline --locked -p xtask --bin local-snapshot -- <submit|retry-main|status-main|cut|adopt|retry|status> <name>\nMain is unqualified development storage. Snapshots require separate release qualification.";
    if args == ["--help"] {
        return Ok(USAGE.to_owned());
    }
    if args.len() == 2 && matches!(args[0].as_str(), "submit" | "retry-main" | "status-main") {
        return submit_main(root, &args[0], &args[1]);
    }
    if args.len() != 2 || !matches!(args[0].as_str(), "cut" | "adopt" | "retry" | "status") {
        return Err(USAGE.to_owned());
    }
    let name = &args[1];
    valid_name(name)?;
    let git = Git { root };
    git.require_original_history()?;
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
            schema: SCHEMA,
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
    if intent.schema != SCHEMA {
        return Err("expected a snapshot intent, not a main submission".to_owned());
    }
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

fn submit_main(root: &Path, command: &str, name: &str) -> Result<String> {
    valid_name(name)?;
    let git = Git { root };
    git.require_original_history()?;
    let path = git.state_path("submissions", name)?;
    let intent = if command == "submit" {
        if path.try_exists().map_err(|e| e.to_string())? {
            return Err(
                "submission name already reserved; use retry-main or status-main".to_owned(),
            );
        }
        if !git
            .text(&["status", "--porcelain", "--untracked-files=normal"])?
            .is_empty()
        {
            return Err(
                "commit the scoped source first; main submission never publishes uncommitted files"
                    .to_owned(),
            );
        }
        let commit = git.text(&["rev-parse", "--verify", "HEAD^{commit}"])?;
        let intent = Intent {
            schema: MAIN_SCHEMA,
            name: name.to_owned(),
            tree: git.tree(&commit)?,
            commit,
            endpoint: git.endpoint()?.digest,
        };
        store_intent(&path, &intent)?;
        intent
    } else if command == "status-main" {
        read_intent(&path, name)?
    } else {
        ready_intent(&path, name)?
    };
    if intent.schema != MAIN_SCHEMA {
        return Err("expected a main submission, not a snapshot intent".to_owned());
    }
    intent.validate(&git)?;
    if command != "status-main" {
        git.ensure_ref(
            &format!("refs/susi-publisher/submissions/{name}/source"),
            &intent.commit,
        )?;
    }
    publish_main(
        &mut GitMain {
            git: &git,
            intent: &intent,
        },
        &intent.commit,
        name,
        command == "status-main",
    )
}

trait MainTransport {
    fn observe(&mut self) -> Result<String>;
    fn ancestor(&mut self, source: &str, head: &str) -> Result<bool>;
    fn compose(&mut self, base: &str) -> Result<String>;
    fn push(&mut self, candidate: &str) -> MainPush;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainPush {
    Accepted,
    Raced,
    Rejected,
    Unknown,
}

struct GitMain<'a, 'b> {
    git: &'a Git<'b>,
    intent: &'a Intent,
}

impl MainTransport for GitMain<'_, '_> {
    fn observe(&mut self) -> Result<String> {
        let endpoint = self.intent.validate(self.git)?;
        let head = self
            .git
            .remote_ref(&endpoint, "refs/heads/main")?
            .ok_or("remote main is missing at observation; no push attempted")?;
        self.git.fetch_commit(&endpoint, &head)?;
        Ok(head)
    }
    fn ancestor(&mut self, source: &str, head: &str) -> Result<bool> {
        self.git.ancestor(source, head)
    }
    fn compose(&mut self, base: &str) -> Result<String> {
        let candidate = self
            .git
            .compose(base, &self.intent.commit, &self.intent.name)?;
        self.git.ensure_ref(
            &format!(
                "refs/susi-publisher/submissions/{}/attempts/{candidate}",
                self.intent.name
            ),
            &candidate,
        )?;
        Ok(candidate)
    }
    fn push(&mut self, candidate: &str) -> MainPush {
        let Ok(endpoint) = self.intent.validate(self.git) else {
            return MainPush::Rejected;
        };
        let refspec = format!("{candidate}:refs/heads/main");
        let result = self.git.output(
            &[
                "push",
                "--porcelain",
                "--no-follow-tags",
                "--recurse-submodules=no",
                "--",
                &endpoint.address,
                &refspec,
            ],
            None,
        );
        let Ok(output) = result else {
            return MainPush::Unknown;
        };
        if output.status.success() {
            return MainPush::Accepted;
        }
        let status = String::from_utf8_lossy(&output.stdout);
        rejected_main_push(&status)
    }
}

fn rejected_main_push(status: &str) -> MainPush {
    let rejection = status.lines().find(|line| line.starts_with("!\t"));
    match rejection {
        Some(line)
            if (line.contains("[rejected]")
                && (line.contains("(fetch first)") || line.contains("(non-fast-forward)")))
                || (line.contains("[remote rejected]")
                    && (line.contains("(failed to update ref)")
                        || line.contains("(incorrect old value provided)"))) =>
        {
            MainPush::Raced
        }
        Some(_) => MainPush::Rejected,
        None => MainPush::Unknown,
    }
}

fn publish_main(
    transport: &mut impl MainTransport,
    source: &str,
    name: &str,
    status_only: bool,
) -> Result<String> {
    for _ in 0..3 {
        let base = transport.observe()?;
        if transport.ancestor(source, &base)? {
            return Ok(format!("Development source {source} is verified on main at {base}. Main is unqualified; no release was created."));
        }
        if status_only {
            return Err(format!("development source {source} is not yet on main; retry-main {name} preserves the original source"));
        }
        let candidate = transport.compose(&base)?;
        // Ordinary fast-forward only. Git receive-pack CAS checks the advertised
        // old tip. Competing source can never be discarded by this command.
        let pushed = transport.push(&candidate);
        let observed = transport.observe()
            .map_err(|_| format!("main publication outcome is unknown for {candidate}; use status-main {name} before retrying"))?;
        if transport.ancestor(&candidate, &observed)? || transport.ancestor(source, &observed)? {
            return Ok(format!("Development source {source} is verified on main at {observed}; composed candidate {candidate}. Main is unqualified; no release was created."));
        }
        match pushed {
            MainPush::Rejected => return Err(format!("main push was rejected; resolve authentication or server policy without bypassing it, then retry-main {name}")),
            MainPush::Unknown => return Err(format!("main publication outcome is unknown for {candidate}; use status-main {name} before retrying")),
            MainPush::Accepted => return Err(format!("main changed unexpectedly after accepted push {candidate}; inspect before retry-main {name}")),
            MainPush::Raced => {},
        }
        if observed == base || !transport.ancestor(&base, &observed)? {
            return Err(format!("main race did not produce a verified fast-forward advance; inspect before retry-main {name}"));
        }
        // A verified competing update moved main. Recompose rather than force,
        // with a bounded loop so one caller cannot monopolize a busy publisher.
    }
    Err(format!(
        "main advanced during three publication attempts; source is preserved, retry-main {name}"
    ))
}

#[cfg(test)]
#[path = "test_fixture_fs.rs"]
mod test_fixture_fs;

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
            schema: SCHEMA,
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

    use super::test_fixture_fs::{copy_dir_all, set_git_origin_url};

    struct Repo {
        root: PathBuf,
        work: PathBuf,
    }
    impl Repo {
        fn new() -> Self {
            static REPO_TPL: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
            let tpl = REPO_TPL.get_or_init(|| {
                let tpl_root =
                    std::env::temp_dir().join(format!("susi-gitpub-tpl-{}", std::process::id()));
                let _ = fs::remove_dir_all(&tpl_root);
                fs::create_dir_all(&tpl_root).unwrap();
                let tpl_work = tpl_root.join("work");
                fs::create_dir(&tpl_work).unwrap();
                let git = Git { root: &tpl_root };
                git.text(&["init", "--bare", "remote.git"]).unwrap();
                let git = Git { root: &tpl_work };
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
                    tpl_root.join("remote.git").to_str().unwrap(),
                ])
                .unwrap();
                fs::write(tpl_work.join("source.txt"), "one").unwrap();
                git.text(&["add", "source.txt"]).unwrap();
                git.text(&["commit", "-m", "one"]).unwrap();
                git.text(&["push", "origin", "main"]).unwrap();
                tpl_root
            });

            let root = std::env::temp_dir().join(format!(
                "susi-git-snapshot-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            copy_dir_all(tpl, &root);
            let work = root.join("work");
            set_git_origin_url(&work, &root.join("remote.git"));
            Self { root, work }
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
    fn adjacent_native_helper_preserves_inherited_search_path() {
        let repo = Repo::new();
        let executable = repo.work.join("local-snapshot");
        let inherited = std::env::join_paths([&repo.root]).unwrap();
        assert!(adjacent_helper_path(&executable, Some(inherited.clone()))
            .unwrap()
            .is_none());
        let helper = repo.work.join(if cfg!(windows) {
            "push-policy.exe"
        } else {
            "push-policy"
        });
        fs::write(&helper, "fixture only").unwrap();
        let path = adjacent_helper_path(&executable, Some(inherited))
            .unwrap()
            .unwrap();
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            vec![repo.work.clone(), repo.root.clone()]
        );
        fs::remove_file(&helper).unwrap();
        fs::create_dir(&helper).unwrap();
        assert!(adjacent_helper_path(&executable, None).is_err());
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
            schema: SCHEMA,
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
            schema: SCHEMA,
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
            schema: SCHEMA,
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
        // Both URLs name the resolved address: on a Windows host whose TEMP
        // is an 8.3 short path the fixture's origin URL is spelled short while
        // the endpoint is canonical, and a pushurl alone would then fail the
        // fetch/push match before the rewrite check under test is reached.
        for key in ["remote.origin.url", "remote.origin.pushurl"] {
            repo.git()
                .text(&["config", key, &endpoint.address])
                .unwrap();
        }
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

    #[test]
    fn main_submission_and_retry_pin_the_committed_source() {
        let repo = Repo::new();
        let source = repo.commit("agent source");
        assert!(repo
            .run("submit", "main-one")
            .unwrap()
            .contains("Main is unqualified"));
        let intent = read_intent(
            &repo.git().state_path("submissions", "main-one").unwrap(),
            "main-one",
        )
        .unwrap();
        assert_eq!(intent.schema, MAIN_SCHEMA);
        assert_eq!(intent.commit, source);
        let newer = repo.commit("not submitted");
        assert!(repo.run("retry-main", "main-one").is_ok());
        assert!(repo.run("status-main", "main-one").is_ok());
        let remote = repo
            .git()
            .remote_ref(&repo.git().endpoint().unwrap(), "refs/heads/main")
            .unwrap()
            .unwrap();
        assert_eq!(remote, source);
        assert!(!repo.git().ancestor(&newer, &remote).unwrap());
    }

    #[test]
    fn divergent_main_is_composed_without_touching_the_worker_checkout() {
        let repo = Repo::new();
        let base = repo.git().text(&["rev-parse", "HEAD"]).unwrap();
        let remote = repo.commit("remote source");
        repo.git().text(&["push", "origin", "main"]).unwrap();
        repo.git().text(&["checkout", "--detach", &base]).unwrap();
        fs::write(repo.work.join("agent.txt"), "worker change").unwrap();
        repo.git().text(&["add", "agent.txt"]).unwrap();
        repo.git().text(&["commit", "-m", "worker source"]).unwrap();
        let source = repo.git().text(&["rev-parse", "HEAD"]).unwrap();
        repo.run("submit", "composed").unwrap();
        let published = repo
            .git()
            .remote_ref(&repo.git().endpoint().unwrap(), "refs/heads/main")
            .unwrap()
            .unwrap();
        assert!(repo.git().ancestor(&remote, &published).unwrap());
        assert!(repo.git().ancestor(&source, &published).unwrap());
        assert_eq!(
            repo.git()
                .text(&["show", &format!("{published}:source.txt")])
                .unwrap(),
            "remote source"
        );
        assert_eq!(
            repo.git()
                .text(&["show", &format!("{published}:agent.txt")])
                .unwrap(),
            "worker change"
        );
        assert_eq!(repo.git().text(&["rev-parse", "HEAD"]).unwrap(), source);
        assert!(repo
            .git()
            .text(&["status", "--porcelain"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn main_conflicts_and_dirty_sources_never_publish() {
        let repo = Repo::new();
        let base = repo.git().text(&["rev-parse", "HEAD"]).unwrap();
        let remote = repo.commit("remote edit");
        repo.git().text(&["push", "origin", "main"]).unwrap();
        repo.git().text(&["checkout", "--detach", &base]).unwrap();
        repo.commit("conflicting worker edit");
        assert!(repo
            .run("submit", "conflict")
            .unwrap_err()
            .contains("conflicts"));
        assert_eq!(
            repo.git()
                .remote_ref(&repo.git().endpoint().unwrap(), "refs/heads/main")
                .unwrap(),
            Some(remote)
        );
        fs::write(repo.work.join("uncommitted.txt"), "not committed").unwrap();
        assert!(repo
            .run("submit", "dirty")
            .unwrap_err()
            .contains("commit the scoped source"));
        assert!(!repo
            .git()
            .state_path("submissions", "dirty")
            .unwrap()
            .exists());
    }

    #[test]
    fn concurrent_main_submissions_preserve_both_workers() {
        let repo = Repo::new();
        let endpoint = repo.git().endpoint().unwrap();
        let mut workers = Vec::new();
        for name in ["worker-a", "worker-b"] {
            let work = repo.root.join(name);
            repo.git()
                .text(&[
                    "clone",
                    "--no-checkout",
                    &endpoint.address,
                    work.to_str().unwrap(),
                ])
                .unwrap();
            let git = Git { root: &work };
            git.text(&["config", "user.name", "Snapshot Test"]).unwrap();
            git.text(&["config", "user.email", "snapshot@test.invalid"])
                .unwrap();
            git.text(&["config", "commit.gpgsign", "false"]).unwrap();
            git.text(&["config", "core.hooksPath", ".no-test-hooks"])
                .unwrap();
            git.text(&["checkout", "-b", name, "origin/main"]).unwrap();
            fs::write(work.join(format!("{name}.txt")), name).unwrap();
            git.text(&["add", "."]).unwrap();
            git.text(&["commit", "-m", name]).unwrap();
            let source = git.text(&["rev-parse", "HEAD"]).unwrap();
            workers.push((work, name, source));
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = workers
                .iter()
                .map(|(work, name, _)| {
                    scope.spawn(move || run(work, &["submit".into(), (*name).into()]))
                })
                .collect();
            for handle in handles {
                handle.join().unwrap().unwrap();
            }
        });
        let published = repo
            .git()
            .remote_ref(&endpoint, "refs/heads/main")
            .unwrap()
            .unwrap();
        repo.git().fetch_commit(&endpoint, &published).unwrap();
        for (_, name, source) in workers {
            assert!(repo.git().ancestor(&source, &published).unwrap());
            assert_eq!(
                repo.git()
                    .text(&["show", &format!("{published}:{name}.txt")])
                    .unwrap(),
                name
            );
        }
    }

    struct FakeMain {
        observations: VecDeque<Result<String>>,
        pushes: usize,
        result: MainPush,
    }
    impl MainTransport for FakeMain {
        fn observe(&mut self) -> Result<String> {
            self.observations.pop_front().unwrap()
        }
        fn ancestor(&mut self, source: &str, head: &str) -> Result<bool> {
            Ok(source == head
                || (source.starts_with("base") && head.starts_with("base") && source < head))
        }
        fn compose(&mut self, _: &str) -> Result<String> {
            Ok("candidate".into())
        }
        fn push(&mut self, _: &str) -> MainPush {
            self.pushes += 1;
            self.result
        }
    }

    #[test]
    fn main_accepted_but_error_is_reconciled_without_another_push() {
        let mut transport = FakeMain {
            observations: [Ok("base".into()), Ok("candidate".into())].into(),
            pushes: 0,
            result: MainPush::Unknown,
        };
        assert!(publish_main(&mut transport, "source", "operation", false).is_ok());
        assert_eq!(transport.pushes, 1);
    }

    #[test]
    fn main_unreadable_readback_stops_with_unknown_outcome() {
        let mut transport = FakeMain {
            observations: [Ok("base".into()), Err("offline".into())].into(),
            pushes: 0,
            result: MainPush::Unknown,
        };
        assert!(publish_main(&mut transport, "source", "operation", false)
            .unwrap_err()
            .contains("outcome is unknown"));
        assert_eq!(transport.pushes, 1);
    }

    #[test]
    fn main_repeated_races_stop_after_three_attempts() {
        let mut transport = FakeMain {
            observations: ["base1", "base2", "base2", "base3", "base3", "base4"]
                .map(|s| Ok(s.into()))
                .into(),
            pushes: 0,
            result: MainPush::Raced,
        };
        assert!(publish_main(&mut transport, "source", "operation", false)
            .unwrap_err()
            .contains("three publication attempts"));
        assert_eq!(transport.pushes, 3);
    }

    #[test]
    fn main_rejected_push_and_status_never_loop() {
        let mut rejected = FakeMain {
            observations: [Ok("base".into()), Ok("base".into())].into(),
            pushes: 0,
            result: MainPush::Rejected,
        };
        assert!(publish_main(&mut rejected, "source", "operation", false)
            .unwrap_err()
            .contains("was rejected"));
        assert_eq!(rejected.pushes, 1);
        let mut status = FakeMain {
            observations: [Ok("base".into())].into(),
            pushes: 0,
            result: MainPush::Unknown,
        };
        assert!(publish_main(&mut status, "source", "operation", true).is_err());
        assert_eq!(status.pushes, 0);
    }

    #[test]
    fn git_porcelain_cas_races_are_distinct_from_policy_rejections() {
        for reason in [
            "[rejected] (fetch first)",
            "[rejected] (non-fast-forward)",
            "[remote rejected] (failed to update ref)",
            "[remote rejected] (incorrect old value provided)",
        ] {
            assert_eq!(
                rejected_main_push(&format!("!\tcommit:refs/heads/main\t{reason}\n")),
                MainPush::Raced
            );
        }
        assert_eq!(
            rejected_main_push(
                "!\tcommit:refs/heads/main\t[remote rejected] (protected branch hook declined)\n"
            ),
            MainPush::Rejected
        );
        assert_eq!(rejected_main_push("connection lost"), MainPush::Unknown);
    }

    #[test]
    fn legacy_grafts_cannot_falsify_main_publication_evidence() {
        let repo = Repo::new();
        let remote = repo.git().text(&["rev-parse", "HEAD"]).unwrap();
        let source = repo.commit("unpublished source");
        let grafts = PathBuf::from(
            repo.git()
                .text(&[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "info/grafts",
                ])
                .unwrap(),
        );
        fs::create_dir_all(grafts.parent().unwrap()).unwrap();
        fs::write(grafts, format!("{remote} {source}\n")).unwrap();
        assert!(repo
            .run("submit", "grafted")
            .unwrap_err()
            .contains("original commit graph"));
        assert_eq!(
            repo.git()
                .remote_ref(&repo.git().endpoint().unwrap(), "refs/heads/main")
                .unwrap(),
            Some(remote)
        );
    }
}

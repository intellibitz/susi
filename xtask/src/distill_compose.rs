//! Portable Git-native feature and release distillation engine.
//!
//! Reads `.agents/tasks/*.json` task manifests and optional `susi-feature-manifest-v1`
//! specs directly from Git commits, computes bounded transitive dependency closures,
//! rejects cycles, missing manifests, disallowed SPDX licenses and out-of-scope diffs,
//! and selectively composes candidate branches via 3-way `git merge-tree --write-tree
//! --merge-base` without leaking unselected commits from development `main`.
//!
//! Produces a deterministic, content-addressed `susi-distillation-receipt-v1` receipt
//! recording every `included`, `satisfied_in_base`, `deferred` and `rejected` task with
//! structured reasons, creates local branches via compare-and-swap, and optionally
//! publishes immutable `refs/tags/susi-distill/<name>` snapshots with post-push
//! remote verification. Full release qualification gates remain separate.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[path = "distill/hostmerge.rs"]
mod hostmerge;

type Result<T> = std::result::Result<T, String>;

pub const RECEIPT_SCHEMA: &str = "susi-distillation-receipt-v1";
pub const FEATURE_SCHEMA: &str = "susi-feature-manifest-v1";
pub const PLAN_SCHEMA: &str = "susi-distill-plan/v1";
const MAX_CLOSURE_DEPTH: usize = 256;
const ZERO_SHA1: &str = "0000000000000000000000000000000000000000";
const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const USAGE: &str = "usage: cargo run --offline --locked -p xtask --bin local-distill -- <plan|compose|orchestrate> --name <name> --base <rev> [--from <rev>] [--select <TASK_ID>...] [--tasks-from-range <A..B>] [--feature <path-or-rev:path>] [--plan-manifest <path>] [--verify-accept] [--publish-snapshot] [--publish-release-branch] [--release-candidate] [--strict] [--adopt-host-commit <full-commit-id>]... [--receipt-out <path>] [--json]\n       cargo run --offline --locked -p xtask --bin local-distill -- <publish|publish-branch|status> <name> [--json]\n       cargo run --offline --locked -p xtask --bin local-distill -- submit-main [--rev <rev>] [--max-attempts <n>] [--json]\nDistillation composes an audited candidate from labelled contributions. Full release qualification gates remain separate.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandMode {
    Plan,
    Compose,
    Orchestrate,
    Publish,
    PublishBranch,
    Status,
}

#[derive(Clone, Debug)]
struct DistillArgs {
    mode: CommandMode,
    name: String,
    base_rev: String,
    from_rev: Option<String>,
    selected: Vec<String>,
    tasks_from_range: Option<String>,
    feature_spec: Option<String>,
    plan_manifest: Option<PathBuf>,
    verify_accept: bool,
    publish_snapshot: bool,
    publish_release_branch: bool,
    release_candidate: bool,
    strict: bool,
    /// Host-created commits the distilling agent explicitly adopts (full ids);
    /// every adoption is written into the receipt.
    adopt_host_commits: BTreeSet<String>,
    receipt_out: Option<PathBuf>,
    json_output: bool,
}

#[derive(Clone, Debug)]
struct InspectArgs {
    mode: CommandMode,
    name: String,
    json_output: bool,
}

#[derive(Clone, Debug)]
struct SubmitMainArgs {
    rev: Option<String>,
    max_attempts: usize,
    json_output: bool,
}

#[derive(Clone, Debug)]
enum ParsedCli {
    Help,
    Distill(DistillArgs),
    Inspect(InspectArgs),
    SubmitMain(SubmitMainArgs),
}

#[derive(Clone, Debug)]
struct Endpoint {
    address: String,
    digest: String,
}

#[derive(Clone, Debug)]
struct TaskManifest {
    id: String,
    key: String,
    path: String,
    title: String,
    goal: String,
    owner: String,
    deps: Vec<String>,
    scopes: Vec<String>,
    accept_cmd: Vec<String>,
    source_commits: Vec<String>,
}

#[derive(Clone, Debug)]
struct FeatureManifest {
    id: String,
    title: String,
    tasks: Vec<String>,
    deps: Vec<String>,
    destination_branch: Option<String>,
    license: String,
    enabled_capabilities: Vec<String>,
    disabled_capabilities: Vec<String>,
}

/// Tasks and stream commits a release base already carries through earlier
/// distillations (`Distilled-Task:` / `Source-Commits:` trailers).
#[derive(Debug, Default)]
struct DistilledInBase {
    tasks: BTreeSet<String>,
    source_commits: BTreeSet<String>,
}

#[derive(Clone, Debug)]
struct CommitInfo {
    oid: String,
    parent: String,
    subject: String,
    task_trailers: Vec<String>,
    changed_paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Visited,
}

#[derive(Clone, Debug)]
struct IncludedEntry {
    task_id: String,
    title: String,
    goal: String,
    owner: String,
    deps: Vec<String>,
    scopes: Vec<String>,
    accept_cmd: Vec<String>,
    source_commits: Vec<String>,
    composed_commit: String,
    composed_tree: String,
    changed_files: Vec<String>,
}

#[derive(Clone, Debug)]
struct BaseSatisfiedEntry {
    task_id: String,
    reason_code: &'static str,
    detail: String,
}

#[derive(Clone, Debug)]
struct DecisionEntry {
    task_id: String,
    source_commits: Vec<String>,
    deps: Vec<String>,
    reason_code: &'static str,
    detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskOutcome {
    Included,
    SatisfiedInBase,
    Deferred,
    Rejected,
}

struct Git<'a> {
    root: &'a Path,
}

impl Git<'_> {
    fn output(&self, args: &[&str], input: Option<&[u8]>) -> Result<Output> {
        let mut command = Command::new("git");
        command
            .current_dir(self.root)
            .arg("--literal-pathspecs")
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
                .env("GIT_AUTHOR_NAME", "Susi Distiller")
                .env("GIT_AUTHOR_EMAIL", "distiller@localhost")
                .env("GIT_COMMITTER_NAME", "Susi Distiller")
                .env("GIT_COMMITTER_EMAIL", "distiller@localhost")
                .env("GIT_TERMINAL_PROMPT", "0");
        }
        if args.first() == Some(&"push") {
            let executable = std::env::current_exe().map_err(|e| e.to_string())?;
            if let Some(path) = adjacent_helper_path(&executable, std::env::var_os("PATH"))? {
                command.env("PATH", path);
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start Git: {e}"))?;
        if let Some(bytes) = input {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "Git stdin unavailable".to_owned())?;
            return std::thread::scope(|scope| {
                let writer = scope.spawn(move || stdin.write_all(bytes));
                let out = child
                    .wait_with_output()
                    .map_err(|e| format!("cannot wait for Git: {e}"))?;
                let write_res = writer
                    .join()
                    .map_err(|_| "Git stdin writer thread panicked".to_owned())?;
                if out.status.success() {
                    write_res.map_err(|e| format!("cannot write Git input: {e}"))?;
                }
                Ok(out)
            });
        }
        child
            .wait_with_output()
            .map_err(|e| format!("cannot wait for Git: {e}"))
    }

    fn text(&self, args: &[&str]) -> Result<String> {
        let output = self.output(args, None)?;
        if !output.status.success() {
            return Err(format!("Git {} failed ({})", args[0], output.status));
        }
        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_owned())
            .map_err(|_| "Git returned non-UTF-8 output".to_owned())
    }

    fn bytes(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let output = self.output(args, input)?;
        if !output.status.success() {
            return Err(format!("Git {} failed ({})", args[0], output.status));
        }
        Ok(output.stdout)
    }

    fn require_original_history(&self) -> Result<()> {
        if std::env::var_os("GIT_GRAFT_FILE").is_some() {
            return Err("distillation refuses a GIT_GRAFT_FILE override".to_owned());
        }
        let path = PathBuf::from(self.text(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/grafts",
        ])?);
        if !path.is_absolute() || path.try_exists().map_err(|e| e.to_string())? {
            return Err(
                "legacy Git grafts can alter history; distillation requires the original commit graph"
                    .to_owned(),
            );
        }
        Ok(())
    }

    fn resolve_commit(&self, rev: &str) -> Result<String> {
        if rev.is_empty() || rev.starts_with('-') || rev.chars().any(char::is_control) {
            return Err("invalid Git revision".to_owned());
        }
        let spec = format!("{rev}^{{commit}}");
        let oid = self.text(&["rev-parse", "--verify", &spec])?;
        valid_oid(&oid)?;
        Ok(oid)
    }

    fn tree(&self, commit: &str) -> Result<String> {
        valid_oid(commit)?;
        let spec = format!("{commit}^{{tree}}");
        let oid = self.text(&["rev-parse", "--verify", &spec])?;
        valid_oid(&oid)?;
        Ok(oid)
    }

    fn ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        valid_oid(ancestor)?;
        valid_oid(descendant)?;
        let output = self.output(&["merge-base", "--is-ancestor", ancestor, descendant], None)?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err("cannot verify Git ancestry".to_owned()),
        }
    }

    fn state_path(&self, category: &str, name: &str) -> Result<PathBuf> {
        valid_name(name)?;
        let common = PathBuf::from(self.text(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])?);
        if !common.is_absolute() {
            return Err("Git common directory is not absolute".to_owned());
        }
        Ok(common
            .join("susi-distill")
            .join(category)
            .join(format!("{name}.json")))
    }

    fn zero_oid(&self, sample_oid: &str) -> &'static str {
        if sample_oid.len() == 64 {
            ZERO_SHA256
        } else {
            ZERO_SHA1
        }
    }

    fn ensure_cas_ref(&self, reference: &str, commit: &str) -> Result<()> {
        valid_oid(commit)?;
        if let Ok(existing) = self.text(&["show-ref", "--verify", "--hash", reference]) {
            if existing == commit {
                return Ok(());
            }
            return Err(format!(
                "ref {reference} already exists at a different commit; refusing to overwrite"
            ));
        }
        let zero = self.zero_oid(commit);
        let output = self.output(&["update-ref", reference, commit, zero], None)?;
        if output.status.success() {
            return Ok(());
        }
        if self.text(&["show-ref", "--verify", "--hash", reference])? == commit {
            return Ok(());
        }
        Err(format!(
            "concurrent creation raced on {reference}; no ref was overwritten"
        ))
    }

    fn endpoint_optional(&self) -> Result<Option<Endpoint>> {
        let fetch_out = self.output(&["remote", "get-url", "--all", "origin"], None)?;
        if !fetch_out.status.success() {
            return Ok(None);
        }
        let fetch = String::from_utf8(fetch_out.stdout)
            .map_err(|_| "non-UTF-8 origin URL".to_owned())?
            .trim()
            .to_owned();
        let push = self.text(&["remote", "get-url", "--push", "--all", "origin"])?;
        if fetch.is_empty() || fetch.lines().count() != 1 || fetch != push {
            return Err(
                "origin must have exactly one matching fetch and push destination".to_owned(),
            );
        }
        let address = normalize_endpoint(self.root, &fetch)?;
        let digest = sha256_hex(format!("susi-distill-endpoint-v1\n{address}\n").as_bytes());
        Ok(Some(Endpoint { address, digest }))
    }

    fn remote_ref(&self, endpoint: &Endpoint, reference: &str) -> Result<Option<String>> {
        let output = self.text(&["ls-remote", "--refs", "--", &endpoint.address, reference])?;
        if output.is_empty() {
            return Ok(None);
        }
        let mut lines = output.lines();
        let line = lines
            .next()
            .ok_or_else(|| "missing remote ref".to_owned())?;
        let (oid, name) = line
            .split_once('\t')
            .ok_or_else(|| "malformed remote ref".to_owned())?;
        valid_oid(oid)?;
        if name != reference || lines.next().is_some() {
            return Err("ambiguous or unexpected remote ref".to_owned());
        }
        Ok(Some(oid.to_owned()))
    }

    fn tree_blobs(&self, commit: &str, prefix: &str) -> Result<Vec<(String, String)>> {
        valid_oid(commit)?;
        let raw = self.bytes(&["ls-tree", "-r", "-z", commit, "--", prefix], None)?;
        let mut out = Vec::new();
        for record in raw.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let text =
                std::str::from_utf8(record).map_err(|_| "non-UTF-8 path in Git tree".to_owned())?;
            let (meta, path) = text
                .split_once('\t')
                .ok_or_else(|| "malformed ls-tree entry".to_owned())?;
            let parts: Vec<&str> = meta.split_whitespace().collect();
            if parts.len() != 3 || parts[1] != "blob" {
                continue;
            }
            valid_oid(parts[2])?;
            out.push((path.to_owned(), parts[2].to_owned()));
        }
        Ok(out)
    }

    fn read_blobs(&self, oids: &[String]) -> Result<Vec<Vec<u8>>> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        let mut stdin_buf = String::new();
        for oid in oids {
            valid_oid(oid)?;
            stdin_buf.push_str(oid);
            stdin_buf.push('\n');
        }
        let raw = self.bytes(&["cat-file", "--batch"], Some(stdin_buf.as_bytes()))?;
        let mut cursor = 0usize;
        let mut results = Vec::with_capacity(oids.len());
        for expected in oids {
            let nl = raw[cursor..]
                .iter()
                .position(|b| *b == b'\n')
                .ok_or_else(|| "truncated cat-file header".to_owned())?;
            let header = std::str::from_utf8(&raw[cursor..cursor + nl])
                .map_err(|_| "invalid cat-file header".to_owned())?;
            cursor += nl + 1;
            let parts: Vec<&str> = header.split_whitespace().collect();
            if parts.len() != 3 || parts[0] != expected || parts[1] != "blob" {
                return Err(format!("missing or non-blob Git object {expected}"));
            }
            let size: usize = parts[2]
                .parse()
                .map_err(|_| "invalid cat-file blob size".to_owned())?;
            let end = cursor
                .checked_add(size)
                .filter(|end| *end < raw.len() && raw[*end] == b'\n')
                .ok_or_else(|| "truncated cat-file payload".to_owned())?;
            results.push(raw[cursor..end].to_vec());
            cursor = end + 1;
        }
        Ok(results)
    }

    fn diff_paths(&self, left: &str, right: &str) -> Result<Vec<String>> {
        valid_oid(left)?;
        valid_oid(right)?;
        let raw = self.bytes(
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "-z",
                left,
                right,
            ],
            None,
        )?;
        let mut paths = BTreeSet::new();
        for entry in raw.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let path =
                std::str::from_utf8(entry).map_err(|_| "non-UTF-8 path in diff-tree".to_owned())?;
            paths.insert(path.replace('\\', "/"));
        }
        Ok(paths.into_iter().collect())
    }

    fn inspect_commit(&self, oid: &str) -> Result<CommitInfo> {
        self.inspect_commit_inner(oid, false)
    }

    fn inspect_commit_allow_root(&self, oid: &str) -> Result<CommitInfo> {
        self.inspect_commit_inner(oid, true)
    }

    fn inspect_commit_inner(&self, oid: &str, allow_root: bool) -> Result<CommitInfo> {
        valid_oid(oid)?;
        let meta = self.text(&["log", "-n", "1", "--format=%P%x00%s%x00%B", oid])?;
        let mut fields = meta.splitn(3, '\0');
        let parents_str = fields.next().unwrap_or("").trim();
        let subject = fields.next().unwrap_or("").trim().to_owned();
        let body = fields.next().unwrap_or("");
        let mut task_trailers = Vec::new();
        for line in body.lines() {
            push_task_trailer_ids(line, &mut task_trailers);
        }
        let (parent, changed_paths) = match parents_str.split_whitespace().next() {
            Some(p) => {
                valid_oid(p)?;
                let paths = self.diff_paths(p, oid)?;
                (p.to_owned(), paths)
            }
            None if allow_root => {
                let paths: Vec<String> = self
                    .tree_blobs(oid, ".")?
                    .into_iter()
                    .map(|(p, _)| p)
                    .collect();
                (self.zero_oid(oid).to_owned(), paths)
            }
            None => {
                return Err(format!("commit {oid} has no parent commit to diff against"));
            }
        };
        Ok(CommitInfo {
            oid: oid.to_owned(),
            parent,
            subject,
            task_trailers,
            changed_paths,
        })
    }

    /// What `base` already carries from earlier distillations: the stream
    /// commits named in `Source-Commits:` and the ids in `Distilled-Task:`
    /// of every distilled commit in `from..base`, i.e. the history a release
    /// base has and the development stream lacks. A release distilled from
    /// main holds those commits as new composed commits, so without this the
    /// next distillation onto that release would apply them a second time.
    fn distilled_into_base(&self, from: &str, base: &str) -> Result<DistilledInBase> {
        valid_oid(from)?;
        valid_oid(base)?;
        let range = format!("{from}..{base}");
        let raw = self.bytes(&["log", "--no-merges", "-z", "--format=%B", &range], None)?;
        let mut found = DistilledInBase::default();
        for body in raw.split(|byte| *byte == 0) {
            let body = std::str::from_utf8(body)
                .map_err(|_| "non-UTF-8 commit message in Git log".to_owned())?;
            let mut tasks = Vec::new();
            let mut sources = Vec::new();
            for line in body.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("Distilled-Task:") {
                    tasks.extend(
                        rest.split(|c: char| c == ',' || c.is_whitespace())
                            .filter(|id| !id.is_empty())
                            .map(str::to_ascii_uppercase),
                    );
                } else if let Some(rest) = line.strip_prefix("Source-Commits:") {
                    for oid in rest.split(|c: char| c == ',' || c.is_whitespace()) {
                        if oid.is_empty() {
                            continue;
                        }
                        valid_oid(oid).map_err(|_| {
                            format!("distilled commit in {range} names an invalid source commit '{oid}'")
                        })?;
                        sources.push(oid.to_owned());
                    }
                }
            }
            if !tasks.is_empty() {
                found.tasks.extend(tasks);
                found.source_commits.extend(sources);
            }
        }
        Ok(found)
    }

    fn non_merge_commits(&self, base: &str, from: &str) -> Result<Vec<CommitInfo>> {
        valid_oid(base)?;
        valid_oid(from)?;
        let range = format!("{base}..{from}");
        let raw = self.bytes(
            &[
                "log",
                "--topo-order",
                "--reverse",
                "--no-merges",
                "-z",
                "--raw",
                "--no-abbrev",
                "--no-renames",
                "--format=%H%x00%P%x00%s%x00%B",
                &range,
            ],
            None,
        )?;
        parse_batched_commit_stream(&raw)
    }

    fn merge_three_way(&self, merge_base: &str, ours: &str, theirs: &str) -> Result<ThreeWayMerge> {
        use std::sync::atomic::{AtomicBool, Ordering};
        static MERGE_BASE_FLAG_UNSUPPORTED: AtomicBool = AtomicBool::new(false);

        valid_oid(merge_base)?;
        valid_oid(ours)?;
        valid_oid(theirs)?;

        if !MERGE_BASE_FLAG_UNSUPPORTED.load(Ordering::Relaxed) {
            let merge_base_arg = format!("--merge-base={merge_base}");
            let output = self.output(
                &["merge-tree", "--write-tree", &merge_base_arg, ours, theirs],
                None,
            )?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.code() == Some(129) && stderr.contains("merge-base") {
                MERGE_BASE_FLAG_UNSUPPORTED.store(true, Ordering::Relaxed);
            } else {
                return parse_merge_tree_output(output, merge_base, theirs);
            }
        }
        self.merge_three_way_synthetic_base(merge_base, ours, theirs)
    }

    /// Git 2.38–2.39 (`merge-tree --write-tree` without `--merge-base=`): bind the
    /// exact `merge_base`, `ours` and `theirs` trees into a deterministic 3-commit
    /// object-store DAG (`ours <- base -> theirs`) so `merge-ort` resolves against
    /// the identical explicit base tree without touching any index or worktree.
    fn merge_three_way_synthetic_base(
        &self,
        merge_base: &str,
        ours: &str,
        theirs: &str,
    ) -> Result<ThreeWayMerge> {
        let base_tree = self.tree(merge_base)?;
        let ours_tree = self.tree(ours)?;
        let theirs_tree = self.tree(theirs)?;
        let synth_base = self.commit_tree_raw(&base_tree, &[], "distill-merge-base\n")?;
        let synth_ours =
            self.commit_tree_raw(&ours_tree, &[synth_base.as_str()], "distill-ours\n")?;
        let synth_theirs =
            self.commit_tree_raw(&theirs_tree, &[synth_base.as_str()], "distill-theirs\n")?;
        let output = self.output(
            &["merge-tree", "--write-tree", &synth_ours, &synth_theirs],
            None,
        )?;
        parse_merge_tree_output(output, merge_base, theirs)
    }

    fn commit_tree(&self, tree: &str, parent: &str, message: &str) -> Result<String> {
        self.commit_tree_parents(tree, &[parent], message)
    }

    fn commit_tree_parents(&self, tree: &str, parents: &[&str], message: &str) -> Result<String> {
        if parents.is_empty() {
            return Err("commit-tree requires at least one parent".to_owned());
        }
        self.commit_tree_raw(tree, parents, message)
    }

    fn commit_tree_raw(&self, tree: &str, parents: &[&str], message: &str) -> Result<String> {
        valid_oid(tree)?;
        let mut command = Command::new("git");
        command
            .current_dir(self.root)
            .arg("--no-replace-objects")
            .arg("commit-tree")
            .arg(tree);
        for parent in parents {
            valid_oid(parent)?;
            command.args(["-p", parent]);
        }
        command
            .args(["-F", "-"])
            .env("GIT_AUTHOR_NAME", "Susi Distiller")
            .env("GIT_AUTHOR_EMAIL", "distiller@localhost")
            .env("GIT_AUTHOR_DATE", "2026-11-10T00:00:00Z")
            .env("GIT_COMMITTER_NAME", "Susi Distiller")
            .env("GIT_COMMITTER_EMAIL", "distiller@localhost")
            .env("GIT_COMMITTER_DATE", "2026-11-10T00:00:00Z")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::piped())
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
            command
                .env("HOME", self.root)
                .env("USERPROFILE", self.root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env(
                    "GIT_CONFIG_GLOBAL",
                    self.root.join(".absent-fixture-config"),
                );
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start git commit-tree: {e}"))?;
        child
            .stdin
            .take()
            .ok_or_else(|| "commit-tree stdin unavailable".to_owned())?
            .write_all(message.as_bytes())
            .map_err(|e| format!("cannot write commit-tree message: {e}"))?;
        let output = child
            .wait_with_output()
            .map_err(|e| format!("cannot wait for commit-tree: {e}"))?;
        if !output.status.success() {
            return Err("cannot create distilled commit object".to_owned());
        }
        let commit = String::from_utf8(output.stdout)
            .map_err(|_| "invalid commit-tree OID".to_owned())?
            .trim()
            .to_owned();
        valid_oid(&commit)?;
        Ok(commit)
    }

    fn commit_single_blob_envelope(
        &self,
        parent_commit: &str,
        rel_path: &str,
        content: &[u8],
        message: &str,
    ) -> Result<String> {
        valid_oid(parent_commit)?;
        let blob_raw = self.bytes(&["hash-object", "-w", "--stdin"], Some(content))?;
        let blob_oid = String::from_utf8(blob_raw)
            .map_err(|_| "invalid blob OID".to_owned())?
            .trim()
            .to_owned();
        valid_oid(&blob_oid)?;

        let common = PathBuf::from(self.text(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])?);
        let tmp_dir = common.join("susi-distill").join("tmp");
        fs::create_dir_all(&tmp_dir).map_err(|e| format!("cannot create temp index dir: {e}"))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_index = tmp_dir.join(format!("envelope-{}-{nanos}.idx", std::process::id()));
        struct TempIndexGuard(PathBuf);
        impl Drop for TempIndexGuard {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let _guard = TempIndexGuard(tmp_index.clone());

        let run_with_index = |args: &[&str]| -> Result<String> {
            let mut command = Command::new("git");
            command
                .current_dir(self.root)
                .arg("--literal-pathspecs")
                .arg("--no-replace-objects")
                .args(args)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::null());
            for key in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_COMMON_DIR",
                "GIT_OBJECT_DIRECTORY",
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                "GIT_GRAFT_FILE",
            ] {
                command.env_remove(key);
            }
            command.env("GIT_INDEX_FILE", &tmp_index);
            let out = command
                .output()
                .map_err(|e| format!("cannot run git index helper: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "git {} failed ({}): {}",
                    args[0],
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            String::from_utf8(out.stdout)
                .map(|s| s.trim().to_owned())
                .map_err(|_| "non-UTF-8 git index output".to_owned())
        };

        run_with_index(&["read-tree", parent_commit])?;
        let cacheinfo = format!("100644,{blob_oid},{rel_path}");
        run_with_index(&["update-index", "--add", "--cacheinfo", &cacheinfo])?;
        let tree_oid = run_with_index(&["write-tree"])?;
        valid_oid(&tree_oid)?;
        self.commit_tree_parents(&tree_oid, &[parent_commit], message)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ThreeWayMerge {
    Clean(String),
    Conflict(String),
}

fn parse_merge_tree_output(
    output: Output,
    merge_base: &str,
    theirs: &str,
) -> Result<ThreeWayMerge> {
    match output.status.code() {
        Some(0) => {
            let stdout = String::from_utf8(output.stdout)
                .map_err(|_| "non-UTF-8 merge-tree output".to_owned())?;
            let tree = stdout
                .lines()
                .next()
                .ok_or_else(|| "empty merge-tree output".to_owned())?
                .trim()
                .to_owned();
            valid_oid(&tree)?;
            Ok(ThreeWayMerge::Clean(tree))
        }
        Some(1) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let first_line = stdout.lines().nth(1).unwrap_or("conflicting paths");
            Ok(ThreeWayMerge::Conflict(format!(
                "3-way merge-tree conflict applying {theirs} (base {merge_base}): {first_line}"
            )))
        }
        Some(code) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr
                .lines()
                .next()
                .unwrap_or("unexpected merge-tree failure");
            Err(format!("git merge-tree failed (exit {code}): {detail}"))
        }
        None => Err("git merge-tree terminated by signal".to_owned()),
    }
}

fn parse_batched_commit_stream(raw: &[u8]) -> Result<Vec<CommitInfo>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let tokens: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    let mut commits = Vec::new();
    let mut cursor = 0usize;

    while cursor < tokens.len() {
        let first = std::str::from_utf8(tokens[cursor])
            .map_err(|_| "non-UTF-8 commit header in Git log".to_owned())?
            .trim_start_matches(['\n', '\r'])
            .trim();
        if first.is_empty() {
            cursor += 1;
            continue;
        }
        valid_oid(first)?;
        if cursor + 3 >= tokens.len() {
            return Err(format!("truncated batched Git log header for {first}"));
        }
        let parents_str = std::str::from_utf8(tokens[cursor + 1])
            .map_err(|_| "non-UTF-8 parent list in Git log".to_owned())?
            .trim();
        let subject = std::str::from_utf8(tokens[cursor + 2])
            .map_err(|_| "non-UTF-8 subject in Git log".to_owned())?
            .trim()
            .to_owned();
        let body = std::str::from_utf8(tokens[cursor + 3])
            .map_err(|_| "non-UTF-8 body in Git log".to_owned())?;
        let parent = parents_str
            .split_whitespace()
            .next()
            .ok_or_else(|| format!("commit {first} has no parent commit to diff against"))?
            .to_owned();
        valid_oid(&parent)?;

        let mut task_trailers = Vec::new();
        for line in body.lines() {
            push_task_trailer_ids(line, &mut task_trailers);
        }
        cursor += 4;

        let mut path_set = BTreeSet::new();
        while cursor < tokens.len() {
            let peek = std::str::from_utf8(tokens[cursor])
                .map_err(|_| "non-UTF-8 diff header in Git log".to_owned())?
                .trim_start_matches(['\n', '\r']);
            if !peek.starts_with(':') {
                break;
            }
            if cursor + 1 >= tokens.len() {
                return Err(format!("truncated raw diff path entry in commit {first}"));
            }
            let path = std::str::from_utf8(tokens[cursor + 1])
                .map_err(|_| "non-UTF-8 file path in Git log".to_owned())?
                .replace('\\', "/");
            if !path.is_empty() {
                path_set.insert(path);
            }
            cursor += 2;
        }

        commits.push(CommitInfo {
            oid: first.to_owned(),
            parent,
            subject,
            task_trailers,
            changed_paths: path_set.into_iter().collect(),
        });
    }

    Ok(commits)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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

/// Adds the task ids named by one `Task:` trailer line. A trailer may name
/// several ids separated by commas and/or whitespace (labelling rule 1 in
/// docs/distillation.md, as `distill lint` reads it); ids are uppercased and
/// kept once each.
fn push_task_trailer_ids(line: &str, out: &mut Vec<String>) {
    let trimmed = line.trim();
    let Some(prefix) = trimmed.get(..5) else {
        return;
    };
    if !prefix.eq_ignore_ascii_case("task:") {
        return;
    }
    for id in trimmed[5..].split(|c: char| c == ',' || c.is_whitespace()) {
        if id.is_empty() {
            continue;
        }
        let id = id.to_ascii_uppercase();
        if !out.contains(&id) {
            out.push(id);
        }
    }
}

fn valid_name(name: &str) -> Result<()> {
    let stem = name.split('.').next().unwrap_or(name);
    let device = matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if name.is_empty()
        || name.len() > 48
        || device
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
        || name.starts_with(['-', '.'])
        || name.ends_with(['-', '.'])
        || name.contains("..")
    {
        return Err(
            "distillation name needs 1-48 lowercase letters/digits/interior hyphens or dots, not a Windows device name"
                .to_owned(),
        );
    }
    Ok(())
}

fn adjacent_helper_path(
    executable: &Path,
    inherited: Option<std::ffi::OsString>,
) -> Result<Option<std::ffi::OsString>> {
    let directory = executable
        .parent()
        .ok_or_else(|| "distiller executable has no directory".to_owned())?;
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
            return Err(
                "embedded endpoint credentials are not supported; use Git's credential or SSH helper"
                    .to_owned(),
            );
        }
        return Ok(value.to_owned());
    }
    if !Path::new(value).is_absolute() && value.contains(':') {
        let (host, path) = value
            .split_once(':')
            .ok_or_else(|| "invalid SSH endpoint".to_owned())?;
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
    let value = path
        .to_str()
        .ok_or_else(|| "Git endpoint is not UTF-8".to_owned())?;
    Ok(if let Some(unc) = value.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        value.strip_prefix("\\\\?\\").unwrap_or(value).to_owned()
    })
}

fn parse_cli(args: &[String]) -> Result<ParsedCli> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return if args.is_empty() {
            Err(USAGE.to_owned())
        } else {
            Ok(ParsedCli::Help)
        };
    }
    if args[0] == "submit-main" {
        return parse_submit_main_flags(&args[1..]);
    }
    let mode = match args[0].as_str() {
        "plan" => CommandMode::Plan,
        "compose" => CommandMode::Compose,
        "orchestrate" => CommandMode::Orchestrate,
        "publish" => CommandMode::Publish,
        "publish-branch" => CommandMode::PublishBranch,
        "status" => CommandMode::Status,
        _ => return Err(USAGE.to_owned()),
    };
    if matches!(
        mode,
        CommandMode::Publish | CommandMode::PublishBranch | CommandMode::Status
    ) {
        if args.len() < 2 || args.len() > 3 {
            return Err(USAGE.to_owned());
        }
        let json_output = if args.len() == 3 {
            if args[2] != "--json" {
                return Err(USAGE.to_owned());
            }
            true
        } else {
            false
        };
        valid_name(&args[1])?;
        return Ok(ParsedCli::Inspect(InspectArgs {
            mode,
            name: args[1].clone(),
            json_output,
        }));
    }
    parse_distill_flags(mode, &args[1..])
}

fn parse_submit_main_flags(rest: &[String]) -> Result<ParsedCli> {
    let mut rev: Option<String> = None;
    let mut max_attempts: usize = 4;
    let mut json_output = false;
    let mut idx = 0usize;
    while idx < rest.len() {
        match rest[idx].as_str() {
            "--rev" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if rev.replace(val.clone()).is_some() {
                    return Err("duplicate --rev".to_owned());
                }
            }
            "--max-attempts" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                let parsed: usize = val
                    .parse()
                    .map_err(|_| "invalid --max-attempts integer".to_owned())?;
                if parsed == 0 || parsed > 16 {
                    return Err("--max-attempts must be between 1 and 16".to_owned());
                }
                max_attempts = parsed;
            }
            "--json" => json_output = true,
            _ => return Err(USAGE.to_owned()),
        }
        idx += 1;
    }
    Ok(ParsedCli::SubmitMain(SubmitMainArgs {
        rev,
        max_attempts,
        json_output,
    }))
}

fn parse_distill_flags(mode: CommandMode, rest: &[String]) -> Result<ParsedCli> {
    let mut name: Option<String> = None;
    let mut base_rev: Option<String> = None;
    let mut from_rev: Option<String> = None;
    let mut selected = Vec::new();
    let mut tasks_from_range: Option<String> = None;
    let mut feature_spec: Option<String> = None;
    let mut plan_manifest: Option<PathBuf> = None;
    let mut verify_accept = false;
    let mut publish_snapshot = false;
    let mut publish_release_branch = false;
    let mut release_candidate = false;
    let mut strict = false;
    let mut adopt_host_commits: BTreeSet<String> = BTreeSet::new();
    let mut receipt_out: Option<PathBuf> = None;
    let mut json_output = false;

    let mut idx = 0usize;
    while idx < rest.len() {
        match rest[idx].as_str() {
            "--name" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if name.replace(val.clone()).is_some() {
                    return Err("duplicate --name".to_owned());
                }
            }
            "--base" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if base_rev.replace(val.clone()).is_some() {
                    return Err("duplicate --base".to_owned());
                }
            }
            "--from" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if from_rev.replace(val.clone()).is_some() {
                    return Err("duplicate --from".to_owned());
                }
            }
            "--select" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                for item in val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    selected.push(item.to_owned());
                }
            }
            "--tasks-from-range" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if tasks_from_range.replace(val.clone()).is_some() {
                    return Err("duplicate --tasks-from-range".to_owned());
                }
            }
            "--feature" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if feature_spec.replace(val.clone()).is_some() {
                    return Err("duplicate --feature".to_owned());
                }
            }
            "--plan-manifest" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if plan_manifest.replace(PathBuf::from(val)).is_some() {
                    return Err("duplicate --plan-manifest".to_owned());
                }
            }
            "--receipt-out" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                if receipt_out.replace(PathBuf::from(val)).is_some() {
                    return Err("duplicate --receipt-out".to_owned());
                }
            }
            "--verify-accept" => verify_accept = true,
            "--publish-snapshot" => publish_snapshot = true,
            "--publish-release-branch" => publish_release_branch = true,
            "--release-candidate" => release_candidate = true,
            "--strict" => strict = true,
            "--adopt-host-commit" => {
                idx += 1;
                let val = rest.get(idx).ok_or_else(|| USAGE.to_owned())?;
                adopt_host_commits.extend(hostmerge::parse_adoptions(std::slice::from_ref(val))?);
            }
            "--json" => json_output = true,
            _ => return Err(USAGE.to_owned()),
        }
        idx += 1;
    }

    if verify_accept && !matches!(mode, CommandMode::Compose | CommandMode::Orchestrate) {
        return Err("--verify-accept requires compose or orchestrate mode".to_owned());
    }
    if publish_snapshot && mode != CommandMode::Orchestrate {
        return Err("--publish-snapshot requires orchestrate mode".to_owned());
    }
    if publish_release_branch {
        if mode != CommandMode::Orchestrate {
            return Err("--publish-release-branch requires orchestrate mode".to_owned());
        }
        if !release_candidate || !verify_accept {
            return Err(
                "--publish-release-branch requires --release-candidate and --verify-accept"
                    .to_owned(),
            );
        }
    }
    let name = name.ok_or_else(|| "missing --name <name>".to_owned())?;
    valid_name(&name)?;
    let base_rev = base_rev.ok_or_else(|| "missing --base <rev>".to_owned())?;
    if selected.is_empty()
        && tasks_from_range.is_none()
        && feature_spec.is_none()
        && plan_manifest.is_none()
        && mode != CommandMode::Orchestrate
    {
        return Err(
            "specify at least one --select <TASK_ID>, --tasks-from-range <A..B>, --feature <manifest>, or --plan-manifest <path>"
                .to_owned(),
        );
    }
    Ok(ParsedCli::Distill(DistillArgs {
        mode,
        name,
        base_rev,
        from_rev,
        selected,
        tasks_from_range,
        feature_spec,
        plan_manifest,
        verify_accept,
        publish_snapshot,
        publish_release_branch,
        release_candidate,
        strict,
        adopt_host_commits,
        receipt_out,
        json_output,
    }))
}

fn normalize_scope(scope: &str) -> Result<String> {
    let trimmed = scope.trim().replace('\\', "/");
    let clean = trimmed.trim_end_matches('/');
    if clean.is_empty()
        || clean.starts_with('/')
        || clean.chars().any(char::is_control)
        || clean
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(format!("invalid scope path '{scope}'"));
    }
    Ok(clean.to_owned())
}

fn is_forbidden_confidential_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);
    file_name == ".env"
        || file_name.starts_with(".env.")
        || file_name == "credentials.json"
        || Path::new(file_name)
            .extension()
            .is_some_and(|ext| ext == "pem" || ext == "key" || ext == "p12")
}

fn scope_matches_path(path: &str, scope: &str) -> bool {
    if path.eq_ignore_ascii_case(scope) {
        return true;
    }
    path.len() > scope.len()
        && path.as_bytes()[scope.len()] == b'/'
        && path[..scope.len()].eq_ignore_ascii_case(scope)
}

fn path_within_scopes(path: &str, scopes: &[String], manifest_path: &str) -> bool {
    if path.eq_ignore_ascii_case(manifest_path) {
        return true;
    }
    scopes.iter().any(|scope| scope_matches_path(path, scope))
}

fn spdx_allowed(expr: &str) -> bool {
    const ALLOWED: [&str; 4] = ["Apache-2.0", "MIT", "BSD-2-Clause", "BSD-3-Clause"];
    let cleaned = expr.replace(['(', ')'], " ");
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    tokens
        .iter()
        .all(|tok| matches!(*tok, "AND" | "OR") || ALLOWED.contains(tok))
        && tokens.iter().any(|tok| ALLOWED.contains(tok))
}

fn load_feature_manifest(git: &Git<'_>, root: &Path, spec: &str) -> Result<FeatureManifest> {
    let bytes = if let Some((rev, path)) = spec.split_once(':') {
        if !rev.is_empty() && !Path::new(spec).is_absolute() && !rev.ends_with('\\') {
            let commit = git.resolve_commit(rev)?;
            let object = format!("{commit}:{path}");
            git.bytes(&["show", &object], None)?
        } else {
            fs::read(root.join(spec))
                .map_err(|e| format!("cannot read feature manifest {spec}: {e}"))?
        }
    } else {
        fs::read(root.join(spec))
            .map_err(|e| format!("cannot read feature manifest {spec}: {e}"))?
    };
    let val: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid feature manifest JSON {spec}: {e}"))?;
    if val.get("schema").and_then(Value::as_str) != Some(FEATURE_SCHEMA) {
        return Err(format!(
            "feature manifest must declare schema {FEATURE_SCHEMA}"
        ));
    }
    let id = val
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "feature manifest missing string 'id'".to_owned())?
        .to_owned();
    let title = val
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_owned();
    let license = val
        .get("license")
        .and_then(Value::as_str)
        .unwrap_or("Apache-2.0")
        .to_owned();
    if !spdx_allowed(&license) {
        return Err(format!(
            "disallowed_license: feature {id} declares non-allowlisted SPDX expression '{license}'"
        ));
    }
    let tasks = json_string_vec(&val, "tasks", true)?;
    if tasks.is_empty() {
        return Err(format!(
            "feature manifest {id} must list at least one task in 'tasks'"
        ));
    }
    let deps = json_string_vec(&val, "deps", false)?;
    let enabled_capabilities = json_string_vec(&val, "enabled_capabilities", false)?;
    let disabled_capabilities = json_string_vec(&val, "disabled_capabilities", false)?;
    let destination_branch = val
        .get("destination_branch")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(FeatureManifest {
        id,
        title,
        tasks,
        deps,
        destination_branch,
        license,
        enabled_capabilities,
        disabled_capabilities,
    })
}

fn json_string_vec(val: &Value, field: &str, required: bool) -> Result<Vec<String>> {
    let Some(raw) = val.get(field) else {
        return if required {
            Err(format!("missing required array field '{field}'"))
        } else {
            Ok(Vec::new())
        };
    };
    let arr = raw
        .as_array()
        .ok_or_else(|| format!("field '{field}' must be an array of strings"))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| format!("field '{field}' contains non-string element"))?;
        out.push(s.to_owned());
    }
    Ok(out)
}

struct PlanManifestSpec {
    sha256: String,
    base_commit: Option<String>,
    task_ids: Vec<String>,
}

fn load_plan_manifest(root: &Path, path: &Path) -> Result<PlanManifestSpec> {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let bytes = fs::read(&resolved)
        .map_err(|e| format!("cannot read --plan-manifest {}: {e}", resolved.display()))?;
    let val: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("invalid --plan-manifest JSON {}: {e}", resolved.display()))?;
    if val.get("schema").and_then(Value::as_str) != Some(PLAN_SCHEMA) {
        return Err(format!("--plan-manifest must declare schema {PLAN_SCHEMA}"));
    }
    let base_commit = val
        .get("base")
        .and_then(|b| b.get("commit"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(oid) = &base_commit {
        valid_oid(oid)?;
    }
    let mut task_ids = Vec::new();
    if let Some(inc) = val.get("included").and_then(Value::as_array) {
        for entry in inc {
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                if !id.trim().is_empty() {
                    task_ids.push(id.trim().to_owned());
                }
            }
        }
    }
    if task_ids.is_empty() {
        task_ids = json_string_vec(&val, "requested", false)?;
    }
    if task_ids.is_empty() {
        return Err("--plan-manifest contains no included or requested task IDs".to_owned());
    }
    Ok(PlanManifestSpec {
        sha256: sha256_hex(&bytes),
        base_commit,
        task_ids,
    })
}

struct LoadedTasks {
    valid: BTreeMap<String, TaskManifest>,
    invalid: BTreeMap<String, String>,
    done_deps: BTreeMap<String, Vec<String>>,
}

impl LoadedTasks {
    fn deps_of(&self, key: &str) -> Option<&[String]> {
        self.valid
            .get(key)
            .map(|m| m.deps.as_slice())
            .or_else(|| self.done_deps.get(key).map(Vec::as_slice))
    }
}

fn parse_closed_done_deps(bytes: &[u8]) -> Option<(String, Vec<String>)> {
    let val: Value = serde_json::from_slice(bytes).ok()?;
    let id = val.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let mut deps = json_string_vec(&val, "deps", false).unwrap_or_default();
    if deps.is_empty() {
        deps = json_string_vec(&val, "dependencies", false).unwrap_or_default();
    }
    Some((id.to_ascii_uppercase(), deps))
}

fn load_tasks_from_commit(git: &Git<'_>, commit: &str) -> Result<LoadedTasks> {
    let entries: Vec<(String, String)> = git
        .tree_blobs(commit, ".agents/tasks")?
        .into_iter()
        .filter(|(path, _)| {
            path.ends_with(".json")
                && (path
                    .strip_prefix(".agents/tasks/")
                    .is_some_and(|rest| !rest.contains('/'))
                    || path
                        .strip_prefix(".agents/tasks/done/")
                        .is_some_and(|rest| !rest.contains('/'))
                    || path
                        .strip_prefix(".agents/tasks/in-progress/")
                        .is_some_and(|rest| !rest.contains('/')))
        })
        .collect();
    let oids: Vec<String> = entries.iter().map(|(_, oid)| oid.clone()).collect();
    let payloads = git.read_blobs(&oids)?;

    let mut valid: BTreeMap<String, TaskManifest> = BTreeMap::new();
    let mut invalid: BTreeMap<String, String> = BTreeMap::new();
    let mut done_deps: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for ((path, _), bytes) in entries.into_iter().zip(payloads) {
        let is_done = path.starts_with(".agents/tasks/done/");
        let stem = Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_uppercase();
        if is_done {
            if let Some((done_key, deps)) = parse_closed_done_deps(&bytes) {
                done_deps.insert(done_key, deps);
            }
        }
        match parse_task_manifest(&path, &bytes) {
            Ok(task) => {
                if valid.contains_key(&task.key) || invalid.contains_key(&task.key) {
                    valid.remove(&task.key);
                    invalid.insert(
                        task.key.clone(),
                        format!("duplicate task identifier across .agents/tasks records ({path})"),
                    );
                } else {
                    valid.insert(task.key.clone(), task);
                }
            }
            Err(err) => {
                if !stem.is_empty() {
                    invalid.insert(stem, format!("{path}: {err}"));
                }
            }
        }
    }
    Ok(LoadedTasks {
        valid,
        invalid,
        done_deps,
    })
}

fn parse_task_manifest(path: &str, bytes: &[u8]) -> Result<TaskManifest> {
    let val: Value = serde_json::from_slice(bytes).map_err(|e| format!("malformed JSON: {e}"))?;
    let id = val
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing non-empty string 'id'".to_owned())?
        .trim()
        .to_owned();
    let key = id.to_ascii_uppercase();
    let title = val
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| val.get("goal").and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing non-empty 'title' or 'goal'".to_owned())?
        .to_owned();
    let goal = val
        .get("goal")
        .and_then(Value::as_str)
        .unwrap_or(&title)
        .to_owned();
    let owner = val
        .get("created_by")
        .and_then(Value::as_str)
        .or_else(|| val.get("native_worker").and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing 'created_by'".to_owned())?
        .to_owned();
    let mut deps = json_string_vec(&val, "deps", false)?;
    if deps.is_empty() {
        deps = json_string_vec(&val, "dependencies", false)?;
    }
    let mut raw_scopes = json_string_vec(&val, "scopes", false)?;
    if raw_scopes.is_empty() {
        raw_scopes = json_string_vec(&val, "scope", false)?;
    }
    if raw_scopes.is_empty() {
        return Err("missing_scopes: task manifest must declare non-empty 'scopes'".to_owned());
    }
    let mut scopes = Vec::with_capacity(raw_scopes.len());
    for s in &raw_scopes {
        scopes.push(normalize_scope(s)?);
    }
    let accept_obj = val
        .get("accept")
        .ok_or_else(|| "missing 'accept.cmd'".to_owned())?;
    let accept_cmd = json_string_vec(accept_obj, "cmd", true)?;
    if accept_cmd.is_empty() {
        return Err("accept.cmd must not be empty".to_owned());
    }
    let source_commits = json_string_vec(&val, "source_commits", false)?;
    for oid in &source_commits {
        valid_oid(oid)?;
    }
    Ok(TaskManifest {
        id,
        key,
        path: path.to_owned(),
        title,
        goal,
        owner,
        deps,
        scopes,
        accept_cmd,
        source_commits,
    })
}

struct ClosurePlan {
    order: Vec<String>,
    cycle_errors: BTreeMap<String, String>,
}

struct ClosureWalk {
    state: BTreeMap<String, VisitState>,
    stack: Vec<String>,
    order: Vec<String>,
    cycle_errors: BTreeMap<String, String>,
}

fn compute_closure(
    roots: &[String],
    tasks: &LoadedTasks,
    commit_rank: &BTreeMap<String, usize>,
) -> ClosurePlan {
    let mut walk = ClosureWalk {
        state: BTreeMap::new(),
        stack: Vec::new(),
        order: Vec::new(),
        cycle_errors: BTreeMap::new(),
    };

    for root in roots {
        let key = root.trim().to_ascii_uppercase();
        if !key.is_empty() {
            dfs_closure(&key, tasks, &mut walk);
        }
    }

    // Preserve dependency-before-dependent ordering while sorting independent
    // ready tasks deterministically by earliest commit rank in base..from, then key.
    let ordered = deterministic_toposort(&walk.order, tasks, commit_rank, &walk.cycle_errors);
    ClosurePlan {
        order: ordered,
        cycle_errors: walk.cycle_errors,
    }
}

fn dfs_closure(key: &str, tasks: &LoadedTasks, walk: &mut ClosureWalk) {
    if walk.stack.len() >= MAX_CLOSURE_DEPTH {
        walk.cycle_errors.insert(
            key.to_owned(),
            format!("dependency depth exceeded limit {MAX_CLOSURE_DEPTH}"),
        );
        return;
    }
    match walk.state.get(key).copied() {
        Some(VisitState::Visited) => return,
        Some(VisitState::Visiting) => {
            let pos = walk.stack.iter().position(|k| k == key).unwrap_or(0);
            let mut chain: Vec<String> = walk.stack[pos..].to_vec();
            chain.push(key.to_owned());
            let detail = format!("cycle detected: {}", chain.join(" -> "));
            for member in &walk.stack[pos..] {
                walk.cycle_errors.insert(member.clone(), detail.clone());
            }
            return;
        }
        None => {}
    }

    walk.state.insert(key.to_owned(), VisitState::Visiting);
    walk.stack.push(key.to_owned());

    if let Some(raw_deps) = tasks.deps_of(key) {
        let mut sorted_deps: Vec<String> = raw_deps
            .iter()
            .map(|d| d.trim().to_ascii_uppercase())
            .filter(|d| !d.is_empty())
            .collect();
        sorted_deps.sort();
        sorted_deps.dedup();
        for dep_key in sorted_deps {
            dfs_closure(&dep_key, tasks, walk);
        }
    }

    let _ = walk.stack.pop();
    walk.state.insert(key.to_owned(), VisitState::Visited);
    walk.order.push(key.to_owned());
}

fn deterministic_toposort(
    visited_order: &[String],
    tasks: &LoadedTasks,
    commit_rank: &BTreeMap<String, usize>,
    cycle_errors: &BTreeMap<String, String>,
) -> Vec<String> {
    let nodes: BTreeSet<String> = visited_order.iter().cloned().collect();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for key in &nodes {
        indegree.insert(key.clone(), 0);
    }
    for key in &nodes {
        if cycle_errors.contains_key(key) {
            continue;
        }
        if let Some(raw_deps) = tasks.deps_of(key) {
            let mut deps: BTreeSet<String> = BTreeSet::new();
            for dep in raw_deps {
                let dep_key = dep.trim().to_ascii_uppercase();
                if nodes.contains(&dep_key)
                    && dep_key != *key
                    && !cycle_errors.contains_key(&dep_key)
                {
                    deps.insert(dep_key);
                }
            }
            indegree.insert(key.clone(), deps.len());
            for dep_key in deps {
                dependents.entry(dep_key).or_default().push(key.clone());
            }
        }
    }

    let mut ready: BTreeSet<(usize, String)> = BTreeSet::new();
    for (key, deg) in &indegree {
        if *deg == 0 {
            let rank = commit_rank.get(key).copied().unwrap_or(usize::MAX);
            ready.insert((rank, key.clone()));
        }
    }

    let mut out = Vec::with_capacity(nodes.len());
    while let Some((_, current)) = ready.pop_first() {
        out.push(current.clone());
        if let Some(children) = dependents.get(&current) {
            for child in children {
                if let Some(deg) = indegree.get_mut(child) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        let rank = commit_rank.get(child).copied().unwrap_or(usize::MAX);
                        ready.insert((rank, child.clone()));
                    }
                }
            }
        }
    }
    for key in visited_order {
        if !out.contains(key) {
            out.push(key.clone());
        }
    }
    out
}

fn attribute_task_commits(
    git: &Git<'_>,
    from_commit: &str,
    task: &TaskManifest,
    stream_commits: &[CommitInfo],
) -> Result<Vec<CommitInfo>> {
    if !task.source_commits.is_empty() {
        let mut pinned = Vec::with_capacity(task.source_commits.len());
        for oid in &task.source_commits {
            if !git.ancestor(oid, from_commit)? {
                return Err(format!(
                    "pinned source_commit {oid} for {} is not an ancestor of stream {from_commit}",
                    task.id
                ));
            }
            pinned.push(git.inspect_commit(oid)?);
        }
        return Ok(pinned);
    }
    let matched: Vec<CommitInfo> = stream_commits
        .iter()
        .filter(|commit| {
            commit.task_trailers.iter().any(|t| t == &task.key)
                || commit.changed_paths.iter().any(|p| p == &task.path)
        })
        .cloned()
        .collect();
    Ok(matched)
}

type ReasonErr = (&'static str, String);

struct ComposeCursor<'a> {
    base_commit: &'a str,
    current_commit: &'a str,
    current_tree: &'a str,
}

struct ComposedStep {
    commit: String,
    tree: String,
    changed_files: Vec<String>,
}

fn audit_task_scopes(
    task: &TaskManifest,
    commits: &[CommitInfo],
) -> std::result::Result<(), ReasonErr> {
    for commit in commits {
        for path in &commit.changed_paths {
            if is_forbidden_confidential_path(path) {
                return Err((
                    "confidentiality_violation",
                    format!(
                        "commit {} touches forbidden credential/key path '{path}'",
                        commit.oid
                    ),
                ));
            }
            if !path_within_scopes(path, &task.scopes, &task.path) {
                return Err((
                    "out_of_scope_diff",
                    format!(
                        "commit {} modifies '{path}' outside declared scopes {:?}",
                        commit.oid, task.scopes
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn compose_task_onto(
    git: &Git<'_>,
    cursor: &ComposeCursor<'_>,
    task: &TaskManifest,
    commits: &[CommitInfo],
) -> Result<std::result::Result<ComposedStep, ReasonErr>> {
    let mut step_commit = cursor.current_commit.to_owned();
    let mut step_tree = cursor.current_tree.to_owned();

    for commit in commits {
        let merged_tree = match git.merge_three_way(&commit.parent, &step_commit, &commit.oid)? {
            ThreeWayMerge::Clean(tree) => tree,
            ThreeWayMerge::Conflict(conflict_msg) => {
                return Ok(Err(("merge_conflict", conflict_msg)));
            }
        };
        if merged_tree != step_tree {
            let delta_paths = git.diff_paths(&step_tree, &merged_tree)?;
            for path in &delta_paths {
                if !path_within_scopes(path, &task.scopes, &task.path) {
                    return Ok(Err((
                        "out_of_scope_diff",
                        format!(
                            "3-way composition of {} leaked out-of-scope path '{path}'",
                            commit.oid
                        ),
                    )));
                }
            }
            let step_msg = format!("distill-step {}: {}\n", task.id, commit.subject);
            step_commit = git.commit_tree(&merged_tree, &step_commit, &step_msg)?;
            step_tree = merged_tree;
        }
    }

    let changed_files = git.diff_paths(cursor.current_tree, &step_tree)?;
    let source_list: Vec<&str> = commits.iter().map(|c| c.oid.as_str()).collect();
    let message = format!(
        "Distill task {}: {}\n\nUnqualified distilled candidate. Full exact-tree release gates remain required.\nDistilled-Task: {}\nSource-Commits: {}\nDistill-Base: {}\n",
        task.id,
        task.title,
        task.id,
        source_list.join(","),
        cursor.base_commit
    );
    let final_task_commit = git.commit_tree(&step_tree, cursor.current_commit, &message)?;
    Ok(Ok(ComposedStep {
        commit: final_task_commit,
        tree: step_tree,
        changed_files,
    }))
}

struct DetachedVerifyWorktree<'a> {
    git: &'a Git<'a>,
    temp_dir: PathBuf,
    wt_dir: PathBuf,
}

impl Drop for DetachedVerifyWorktree<'_> {
    fn drop(&mut self) {
        if let Some(wt_str) = self.wt_dir.to_str() {
            let _ = self
                .git
                .output(&["worktree", "remove", "--force", wt_str], None);
        }
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

/// Toolchain homes an accept command keeps when `HOME` is replaced.
///
/// Cargo and rustup default to `$HOME/.cargo` and `$HOME/.rustup`, so the
/// scratch `HOME` below would hide the registry cache and the toolchains, and
/// every `--offline` accept command would fail to resolve its first crate.
/// Both are immutable shared caches (AGENTS.md permits sharing those): an
/// explicit non-empty value is kept, otherwise the caller's default is pinned.
fn toolchain_homes(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(&'static str, std::ffi::OsString)> {
    let home = get("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| get("USERPROFILE").filter(|value| !value.is_empty()));
    [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")]
        .into_iter()
        .filter_map(|(key, default_dir)| {
            get(key)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    home.as_ref()
                        .map(|home| Path::new(home).join(default_dir).into_os_string())
                })
                .map(|value| (key, value))
        })
        .collect()
}

fn verify_composed_accept_commands(
    git: &Git<'_>,
    final_commit: &str,
    included: &[IncludedEntry],
) -> Result<Vec<Value>> {
    valid_oid(final_commit)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temp_dir = std::env::temp_dir().join(format!(
        "susi-distill-verify-{}-{nonce}",
        std::process::id()
    ));
    let wt_dir = temp_dir.join("candidate");
    let home_dir = temp_dir.join("home");
    let runtime_dir = temp_dir.join("runtime");
    fs::create_dir_all(&home_dir)
        .and_then(|()| fs::create_dir_all(&runtime_dir))
        .map_err(|e| format!("cannot prepare hermetic accept-verify dirs: {e}"))?;

    let wt_str = wt_dir
        .to_str()
        .ok_or_else(|| "non-UTF-8 temporary worktree path".to_owned())?
        .to_owned();
    let guard = DetachedVerifyWorktree {
        git,
        temp_dir,
        wt_dir: wt_dir.clone(),
    };
    git.text(&["worktree", "add", "--detach", &wt_str, final_commit])?;

    let mut records = Vec::with_capacity(included.len());
    for entry in included {
        let Some((program, cmd_args)) = entry.accept_cmd.split_first() else {
            return Err(format!(
                "accept_verification_failed: {} has empty accept.cmd",
                entry.task_id
            ));
        };
        let mut command = Command::new(program);
        command
            .args(cmd_args)
            .current_dir(&guard.wt_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("HOME", &home_dir)
            .env("USERPROFILE", &home_dir)
            .env("SUSI_HOME", home_dir.join(".susi"))
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("TMPDIR", &runtime_dir)
            .env("TEMP", &runtime_dir)
            .env("TMP", &runtime_dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("SUSI_NO_REGISTRY_ENV", "1");
        for (key, value) in toolchain_homes(|key| std::env::var_os(key)) {
            command.env(key, value);
        }
        for secret_key in [
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "HF_TOKEN",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "OPENROUTER_API_KEY",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
        ] {
            command.env_remove(secret_key);
        }
        let output = command.output().map_err(|e| {
            format!(
                "accept_verification_failed: cannot execute {:?} for {}: {e}",
                entry.accept_cmd, entry.task_id
            )
        })?;
        let mut combined = output.stdout;
        combined.extend_from_slice(&output.stderr);
        let output_sha256 = sha256_hex(&combined);
        let code = output.status.code().unwrap_or(-1);
        if !output.status.success() {
            let preview = String::from_utf8_lossy(&combined);
            let tail: String = preview
                .chars()
                .rev()
                .take(400)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            return Err(format!(
                "accept_verification_failed: task {} command {:?} exited with {} ({})",
                entry.task_id,
                entry.accept_cmd,
                code,
                tail.trim()
            ));
        }
        records.push(json!({
            "task_id": entry.task_id,
            "cmd": entry.accept_cmd,
            "exit_code": code,
            "passed": true,
            "output_sha256": output_sha256,
        }));
    }
    Ok(records)
}

struct DistillOutcome {
    receipt: Value,
    receipt_bytes: Vec<u8>,
    receipt_sha256: String,
    final_commit: String,
    final_tree: String,
    branch_ref: String,
    has_deferred_or_rejected: bool,
}

fn execute_distill(git: &Git<'_>, root: &Path, args: &DistillArgs) -> Result<DistillOutcome> {
    git.require_original_history()?;
    let plan_spec = match &args.plan_manifest {
        Some(path) => Some(load_plan_manifest(root, path)?),
        None => None,
    };
    let effective_from = args
        .from_rev
        .as_deref()
        .or_else(|| plan_spec.as_ref().and_then(|p| p.base_commit.as_deref()))
        .unwrap_or("HEAD");

    let base_commit = git.resolve_commit(&args.base_rev)?;
    let base_tree = git.tree(&base_commit)?;
    let from_commit = git.resolve_commit(effective_from)?;
    let from_tree = git.tree(&from_commit)?;

    let feature = match &args.feature_spec {
        Some(spec) => Some(load_feature_manifest(git, root, spec)?),
        None => None,
    };

    let mut requested: Vec<String> = args.selected.clone();
    if let Some(range_spec) = &args.tasks_from_range {
        requested.extend(discover_tasks_in_range(git, range_spec)?);
    }
    if let Some(plan) = &plan_spec {
        requested.extend(plan.task_ids.clone());
    }
    if let Some(feat) = &feature {
        requested.extend(feat.tasks.clone());
        requested.extend(feat.deps.clone());
    }

    let from_tasks = load_tasks_from_commit(git, &from_commit)?;
    let base_tasks = load_tasks_from_commit(git, &base_commit)?;
    let distilled_in_base = git.distilled_into_base(&from_commit, &base_commit)?;
    let stream_commits: Vec<CommitInfo> = git
        .non_merge_commits(&base_commit, &from_commit)?
        .into_iter()
        .filter(|commit| !distilled_in_base.source_commits.contains(&commit.oid))
        .collect();
    let base_reason = |key: &str| -> (&'static str, &'static str) {
        if distilled_in_base.tasks.contains(key) {
            (
                "already_distilled_in_base",
                "was distilled into base by an earlier distillation (Distilled-Task/Source-Commits)",
            )
        } else {
            ("already_in_base", "is already present in base")
        }
    };

    if args.mode == CommandMode::Orchestrate && requested.is_empty() {
        let mut seen = BTreeSet::new();
        for commit in &stream_commits {
            for trailer in &commit.task_trailers {
                if seen.insert(trailer.clone()) {
                    requested.push(trailer.clone());
                }
            }
        }
    }

    let mut commit_rank: BTreeMap<String, usize> = BTreeMap::new();
    for (idx, commit) in stream_commits.iter().enumerate() {
        for trailer in &commit.task_trailers {
            commit_rank.entry(trailer.clone()).or_insert(idx);
        }
        for (key, manifest) in &from_tasks.valid {
            if commit.changed_paths.iter().any(|p| p == &manifest.path) {
                commit_rank.entry(key.clone()).or_insert(idx);
            }
        }
    }

    let closure = compute_closure(&requested, &from_tasks, &commit_rank);

    let mut outcomes: BTreeMap<String, TaskOutcome> = BTreeMap::new();
    let mut included: Vec<IncludedEntry> = Vec::new();
    let mut satisfied_in_base: Vec<BaseSatisfiedEntry> = Vec::new();
    let mut deferred: Vec<DecisionEntry> = Vec::new();
    let mut rejected: Vec<DecisionEntry> = Vec::new();

    let mut current_commit = base_commit.clone();
    let mut current_tree = base_tree.clone();

    // Merges happen in Susi; the Git host is storage only. Work whose commits
    // the host created itself (web merge, web edit, API commit) is refused
    // unless the distilling agent adopts each commit by full id, on record.
    let host_identities = hostmerge::identities(root)?;
    let host_in_stream = hostmerge::scan_range(
        root,
        &format!("{base_commit}..{from_commit}"),
        &host_identities,
    )?;
    let mut adopted_host_commits: BTreeMap<String, String> = BTreeMap::new();

    for key in &closure.order {
        if let Some(cycle_detail) = closure.cycle_errors.get(key) {
            let label = from_tasks
                .valid
                .get(key)
                .map_or_else(|| key.clone(), |t| t.id.clone());
            let deps = from_tasks
                .valid
                .get(key)
                .map_or_else(Vec::new, |t| t.deps.clone());
            rejected.push(DecisionEntry {
                task_id: label,
                source_commits: Vec::new(),
                deps,
                reason_code: "cyclic_dependency",
                detail: cycle_detail.clone(),
            });
            outcomes.insert(key.clone(), TaskOutcome::Rejected);
            continue;
        }

        if let Some(base_done_deps) = base_tasks.done_deps.get(key) {
            let has_stream_commits = stream_commits.iter().any(|c| {
                c.task_trailers.iter().any(|t| t == key)
                    || c.changed_paths
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(&format!(".agents/tasks/done/{key}.json")))
            });
            if !has_stream_commits {
                if let Some((dep_reason, dep_detail, is_reject)) =
                    check_dep_list_outcomes(key, base_done_deps, &outcomes)
                {
                    let entry = DecisionEntry {
                        task_id: key.clone(),
                        source_commits: Vec::new(),
                        deps: base_done_deps.clone(),
                        reason_code: dep_reason,
                        detail: dep_detail,
                    };
                    if is_reject {
                        rejected.push(entry);
                        outcomes.insert(key.clone(), TaskOutcome::Rejected);
                    } else {
                        deferred.push(entry);
                        outcomes.insert(key.clone(), TaskOutcome::Deferred);
                    }
                } else {
                    let (reason_code, how) = base_reason(key);
                    satisfied_in_base.push(BaseSatisfiedEntry {
                        task_id: key.clone(),
                        reason_code,
                        detail: format!(
                            "closed task record for {key} {how} {base_commit} with zero newer commits"
                        ),
                    });
                    outcomes.insert(key.clone(), TaskOutcome::SatisfiedInBase);
                }
                continue;
            }
        }

        if let Some(inv_detail) = from_tasks.invalid.get(key) {
            let reason = if inv_detail.contains("missing_scopes") {
                "missing_scopes"
            } else {
                "invalid_manifest"
            };
            rejected.push(DecisionEntry {
                task_id: key.clone(),
                source_commits: Vec::new(),
                deps: Vec::new(),
                reason_code: reason,
                detail: inv_detail.clone(),
            });
            outcomes.insert(key.clone(), TaskOutcome::Rejected);
            continue;
        }

        let Some(task) = from_tasks.valid.get(key) else {
            if base_tasks.valid.contains_key(key) {
                satisfied_in_base.push(BaseSatisfiedEntry {
                    task_id: key.clone(),
                    reason_code: "already_in_base",
                    detail: "task manifest is already present in base commit".to_owned(),
                });
                outcomes.insert(key.clone(), TaskOutcome::SatisfiedInBase);
            } else {
                rejected.push(DecisionEntry {
                    task_id: key.clone(),
                    source_commits: Vec::new(),
                    deps: Vec::new(),
                    reason_code: "missing_manifest",
                    detail: format!("no .agents/tasks manifest found for {key} in {from_commit}"),
                });
                outcomes.insert(key.clone(), TaskOutcome::Rejected);
            }
            continue;
        };

        if let Some((dep_reason, dep_detail, is_reject)) = check_dep_outcomes(task, &outcomes) {
            let entry = DecisionEntry {
                task_id: task.id.clone(),
                source_commits: Vec::new(),
                deps: task.deps.clone(),
                reason_code: dep_reason,
                detail: dep_detail,
            };
            if is_reject {
                rejected.push(entry);
                outcomes.insert(key.clone(), TaskOutcome::Rejected);
            } else {
                deferred.push(entry);
                outcomes.insert(key.clone(), TaskOutcome::Deferred);
            }
            continue;
        }

        let attributed = match attribute_task_commits(git, &from_commit, task, &stream_commits) {
            Ok(mut list) => {
                list.retain(|commit| !distilled_in_base.source_commits.contains(&commit.oid));
                list
            }
            Err(err) => {
                rejected.push(DecisionEntry {
                    task_id: task.id.clone(),
                    source_commits: task.source_commits.clone(),
                    deps: task.deps.clone(),
                    reason_code: "invalid_source_commit",
                    detail: err,
                });
                outcomes.insert(key.clone(), TaskOutcome::Rejected);
                continue;
            }
        };

        if attributed.is_empty() {
            if base_tasks.valid.contains_key(key) || distilled_in_base.tasks.contains(key) {
                let (reason_code, how) = base_reason(key);
                satisfied_in_base.push(BaseSatisfiedEntry {
                    task_id: task.id.clone(),
                    reason_code,
                    detail: format!(
                        "task {} {how} {base_commit} with zero newer commits",
                        task.id
                    ),
                });
                outcomes.insert(key.clone(), TaskOutcome::SatisfiedInBase);
            } else {
                deferred.push(DecisionEntry {
                    task_id: task.id.clone(),
                    source_commits: Vec::new(),
                    deps: task.deps.clone(),
                    reason_code: "unresolved_commit",
                    detail: format!(
                        "no non-merge commits in {base_commit}..{from_commit} matched Task: {}",
                        task.id
                    ),
                });
                outcomes.insert(key.clone(), TaskOutcome::Deferred);
            }
            continue;
        }

        let commit_oids: Vec<String> = attributed.iter().map(|c| c.oid.clone()).collect();
        if let Err((code, detail)) = audit_task_scopes(task, &attributed) {
            rejected.push(DecisionEntry {
                task_id: task.id.clone(),
                source_commits: commit_oids,
                deps: task.deps.clone(),
                reason_code: code,
                detail,
            });
            outcomes.insert(key.clone(), TaskOutcome::Rejected);
            continue;
        }

        let attributed_oids: BTreeSet<String> = commit_oids.iter().cloned().collect();
        let host_made = hostmerge::among(root, &attributed_oids, &host_identities)?;
        let refused: Vec<String> = host_made
            .values()
            .filter(|c| !args.adopt_host_commits.contains(&c.oid))
            .map(|c| format!("{} ({} <{}>)", c.oid, c.committer, c.email))
            .collect();
        if !refused.is_empty() {
            rejected.push(DecisionEntry {
                task_id: task.id.clone(),
                source_commits: commit_oids,
                deps: task.deps.clone(),
                reason_code: "merged_outside_susi",
                detail: format!(
                    "commit(s) created by the Git host, not by a Susi merge: {}; re-land them through Susi or adopt each with --adopt-host-commit <full-id>",
                    refused.join(", ")
                ),
            });
            outcomes.insert(key.clone(), TaskOutcome::Rejected);
            continue;
        }
        for c in host_made.values() {
            adopted_host_commits.insert(
                c.oid.clone(),
                format!("{} <{}> for {}", c.committer, c.email, task.id),
            );
        }

        let cursor = ComposeCursor {
            base_commit: &base_commit,
            current_commit: &current_commit,
            current_tree: &current_tree,
        };
        match compose_task_onto(git, &cursor, task, &attributed)? {
            Ok(composed) => {
                current_commit = composed.commit.clone();
                current_tree = composed.tree.clone();
                included.push(IncludedEntry {
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    goal: task.goal.clone(),
                    owner: task.owner.clone(),
                    deps: task.deps.clone(),
                    scopes: task.scopes.clone(),
                    accept_cmd: task.accept_cmd.clone(),
                    source_commits: commit_oids,
                    composed_commit: composed.commit,
                    composed_tree: composed.tree,
                    changed_files: composed.changed_files,
                });
                outcomes.insert(key.clone(), TaskOutcome::Included);
            }
            Err((code, detail)) => {
                let entry = DecisionEntry {
                    task_id: task.id.clone(),
                    source_commits: commit_oids,
                    deps: task.deps.clone(),
                    reason_code: code,
                    detail,
                };
                if code == "out_of_scope_diff" {
                    rejected.push(entry);
                    outcomes.insert(key.clone(), TaskOutcome::Rejected);
                } else {
                    deferred.push(entry);
                    outcomes.insert(key.clone(), TaskOutcome::Deferred);
                }
            }
        }
    }

    let branch_ref = resolve_branch_ref(args, feature.as_ref())?;
    let endpoint_digest = git.endpoint_optional()?.map(|e| e.digest);
    let has_deferred_or_rejected = !deferred.is_empty() || !rejected.is_empty();
    let accept_verification = if args.verify_accept && !(args.strict && has_deferred_or_rejected) {
        Some(verify_composed_accept_commands(
            git,
            &current_commit,
            &included,
        )?)
    } else {
        None
    };
    let qualification_status = if accept_verification.is_some() {
        "accept_verified_candidate"
    } else {
        "unqualified_candidate"
    };

    let mut body = json!({
        "schema": RECEIPT_SCHEMA,
        "name": args.name,
        "mode": match args.mode {
            CommandMode::Plan => "plan",
            CommandMode::Compose => "compose",
            CommandMode::Orchestrate => "orchestrate",
            CommandMode::Publish => "publish",
            CommandMode::PublishBranch => "publish-branch",
            CommandMode::Status => "status",
        },
        "qualification_status": qualification_status,
        "base_commit": base_commit,
        "base_tree": base_tree,
        "source_stream_commit": from_commit,
        "source_stream_tree": from_tree,
        "branch_ref": branch_ref,
        "snapshot_tag_ref": format!("refs/tags/susi-distill/{}", args.name),
        "endpoint_digest": endpoint_digest,
        "plan_manifest_sha256": plan_spec.as_ref().map(|p| &p.sha256),
        "requested_tasks": requested,
        "closure_order": closure.order,
        "feature": feature.as_ref().map(|f| json!({
            "id": f.id,
            "title": f.title,
            "license": f.license,
            "destination_branch": f.destination_branch,
            "enabled_capabilities": f.enabled_capabilities,
            "disabled_capabilities": f.disabled_capabilities,
        })),
        "accept_verification": accept_verification,
        "included": included.iter().map(|i| json!({
            "task_id": i.task_id,
            "title": i.title,
            "goal": i.goal,
            "owner": i.owner,
            "deps": i.deps,
            "scopes": i.scopes,
            "accept_cmd": i.accept_cmd,
            "source_commits": i.source_commits,
            "composed_commit": i.composed_commit,
            "composed_tree": i.composed_tree,
            "changed_files": i.changed_files,
            "reason_code": "selected_and_composed",
        })).collect::<Vec<_>>(),
        "satisfied_in_base": satisfied_in_base.iter().map(|b| json!({
            "task_id": b.task_id,
            "reason_code": b.reason_code,
            "detail": b.detail,
        })).collect::<Vec<_>>(),
        "deferred": deferred.iter().map(render_decision).collect::<Vec<_>>(),
        "rejected": rejected.iter().map(render_decision).collect::<Vec<_>>(),
        "git_host_identities": host_identities,
        "git_host_created_in_stream": host_in_stream.iter().map(|c| json!({
            "commit": c.oid,
            "merge": c.merge,
            "committer": format!("{} <{}>", c.committer, c.email),
            "subject": c.subject,
        })).collect::<Vec<_>>(),
        "adopted_host_commits": adopted_host_commits,
        "final_commit": current_commit,
        "final_tree": current_tree,
    });

    let canonical_payload =
        serde_json::to_vec(&body).map_err(|e| format!("cannot serialize receipt: {e}"))?;
    let receipt_sha256 = sha256_hex(&canonical_payload);
    if let Some(map) = body.as_object_mut() {
        map.insert("receipt_sha256".to_owned(), json!(receipt_sha256));
    }
    let mut receipt_bytes =
        serde_json::to_vec_pretty(&body).map_err(|e| format!("cannot format receipt: {e}"))?;
    receipt_bytes.push(b'\n');

    Ok(DistillOutcome {
        receipt: body,
        receipt_bytes,
        receipt_sha256,
        final_commit: current_commit,
        final_tree: current_tree,
        branch_ref,
        has_deferred_or_rejected,
    })
}

fn render_decision(d: &DecisionEntry) -> Value {
    json!({
        "task_id": d.task_id,
        "source_commits": d.source_commits,
        "deps": d.deps,
        "reason_code": d.reason_code,
        "detail": d.detail,
    })
}

fn resolve_branch_ref(args: &DistillArgs, feature: Option<&FeatureManifest>) -> Result<String> {
    if args.release_candidate {
        return Ok(format!("refs/heads/release/{}", args.name));
    }
    if let Some(dest) = feature.and_then(|f| f.destination_branch.as_deref()) {
        let clean = normalize_scope(dest)?;
        if let Some(stripped) = clean.strip_prefix("refs/heads/") {
            return Ok(format!("refs/heads/{stripped}"));
        }
        return Ok(format!("refs/heads/{clean}"));
    }
    Ok(format!("refs/heads/distill/{}", args.name))
}

fn check_dep_outcomes(
    task: &TaskManifest,
    outcomes: &BTreeMap<String, TaskOutcome>,
) -> Option<(&'static str, String, bool)> {
    check_dep_list_outcomes(&task.key, &task.deps, outcomes)
}

fn check_dep_list_outcomes(
    self_key: &str,
    deps: &[String],
    outcomes: &BTreeMap<String, TaskOutcome>,
) -> Option<(&'static str, String, bool)> {
    for dep in deps {
        let dep_key = dep.trim().to_ascii_uppercase();
        if dep_key.is_empty() || dep_key == self_key {
            continue;
        }
        match outcomes.get(&dep_key).copied() {
            Some(TaskOutcome::Included | TaskOutcome::SatisfiedInBase) => {}
            Some(TaskOutcome::Rejected) => {
                return Some((
                    "dependency_rejected",
                    format!("prerequisite task {dep} was rejected"),
                    true,
                ));
            }
            Some(TaskOutcome::Deferred) | None => {
                return Some((
                    "dependency_deferred",
                    format!("prerequisite task {dep} was deferred"),
                    false,
                ));
            }
        }
    }
    None
}

fn store_durable_receipt(path: &Path, expected_sha256: &str, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| "receipt parent directory missing".to_owned())?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create receipt dir: {e}"))?;
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes)
                .map_err(|e| format!("cannot write distillation receipt: {e}"))?;
            file.sync_all()
                .map_err(|e| format!("cannot sync distillation receipt: {e}"))?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(format!("cannot create distillation receipt: {err}")),
    }
    let stored = read_and_sync_receipt(path)?;
    let stored_sha = stored
        .get("receipt_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| "stored distillation receipt missing receipt_sha256".to_owned())?;
    if stored_sha != expected_sha256 {
        return Err(
            "distillation name already reserved for different inputs/evidence; choose a new --name"
                .to_owned(),
        );
    }
    Ok(())
}

fn read_and_sync_receipt(path: &Path) -> Result<Value> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("cannot open distillation receipt {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read distillation receipt: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("cannot sync distillation receipt: {e}"))?;
    parse_verified_receipt(&bytes)
}

fn read_receipt_readonly(path: &Path) -> Result<Value> {
    let bytes = fs::read(path)
        .map_err(|e| format!("cannot read distillation receipt {}: {e}", path.display()))?;
    parse_verified_receipt(&bytes)
}

fn parse_verified_receipt(bytes: &[u8]) -> Result<Value> {
    let mut val: Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("corrupt distillation receipt JSON: {e}"))?;
    if val.get("schema").and_then(Value::as_str) != Some(RECEIPT_SCHEMA) {
        return Err("unknown distillation receipt schema".to_owned());
    }
    let recorded_sha = val
        .get("receipt_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| "distillation receipt missing receipt_sha256".to_owned())?
        .to_owned();
    valid_oid(&recorded_sha)?;
    if let Some(map) = val.as_object_mut() {
        map.remove("receipt_sha256");
    }
    let canonical = serde_json::to_vec(&val).map_err(|e| e.to_string())?;
    if sha256_hex(&canonical) != recorded_sha {
        return Err("distillation receipt digest mismatch; refusing tampered record".to_owned());
    }
    if let Some(map) = val.as_object_mut() {
        map.insert("receipt_sha256".to_owned(), json!(recorded_sha));
    }
    Ok(val)
}

fn format_summary(outcome: &DistillOutcome, json_output: bool) -> String {
    if json_output {
        return String::from_utf8_lossy(&outcome.receipt_bytes)
            .trim_end()
            .to_owned();
    }
    let inc = outcome.receipt["included"].as_array().map_or(0, Vec::len);
    let base_sat = outcome.receipt["satisfied_in_base"]
        .as_array()
        .map_or(0, Vec::len);
    let def = outcome.receipt["deferred"].as_array().map_or(0, Vec::len);
    let rej = outcome.receipt["rejected"].as_array().map_or(0, Vec::len);
    format!(
        "Distillation {}: included={inc} satisfied_in_base={base_sat} deferred={def} rejected={rej} commit={} tree={} branch={} receipt_sha256={}\nUnqualified candidate only; full exact-tree release qualification remains required.",
        outcome.receipt["name"].as_str().unwrap_or(""),
        outcome.final_commit,
        outcome.final_tree,
        outcome.branch_ref,
        outcome.receipt_sha256
    )
}

fn discover_tasks_in_range(git: &Git<'_>, range_spec: &str) -> Result<Vec<String>> {
    let (left_raw, right_raw) = range_spec
        .split_once("..")
        .ok_or_else(|| "invalid --tasks-from-range; expected <base-rev>..<from-rev>".to_owned())?;
    if left_raw.trim().is_empty() || right_raw.trim().is_empty() || right_raw.starts_with('.') {
        return Err(
            "invalid --tasks-from-range; expected two-dot <base-rev>..<from-rev>".to_owned(),
        );
    }
    let left_oid = git.resolve_commit(left_raw.trim())?;
    let right_oid = git.resolve_commit(right_raw.trim())?;
    let commits = git.non_merge_commits(&left_oid, &right_oid)?;
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for commit in commits {
        for trailer in commit.task_trailers {
            if seen.insert(trailer.clone()) {
                ordered.push(trailer);
            }
        }
    }
    Ok(ordered)
}

fn run_distill_command(root: &Path, args: &DistillArgs) -> Result<String> {
    let git = Git { root };
    let outcome = execute_distill(&git, root, args)?;
    if let Some(out_path) = &args.receipt_out {
        if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create output dir: {e}"))?;
        }
        fs::write(out_path, &outcome.receipt_bytes)
            .map_err(|e| format!("cannot write --receipt-out: {e}"))?;
    }

    if args.strict && outcome.has_deferred_or_rejected {
        return Err(format!(
            "strict distillation refused deferred/rejected tasks: {}",
            format_summary(&outcome, args.json_output)
        ));
    }

    if matches!(args.mode, CommandMode::Compose | CommandMode::Orchestrate) {
        let receipt_path = git.state_path("receipts", &args.name)?;
        store_durable_receipt(
            &receipt_path,
            &outcome.receipt_sha256,
            &outcome.receipt_bytes,
        )?;
        git.ensure_cas_ref(
            &format!("refs/susi-distill/candidates/{}", args.name),
            &outcome.final_commit,
        )?;
        let branch_tip = if let Some(rel_name) =
            outcome.branch_ref.strip_prefix("refs/heads/release/")
        {
            let rel_path = format!("releases/distill/{rel_name}.json");
            let env_msg = format!(
                "evidence(distill): release candidate receipt {rel_name}\n\nDistilled-Commit: {}\nReceipt-Sha256: {}\n",
                outcome.final_commit, outcome.receipt_sha256
            );
            git.commit_single_blob_envelope(
                &outcome.final_commit,
                &rel_path,
                &outcome.receipt_bytes,
                &env_msg,
            )?
        } else {
            outcome.final_commit.clone()
        };
        git.ensure_cas_ref(&outcome.branch_ref, &branch_tip)?;

        let mut extra_summaries = Vec::new();
        if args.mode == CommandMode::Orchestrate && args.publish_snapshot {
            extra_summaries.push(run_inspect_command(
                root,
                &InspectArgs {
                    mode: CommandMode::Publish,
                    name: args.name.clone(),
                    json_output: false,
                },
            )?);
        }
        if args.mode == CommandMode::Orchestrate && args.publish_release_branch {
            extra_summaries.push(run_inspect_command(
                root,
                &InspectArgs {
                    mode: CommandMode::PublishBranch,
                    name: args.name.clone(),
                    json_output: false,
                },
            )?);
        }
        if !extra_summaries.is_empty() && !args.json_output {
            return Ok(format!(
                "{}\n{}",
                format_summary(&outcome, false),
                extra_summaries.join("\n")
            ));
        }
    }

    Ok(format_summary(&outcome, args.json_output))
}

fn verify_local_release_envelope(
    git: &Git<'_>,
    name: &str,
    branch_oid: &str,
    expected_parent: &str,
    expected_sha256: &str,
) -> Result<()> {
    let lineage = git.text(&["rev-list", "--parents", "-n", "1", branch_oid])?;
    let parents: Vec<&str> = lineage.split_whitespace().skip(1).collect();
    if parents != [expected_parent] {
        return Err("release branch envelope parent does not match distilled commit".to_owned());
    }
    let rel_path = format!("releases/distill/{name}.json");
    let changed = git.text(&[
        "diff-tree",
        "--no-commit-id",
        "--no-renames",
        "--name-only",
        "-r",
        expected_parent,
        branch_oid,
    ])?;
    if changed != rel_path {
        return Err(format!(
            "release branch envelope must change only {rel_path}, found: {changed}"
        ));
    }
    let raw = git.bytes(
        &["cat-file", "blob", &format!("{branch_oid}:{rel_path}")],
        None,
    )?;
    let parsed = parse_verified_receipt(&raw)?;
    if parsed.get("receipt_sha256").and_then(Value::as_str) != Some(expected_sha256) {
        return Err(
            "release branch envelope receipt digest does not match stored receipt".to_owned(),
        );
    }
    Ok(())
}

fn run_inspect_command(root: &Path, args: &InspectArgs) -> Result<String> {
    let git = Git { root };
    git.require_original_history()?;
    let receipt_path = git.state_path("receipts", &args.name)?;
    let receipt = if args.mode == CommandMode::Status {
        read_receipt_readonly(&receipt_path)?
    } else {
        read_and_sync_receipt(&receipt_path)?
    };
    let final_commit = receipt["final_commit"]
        .as_str()
        .ok_or_else(|| "receipt missing final_commit".to_owned())?;
    let final_tree = receipt["final_tree"]
        .as_str()
        .ok_or_else(|| "receipt missing final_tree".to_owned())?;
    let branch_ref = receipt["branch_ref"]
        .as_str()
        .ok_or_else(|| "receipt missing branch_ref".to_owned())?;
    let tag_ref = receipt["snapshot_tag_ref"]
        .as_str()
        .ok_or_else(|| "receipt missing snapshot_tag_ref".to_owned())?;
    let receipt_sha256 = receipt["receipt_sha256"]
        .as_str()
        .ok_or_else(|| "receipt missing receipt_sha256".to_owned())?;
    if git.tree(final_commit)? != final_tree {
        return Err("distilled commit/tree evidence does not match stored receipt".to_owned());
    }

    if args.mode == CommandMode::Status {
        return report_status(&git, &receipt, branch_ref, tag_ref, args.json_output);
    }

    let endpoint = git
        .endpoint_optional()?
        .ok_or_else(|| "origin remote is not configured; cannot publish".to_owned())?;
    if let Some(expected_ep) = receipt.get("endpoint_digest").and_then(Value::as_str) {
        if expected_ep != endpoint.digest {
            return Err(
                "origin destination changed since distillation compose; refusing to publish"
                    .to_owned(),
            );
        }
    }

    if args.mode == CommandMode::PublishBranch {
        let Some(rel_name) = branch_ref.strip_prefix("refs/heads/release/") else {
            return Err(format!(
                "publish-branch requires a release-candidate distillation (branch_ref is {branch_ref})"
            ));
        };
        if receipt["qualification_status"].as_str() != Some("accept_verified_candidate") {
            return Err(
                "publish-branch requires an accept-verified candidate (--verify-accept)".to_owned(),
            );
        }
        if receipt["rejected"].as_array().is_none_or(|r| !r.is_empty()) {
            return Err("publish-branch refuses receipts with rejected tasks".to_owned());
        }
        let branch_oid = git
            .text(&["show-ref", "--verify", "--hash", branch_ref])
            .map_err(|_| format!("local release branch {branch_ref} is missing"))?;
        verify_local_release_envelope(&git, rel_name, &branch_oid, final_commit, receipt_sha256)?;

        if let Some(remote_oid) = git.remote_ref(&endpoint, branch_ref)? {
            if remote_oid == branch_oid {
                if args.json_output {
                    return serde_json::to_string_pretty(&json!({
                        "status": "published_exact",
                        "branch_ref": branch_ref,
                        "envelope_commit": branch_oid,
                        "distilled_commit": final_commit,
                        "distilled_tree": final_tree,
                        "receipt_sha256": receipt_sha256,
                    }))
                    .map_err(|e| e.to_string());
                }
                return Ok(format!(
                    "Published distilled release branch verified: branch={branch_ref} envelope={branch_oid} distilled_commit={final_commit} tree={final_tree}"
                ));
            }
            if !git.ancestor(&remote_oid, &branch_oid)? {
                return Err(format!(
                    "remote release branch {branch_ref} at {remote_oid} is not an ancestor of {branch_oid}; refusing non-fast-forward update"
                ));
            }
        }

        let refspec = format!("{branch_oid}:{branch_ref}");
        let push_out = git.output(
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
        )?;
        let push_stderr = String::from_utf8_lossy(&push_out.stderr).trim().to_owned();

        return match git.remote_ref(&endpoint, branch_ref) {
            Ok(Some(remote_oid)) if remote_oid == branch_oid => {
                if args.json_output {
                    serde_json::to_string_pretty(&json!({
                        "status": "published_exact",
                        "branch_ref": branch_ref,
                        "envelope_commit": branch_oid,
                        "distilled_commit": final_commit,
                        "distilled_tree": final_tree,
                        "receipt_sha256": receipt_sha256,
                    }))
                    .map_err(|e| e.to_string())
                } else {
                    Ok(format!(
                        "Published distilled release branch verified: branch={branch_ref} envelope={branch_oid} distilled_commit={final_commit} tree={final_tree}\nFull dual-OS qualification remains separate before any vX.Y.Z production release tag."
                    ))
                }
            }
            Ok(Some(other)) => Err(format!(
                "remote release branch {branch_ref} points to {other} instead of {branch_oid}: {push_stderr}"
            )),
            Ok(None) => Err(format!(
                "distilled release branch {branch_ref} was not published: {push_stderr}"
            )),
            Err(_) => Err(format!(
                "publication outcome for {branch_ref} is unknown; run status {} before retrying",
                args.name
            )),
        };
    }

    if let Some(remote_oid) = git.remote_ref(&endpoint, tag_ref)? {
        if remote_oid == final_commit {
            return Ok(format!(
                "Published distilled snapshot verified: commit={final_commit} tree={final_tree} tag={tag_ref}"
            ));
        }
        return Err(format!(
            "remote snapshot tag {tag_ref} already points to {remote_oid}; left unchanged"
        ));
    }

    let refspec = format!("{final_commit}:{tag_ref}");
    let _ = git.output(
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

    match git.remote_ref(&endpoint, tag_ref) {
        Ok(Some(remote_oid)) if remote_oid == final_commit => Ok(format!(
            "Published distilled snapshot verified: commit={final_commit} tree={final_tree} tag={tag_ref}\nQualification remains separate before any production release tag."
        )),
        Ok(Some(other)) => Err(format!(
            "remote snapshot tag {tag_ref} points to {other}; left unchanged"
        )),
        Ok(None) => Err(format!(
            "distilled snapshot tag {tag_ref} was not published; safe to retry publish {}",
            args.name
        )),
        Err(_) => Err(format!(
            "publication outcome for {tag_ref} is unknown; run status {} before retrying",
            args.name
        )),
    }
}

fn report_status(
    git: &Git<'_>,
    receipt: &Value,
    branch_ref: &str,
    tag_ref: &str,
    json_output: bool,
) -> Result<String> {
    let final_commit = receipt["final_commit"]
        .as_str()
        .ok_or_else(|| "receipt missing final_commit".to_owned())?;
    let receipt_sha256 = receipt["receipt_sha256"]
        .as_str()
        .ok_or_else(|| "receipt missing receipt_sha256".to_owned())?;
    let local_branch_oid = git
        .text(&["show-ref", "--verify", "--hash", branch_ref])
        .ok();
    let branch_state = match local_branch_oid.as_deref() {
        Some(oid) if oid == final_commit => "present_exact",
        Some(oid)
            if branch_ref
                .strip_prefix("refs/heads/release/")
                .is_some_and(|rel| {
                    verify_local_release_envelope(git, rel, oid, final_commit, receipt_sha256)
                        .is_ok()
                }) =>
        {
            "present_exact"
        }
        Some(_) => "present_diverged",
        None => "absent",
    };
    let (remote_state, remote_branch_state) = match git.endpoint_optional()? {
        None => ("no_remote", "no_remote"),
        Some(endpoint) => {
            if receipt
                .get("endpoint_digest")
                .and_then(Value::as_str)
                .is_some_and(|exp| exp != endpoint.digest)
            {
                ("endpoint_changed", "endpoint_changed")
            } else {
                let tag_st = match git.remote_ref(&endpoint, tag_ref) {
                    Ok(Some(oid)) if oid == final_commit => "published_exact",
                    Ok(Some(_)) => "conflict",
                    Ok(None) => "absent",
                    Err(_) => "unreadable",
                };
                let br_st = match (
                    local_branch_oid.as_deref(),
                    git.remote_ref(&endpoint, branch_ref),
                ) {
                    (Some(expected), Ok(Some(oid)))
                        if branch_state == "present_exact" && oid == expected =>
                    {
                        "published_exact"
                    }
                    (_, Ok(Some(_))) => "diverged",
                    (_, Ok(None)) => "absent",
                    (_, Err(_)) => "unreadable",
                };
                (tag_st, br_st)
            }
        }
    };
    if json_output {
        let out = json!({
            "name": receipt["name"],
            "final_commit": final_commit,
            "final_tree": receipt["final_tree"],
            "branch_ref": branch_ref,
            "branch_state": branch_state,
            "remote_branch_state": remote_branch_state,
            "snapshot_tag_ref": tag_ref,
            "remote_state": remote_state,
            "receipt_sha256": receipt_sha256,
            "included_count": receipt["included"].as_array().map_or(0, Vec::len),
            "deferred_count": receipt["deferred"].as_array().map_or(0, Vec::len),
            "rejected_count": receipt["rejected"].as_array().map_or(0, Vec::len),
        });
        return serde_json::to_string_pretty(&out).map_err(|e| e.to_string());
    }
    Ok(format!(
        "Distillation status {}: commit={final_commit} branch={branch_ref} ({branch_state}, remote={remote_branch_state}) tag={tag_ref} ({remote_state})",
        receipt["name"].as_str().unwrap_or("")
    ))
}

fn audit_candidate_commits_for_main(commits: &[CommitInfo]) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut tasks = Vec::new();
    for commit in commits {
        if commit.task_trailers.is_empty() {
            return Err(format!(
                "missing_task_trailer: commit {} ('{}') lacks a 'Task: T-...' trailer required for main distillation",
                commit.oid, commit.subject
            ));
        }
        for path in &commit.changed_paths {
            if is_forbidden_confidential_path(path) {
                return Err(format!(
                    "confidentiality_violation: commit {} touches forbidden credential/key path '{path}'",
                    commit.oid
                ));
            }
        }
        for trailer in &commit.task_trailers {
            if seen.insert(trailer.clone()) {
                tasks.push(trailer.clone());
            }
        }
    }
    Ok(tasks)
}

fn run_submit_main_command(root: &Path, args: &SubmitMainArgs) -> Result<String> {
    let git = Git { root };
    git.require_original_history()?;
    let candidate = git.resolve_commit(args.rev.as_deref().unwrap_or("HEAD"))?;
    let endpoint = git
        .endpoint_optional()?
        .ok_or_else(|| "origin remote is not configured; cannot submit to main".to_owned())?;

    for attempt in 1..=args.max_attempts {
        let remote_main = git.remote_ref(&endpoint, "refs/heads/main")?;
        if let Some(ref remote_oid) = remote_main {
            let fetch_out = git.output(
                &[
                    "fetch",
                    "--no-tags",
                    "--recurse-submodules=no",
                    "--",
                    &endpoint.address,
                    "refs/heads/main",
                ],
                None,
            )?;
            if !fetch_out.status.success() {
                return Err(format!(
                    "cannot fetch origin refs/heads/main ({})",
                    fetch_out.status
                ));
            }
            if remote_oid == &candidate || git.ancestor(&candidate, remote_oid)? {
                let payload = json!({
                    "schema": "susi-main-submission-receipt-v1",
                    "status": "already_landed",
                    "candidate_commit": candidate,
                    "published_commit": remote_oid,
                    "remote_main": remote_oid,
                    "reconciled": false,
                    "attempts": attempt,
                    "endpoint_digest": endpoint.digest,
                    "tasks": Vec::<String>::new(),
                });
                return format_submit_main_result(&payload, args.json_output);
            }
        }

        let (publish_commit, reconciled, tasks) = match remote_main {
            None => {
                let head_info = git.inspect_commit_allow_root(&candidate)?;
                let tasks = audit_candidate_commits_for_main(&[head_info])?;
                (candidate.clone(), false, tasks)
            }
            Some(ref remote_oid) if git.ancestor(remote_oid, &candidate)? => {
                let delta = git.non_merge_commits(remote_oid, &candidate)?;
                let tasks = audit_candidate_commits_for_main(&delta)?;
                (candidate.clone(), false, tasks)
            }
            Some(ref remote_oid) => {
                let merge_base = git.text(&["merge-base", remote_oid, &candidate])?;
                valid_oid(&merge_base)?;
                let delta = git.non_merge_commits(&merge_base, &candidate)?;
                let tasks = audit_candidate_commits_for_main(&delta)?;
                let merged_tree = match git.merge_three_way(&merge_base, remote_oid, &candidate)? {
                    ThreeWayMerge::Clean(tree) => tree,
                    ThreeWayMerge::Conflict(conflict_msg) => {
                        return Err(format!(
                            "reconcile_conflict against concurrent origin/main {remote_oid}: {conflict_msg}"
                        ));
                    }
                };
                let mut msg = format!(
                    "Reconcile autonomous contribution {} onto development main\n\nNon-destructive object-store reconciliation preserving peer commits.\n",
                    &candidate[..12.min(candidate.len())]
                );
                for task_id in &tasks {
                    msg.push_str(&format!("Task: {task_id}\n"));
                }
                let merge_commit = git.commit_tree_parents(
                    &merged_tree,
                    &[remote_oid.as_str(), candidate.as_str()],
                    &msg,
                )?;
                (merge_commit, true, tasks)
            }
        };

        let refspec = format!("{publish_commit}:refs/heads/main");
        let push_out = git.output(
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
        )?;

        let post_remote = git.remote_ref(&endpoint, "refs/heads/main")?;
        if let Some(ref landed) = post_remote {
            if landed == &publish_commit {
                let payload = json!({
                    "schema": "susi-main-submission-receipt-v1",
                    "status": if reconciled { "published_reconciled" } else { "published_fast_forward" },
                    "candidate_commit": candidate,
                    "published_commit": publish_commit,
                    "remote_main": landed,
                    "reconciled": reconciled,
                    "attempts": attempt,
                    "endpoint_digest": endpoint.digest,
                    "tasks": tasks,
                });
                return format_submit_main_result(&payload, args.json_output);
            }
        }

        if attempt == args.max_attempts {
            let stderr = String::from_utf8_lossy(&push_out.stderr);
            return Err(format!(
                "submit-main exhausted {} attempt(s) without fast-forwarding refs/heads/main (remote={:?}): {}",
                args.max_attempts,
                post_remote,
                stderr.trim()
            ));
        }
    }
    Err("submit-main failed".to_owned())
}

fn format_submit_main_result(payload: &Value, json_output: bool) -> Result<String> {
    if json_output {
        return serde_json::to_string_pretty(payload).map_err(|e| e.to_string());
    }
    Ok(format!(
        "Main submission {}: candidate={} published={} remote_main={} reconciled={} attempts={}",
        payload["status"].as_str().unwrap_or(""),
        payload["candidate_commit"].as_str().unwrap_or(""),
        payload["published_commit"].as_str().unwrap_or(""),
        payload["remote_main"].as_str().unwrap_or(""),
        payload["reconciled"].as_bool().unwrap_or(false),
        payload["attempts"].as_u64().unwrap_or(1),
    ))
}

pub fn run(root: &Path, args: &[String]) -> Result<String> {
    match parse_cli(args)? {
        ParsedCli::Help => Ok(USAGE.to_owned()),
        ParsedCli::Distill(distill_args) => run_distill_command(root, &distill_args),
        ParsedCli::Inspect(inspect_args) => run_inspect_command(root, &inspect_args),
        ParsedCli::SubmitMain(submit_args) => run_submit_main_command(root, &submit_args),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn accept_commands_keep_the_callers_toolchain_homes() {
        use std::ffi::OsString;
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        let defaults = toolchain_homes(env(&[("HOME", "/u")]));
        assert_eq!(
            defaults,
            vec![
                (
                    "CARGO_HOME",
                    Path::new("/u").join(".cargo").into_os_string()
                ),
                (
                    "RUSTUP_HOME",
                    Path::new("/u").join(".rustup").into_os_string()
                ),
            ]
        );
        let explicit = toolchain_homes(env(&[
            ("HOME", "/u"),
            ("CARGO_HOME", "/c"),
            ("RUSTUP_HOME", ""),
        ]));
        assert_eq!(explicit[0], ("CARGO_HOME", OsString::from("/c")));
        assert_eq!(
            explicit[1],
            (
                "RUSTUP_HOME",
                Path::new("/u").join(".rustup").into_os_string()
            )
        );
        let windows = toolchain_homes(env(&[("USERPROFILE", "C:/u")]));
        assert_eq!(
            windows[0],
            (
                "CARGO_HOME",
                Path::new("C:/u").join(".cargo").into_os_string()
            )
        );
        assert!(toolchain_homes(env(&[])).is_empty());
    }

    static FIXTURE_SEQ: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        dir: PathBuf,
        remote: PathBuf,
        work: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let id = FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("susi-distill-test-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            let remote = dir.join("origin.git");
            let work = dir.join("work");
            fs::create_dir_all(&work).unwrap();

            let init_bare = Git { root: &dir };
            init_bare
                .text(&[
                    "init",
                    "--bare",
                    "--initial-branch=main",
                    remote.to_str().unwrap(),
                ])
                .unwrap();

            let git = Git { root: &work };
            git.text(&["init", "--initial-branch=main"]).unwrap();
            git.text(&["remote", "add", "origin", remote.to_str().unwrap()])
                .unwrap();
            Self { dir, remote, work }
        }

        fn git(&self) -> Git<'_> {
            Git { root: &self.work }
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.work.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }

        fn write_task(&self, id: &str, deps: &[&str], scopes: &[&str]) {
            let task_path = format!(".agents/tasks/{id}.json");
            let mut full_scopes: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
            if !full_scopes.contains(&task_path) {
                full_scopes.push(task_path.clone());
            }
            let doc = json!({
                "id": id,
                "title": format!("Title for {id}"),
                "goal": format!("Goal for {id}"),
                "created_by": "test-worker",
                "created_unix": 1_794_300_000u64,
                "deps": deps,
                "scopes": full_scopes,
                "accept": { "cmd": ["cargo", "test", "--offline", "--locked"] }
            });
            self.write(
                &task_path,
                &format!("{}\n", serde_json::to_string_pretty(&doc).unwrap()),
            );
        }

        fn commit_all(&self, message: &str) -> String {
            let git = self.git();
            git.text(&["add", "-A"]).unwrap();
            git.text(&[
                "-c",
                "user.name=Worker",
                "-c",
                "user.email=worker@localhost",
                "commit",
                "-m",
                message,
            ])
            .unwrap();
            git.resolve_commit("HEAD").unwrap()
        }

        fn run_args(&self, args: &[&str]) -> Result<String> {
            let vec: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
            run(&self.work, &vec)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn transitive_dependency_closure_selects_prerequisites_without_unselected_leaks() {
        let fx = Fixture::new();
        fx.write("README.md", "base\n");
        let base = fx.commit_all("initial base");

        // Unselected work lands first on main
        fx.write("crates/unselected/lib.rs", "pub fn unselected() {}\n");
        fx.write_task("T-UNSELECTED-1", &[], &["crates/unselected"]);
        fx.commit_all("feat: unselected change\n\nTask: T-UNSELECTED-1");

        // Prerequisite task T-DEP-1
        fx.write("crates/dep/lib.rs", "pub fn dep_v1() -> u32 { 42 }\n");
        fx.write_task("T-DEP-1", &[], &["crates/dep"]);
        fx.commit_all("feat: add dep\n\nTask: T-DEP-1");

        // Feature task T-FEAT-1 depending on T-DEP-1
        fx.write("crates/feat/lib.rs", "pub fn feat() -> u32 { 42 + 1 }\n");
        fx.write_task("T-FEAT-1", &["T-DEP-1"], &["crates/feat"]);
        fx.commit_all("feat: add feat\n\nTask: T-FEAT-1");

        let out = fx
            .run_args(&[
                "compose",
                "--name",
                "feat-slice-1",
                "--base",
                &base,
                "--from",
                "HEAD",
                "--select",
                "T-FEAT-1",
                "--strict",
                "--json",
            ])
            .unwrap();
        let receipt: Value = serde_json::from_str(&out).unwrap();
        let inc: Vec<&str> = receipt["included"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["task_id"].as_str().unwrap())
            .collect();
        assert_eq!(inc, vec!["T-DEP-1", "T-FEAT-1"]);
        assert!(receipt["deferred"].as_array().unwrap().is_empty());
        assert!(receipt["rejected"].as_array().unwrap().is_empty());

        // Verify distilled branch contains dep + feat and ZERO unselected files!
        let files = fx
            .git()
            .tree_blobs(receipt["final_commit"].as_str().unwrap(), "crates")
            .unwrap();
        let paths: Vec<String> = files.into_iter().map(|(p, _)| p).collect();
        assert_eq!(paths, vec!["crates/dep/lib.rs", "crates/feat/lib.rs"]);
    }

    #[test]
    fn host_created_task_commit_is_refused_until_adopted_on_record() {
        let fx = Fixture::new();
        fx.write("README.md", "base\n");
        let base = fx.commit_all("initial base");
        fx.write("crates/local/lib.rs", "pub fn local() {}\n");
        fx.write_task("T-LOCAL-1", &[], &["crates/local"]);
        fx.commit_all("feat: local\n\nTask: T-LOCAL-1");
        // The Git host writes this commit itself (an API commit or web edit).
        fx.write("crates/hosted/lib.rs", "pub fn hosted() {}\n");
        fx.write_task("T-HOSTED-1", &[], &["crates/hosted"]);
        // `Git` pins the distiller's own committer identity, so write this
        // commit with plain Git, the way the host would.
        let host_git = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(&fx.work)
                .args(args)
                .env("GIT_COMMITTER_NAME", "GitHub")
                .env("GIT_COMMITTER_EMAIL", "noreply@github.com")
                .env("GIT_AUTHOR_NAME", "Worker")
                .env("GIT_AUTHOR_EMAIL", "worker@localhost")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        host_git(&["add", "-A"]);
        host_git(&["commit", "-q", "-m", "feat: hosted\n\nTask: T-HOSTED-1"]);
        let hosted = fx.git().resolve_commit("HEAD").unwrap();
        let compose = |name: &str, extra: &[&str]| -> Value {
            let mut args = vec![
                "compose",
                "--name",
                name,
                "--base",
                &base,
                "--from",
                "HEAD",
                "--select",
                "T-LOCAL-1,T-HOSTED-1",
                "--json",
            ];
            args.extend_from_slice(extra);
            serde_json::from_str(&fx.run_args(&args).unwrap()).unwrap()
        };

        let refused = compose("host-refused", &[]);
        assert_eq!(refused["included"][0]["task_id"], "T-LOCAL-1", "{refused}");
        assert_eq!(
            refused["included"].as_array().unwrap().len(),
            1,
            "{refused}"
        );
        assert_eq!(refused["rejected"][0]["task_id"], "T-HOSTED-1");
        assert_eq!(refused["rejected"][0]["reason_code"], "merged_outside_susi");
        assert_eq!(
            refused["git_host_created_in_stream"][0]["commit"],
            hosted.as_str()
        );
        assert!(refused["adopted_host_commits"]
            .as_object()
            .unwrap()
            .is_empty());

        // --strict refuses the candidate outright.
        let strict = fx.run_args(&[
            "compose",
            "--name",
            "host-strict",
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-HOSTED-1",
            "--strict",
        ]);
        assert!(strict.unwrap_err().contains("strict distillation refused"));

        // An abbreviated id never adopts anything.
        assert!(fx
            .run_args(&[
                "compose",
                "--name",
                "host-short",
                "--base",
                &base,
                "--select",
                "T-HOSTED-1",
                "--adopt-host-commit",
                &hosted[..12],
            ])
            .is_err());

        let adopted = compose("host-adopted", &["--adopt-host-commit", &hosted]);
        assert!(
            adopted["rejected"].as_array().unwrap().is_empty(),
            "{adopted}"
        );
        assert_eq!(adopted["included"].as_array().unwrap().len(), 2);
        assert!(adopted["adopted_host_commits"][hosted.as_str()]
            .as_str()
            .unwrap()
            .ends_with("for T-HOSTED-1"));
    }

    #[test]
    fn conflicting_task_is_deferred_with_dependents_while_independent_task_lands() {
        let fx = Fixture::new();
        fx.write("shared/config.txt", "line1\nline2\nline3\n");
        let base = fx.commit_all("base");

        // Unselected commit rewrites shared/config.txt on main
        fx.write("shared/config.txt", "rewritten_by_unselected\n");
        fx.write_task("T-NOISE-1", &[], &["shared"]);
        fx.commit_all("noise\n\nTask: T-NOISE-1");

        // T-CONFLICT-1 edits on top of T-NOISE-1 without declaring dependency on T-NOISE-1
        fx.write(
            "shared/config.txt",
            "rewritten_by_unselected\nplus_conflict\n",
        );
        fx.write_task("T-CONFLICT-1", &[], &["shared"]);
        fx.commit_all("conflict\n\nTask: T-CONFLICT-1");

        // T-CHILD-1 depends on T-CONFLICT-1
        fx.write("child/mod.rs", "pub struct Child;\n");
        fx.write_task("T-CHILD-1", &["T-CONFLICT-1"], &["child"]);
        fx.commit_all("child\n\nTask: T-CHILD-1");

        // T-CLEAN-1 is completely independent
        fx.write("clean/ok.rs", "pub fn clean() {}\n");
        fx.write_task("T-CLEAN-1", &[], &["clean"]);
        fx.commit_all("clean\n\nTask: T-CLEAN-1");

        // Strict mode refuses because of deferred tasks
        let strict_err = fx
            .run_args(&[
                "plan",
                "--name",
                "conflict-check",
                "--base",
                &base,
                "--select",
                "T-CHILD-1,T-CLEAN-1",
                "--strict",
            ])
            .unwrap_err();
        assert!(strict_err.contains("strict distillation refused"));

        // Non-strict plan includes T-CLEAN-1 and defers T-CONFLICT-1 + T-CHILD-1 with exact reasons
        let out = fx
            .run_args(&[
                "plan",
                "--name",
                "conflict-check",
                "--base",
                &base,
                "--select",
                "T-CHILD-1,T-CLEAN-1",
                "--json",
            ])
            .unwrap();
        let receipt: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(receipt["included"][0]["task_id"], "T-CLEAN-1");
        let def = receipt["deferred"].as_array().unwrap();
        assert_eq!(def.len(), 2);
        assert_eq!(def[0]["task_id"], "T-CONFLICT-1");
        assert_eq!(def[0]["reason_code"], "merge_conflict");
        assert_eq!(def[1]["task_id"], "T-CHILD-1");
        assert_eq!(def[1]["reason_code"], "dependency_deferred");
    }

    #[test]
    fn cyclic_dependencies_are_rejected_and_propagate_to_dependents() {
        let fx = Fixture::new();
        fx.write("README.md", "base\n");
        let base = fx.commit_all("base");

        fx.write("a/lib.rs", "// a\n");
        fx.write_task("T-CYC-A", &["T-CYC-B"], &["a"]);
        fx.commit_all("a\n\nTask: T-CYC-A");

        fx.write("b/lib.rs", "// b\n");
        fx.write_task("T-CYC-B", &["T-CYC-A"], &["b"]);
        fx.commit_all("b\n\nTask: T-CYC-B");

        fx.write("c/lib.rs", "// c\n");
        fx.write_task("T-TOP-1", &["T-CYC-A"], &["c"]);
        fx.commit_all("c\n\nTask: T-TOP-1");

        let out = fx
            .run_args(&[
                "plan",
                "--name",
                "cycle-plan",
                "--base",
                &base,
                "--select",
                "T-TOP-1",
                "--json",
            ])
            .unwrap();
        let receipt: Value = serde_json::from_str(&out).unwrap();
        let rej = receipt["rejected"].as_array().unwrap();
        assert!(rej.iter().any(|r| r["reason_code"] == "cyclic_dependency"));
        assert!(rej
            .iter()
            .any(|r| r["task_id"] == "T-TOP-1" && r["reason_code"] == "dependency_rejected"));
    }

    #[test]
    fn missing_manifest_out_of_scope_diff_and_confidential_files_are_rejected() {
        let fx = Fixture::new();
        fx.write("README.md", "base\n");
        let base = fx.commit_all("base");

        // Out-of-scope modification
        fx.write("allowed/ok.rs", "// ok\n");
        fx.write("outside/sneaky.rs", "// sneaky\n");
        fx.write_task("T-ESCAPE-1", &[], &["allowed"]);
        fx.commit_all("escape\n\nTask: T-ESCAPE-1");

        // Confidential file inside scope
        fx.write("sec/server.pem", "PRIVATE KEY\n");
        fx.write_task("T-SECRET-1", &[], &["sec"]);
        fx.commit_all("secret\n\nTask: T-SECRET-1");

        let out = fx
            .run_args(&[
                "plan",
                "--name",
                "reject-audit",
                "--base",
                &base,
                "--select",
                "T-GHOST-1,T-ESCAPE-1,T-SECRET-1",
                "--json",
            ])
            .unwrap();
        let receipt: Value = serde_json::from_str(&out).unwrap();
        let rej = receipt["rejected"].as_array().unwrap();
        assert_eq!(rej.len(), 3);
        assert!(rej
            .iter()
            .any(|r| r["task_id"] == "T-GHOST-1" && r["reason_code"] == "missing_manifest"));
        assert!(rej
            .iter()
            .any(|r| r["task_id"] == "T-ESCAPE-1" && r["reason_code"] == "out_of_scope_diff"));
        assert!(rej.iter().any(
            |r| r["task_id"] == "T-SECRET-1" && r["reason_code"] == "confidentiality_violation"
        ));
    }

    #[test]
    fn prerequisite_already_in_base_satisfies_downstream_task() {
        let fx = Fixture::new();
        fx.write("foundation/core.rs", "pub const V: u8 = 1;\n");
        fx.write_task("T-FOUNDATION-1", &[], &["foundation"]);
        let base = fx.commit_all("foundation in base\n\nTask: T-FOUNDATION-1");

        fx.write("app/use_core.rs", "pub const X: u8 = 2;\n");
        fx.write_task("T-APP-1", &["T-FOUNDATION-1"], &["app"]);
        fx.commit_all("app on top\n\nTask: T-APP-1");

        let out = fx
            .run_args(&[
                "plan",
                "--name",
                "base-dep-test",
                "--base",
                &base,
                "--select",
                "T-APP-1",
                "--strict",
                "--json",
            ])
            .unwrap();
        let receipt: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(receipt["satisfied_in_base"][0]["task_id"], "T-FOUNDATION-1");
        assert_eq!(receipt["included"][0]["task_id"], "T-APP-1");
    }

    #[test]
    fn task_trailer_names_every_comma_or_space_separated_id() {
        let mut ids = Vec::new();
        push_task_trailer_ids("Task: T-A-1, t-b-1", &mut ids);
        push_task_trailer_ids("  task:T-C-1 T-A-1", &mut ids);
        push_task_trailer_ids("Distilled-Task: T-X-1", &mut ids);
        push_task_trailer_ids("Tasks: T-Y-1", &mut ids);
        push_task_trailer_ids("Task:", &mut ids);
        assert_eq!(ids, ["T-A-1", "T-B-1", "T-C-1"]);
    }

    #[test]
    fn distilling_onto_a_distilled_release_skips_what_it_already_carries() {
        let fx = Fixture::new();
        fx.write("README.md", "base\n");
        let base = fx.commit_all("initial base");
        fx.write("crates/a/lib.rs", "pub const A: u8 = 1;\n");
        fx.write_task("T-A-1", &[], &["crates/a"]);
        fx.commit_all("feat: a\n\nTask: T-A-1");
        let out = fx
            .run_args(&[
                "compose",
                "--name",
                "rel-1",
                "--base",
                &base,
                "--from",
                "HEAD",
                "--select",
                "T-A-1",
                "--release-candidate",
                "--strict",
                "--json",
            ])
            .unwrap();
        let first: Value = serde_json::from_str(&out).unwrap();
        let released = first["final_commit"].as_str().unwrap().to_owned();

        // The release branch edits what T-A-1 added (as a version bump does),
        // so applying T-A-1's source commit again would be an add/add conflict.
        let main = fx
            .git()
            .text(&["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap();
        fx.git().text(&["checkout", "-q", &released]).unwrap();
        fx.write("crates/a/lib.rs", "pub const A: u8 = 1; // released\n");
        let release = fx.commit_all("chore: release-only edit");
        fx.git().text(&["checkout", "-q", &main]).unwrap();

        fx.write("crates/b/lib.rs", "pub const B: u8 = 2;\n");
        fx.write_task("T-B-1", &["T-A-1"], &["crates/b"]);
        fx.commit_all("feat: b\n\nTask: T-B-1");

        let out = fx
            .run_args(&[
                "plan",
                "--name",
                "rel-2",
                "--base",
                &release,
                "--from",
                "HEAD",
                "--tasks-from-range",
                &format!("{base}..HEAD"),
                "--strict",
                "--json",
            ])
            .unwrap();
        let second: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(second["satisfied_in_base"][0]["task_id"], "T-A-1");
        assert_eq!(
            second["satisfied_in_base"][0]["reason_code"],
            "already_distilled_in_base"
        );
        let included: Vec<&str> = second["included"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["task_id"].as_str().unwrap())
            .collect();
        assert_eq!(included, ["T-B-1"]);
        assert!(second["deferred"].as_array().unwrap().is_empty());
        assert!(second["rejected"].as_array().unwrap().is_empty());
    }

    #[test]
    fn feature_manifest_and_spdx_license_allowlist_are_enforced() {
        let fx = Fixture::new();
        fx.write("README.md", "base\n");
        let base = fx.commit_all("base");

        fx.write("plug/mod.rs", "pub fn plug() {}\n");
        fx.write_task("T-PLUG-1", &[], &["plug"]);
        fx.commit_all("plug\n\nTask: T-PLUG-1");

        let bad_feature = json!({
            "schema": FEATURE_SCHEMA,
            "id": "F-GPL",
            "title": "Bad License Feature",
            "license": "GPL-3.0-only",
            "tasks": ["T-PLUG-1"]
        });
        fx.write(
            "bad-feature.json",
            &serde_json::to_string_pretty(&bad_feature).unwrap(),
        );
        let err = fx
            .run_args(&[
                "plan",
                "--name",
                "bad-lic",
                "--base",
                &base,
                "--feature",
                "bad-feature.json",
            ])
            .unwrap_err();
        assert!(err.contains("disallowed_license"));
        let _ = fs::remove_file(fx.work.join("bad-feature.json"));

        let good_feature = json!({
            "schema": FEATURE_SCHEMA,
            "id": "F-PLUG",
            "title": "Apache Plug Feature",
            "license": "Apache-2.0 AND MIT",
            "destination_branch": "feature/apache-plug",
            "enabled_capabilities": ["plug.v1"],
            "tasks": ["T-PLUG-1"]
        });
        fx.write(
            "good-feature.json",
            &serde_json::to_string_pretty(&good_feature).unwrap(),
        );
        let out = fx
            .run_args(&[
                "compose",
                "--name",
                "good-lic",
                "--base",
                &base,
                "--feature",
                "good-feature.json",
                "--strict",
                "--json",
            ])
            .unwrap();
        let receipt: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(receipt["branch_ref"], "refs/heads/feature/apache-plug");
        assert_eq!(receipt["feature"]["id"], "F-PLUG");
    }

    #[test]
    fn compose_publish_and_status_enforce_cas_immutability_and_tamper_detection() {
        let fx = Fixture::new();
        fx.write("README.md", "base\n");
        let base = fx.commit_all("base");
        fx.git().text(&["push", "origin", "main"]).unwrap();

        fx.write("mod_a/lib.rs", "pub fn step1() {}\n");
        fx.write_task("T-MULTI-1", &[], &["mod_a"]);
        fx.commit_all("step1\n\nTask: T-MULTI-1");

        fx.write("mod_a/lib.rs", "pub fn step1() {}\npub fn step2() {}\n");
        fx.commit_all("step2\n\nTask: T-MULTI-1");

        // Compose release candidate
        fx.run_args(&[
            "compose",
            "--name",
            "rc-2026-11-11",
            "--base",
            &base,
            "--select",
            "T-MULTI-1",
            "--release-candidate",
            "--strict",
        ])
        .unwrap();

        // Idempotent replay with identical inputs succeeds
        fx.run_args(&[
            "compose",
            "--name",
            "rc-2026-11-11",
            "--base",
            &base,
            "--select",
            "T-MULTI-1",
            "--release-candidate",
            "--strict",
        ])
        .unwrap();

        // Different flags under same name fail closed
        let dup_err = fx
            .run_args(&[
                "compose",
                "--name",
                "rc-2026-11-11",
                "--base",
                &base,
                "--select",
                "T-MULTI-1",
            ])
            .unwrap_err();
        assert!(dup_err.contains("different inputs/evidence"));

        // Status before publish
        let status_before: Value =
            serde_json::from_str(&fx.run_args(&["status", "rc-2026-11-11", "--json"]).unwrap())
                .unwrap();
        assert_eq!(
            status_before["branch_ref"],
            "refs/heads/release/rc-2026-11-11"
        );
        assert_eq!(status_before["branch_state"], "present_exact");
        assert_eq!(status_before["remote_state"], "absent");

        // Publish immutable snapshot tag to bare origin
        let pub_msg = fx.run_args(&["publish", "rc-2026-11-11"]).unwrap();
        assert!(pub_msg.contains("Published distilled snapshot verified"));

        let status_after: Value =
            serde_json::from_str(&fx.run_args(&["status", "rc-2026-11-11", "--json"]).unwrap())
                .unwrap();
        assert_eq!(status_after["remote_state"], "published_exact");

        // Verify bare remote tag cannot be silently replaced if it points elsewhere
        let bare = Git { root: &fx.remote };
        bare.text(&["update-ref", "refs/tags/susi-distill/rc-conflict", &base])
            .unwrap();
        fx.run_args(&[
            "compose",
            "--name",
            "rc-conflict",
            "--base",
            &base,
            "--select",
            "T-MULTI-1",
        ])
        .unwrap();
        let conflict_err = fx.run_args(&["publish", "rc-conflict"]).unwrap_err();
        assert!(conflict_err.contains("already points to"));

        // Grafted history fails closed
        let grafts = PathBuf::from(
            fx.git()
                .text(&[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "info/grafts",
                ])
                .unwrap(),
        );
        fs::write(&grafts, "").unwrap();
        let graft_err = fx.run_args(&["status", "rc-2026-11-11"]).unwrap_err();
        assert!(graft_err.contains("grafts"));
    }

    #[test]
    fn plan_manifest_interop_and_batched_commit_stream_include_empty_and_multi_commit_tasks() {
        let fx = Fixture::new();
        fx.write("README.md", "base\n");
        // Seed 1,600 task records (>65 KB cat-file --batch stdin and >1 MiB stdout) to guard against OS pipe deadlocks
        let padding = "x".repeat(512);
        for n in 0..1600u32 {
            let id = format!("T-BULK-{n}");
            let doc = json!({
                "id": id,
                "title": format!("Bulk task {n} {padding}"),
                "goal": "Exercise large cat-file --batch pipe streaming",
                "created_by": "worker",
                "deps": [],
                "scopes": ["bulk"],
                "accept": { "cmd": ["git", "--version"] }
            });
            fx.write(
                &format!(".agents/tasks/{id}.json"),
                &serde_json::to_string(&doc).unwrap(),
            );
        }
        let base = fx.commit_all("base");

        // Empty marker commit in stream to verify batched git log parser handles zero-diff commits
        fx.git()
            .text(&[
                "-c",
                "user.name=Worker",
                "-c",
                "user.email=worker@localhost",
                "commit",
                "--allow-empty",
                "-m",
                "chore: empty checkpoint",
            ])
            .unwrap();

        // Task stored under .agents/tasks/in-progress/ with legacy dependencies/scope keys
        let in_prog_doc = json!({
            "id": "T-INPROG-1",
            "title": "In-progress layout task",
            "goal": "Verify in-progress dir and legacy field aliases",
            "created_by": "worker",
            "dependencies": [],
            "scope": ["alpha", ".agents/tasks/in-progress/T-INPROG-1.json"],
            "accept": { "cmd": ["git", "rev-parse", "--verify", "HEAD:alpha/one.rs"] }
        });
        fx.write("alpha/one.rs", "pub const ONE: u8 = 1;\n");
        fx.write(
            ".agents/tasks/in-progress/T-INPROG-1.json",
            &format!("{}\n", serde_json::to_string_pretty(&in_prog_doc).unwrap()),
        );
        fx.commit_all("feat: alpha step 1\n\ntask: t-inprog-1");

        fx.write("alpha/two.rs", "pub const TWO: u8 = 2;\n");
        let head = fx.commit_all("feat: alpha step 2\n\nTask: T-INPROG-1");

        let plan_doc = json!({
            "schema": PLAN_SCHEMA,
            "plan_only": true,
            "base": { "rev": "HEAD", "commit": head },
            "requested": ["T-INPROG-1"],
            "included": [{ "id": "T-INPROG-1" }]
        });
        fx.write(
            "slice1-plan.json",
            &serde_json::to_string_pretty(&plan_doc).unwrap(),
        );

        let out = fx
            .run_args(&[
                "compose",
                "--name",
                "from-plan-manifest",
                "--base",
                &base,
                "--plan-manifest",
                "slice1-plan.json",
                "--strict",
                "--json",
            ])
            .unwrap();
        let receipt: Value = serde_json::from_str(&out).unwrap();
        assert!(receipt["plan_manifest_sha256"].as_str().is_some());
        assert_eq!(receipt["included"][0]["task_id"], "T-INPROG-1");
        assert_eq!(
            receipt["included"][0]["source_commits"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn compose_verify_accept_runs_task_commands_in_isolated_worktree_and_fails_closed_on_error() {
        let fx = Fixture::new();
        fx.write("README.md", "base\n");
        let base = fx.commit_all("base");

        let ok_doc = json!({
            "id": "T-ACCEPT-OK",
            "title": "Passing accept task",
            "goal": "Verify composed tree contains ready.txt",
            "created_by": "worker",
            "deps": [],
            "scopes": ["mod_ok", ".agents/tasks/T-ACCEPT-OK.json"],
            "accept": { "cmd": ["git", "rev-parse", "--verify", "HEAD:mod_ok/ready.txt"] }
        });
        fx.write("mod_ok/ready.txt", "verified\n");
        fx.write(
            ".agents/tasks/T-ACCEPT-OK.json",
            &format!("{}\n", serde_json::to_string_pretty(&ok_doc).unwrap()),
        );
        fx.commit_all("feat: mod_ok\n\nTask: T-ACCEPT-OK");

        let bad_doc = json!({
            "id": "T-ACCEPT-FAIL",
            "title": "Failing accept task",
            "goal": "Check missing file fails closed",
            "created_by": "worker",
            "deps": [],
            "scopes": ["mod_bad", ".agents/tasks/T-ACCEPT-FAIL.json"],
            "accept": { "cmd": ["git", "rev-parse", "--verify", "HEAD:mod_bad/absent.txt"] }
        });
        fx.write("mod_bad/present.txt", "present\n");
        fx.write(
            ".agents/tasks/T-ACCEPT-FAIL.json",
            &format!("{}\n", serde_json::to_string_pretty(&bad_doc).unwrap()),
        );
        fx.commit_all("feat: mod_bad\n\nTask: T-ACCEPT-FAIL");

        // Passing --verify-accept records accept_verified_candidate and creates branch ref
        let ok_out = fx
            .run_args(&[
                "compose",
                "--name",
                "verify-pass",
                "--base",
                &base,
                "--select",
                "T-ACCEPT-OK",
                "--verify-accept",
                "--strict",
                "--json",
            ])
            .unwrap();
        let receipt: Value = serde_json::from_str(&ok_out).unwrap();
        assert_eq!(receipt["qualification_status"], "accept_verified_candidate");
        assert_eq!(receipt["accept_verification"][0]["passed"], true);
        assert!(fx
            .git()
            .text(&["show-ref", "--verify", "refs/heads/distill/verify-pass"])
            .is_ok());

        // Failing --verify-accept fails closed before creating refs/heads/distill/verify-fail
        let fail_err = fx
            .run_args(&[
                "compose",
                "--name",
                "verify-fail",
                "--base",
                &base,
                "--select",
                "T-ACCEPT-FAIL",
                "--verify-accept",
                "--strict",
            ])
            .unwrap_err();
        assert!(fail_err.contains("accept_verification_failed"));
        assert!(fx
            .git()
            .text(&["show-ref", "--verify", "refs/heads/distill/verify-fail"])
            .is_err());
    }

    #[test]
    fn orchestrate_discovers_tasks_from_range_composes_and_publishes_snapshot() {
        let fx = Fixture::new();
        fx.write("core/base.txt", "base\n");
        fx.write(
            ".agents/tasks/done/T-LEGACY-BASE.json",
            "{\n  \"id\": \"T-LEGACY-BASE\",\n  \"title\": \"Closed without scopes by susi tasks close\",\n  \"closed\": { \"at_unix\": 1794000000 }\n}\n",
        );
        let base = fx.commit_all("chore: base");

        fx.write("mod_one/lib.rs", "pub fn one() -> usize { 1 }\n");
        fx.write_task("T-ORCH-1", &["T-LEGACY-BASE"], &["mod_one"]);
        let c1 = fx.commit_all("feat: mod_one\n\nTask: T-ORCH-1");

        fx.write("mod_two/lib.rs", "pub fn two() -> usize { 2 }\n");
        fx.write_task("T-ORCH-2", &["T-ORCH-1"], &["mod_two"]);
        let c2 = fx.commit_all("feat: mod_two\n\nTask: T-ORCH-2");

        let range = format!("{base}..{c2}");
        let out = fx
            .run_args(&[
                "orchestrate",
                "--name",
                "orch-wave-1",
                "--base",
                &base,
                "--from",
                &c2,
                "--tasks-from-range",
                &range,
                "--publish-snapshot",
                "--strict",
                "--json",
            ])
            .unwrap();
        let receipt: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(receipt["mode"], "orchestrate");
        assert_eq!(receipt["included"].as_array().unwrap().len(), 2);
        assert_eq!(receipt["included"][0]["task_id"], "T-ORCH-1");
        assert_eq!(receipt["included"][1]["task_id"], "T-ORCH-2");
        assert_eq!(receipt["included"][0]["source_commits"][0], c1);

        let status_json = fx.run_args(&["status", "orch-wave-1", "--json"]).unwrap();
        let st: Value = serde_json::from_str(&status_json).unwrap();
        assert_eq!(st["branch_state"], "present_exact");
        assert_eq!(st["remote_state"], "published_exact");
    }

    #[test]
    fn submit_main_reconciles_concurrent_peer_commits_without_force_push_and_rejects_unlabelled() {
        let fx = Fixture::new();
        fx.write("core/root.txt", "v0\n");
        fx.write_task("T-BASE-1", &[], &["core"]);
        let base = fx.commit_all("feat: base\n\nTask: T-BASE-1");

        // Fast-forward initial main submission
        let ff_json = fx.run_args(&["submit-main", "--json"]).unwrap();
        let ff: Value = serde_json::from_str(&ff_json).unwrap();
        assert_eq!(ff["status"], "published_fast_forward");
        assert_eq!(ff["reconciled"], false);

        // Simulate a concurrent peer pushing disjoint work directly to origin/main
        let peer_dir = fx.dir.join("peer");
        Git { root: &fx.dir }
            .text(&[
                "clone",
                fx.remote.to_str().unwrap(),
                peer_dir.to_str().unwrap(),
            ])
            .unwrap();
        let peer_git = Git { root: &peer_dir };
        fs::create_dir_all(peer_dir.join("peer_mod")).unwrap();
        fs::write(peer_dir.join("peer_mod/peer.txt"), "peer work\n").unwrap();
        peer_git.text(&["add", "-A"]).unwrap();
        peer_git
            .text(&[
                "-c",
                "user.name=Peer",
                "-c",
                "user.email=peer@localhost",
                "commit",
                "-m",
                "feat: peer\n\nTask: T-PEER-1",
            ])
            .unwrap();
        peer_git
            .text(&["push", "origin", "HEAD:refs/heads/main"])
            .unwrap();
        let peer_head = peer_git.resolve_commit("HEAD").unwrap();
        assert_ne!(peer_head, base);

        // Meanwhile local worker commits isolated work on top of `base` (diverged from origin/main)
        fx.write("agent_mod/own.txt", "own isolated work\n");
        fx.write_task("T-OWN-1", &[], &["agent_mod"]);
        let own_commit = fx.commit_all("feat: own\n\nTask: T-OWN-1");

        // submit-main reconciles in-memory via 3-way merge-tree + commit-tree and fast-forwards origin/main!
        let rec_json = fx
            .run_args(&["submit-main", "--rev", &own_commit, "--json"])
            .unwrap();
        let rec: Value = serde_json::from_str(&rec_json).unwrap();
        assert_eq!(rec["status"], "published_reconciled");
        assert_eq!(rec["reconciled"], true);
        let published = rec["published_commit"].as_str().unwrap();

        // Both peer work and own work are preserved on origin/main!
        let bare = Git { root: &fx.remote };
        assert!(bare.ancestor(&peer_head, published).unwrap());
        assert!(bare.ancestor(&own_commit, published).unwrap());

        // Unlabelled commit is refused before touching origin/main
        fx.write("agent_mod/unlabelled.txt", "no trailer\n");
        let bad_commit = fx.commit_all("chore: forgot trailer");
        let err = fx
            .run_args(&["submit-main", "--rev", &bad_commit])
            .unwrap_err();
        assert!(err.contains("missing_task_trailer"));
    }

    #[test]
    fn synthetic_base_merge_three_way_handles_clean_merge_and_conflict() {
        let fx = Fixture::new();
        fx.write("shared.txt", "alpha\nbeta\ngamma\n");
        fx.git().text(&["add", "-A"]).unwrap();
        fx.git().text(&["commit", "-m", "base"]).unwrap();
        let base = fx.git().resolve_commit("HEAD").unwrap();

        fx.write("ours_only.txt", "ours\n");
        fx.git().text(&["add", "-A"]).unwrap();
        fx.git().text(&["commit", "-m", "ours"]).unwrap();
        let ours = fx.git().resolve_commit("HEAD").unwrap();

        fx.git().text(&["checkout", "-q", &base]).unwrap();
        fx.write("shared.txt", "alpha\nbeta\ngamma\ndelta\n");
        fx.git().text(&["add", "-A"]).unwrap();
        fx.git().text(&["commit", "-m", "theirs-clean"]).unwrap();
        let theirs_clean = fx.git().resolve_commit("HEAD").unwrap();

        let clean = fx
            .git()
            .merge_three_way_synthetic_base(&base, &ours, &theirs_clean)
            .unwrap();
        let ThreeWayMerge::Clean(merged_tree) = clean else {
            panic!("expected clean 3-way merge, got {clean:?}");
        };
        assert_eq!(
            fx.git()
                .text(&["cat-file", "-p", &format!("{merged_tree}:shared.txt")])
                .unwrap(),
            "alpha\nbeta\ngamma\ndelta"
        );
        assert_eq!(
            fx.git()
                .text(&["cat-file", "-p", &format!("{merged_tree}:ours_only.txt")])
                .unwrap(),
            "ours"
        );

        fx.git().text(&["checkout", "-q", &base]).unwrap();
        fx.write("ours_only.txt", "conflicting-theirs\n");
        fx.git().text(&["add", "-A"]).unwrap();
        fx.git().text(&["commit", "-m", "theirs-conflict"]).unwrap();
        let theirs_conflict = fx.git().resolve_commit("HEAD").unwrap();

        let conflict = fx
            .git()
            .merge_three_way_synthetic_base(&base, &ours, &theirs_conflict)
            .unwrap();
        assert!(matches!(conflict, ThreeWayMerge::Conflict(_)));
    }
}

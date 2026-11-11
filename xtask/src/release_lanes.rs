//! Multi-agent release lanes (CEO Mandate 74, section 3).
//!
//! Many agents advance one release line in parallel without conflicts:
//!
//! - `init` turns a `local-distill plan` receipt into lanes with disjoint file
//!   scopes (tasks that share a path prefix or a dependency share a lane) and
//!   opens the release line `refs/candidates/release/<name>` with a manifest
//!   commit (`.agents/releases/<name>.json`). The candidate namespace is not a
//!   branch, so a Git host shows no pull-request prompt or merge button for it,
//!   and `push-policy` admits only fast-forwards there and never a deletion.
//! - `claim` pushes a claim blob to `refs/claims/release-lanes/<name>/<lane>`.
//!   Git refuses to replace an existing non-commit ref without force, and the
//!   receiving side compares the old value atomically, so the claim is an
//!   atomic create-if-absent with no lock server.
//! - `pick` cherry-picks the lane's source commits onto the current release tip
//!   in memory (`git merge-tree --write-tree --merge-base`, `commit-tree`, with
//!   the `(cherry picked from commit ...)` line of `cherry-pick -x`), skipping
//!   commits already present by source line or `git patch-id --stable`. It never
//!   touches the worktree.
//! - `verify` runs a qualification command on the exact candidate tree and
//!   writes evidence bound to that tree.
//! - `advance` adds the manifest update on top of the qualified code and pushes
//!   the release line and the claim deletion in one `git push --atomic`, as a
//!   normal fast-forward. Losing the race leaves everything unchanged.
//! - `ship` loops pick, verify and advance with a bounded number of attempts.
//!
//! The final immutable `vX.Y.Z` tag stays with `local-release qualify`, which
//! `push-policy` requires; this tool never forces, never deletes a release line
//! and never calls a hosting API.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const MANIFEST_SCHEMA: &str = "susi-release-lanes-v1";
pub const CLAIM_SCHEMA: &str = "susi-lane-claim-v1";
pub const EVIDENCE_SCHEMA: &str = "susi-lane-evidence-v1";
const RECEIPT_SCHEMA: &str = "susi-distillation-receipt-v1";
/// Error prefix that `ship` treats as retryable.
const LOST_RACE: &str = "lost race";
const DEFAULT_ATTEMPTS: u32 = 3;

type Result<T> = std::result::Result<T, String>;

const USAGE: &str = "\
release-lanes: many agents advance one release line in parallel (CEO Mandate 74)

  init    --name <release> (--plan <receipt.json> | --base <rev> [--from <rev>] [--select <ids>]
          [--tasks-from-range <A..B>] [--feature <spec>]) [--dry-run]
  status  --name <release> [--json]
  claim   --name <release> --lane <lane> [--stale-secs <n>]
  unclaim --name <release> --lane <lane>
  pick    --name <release> --lane <lane> [--resume <rev>]
  verify  --name <release> --lane <lane> [--rev <rev>] [--here] [--keep] --cmd <program> [args...]
  advance --name <release> --lane <lane> [--rev <rev>] [--evidence <file>]
  ship    --name <release> --lane <lane> [--attempts <n>] --cmd <program> [args...]

Common: --remote <name> (default origin), --agent <id> (default $SUSI_AGENT or git user.name).
The release line is refs/candidates/release/<release>; claims are refs/claims/release-lanes/<release>/<lane>.";

// ---------------------------------------------------------------- git

struct Git<'a> {
    root: &'a Path,
}

impl Git<'_> {
    fn raw(&self, args: &[&str], envs: &[(&str, &str)], input: Option<&[u8]>) -> Result<Output> {
        let mut cmd = Command::new("git");
        cmd.current_dir(self.root)
            .arg("--no-replace-objects")
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot run git {}: {e}", args.join(" ")))?;
        if let Some(bytes) = input {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "git stdin unavailable".to_string())?;
            stdin
                .write_all(bytes)
                .map_err(|e| format!("cannot write to git {}: {e}", args.join(" ")))?;
        }
        child
            .wait_with_output()
            .map_err(|e| format!("git {} failed: {e}", args.join(" ")))
    }

    fn text_with(
        &self,
        args: &[&str],
        envs: &[(&str, &str)],
        input: Option<&[u8]>,
    ) -> Result<String> {
        let out = self.raw(args, envs, input)?;
        if !out.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn text(&self, args: &[&str]) -> Result<String> {
        self.text_with(args, &[], None)
    }

    fn succeeds(&self, args: &[&str]) -> bool {
        self.raw(args, &[], None)
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn git_path(&self, rel: &str) -> Result<PathBuf> {
        Ok(PathBuf::from(self.text(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            rel,
        ])?))
    }

    fn commit(&self, rev: &str) -> Result<String> {
        self.text(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ])
    }

    fn tree(&self, rev: &str) -> Result<String> {
        self.text(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{tree}}"),
        ])
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        self.succeeds(&["merge-base", "--is-ancestor", ancestor, descendant])
    }

    fn has_object(&self, oid: &str) -> bool {
        self.succeeds(&["cat-file", "-e", oid])
    }

    /// The object a remote ref points at, without fetching.
    fn ls_remote(&self, remote: &str, name: &str) -> Result<Option<String>> {
        let listing = self.text(&["ls-remote", "--refs", remote, name])?;
        Ok(listing.lines().find_map(|line| {
            let mut fields = line.split_whitespace();
            let oid = fields.next()?;
            (fields.next()? == name).then(|| oid.to_string())
        }))
    }

    /// Fetch a remote ref's objects and return the object it points at.
    fn fetch_ref(&self, remote: &str, name: &str) -> Result<Option<String>> {
        for _ in 0..3 {
            let Some(oid) = self.ls_remote(remote, name)? else {
                return Ok(None);
            };
            if !self.has_object(&oid) {
                self.text(&["fetch", "--no-tags", "--quiet", remote, name])?;
            }
            if self.has_object(&oid) {
                return Ok(Some(oid));
            }
        }
        Err(format!("{name} kept moving while it was fetched; retry"))
    }

    fn push(&self, remote: &str, specs: &[String], atomic: bool) -> Result<Pushed> {
        let mut args = vec!["push", "--porcelain"];
        if atomic {
            args.push("--atomic");
        }
        args.push(remote);
        args.extend(specs.iter().map(String::as_str));
        let out = self.raw(&args, &[], None)?;
        if out.status.success() {
            return Ok(Pushed::Done);
        }
        let all = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let race = [
            "[rejected]",
            "non-fast-forward",
            "fetch first",
            "already exists",
            "stale info",
            "needs force",
            "cannot lock ref",
            "failed to update ref",
            "atomic push failed",
        ];
        if all.contains("hook declined") || all.contains("pre-push") {
            return Err(format!("push refused by policy: {}", all.trim()));
        }
        if race.iter().any(|needle| all.contains(needle)) {
            return Ok(Pushed::Rejected(all.trim().to_string()));
        }
        Err(format!("push failed: {}", all.trim()))
    }

    /// Commit `parent`'s tree with one file replaced, without an index or worktree.
    fn commit_file(&self, parent: &str, path: &str, bytes: &[u8], message: &str) -> Result<String> {
        let blob = self.text_with(&["hash-object", "-w", "--stdin"], &[], Some(bytes))?;
        let index = self.git_path(&format!("susi-lanes/index-{}", unique()))?;
        if let Some(dir) = index.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let index_text = index.to_string_lossy().to_string();
        let env = [("GIT_INDEX_FILE", index_text.as_str())];
        let tree = (|| {
            self.text_with(&["read-tree", parent], &env, None)?;
            self.text_with(
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{blob},{path}"),
                ],
                &env,
                None,
            )?;
            self.text_with(&["write-tree"], &env, None)
        })();
        let _ = fs::remove_file(&index);
        let tree = tree?;
        self.text_with(
            &["commit-tree", &tree, "-p", parent, "-F", "-"],
            &[],
            Some(message.as_bytes()),
        )
    }
}

enum Pushed {
    Done,
    Rejected(String),
}

fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------- options

struct Opts {
    values: BTreeMap<String, Vec<String>>,
    flags: BTreeSet<String>,
    cmd: Vec<String>,
}

const FLAGS: &[&str] = &["--json", "--dry-run", "--here", "--keep"];

impl Opts {
    fn parse(args: &[String]) -> Result<Self> {
        let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut flags = BTreeSet::new();
        let mut cmd = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "--cmd" {
                cmd = args[i + 1..].to_vec();
                break;
            }
            if FLAGS.contains(&arg.as_str()) {
                flags.insert(arg.clone());
                i += 1;
                continue;
            }
            if !arg.starts_with("--") {
                return Err(format!("unexpected argument {arg}\n\n{USAGE}"));
            }
            let value = args
                .get(i + 1)
                .ok_or_else(|| format!("{arg} needs a value"))?;
            values.entry(arg.clone()).or_default().push(value.clone());
            i += 2;
        }
        Ok(Self { values, flags, cmd })
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.values
            .get(key)
            .and_then(|v| v.last())
            .map(String::as_str)
    }

    fn all(&self, key: &str) -> Vec<String> {
        self.values.get(key).cloned().unwrap_or_default()
    }

    fn need(&self, key: &str) -> Result<&str> {
        self.get(key)
            .ok_or_else(|| format!("{key} is required\n\n{USAGE}"))
    }

    fn flag(&self, key: &str) -> bool {
        self.flags.contains(key)
    }

    fn remote(&self) -> &str {
        self.get("--remote").unwrap_or("origin")
    }

    fn name(&self) -> Result<String> {
        segment(self.need("--name")?, "release name")
    }

    fn lane(&self) -> Result<String> {
        segment(self.need("--lane")?, "lane")
    }

    fn agent(&self, git: &Git<'_>) -> Result<String> {
        let agent = match self.get("--agent") {
            Some(a) => a.to_string(),
            None => match std::env::var("SUSI_AGENT") {
                Ok(a) if !a.trim().is_empty() => a,
                _ => git
                    .text(&["config", "user.name"])
                    .map_err(|_| "set --agent, SUSI_AGENT or git user.name".to_string())?,
            },
        };
        let agent = agent.trim().to_string();
        if agent.is_empty() || agent.chars().any(char::is_control) {
            return Err("agent identity must be a non-empty single line".into());
        }
        Ok(agent)
    }
}

/// A ref-safe name segment.
fn segment(value: &str, what: &str) -> Result<String> {
    let ok = !value.is_empty()
        && value.len() <= 100
        && !value.starts_with(['.', '-'])
        && !value.contains("..")
        && !value.ends_with(".lock")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(value.to_string())
    } else {
        Err(format!(
            "invalid {what} {value:?}: use [A-Za-z0-9._-], no leading . or -"
        ))
    }
}

fn release_ref(name: &str) -> String {
    format!("refs/candidates/release/{name}")
}

fn manifest_path(name: &str) -> String {
    format!(".agents/releases/{name}.json")
}

fn claim_ref(name: &str, lane: &str) -> String {
    format!("refs/claims/release-lanes/{name}/{lane}")
}

fn lane_ref(name: &str, lane: &str) -> String {
    format!("refs/susi/lanes/{name}/{lane}")
}

fn evidence_file(git: &Git<'_>, name: &str, lane: &str) -> Result<PathBuf> {
    git.git_path(&format!("susi-lanes/{name}/{lane}.evidence.json"))
}

// ---------------------------------------------------------------- planning

#[derive(Debug, Clone)]
struct PlanTask {
    id: String,
    title: String,
    deps: Vec<String>,
    paths: BTreeSet<String>,
    sources: Vec<String>,
}

fn normalize(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = p.trim_start_matches("./").trim_matches('/');
    if p == "." {
        String::new()
    } else {
        p.to_string()
    }
}

/// Two path prefixes overlap when one contains the other; "" is the whole tree.
fn overlaps(a: &str, b: &str) -> bool {
    a.is_empty()
        || b.is_empty()
        || a == b
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Read the included tasks of a `local-distill plan --json` receipt in closure order.
fn parse_plan(receipt: &Value) -> Result<(String, Vec<PlanTask>)> {
    if receipt["schema"].as_str() != Some(RECEIPT_SCHEMA) {
        return Err(format!(
            "plan must be a {RECEIPT_SCHEMA} receipt (local-distill plan --json)"
        ));
    }
    let base = receipt["base_commit"]
        .as_str()
        .ok_or("plan receipt has no base_commit")?
        .to_string();
    let mut tasks: Vec<PlanTask> = Vec::new();
    for item in receipt["included"].as_array().into_iter().flatten() {
        let id = item["task_id"]
            .as_str()
            .ok_or("included task without task_id")?
            .to_string();
        let mut paths: BTreeSet<String> = strings(item, "scopes")
            .iter()
            .map(|p| normalize(p))
            .collect();
        paths.extend(strings(item, "changed_files").iter().map(|p| normalize(p)));
        let sources = strings(item, "source_commits");
        if sources.is_empty() {
            return Err(format!("{id} has no source commits in the plan"));
        }
        tasks.push(PlanTask {
            title: item["title"].as_str().unwrap_or_default().to_string(),
            deps: strings(item, "deps"),
            paths,
            sources,
            id,
        });
    }
    let order = strings(receipt, "closure_order");
    let rank = |id: &str| order.iter().position(|o| o == id).unwrap_or(usize::MAX);
    tasks.sort_by_key(|t| rank(&t.id));
    Ok((base, tasks))
}

/// Group tasks into lanes: tasks sharing a path prefix or a dependency share a
/// lane; lanes are ordered by their first task, tasks keep their order.
fn compute_lanes(tasks: &[PlanTask]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..tasks.len()).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut root = i;
        while parent[root] != root {
            root = parent[root];
        }
        let mut node = i;
        while parent[node] != root {
            let next = parent[node];
            parent[node] = root;
            node = next;
        }
        root
    }
    for i in 0..tasks.len() {
        for j in i + 1..tasks.len() {
            let linked = tasks[i].deps.contains(&tasks[j].id)
                || tasks[j].deps.contains(&tasks[i].id)
                || tasks[i]
                    .paths
                    .iter()
                    .any(|a| tasks[j].paths.iter().any(|b| overlaps(a, b)));
            if linked {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri.max(rj)] = ri.min(rj);
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..tasks.len() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    groups.into_values().collect()
}

/// Smallest set of prefixes covering `paths`.
fn minimal_paths(paths: &BTreeSet<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in paths {
        if !paths.iter().any(|q| {
            q != p
                && (q.is_empty()
                    || p.strip_prefix(q.as_str())
                        .is_some_and(|r| r.starts_with('/')))
        }) {
            out.push(p.clone());
        }
    }
    out
}

fn new_manifest(name: &str, base: &str, tasks: &[PlanTask], agent: &str, plan_sha: &str) -> Value {
    let lanes: Vec<Value> = compute_lanes(tasks)
        .iter()
        .enumerate()
        .map(|(n, members)| {
            let mut paths = BTreeSet::new();
            for &i in members {
                paths.extend(tasks[i].paths.iter().cloned());
            }
            json!({
                "id": format!("lane-{:02}", n + 1),
                "status": "pending",
                "paths": minimal_paths(&paths),
                "tasks": members.iter().map(|&i| json!({
                    "task_id": tasks[i].id,
                    "title": tasks[i].title,
                    "deps": tasks[i].deps,
                    "source_commits": tasks[i].sources,
                    "status": "pending",
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "schema": MANIFEST_SCHEMA,
        "name": name,
        "release_ref": release_ref(name),
        "base_commit": base,
        "created_by": agent,
        "created_unix": now_unix(),
        "plan_receipt_sha256": plan_sha,
        "lanes": lanes,
        "advances": [],
    })
}

fn read_manifest(git: &Git<'_>, rev: &str, name: &str) -> Result<Value> {
    let text = git.text(&["show", &format!("{rev}:{}", manifest_path(name))])?;
    let manifest: Value =
        serde_json::from_str(&text).map_err(|e| format!("release manifest is not JSON: {e}"))?;
    if manifest["schema"].as_str() != Some(MANIFEST_SCHEMA)
        || manifest["name"].as_str() != Some(name)
    {
        return Err(format!(
            "{} is not a {MANIFEST_SCHEMA} manifest for {name}",
            manifest_path(name)
        ));
    }
    Ok(manifest)
}

fn json_bytes(manifest: &Value) -> Result<Vec<u8>> {
    let mut text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text.into_bytes())
}

fn lane_index(manifest: &Value, lane: &str) -> Result<usize> {
    manifest["lanes"]
        .as_array()
        .and_then(|lanes| lanes.iter().position(|l| l["id"].as_str() == Some(lane)))
        .ok_or_else(|| format!("release has no lane {lane}"))
}

/// The release tip and its manifest, fetched from the remote.
fn current(git: &Git<'_>, opts: &Opts, name: &str) -> Result<(String, Value)> {
    let tip = git
        .fetch_ref(opts.remote(), &release_ref(name))?
        .ok_or_else(|| format!("release {name} does not exist on {}", opts.remote()))?;
    let manifest = read_manifest(git, &tip, name)?;
    Ok((tip, manifest))
}

// ---------------------------------------------------------------- claims

struct Claim {
    agent: String,
    claimed_unix: u64,
}

fn read_claim(
    git: &Git<'_>,
    remote: &str,
    name: &str,
    lane: &str,
) -> Result<Option<(String, Claim)>> {
    let Some(oid) = git.fetch_ref(remote, &claim_ref(name, lane))? else {
        return Ok(None);
    };
    let body = git.text(&["cat-file", "-p", &oid])?;
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if value["schema"].as_str() != Some(CLAIM_SCHEMA) {
        return Err(format!("{} holds a foreign object", claim_ref(name, lane)));
    }
    Ok(Some((
        oid,
        Claim {
            agent: value["agent"].as_str().unwrap_or_default().to_string(),
            claimed_unix: value["claimed_unix"].as_u64().unwrap_or(0),
        },
    )))
}

fn require_own_claim(
    git: &Git<'_>,
    opts: &Opts,
    name: &str,
    lane: &str,
    agent: &str,
) -> Result<()> {
    match read_claim(git, opts.remote(), name, lane)? {
        Some((_, claim)) if claim.agent == agent => Ok(()),
        Some((_, claim)) => Err(format!(
            "lane {lane} is claimed by {}, not {agent}",
            claim.agent
        )),
        None => Err(format!(
            "claim lane {lane} first: release-lanes claim --name {name} --lane {lane}"
        )),
    }
}

// ---------------------------------------------------------------- commands

pub fn run(root: &Path, args: &[String]) -> Result<String> {
    let Some((command, rest)) = args.split_first() else {
        return Err(USAGE.to_string());
    };
    let opts = Opts::parse(rest)?;
    let git = Git { root };
    match command.as_str() {
        "init" => init(&git, root, &opts),
        "status" => status(&git, &opts),
        "claim" => claim(&git, &opts),
        "unclaim" => unclaim(&git, &opts),
        "pick" => pick(&git, &opts).map(|p| p.report),
        "verify" => verify(&git, &opts).map(|(text, _)| text),
        "advance" => advance(&git, &opts),
        "ship" => ship(&git, &opts),
        "help" | "--help" | "-h" => Ok(USAGE.to_string()),
        other => Err(format!("unknown command {other}\n\n{USAGE}")),
    }
}

fn init(git: &Git<'_>, root: &Path, opts: &Opts) -> Result<String> {
    let name = opts.name()?;
    let agent = opts.agent(git)?;
    let receipt_text = if let Some(file) = opts.get("--plan") {
        fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?
    } else {
        let base = opts.need("--base")?;
        let mut distill = vec![
            "plan".to_string(),
            "--name".to_string(),
            format!("lanes-{name}"),
            "--base".to_string(),
            base.to_string(),
        ];
        for key in [
            "--from",
            "--select",
            "--tasks-from-range",
            "--feature",
            "--plan-manifest",
        ] {
            for value in opts.all(key) {
                distill.push(key.to_string());
                distill.push(value);
            }
        }
        distill.push("--json".to_string());
        crate::distill_compose::run(root, &distill)?
    };
    let receipt: Value = serde_json::from_str(&receipt_text)
        .map_err(|e| format!("plan receipt is not JSON: {e}"))?;
    let (base, tasks) = parse_plan(&receipt)?;
    if tasks.is_empty() {
        return Err("the plan includes no tasks; nothing to release".into());
    }
    let base = git.commit(&base)?;
    for task in &tasks {
        for source in &task.sources {
            if !git.has_object(source) {
                git.text(&["fetch", "--no-tags", "--quiet", opts.remote(), "main"])?;
                break;
            }
        }
    }
    let manifest = new_manifest(
        &name,
        &base,
        &tasks,
        &agent,
        &sha256_hex(receipt_text.as_bytes()),
    );
    let lanes = manifest["lanes"].as_array().cloned().unwrap_or_default();
    let mut summary = format!(
        "release {name}: {} task(s) in {} lane(s) on base {}\n",
        tasks.len(),
        lanes.len(),
        &base[..12.min(base.len())]
    );
    for lane in manifest["lanes"].as_array().into_iter().flatten() {
        summary.push_str(&format!(
            "  {}  {}  [{}]\n",
            lane["id"].as_str().unwrap_or_default(),
            strings(lane, "paths").join(", "),
            lane["tasks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["task_id"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if opts.flag("--dry-run") {
        let text = String::from_utf8(json_bytes(&manifest)?).map_err(|e| e.to_string())?;
        return Ok(format!("{summary}\n{text}"));
    }
    if git.ls_remote(opts.remote(), &release_ref(&name))?.is_some() {
        return Err(format!(
            "release {name} already exists on {}",
            opts.remote()
        ));
    }
    let message = format!(
        "release-lanes: open {name} with {} lane(s)\n\nRelease-Lanes: {name}\n",
        lanes.len()
    );
    let commit = git.commit_file(
        &base,
        &manifest_path(&name),
        &json_bytes(&manifest)?,
        &message,
    )?;
    match git.push(
        opts.remote(),
        &[format!("{commit}:{}", release_ref(&name))],
        false,
    )? {
        Pushed::Done => Ok(format!(
            "{summary}opened {} at {commit}\nnext: release-lanes claim --name {name} --lane <lane>",
            release_ref(&name)
        )),
        Pushed::Rejected(why) => Err(format!("release {name} was created concurrently: {why}")),
    }
}

fn status(git: &Git<'_>, opts: &Opts) -> Result<String> {
    let name = opts.name()?;
    let (tip, manifest) = current(git, opts, &name)?;
    let mut claims = BTreeMap::new();
    let prefix = format!("refs/claims/release-lanes/{name}/");
    let listing = git.text(&["ls-remote", "--refs", opts.remote(), &format!("{prefix}*")])?;
    for line in listing.lines() {
        if let Some(lane) = line
            .split_whitespace()
            .nth(1)
            .and_then(|r| r.strip_prefix(&prefix))
        {
            if let Some((_, claim)) = read_claim(git, opts.remote(), &name, lane)? {
                claims.insert(lane.to_string(), claim);
            }
        }
    }
    let lanes: Vec<Value> = manifest["lanes"].as_array().cloned().unwrap_or_default();
    let complete = lanes
        .iter()
        .all(|l| l["status"].as_str() == Some("qualified"));
    if opts.flag("--json") {
        let report = json!({
            "release": name,
            "tip": tip,
            "complete": complete,
            "lanes": lanes.iter().map(|l| {
                let id = l["id"].as_str().unwrap_or_default();
                json!({
                    "id": id,
                    "status": l["status"],
                    "paths": l["paths"],
                    "tasks": l["tasks"].as_array().into_iter().flatten().map(|t| t["task_id"].clone()).collect::<Vec<_>>(),
                    "claimed_by": claims.get(id).map(|c| c.agent.clone()),
                    "claimed_unix": claims.get(id).map(|c| c.claimed_unix),
                })
            }).collect::<Vec<_>>(),
        });
        return serde_json::to_string_pretty(&report).map_err(|e| e.to_string());
    }
    let mut out = format!("release {name} at {tip}\n");
    for lane in &lanes {
        let id = lane["id"].as_str().unwrap_or_default();
        let holder = claims
            .get(id)
            .map(|c| {
                format!(
                    "claimed by {} ({}s ago)",
                    c.agent,
                    now_unix().saturating_sub(c.claimed_unix)
                )
            })
            .unwrap_or_else(|| "unclaimed".to_string());
        out.push_str(&format!(
            "  {id:<8} {:<10} {holder:<32} {}\n",
            lane["status"].as_str().unwrap_or_default(),
            strings(lane, "paths").join(", ")
        ));
    }
    if complete {
        out.push_str(&format!(
            "complete: every lane is qualified. The release candidate is {tip}; cut the immutable vX.Y.Z tag with local-release qualify on this exact commit.\n"
        ));
    }
    Ok(out)
}

fn claim(git: &Git<'_>, opts: &Opts) -> Result<String> {
    let (name, lane) = (opts.name()?, opts.lane()?);
    let agent = opts.agent(git)?;
    let (_, manifest) = current(git, opts, &name)?;
    let index = lane_index(&manifest, &lane)?;
    if manifest["lanes"][index]["status"].as_str() != Some("pending") {
        return Err(format!("lane {lane} is already qualified"));
    }
    let target = claim_ref(&name, &lane);
    if let Some((oid, held)) = read_claim(git, opts.remote(), &name, &lane)? {
        if held.agent == agent {
            return Ok(format!("lane {lane} of {name} is already claimed by you"));
        }
        let age = now_unix().saturating_sub(held.claimed_unix);
        let stale = opts
            .get("--stale-secs")
            .map(|s| {
                s.parse::<u64>()
                    .map_err(|_| "--stale-secs takes seconds".to_string())
            })
            .transpose()?;
        match stale {
            Some(limit) if age > limit => {
                // Deleting names the claim we read; Git compares it atomically.
                if let Pushed::Rejected(why) =
                    git.push(opts.remote(), &[format!(":{target}")], false)?
                {
                    return Err(format!(
                        "stale claim {oid} changed while being released: {why}"
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "lane {lane} is claimed by {} ({age}s ago)",
                    held.agent
                ))
            }
        }
    }
    let body = json!({
        "schema": CLAIM_SCHEMA,
        "release": name,
        "lane": lane,
        "agent": agent,
        "claimed_unix": now_unix(),
        "nonce": unique(),
    });
    let text = serde_json::to_string(&body).map_err(|e| e.to_string())?;
    let blob = git.text_with(
        &["hash-object", "-w", "--stdin"],
        &[],
        Some(text.as_bytes()),
    )?;
    match git.push(opts.remote(), &[format!("{blob}:{target}")], false)? {
        Pushed::Done => Ok(format!(
            "claimed lane {lane} of {name} as {agent}\nnext: release-lanes ship --name {name} --lane {lane} --cmd <qualification command>"
        )),
        Pushed::Rejected(_) => {
            let holder = read_claim(git, opts.remote(), &name, &lane)?
                .map(|(_, c)| c.agent)
                .unwrap_or_else(|| "another agent".to_string());
            Err(format!("lost the claim race for lane {lane} to {holder}"))
        }
    }
}

fn unclaim(git: &Git<'_>, opts: &Opts) -> Result<String> {
    let (name, lane) = (opts.name()?, opts.lane()?);
    let agent = opts.agent(git)?;
    require_own_claim(git, opts, &name, &lane, &agent)?;
    match git.push(
        opts.remote(),
        &[format!(":{}", claim_ref(&name, &lane))],
        false,
    )? {
        Pushed::Done => Ok(format!("released lane {lane} of {name}")),
        Pushed::Rejected(why) => Err(format!("claim changed while being released: {why}")),
    }
}

struct Picked {
    head: String,
    report: String,
}

fn picked_sources(git: &Git<'_>, range: &str) -> Result<BTreeSet<String>> {
    let log = git.text(&["log", "--format=%B", range])?;
    Ok(log
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("(cherry picked from commit ")
                .and_then(|rest| rest.strip_suffix(')'))
                .map(str::to_string)
        })
        .collect())
}

fn patch_ids(git: &Git<'_>, range: &str) -> Result<BTreeSet<String>> {
    let patches = git.raw(
        &["log", "-p", "--no-merges", "--format=commit %H", range],
        &[],
        None,
    )?;
    if !patches.status.success() {
        return Err(format!("git log -p {range} failed"));
    }
    let ids = git.text_with(&["patch-id", "--stable"], &[], Some(&patches.stdout))?;
    Ok(ids
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect())
}

fn patch_id(git: &Git<'_>, parent: &str, commit: &str) -> Result<Option<String>> {
    let diff = git.raw(&["diff", "--binary", parent, commit], &[], None)?;
    if !diff.status.success() {
        return Err(format!("git diff {parent} {commit} failed"));
    }
    if diff.stdout.is_empty() {
        return Ok(None);
    }
    let id = git.text_with(&["patch-id", "--stable"], &[], Some(&diff.stdout))?;
    Ok(id.split_whitespace().next().map(str::to_string))
}

fn pick(git: &Git<'_>, opts: &Opts) -> Result<Picked> {
    let (name, lane) = (opts.name()?, opts.lane()?);
    let agent = opts.agent(git)?;
    require_own_claim(git, opts, &name, &lane, &agent)?;
    let (tip, manifest) = current(git, opts, &name)?;
    let index = lane_index(&manifest, &lane)?;
    if manifest["lanes"][index]["status"].as_str() != Some("pending") {
        return Err(format!("lane {lane} is already qualified"));
    }
    let base = manifest["base_commit"]
        .as_str()
        .ok_or("manifest has no base_commit")?
        .to_string();
    let mut head = match opts.get("--resume") {
        Some(rev) => {
            let resumed = git.commit(rev)?;
            if !git.is_ancestor(&tip, &resumed) {
                return Err(format!(
                    "--resume {rev} does not build on the release tip {tip}"
                ));
            }
            resumed
        }
        None => tip.clone(),
    };
    let range = format!("{base}..{head}");
    let mut already = picked_sources(git, &range)?;
    let mut present_patches = patch_ids(git, &range)?;
    let sources: Vec<String> = manifest["lanes"][index]["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| strings(t, "source_commits"))
        .collect();
    if sources.iter().any(|s| !git.has_object(s)) {
        git.text(&["fetch", "--no-tags", "--quiet", opts.remote(), "main"])?;
    }
    let mut picked = Vec::new();
    let mut skipped = Vec::new();
    for source in &sources {
        let source = git
            .commit(source)
            .map_err(|_| format!("source commit {source} is not available"))?;
        if already.contains(&source) {
            skipped.push(format!("{source} already picked"));
            continue;
        }
        let parents = git.text(&["rev-list", "--parents", "-n", "1", &source])?;
        let Some(parent) = parents.split_whitespace().nth(1).map(str::to_string) else {
            skipped.push(format!("{source} is a root commit"));
            continue;
        };
        match patch_id(git, &parent, &source)? {
            Some(pid) if present_patches.contains(&pid) => {
                skipped.push(format!("{source} patch already in the release"));
                continue;
            }
            None => {
                skipped.push(format!("{source} changes nothing"));
                continue;
            }
            Some(pid) => {
                present_patches.insert(pid);
            }
        }
        let merged = git.raw(
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                &format!("--merge-base={parent}"),
                &head,
                &source,
            ],
            &[],
            None,
        )?;
        let stdout = String::from_utf8_lossy(&merged.stdout).to_string();
        let mut lines = stdout.lines();
        let tree = lines.next().unwrap_or_default().trim().to_string();
        match merged.status.code() {
            Some(0) => {}
            Some(1) => {
                let files: Vec<&str> = lines
                    .take_while(|l| !l.trim().is_empty())
                    .map(str::trim)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                return Err(format!(
                    "picking {source} onto {head} conflicts in: {}\n\
                     Resolve in your worktree: git switch --detach {head} && git cherry-pick -x {source}, \
                     fix and commit, then: release-lanes pick --name {name} --lane {lane} --resume HEAD",
                    files.join(", ")
                ));
            }
            _ => {
                return Err(format!(
                    "git merge-tree failed for {source}: {}",
                    String::from_utf8_lossy(&merged.stderr).trim()
                ))
            }
        }
        if tree == git.tree(&head)? {
            skipped.push(format!("{source} empty after pick"));
            continue;
        }
        let meta = git.text(&["log", "-1", "--format=%an%x00%ae%x00%at %az%x00%B", &source])?;
        let mut fields = meta.splitn(4, '\0');
        let (author, email, date, body) = (
            fields.next().unwrap_or_default().to_string(),
            fields.next().unwrap_or_default().to_string(),
            fields.next().unwrap_or_default().to_string(),
            fields.next().unwrap_or_default().trim_end().to_string(),
        );
        let message = format!("{body}\n\n(cherry picked from commit {source})\n");
        head = git.text_with(
            &["commit-tree", &tree, "-p", &head, "-F", "-"],
            &[
                ("GIT_AUTHOR_NAME", author.as_str()),
                ("GIT_AUTHOR_EMAIL", email.as_str()),
                ("GIT_AUTHOR_DATE", date.as_str()),
            ],
            Some(message.as_bytes()),
        )?;
        already.insert(source.clone());
        picked.push(source);
    }
    git.text(&["update-ref", &lane_ref(&name, &lane), &head])?;
    let mut report = format!(
        "lane {lane} of {name}: picked {} commit(s) onto {tip}\ncandidate {head} (tree {})\nlocal ref {}\n",
        picked.len(),
        git.tree(&head)?,
        lane_ref(&name, &lane)
    );
    for s in &skipped {
        report.push_str(&format!("  skipped {s}\n"));
    }
    report.push_str(&format!(
        "next: release-lanes verify --name {name} --lane {lane} --cmd <qualification command>"
    ));
    Ok(Picked { head, report })
}

fn verify(git: &Git<'_>, opts: &Opts) -> Result<(String, PathBuf)> {
    let (name, lane) = (opts.name()?, opts.lane()?);
    if opts.cmd.is_empty() {
        return Err("verify needs --cmd <program> [args...]".into());
    }
    let rev = opts
        .get("--rev")
        .map(str::to_string)
        .unwrap_or_else(|| lane_ref(&name, &lane));
    let commit = git.commit(&rev)?;
    let tree = git.tree(&commit)?;
    let (dir, temporary) = if opts.flag("--here") {
        if git.tree("HEAD")? != tree
            || !git
                .text(&["status", "--porcelain", "--untracked-files=no"])?
                .is_empty()
        {
            return Err(format!("--here needs a clean checkout of exactly {commit}"));
        }
        (git.root.to_path_buf(), false)
    } else {
        let dir = git.git_path(&format!("susi-lanes/wt-{name}-{lane}-{}", unique()))?;
        let dir_text = dir.to_string_lossy().to_string();
        git.text(&["worktree", "add", "--quiet", "--detach", &dir_text, &commit])?;
        (dir, true)
    };
    let started = Instant::now();
    let started_unix = now_unix();
    let status = Command::new(&opts.cmd[0])
        .args(&opts.cmd[1..])
        .current_dir(&dir)
        .status();
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if temporary && !opts.flag("--keep") {
        let _ = git.text(&["worktree", "remove", "--force", &dir.to_string_lossy()]);
    }
    let (exit_code, passed) = match &status {
        Ok(s) => (s.code(), s.success()),
        Err(_) => (None, false),
    };
    let evidence = json!({
        "schema": EVIDENCE_SCHEMA,
        "release": name,
        "lane": lane,
        "commit": commit,
        "tree": tree,
        "cmd": opts.cmd,
        "exit_code": exit_code,
        "passed": passed,
        "duration_ms": duration_ms,
        "started_unix": started_unix,
    });
    let path = match opts.get("--evidence") {
        Some(p) => PathBuf::from(p),
        None => evidence_file(git, &name, &lane)?,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    fs::write(&path, json_bytes(&evidence)?)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    if let Err(e) = status {
        return Err(format!("cannot run {}: {e}", opts.cmd[0]));
    }
    if !passed {
        return Err(format!(
            "qualification failed (exit {exit_code:?}) on tree {tree}; evidence {}",
            path.display()
        ));
    }
    Ok((
        format!(
            "qualified tree {tree} of lane {lane} in {duration_ms} ms\nevidence {}\nnext: release-lanes advance --name {name} --lane {lane}",
            path.display()
        ),
        path,
    ))
}

fn advance(git: &Git<'_>, opts: &Opts) -> Result<String> {
    let (name, lane) = (opts.name()?, opts.lane()?);
    let agent = opts.agent(git)?;
    require_own_claim(git, opts, &name, &lane, &agent)?;
    let rev = opts
        .get("--rev")
        .map(str::to_string)
        .unwrap_or_else(|| lane_ref(&name, &lane));
    let code = git.commit(&rev)?;
    let code_tree = git.tree(&code)?;
    let evidence_path = match opts.get("--evidence") {
        Some(p) => PathBuf::from(p),
        None => evidence_file(git, &name, &lane)?,
    };
    let evidence_bytes = fs::read(&evidence_path).map_err(|e| {
        format!(
            "no qualification evidence at {}: {e}",
            evidence_path.display()
        )
    })?;
    let evidence: Value = serde_json::from_slice(&evidence_bytes)
        .map_err(|e| format!("evidence is not JSON: {e}"))?;
    if evidence["schema"].as_str() != Some(EVIDENCE_SCHEMA)
        || evidence["release"].as_str() != Some(name.as_str())
        || evidence["lane"].as_str() != Some(lane.as_str())
    {
        return Err(format!(
            "{} is not evidence for lane {lane} of {name}",
            evidence_path.display()
        ));
    }
    if evidence["tree"].as_str() != Some(code_tree.as_str()) {
        return Err(format!(
            "evidence qualified tree {}, but the candidate tree is {code_tree}; run verify on this exact tree",
            evidence["tree"].as_str().unwrap_or("?")
        ));
    }
    if evidence["passed"].as_bool() != Some(true) {
        return Err("the evidence records a failed qualification".into());
    }
    let (tip, manifest) = current(git, opts, &name)?;
    if !git.is_ancestor(&tip, &code) {
        return Err(format!(
            "{LOST_RACE}: release {name} moved to {tip} after this candidate was picked; pick, verify and advance again"
        ));
    }
    let index = lane_index(&manifest, &lane)?;
    if manifest["lanes"][index]["status"].as_str() != Some("pending") {
        return Err(format!("lane {lane} was already advanced by another agent"));
    }
    if !git.succeeds(&["diff", "--quiet", &tip, &code, "--", &manifest_path(&name)]) {
        return Err(format!(
            "the candidate edits {}; picks must not touch the manifest",
            manifest_path(&name)
        ));
    }
    let mut picks = Vec::new();
    let commits = git.text(&["rev-list", "--reverse", &format!("{tip}..{code}")])?;
    for commit in commits.lines() {
        let sources = picked_sources(git, &format!("{commit}^!"))?;
        picks.push(json!({ "commit": commit, "source": sources.into_iter().next() }));
    }
    let mut updated = manifest.clone();
    let lane_value = &mut updated["lanes"][index];
    lane_value["status"] = json!("qualified");
    if let Some(tasks) = lane_value["tasks"].as_array_mut() {
        for task in tasks {
            task["status"] = json!("qualified");
        }
    }
    let evidence_sha = sha256_hex(&evidence_bytes);
    let record = json!({
        "lane": lane,
        "agent": agent,
        "advanced_unix": now_unix(),
        "from_tip": tip,
        "code_commit": code,
        "code_tree": code_tree,
        "evidence_sha256": evidence_sha,
        "evidence_cmd": evidence["cmd"],
        "evidence_duration_ms": evidence["duration_ms"],
        "picks": picks,
    });
    lane_value["advance"] = record.clone();
    if let Some(advances) = updated["advances"].as_array_mut() {
        advances.push(record);
    }
    let message = format!(
        "release-lanes: advance {name} lane {lane}\n\nRelease-Lanes: {name}\nLane: {lane}\nQualified-Tree: {code_tree}\nEvidence-Sha256: {evidence_sha}\n"
    );
    let release_commit = git.commit_file(
        &code,
        &manifest_path(&name),
        &json_bytes(&updated)?,
        &message,
    )?;
    let specs = [
        format!("{release_commit}:{}", release_ref(&name)),
        format!(":{}", claim_ref(&name, &lane)),
    ];
    match git.push(opts.remote(), &specs, true)? {
        Pushed::Done => {}
        Pushed::Rejected(why) => {
            return Err(format!(
                "{LOST_RACE}: another agent advanced {name} first ({why})"
            ));
        }
    }
    // A later agent may already have advanced past us; it must contain our commit.
    let landed = git.fetch_ref(opts.remote(), &release_ref(&name))?;
    if !landed.is_some_and(|tip| tip == release_commit || git.is_ancestor(&release_commit, &tip)) {
        return Err(format!(
            "push reported success but {} does not contain {release_commit}",
            release_ref(&name)
        ));
    }
    let remaining = updated["lanes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| l["status"].as_str() != Some("qualified"))
        .count();
    let mut out = format!(
        "advanced {} to {release_commit} with lane {lane} ({} pick(s), qualified tree {code_tree}); claim released\n",
        release_ref(&name),
        picks.len()
    );
    if remaining == 0 {
        out.push_str("release complete: every lane is qualified; cut the vX.Y.Z tag with local-release qualify on this commit\n");
    } else {
        out.push_str(&format!("{remaining} lane(s) still pending\n"));
    }
    Ok(out)
}

fn ship(git: &Git<'_>, opts: &Opts) -> Result<String> {
    let attempts = match opts.get("--attempts") {
        Some(n) => n
            .parse::<u32>()
            .map_err(|_| "--attempts takes a number".to_string())?
            .max(1),
        None => DEFAULT_ATTEMPTS,
    };
    if opts.cmd.is_empty() {
        return Err("ship needs --cmd <program> [args...]".into());
    }
    let mut log = String::new();
    for attempt in 1..=attempts {
        let picked = pick(git, opts)?;
        log.push_str(&format!("attempt {attempt}: {}\n", picked.report));
        let mut verify_opts = Opts {
            values: opts.values.clone(),
            flags: opts.flags.clone(),
            cmd: opts.cmd.clone(),
        };
        verify_opts
            .values
            .insert("--rev".into(), vec![picked.head.clone()]);
        let (text, evidence) = verify(git, &verify_opts)?;
        log.push_str(&text);
        log.push('\n');
        verify_opts.values.insert(
            "--evidence".into(),
            vec![evidence.to_string_lossy().to_string()],
        );
        match advance(git, &verify_opts) {
            Ok(text) => {
                log.push_str(&text);
                return Ok(log);
            }
            Err(e) if e.starts_with(LOST_RACE) && attempt < attempts => {
                log.push_str(&format!("{e}; re-picking on the new tip\n"));
            }
            Err(e) => return Err(format!("{log}{e}")),
        }
    }
    Err(format!(
        "{log}gave up after {attempts} attempt(s); the claim is still yours"
    ))
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn task(id: &str, deps: &[&str], paths: &[&str]) -> PlanTask {
        PlanTask {
            id: id.into(),
            title: String::new(),
            deps: deps.iter().map(|d| (*d).to_string()).collect(),
            paths: paths.iter().map(|p| normalize(p)).collect(),
            sources: vec!["x".into()],
        }
    }

    #[test]
    fn lanes_split_on_disjoint_paths_and_join_on_overlap_or_dependency() {
        let tasks = vec![
            task("T-A", &[], &["crates/a"]),
            task("T-B", &[], &["crates/b/src"]),
            task("T-C", &[], &["crates/a/src/lib.rs"]),
            task("T-D", &["T-B"], &["docs/d.md"]),
            task("T-E", &[], &["docs/e.md"]),
            task("T-F", &[], &["crates/ab"]),
        ];
        assert_eq!(
            compute_lanes(&tasks),
            vec![vec![0, 2], vec![1, 3], vec![4], vec![5]]
        );
        assert!(overlaps("crates/a", "crates/a/x"));
        assert!(!overlaps("crates/a", "crates/ab"));
        assert!(overlaps("", "anything"));
        let whole = vec![task("T-X", &[], &["."]), task("T-Y", &[], &["y"])];
        assert_eq!(compute_lanes(&whole), vec![vec![0, 1]]);
    }

    #[test]
    fn segments_reject_ref_injection() {
        for bad in ["", "../x", "-x", ".x", "a/b", "a b", "x.lock", "a..b"] {
            assert!(segment(bad, "name").is_err(), "{bad}");
        }
        assert!(segment("2026-11-11-rc4", "name").is_ok());
    }

    struct Fx {
        dir: PathBuf,
        origin: PathBuf,
    }

    impl Fx {
        fn new() -> Self {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("susi-lanes-test-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let origin = dir.join("origin.git");
            Git { root: &dir }
                .text(&[
                    "init",
                    "--quiet",
                    "--bare",
                    "--initial-branch=main",
                    origin.to_str().unwrap(),
                ])
                .unwrap();
            Self { dir, origin }
        }

        fn clone(&self, who: &str) -> PathBuf {
            let path = self.dir.join(who);
            Git { root: &self.dir }
                .text(&[
                    "clone",
                    "--quiet",
                    self.origin.to_str().unwrap(),
                    path.to_str().unwrap(),
                ])
                .unwrap();
            let git = Git { root: &path };
            git.text(&["config", "user.name", who]).unwrap();
            git.text(&["config", "user.email", &format!("{who}@localhost")])
                .unwrap();
            path
        }

        fn commit(&self, work: &Path, files: &[(&str, &str)], message: &str) -> String {
            for (rel, content) in files {
                let path = work.join(rel);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, content).unwrap();
            }
            let git = Git { root: work };
            git.text(&["add", "-A"]).unwrap();
            git.text(&["commit", "--quiet", "-m", message]).unwrap();
            git.text(&["rev-parse", "HEAD"]).unwrap()
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn run_in(work: &Path, args: &[&str]) -> Result<String> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        run(work, &args)
    }

    type Included<'a> = (&'a str, &'a [&'a str], &'a [&'a str], &'a [&'a str]);

    fn receipt(base: &str, included: &[Included<'_>]) -> String {
        json!({
            "schema": RECEIPT_SCHEMA,
            "base_commit": base,
            "closure_order": included.iter().map(|i| i.0).collect::<Vec<_>>(),
            "included": included.iter().map(|(id, deps, scopes, sources)| json!({
                "task_id": id, "title": id, "deps": deps, "scopes": scopes, "source_commits": sources,
            })).collect::<Vec<_>>(),
        })
        .to_string()
    }

    /// Base plus three labelled contributions on main: two independent lanes.
    fn release_fixture(fx: &Fx) -> (PathBuf, PathBuf, String) {
        let dev = fx.clone("dev");
        let base = fx.commit(&dev, &[("a/lib.rs", "a0\n"), ("b/lib.rs", "b0\n")], "base");
        let a1 = fx.commit(&dev, &[("a/lib.rs", "a1\n")], "a one\n\nTask: T-A-1");
        let b1 = fx.commit(&dev, &[("b/lib.rs", "b1\n")], "b one\n\nTask: T-B-1");
        let a2 = fx.commit(&dev, &[("a/more.rs", "a2\n")], "a two\n\nTask: T-A-2");
        Git { root: &dev }
            .text(&["push", "--quiet", "origin", "main"])
            .unwrap();
        let plan = fx.dir.join("plan.json");
        fs::write(
            &plan,
            receipt(
                &base,
                &[
                    ("T-A-1", &[], &["a"], &[a1.as_str()]),
                    ("T-B-1", &[], &["b"], &[b1.as_str()]),
                    ("T-A-2", &["T-A-1"], &["a/more.rs"], &[a2.as_str()]),
                ],
            ),
        )
        .unwrap();
        let alice = fx.clone("alice");
        let bob = fx.clone("bob");
        run_in(
            &alice,
            &["init", "--name", "rc1", "--plan", plan.to_str().unwrap()],
        )
        .unwrap();
        (alice, bob, base)
    }

    fn git_cmd() -> [&'static str; 3] {
        ["--cmd", "git", "--version"]
    }

    #[test]
    fn two_agents_advance_one_release_in_parallel_without_conflicts() {
        let fx = Fx::new();
        let (alice, bob, _) = release_fixture(&fx);
        let st = run_in(&alice, &["status", "--name", "rc1", "--json"]).unwrap();
        let st: Value = serde_json::from_str(&st).unwrap();
        assert_eq!(st["lanes"].as_array().unwrap().len(), 2);
        assert_eq!(st["lanes"][0]["tasks"], json!(["T-A-1", "T-A-2"]));
        assert_eq!(st["lanes"][1]["tasks"], json!(["T-B-1"]));

        run_in(&alice, &["claim", "--name", "rc1", "--lane", "lane-01"]).unwrap();
        let taken = run_in(&bob, &["claim", "--name", "rc1", "--lane", "lane-01"]).unwrap_err();
        assert!(taken.contains("claimed by alice"), "{taken}");
        run_in(&bob, &["claim", "--name", "rc1", "--lane", "lane-02"]).unwrap();
        let foreign = run_in(&bob, &["pick", "--name", "rc1", "--lane", "lane-01"]).unwrap_err();
        assert!(foreign.contains("claimed by alice"), "{foreign}");

        // Both pick from the same tip in parallel.
        run_in(&alice, &["pick", "--name", "rc1", "--lane", "lane-01"]).unwrap();
        run_in(&bob, &["pick", "--name", "rc1", "--lane", "lane-02"]).unwrap();
        let mut verify_a = vec!["verify", "--name", "rc1", "--lane", "lane-01"];
        verify_a.extend(git_cmd());
        run_in(&alice, &verify_a).unwrap();
        let mut verify_b = vec!["verify", "--name", "rc1", "--lane", "lane-02"];
        verify_b.extend(git_cmd());
        run_in(&bob, &verify_b).unwrap();

        run_in(&alice, &["advance", "--name", "rc1", "--lane", "lane-01"]).unwrap();
        // Bob qualified the old tip: the release refuses his stale candidate.
        let race = run_in(&bob, &["advance", "--name", "rc1", "--lane", "lane-02"]).unwrap_err();
        assert!(race.starts_with(LOST_RACE), "{race}");
        // ship re-picks on the new tip, re-qualifies and lands.
        let mut ship_b = vec!["ship", "--name", "rc1", "--lane", "lane-02"];
        ship_b.extend(git_cmd());
        let shipped = run_in(&bob, &ship_b).unwrap();
        assert!(shipped.contains("release complete"), "{shipped}");

        let st = run_in(&alice, &["status", "--name", "rc1", "--json"]).unwrap();
        let st: Value = serde_json::from_str(&st).unwrap();
        assert_eq!(st["complete"], json!(true));
        assert!(st["lanes"][0]["claimed_by"].is_null() && st["lanes"][1]["claimed_by"].is_null());

        let git = Git { root: &alice };
        git.text(&["fetch", "--quiet", "origin", "refs/candidates/release/rc1"])
            .unwrap();
        assert_eq!(git.text(&["show", "FETCH_HEAD:a/lib.rs"]).unwrap(), "a1");
        assert_eq!(git.text(&["show", "FETCH_HEAD:a/more.rs"]).unwrap(), "a2");
        assert_eq!(git.text(&["show", "FETCH_HEAD:b/lib.rs"]).unwrap(), "b1");
        let log = git.text(&["log", "--format=%B", "FETCH_HEAD"]).unwrap();
        assert_eq!(log.matches("(cherry picked from commit ").count(), 3);
        let manifest = read_manifest(&git, "FETCH_HEAD", "rc1").unwrap();
        assert_eq!(manifest["advances"].as_array().unwrap().len(), 2);
        assert_eq!(manifest["lanes"][1]["advance"]["agent"], json!("bob"));
        // History only moved forward.
        let first = manifest["advances"][0]["code_commit"].as_str().unwrap();
        assert!(git.is_ancestor(first, "FETCH_HEAD"));
        // Main was never touched by the release.
        let main = git
            .text(&["ls-remote", "origin", "refs/heads/main"])
            .unwrap();
        assert!(!main.is_empty());
        assert!(git
            .ls_remote("origin", "refs/heads/release/rc1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn evidence_must_match_the_exact_tree_and_pass() {
        let fx = Fx::new();
        let (alice, _, _) = release_fixture(&fx);
        run_in(&alice, &["claim", "--name", "rc1", "--lane", "lane-02"]).unwrap();
        run_in(&alice, &["pick", "--name", "rc1", "--lane", "lane-02"]).unwrap();
        let failing = run_in(
            &alice,
            &[
                "verify",
                "--name",
                "rc1",
                "--lane",
                "lane-02",
                "--cmd",
                "git",
                "no-such-subcommand",
            ],
        )
        .unwrap_err();
        assert!(failing.contains("qualification failed"), "{failing}");
        let refused =
            run_in(&alice, &["advance", "--name", "rc1", "--lane", "lane-02"]).unwrap_err();
        assert!(refused.contains("failed qualification"), "{refused}");

        let mut ok = vec![
            "verify",
            "--name",
            "rc1",
            "--lane",
            "lane-02",
            "--rev",
            "refs/susi/lanes/rc1/lane-02",
        ];
        ok.extend(git_cmd());
        run_in(&alice, &ok).unwrap();
        // A different candidate tree is not covered by that evidence.
        let git = Git { root: &alice };
        let other = git
            .commit_file("refs/susi/lanes/rc1/lane-02", "extra.txt", b"x\n", "extra")
            .unwrap();
        let mismatch = run_in(
            &alice,
            &[
                "advance", "--name", "rc1", "--lane", "lane-02", "--rev", &other,
            ],
        )
        .unwrap_err();
        assert!(
            mismatch.contains("run verify on this exact tree"),
            "{mismatch}"
        );
        run_in(&alice, &["advance", "--name", "rc1", "--lane", "lane-02"]).unwrap();
    }

    #[test]
    fn pick_reports_conflicts_and_resume_skips_what_is_already_picked() {
        let fx = Fx::new();
        let dev = fx.clone("dev");
        let base = fx.commit(&dev, &[("c/lib.rs", "c0\n")], "base");
        // An unselected change sits between base and the selected one.
        fx.commit(
            &dev,
            &[("c/lib.rs", "unselected\n")],
            "unselected\n\nTask: T-U-1",
        );
        let wanted = fx.commit(&dev, &[("c/lib.rs", "wanted\n")], "wanted\n\nTask: T-C-1");
        let clean = fx.commit(&dev, &[("d/lib.rs", "d1\n")], "clean\n\nTask: T-D-1");
        Git { root: &dev }
            .text(&["push", "--quiet", "origin", "main"])
            .unwrap();
        let plan = fx.dir.join("plan.json");
        fs::write(
            &plan,
            receipt(
                &base,
                &[
                    ("T-C-1", &[], &["c"], &[wanted.as_str()]),
                    ("T-D-1", &[], &["d"], &[clean.as_str()]),
                ],
            ),
        )
        .unwrap();
        let agent = fx.clone("carol");
        run_in(
            &agent,
            &["init", "--name", "rc2", "--plan", plan.to_str().unwrap()],
        )
        .unwrap();
        run_in(&agent, &["claim", "--name", "rc2", "--lane", "lane-01"]).unwrap();
        let conflict = run_in(&agent, &["pick", "--name", "rc2", "--lane", "lane-01"]).unwrap_err();
        assert!(conflict.contains("conflicts in: c/lib.rs"), "{conflict}");

        run_in(&agent, &["claim", "--name", "rc2", "--lane", "lane-02"]).unwrap();
        let first = run_in(&agent, &["pick", "--name", "rc2", "--lane", "lane-02"]).unwrap();
        assert!(first.contains("picked 1 commit"), "{first}");
        let again = run_in(
            &agent,
            &[
                "pick",
                "--name",
                "rc2",
                "--lane",
                "lane-02",
                "--resume",
                "refs/susi/lanes/rc2/lane-02",
            ],
        )
        .unwrap();
        assert!(
            again.contains("picked 0 commit") && again.contains("already picked"),
            "{again}"
        );
    }

    #[test]
    fn stale_claims_can_be_taken_over_and_releases_are_create_only() {
        let fx = Fx::new();
        let (alice, bob, _) = release_fixture(&fx);
        run_in(&alice, &["claim", "--name", "rc1", "--lane", "lane-01"]).unwrap();
        let fresh = run_in(
            &bob,
            &[
                "claim",
                "--name",
                "rc1",
                "--lane",
                "lane-01",
                "--stale-secs",
                "3600",
            ],
        )
        .unwrap_err();
        assert!(fresh.contains("claimed by alice"), "{fresh}");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        run_in(
            &bob,
            &[
                "claim",
                "--name",
                "rc1",
                "--lane",
                "lane-01",
                "--stale-secs",
                "0",
            ],
        )
        .unwrap();
        let lost = run_in(&alice, &["pick", "--name", "rc1", "--lane", "lane-01"]).unwrap_err();
        assert!(lost.contains("claimed by bob"), "{lost}");
        run_in(&bob, &["unclaim", "--name", "rc1", "--lane", "lane-01"]).unwrap();

        let plan = fx.dir.join("plan.json");
        let again = run_in(
            &alice,
            &["init", "--name", "rc1", "--plan", plan.to_str().unwrap()],
        )
        .unwrap_err();
        assert!(again.contains("already exists"), "{again}");
    }
}

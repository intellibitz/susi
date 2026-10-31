//! The shared task queue: `.agents/tasks/<id>.json`, one file per task.
//!
//! * **Parallel-safe.** One file per task means adding a task never conflicts,
//!   and IDs are namespaced per agent (`T-CLAUDE-3`), like the evidence ledger.
//! * **Atomic claims through git.** A claim is a blob pushed to
//!   `refs/claims/<id>` on the shared remote. The server accepts the first
//!   push and rejects every later one, so two agents can never both win — no
//!   lock file, no service. Claims carry a lease; an expired claim is taken
//!   over with a compare-and-swap push so a crashed agent cannot hold a task
//!   forever.
//! * **Closed by a check, not a claim.** A task names an acceptance command;
//!   `close` runs it, and only a passing run moves the task to
//!   `.agents/tasks/done/` (the record keeps who, when and at which commit).
use super::queue_relay;
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Remote that carries `refs/claims/*`; override with `SUSI_TASK_REMOTE`.
pub const DEFAULT_REMOTE: &str = "origin";
/// Lease a claim holds by default.
pub const DEFAULT_LEASE_HOURS: u64 = 4;
/// Lease a size-`l` claim holds. Large tasks are integration work — the swarm
/// gap tasks demand nonzero tests through the real CLI/daemon path — and an
/// agent at work does not renew: `renew` runs at the loop's boundaries, and an
/// expired lease is taken over by compare-and-swap, which would let a second
/// agent start the task while the first still holds uncommitted work.
pub const LARGE_TASK_LEASE_HOURS: u64 = 12;

/// The lease a task of this size should start with.
#[must_use]
pub fn lease_hours_for_size(size: &str) -> u64 {
    if size == "l" {
        LARGE_TASK_LEASE_HOURS
    } else {
        DEFAULT_LEASE_HOURS
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Accept {
    /// Argv run from the repo root; exit 0 means the task is done.
    pub cmd: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Closed {
    pub at_unix: u64,
    pub commit: String,
    pub by: String,
}

/// A durable "accepted, not yet on `origin/main`".
///
/// `close` pushes this to `refs/closed/<id>` the moment acceptance passes, so
/// the fact is on the shared remote instead of only in the working tree that
/// happened to run the check. `release` deletes the claim; this outlives it,
/// which is what stops an agent that crashed, gave up, or simply reported
/// success from claiming the next task while the accepted work is unmerged.
/// The receipt is cleared only by *observing* the close on `origin/main`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloseRecord {
    pub task: String,
    pub agent: String,
    /// Branch head whose acceptance check passed.
    pub head: String,
    pub closed_unix: u64,
    /// The files the claim reserved. They stay reserved until the close is on
    /// `origin/main`: `finish` releases the claim as soon as the branch run is
    /// green, before the merge, and another agent editing the same files in
    /// that window met the conflict only at the merge gate, after it had
    /// moved on. Receipts written before this field reserve nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

/// A task given up deliberately, pushed to `refs/abandoned/<id>`: the reason is
/// a remote record, so "we stopped working on this accepted task" is visible
/// rather than an agent quietly dropping it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Abandoned {
    pub task: String,
    pub agent: String,
    pub reason: String,
    pub at_unix: u64,
}

/// The merger's attestation that a task reached `main`, written to
/// `refs/merged/<id>` by the auto-merge job — the one party that observes the
/// merge — right after the pull request lands.
///
/// Unlike a [`CloseRecord`], which the accepting agent writes, this is written
/// on the other side of the merge. It is still *checkable rather than trusted*:
/// [`merged_is_real`] re-derives it from `origin/main`, so a forged ref would
/// have to name a merge commit that is on `main` and actually contains the
/// tested head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergedTask {
    pub task: String,
    /// Branch head whose acceptance passed, and which the merge contains.
    pub head: String,
    /// The merge commit on `main`.
    pub merge: String,
    /// Pull request number, when the merger knew it.
    #[serde(default)]
    pub pr: u64,
    pub merged_unix: u64,
}

/// The re-run of a task's acceptance on the *merged* tree, written to
/// `refs/verified/<id>` by the job that verifies `main`.
///
/// `result` is `passed`, or `skipped:<why>` when the acceptance cannot be
/// re-run off its author's host (a host-specific `scripts/…` checker failing on
/// a bare runner is not evidence about the merge). A failure writes nothing, so
/// the next run retries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedTask {
    pub task: String,
    pub head: String,
    pub merge: String,
    pub result: String,
    pub verified_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub goal: String,
    /// `s` | `m` | `l`.
    #[serde(default = "default_size")]
    pub size: String,
    /// Task ids that must be closed before this one can be claimed.
    #[serde(default)]
    pub deps: Vec<String>,
    pub accept: Accept,
    /// The roadmap vector (`VC-<n>-<n>` in `.agents/roadmap.json`) this task
    /// delivers, so a vector's progress comes from its tasks closing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roadmap: Option<String>,
    pub created_by: String,
    pub created_unix: u64,
    /// True when the backlog's own author path wrote this — `created_by`
    /// says *who*, this says *how*: a human `tasks add` vs the gated
    /// machine-authored path the rate bound applies to.
    #[serde(default, skip_serializing_if = "is_false")]
    pub authored: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<Closed>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn default_size() -> String {
    "m".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    /// Branch that owns this lease; absent only on legacy claim records.
    #[serde(default)]
    pub branch: Option<String>,
    /// Repo-relative files or directories reserved by this task.
    #[serde(default)]
    pub scopes: Vec<String>,
    pub task: String,
    pub agent: String,
    pub claimed_unix: u64,
    pub lease_until_unix: u64,
}

impl Claim {
    pub fn expired(&self, now: u64) -> bool {
        now >= self.lease_until_unix
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn tasks_dir(ws: &Path) -> PathBuf {
    ws.join(".agents").join("tasks")
}

fn done_dir(ws: &Path) -> PathBuf {
    tasks_dir(ws).join("done")
}

fn remote() -> String {
    std::env::var("SUSI_TASK_REMOTE")
        .ok()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REMOTE.into())
}

/// Whether the task queue remote is the default `origin`.
/// When true, [`sync_queue`] already mirrors `+refs/heads/*:refs/remotes/origin/*`,
/// so callers need no separate `git fetch origin`.
#[must_use]
pub fn is_default_remote() -> bool {
    remote() == DEFAULT_REMOTE
}

/// `T-<AGENT>-<n>`: agent is `[A-Z0-9]+`, so ids are safe in paths and refs.
pub fn valid_id(id: &str) -> bool {
    let mut parts = id.splitn(3, '-');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some("T"), Some(agent), Some(n))
            if !agent.is_empty()
                && agent.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
    )
}

fn agent_token(agent: &str) -> EaiResult<String> {
    let t: String = agent
        .trim()
        .to_ascii_uppercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    if t.is_empty() {
        return Err(EaiError::config(
            "agent name must contain letters or digits",
        ));
    }
    Ok(t)
}

/// Acceptance commands run on whoever closes the task, so they are limited to
/// the repo's own build/test entry points — a task file is not a shell.
pub fn accept_allowed(cmd: &[String]) -> bool {
    let Some(program) = cmd.first() else {
        return false;
    };
    let ok_program = program == "cargo"
        || program == "susi"
        || program == "./target/debug/susi"
        || (program.starts_with("scripts/")
            && !program.contains("..")
            && program
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b)));
    ok_program && cmd.iter().all(|a| !a.contains('\0'))
}

fn read_dir_tasks(dir: &Path) -> Vec<Task> {
    let paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    let parse_one =
        |p: &PathBuf| -> Option<Task> { serde_json::from_slice(&std::fs::read(p).ok()?).ok() };
    let mut out: Vec<Task> = if paths.len() < 16 {
        paths.iter().filter_map(parse_one).collect()
    } else {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 8))
            .unwrap_or(4);
        let chunk_size = paths.len().div_ceil(workers);
        std::thread::scope(|scope| {
            let handles: Vec<_> = paths
                .chunks(chunk_size)
                .map(|chunk| {
                    scope.spawn(move || chunk.iter().filter_map(parse_one).collect::<Vec<_>>())
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        })
    };
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn list_open(ws: &Path) -> Vec<Task> {
    read_dir_tasks(&tasks_dir(ws))
}

pub fn list_done(ws: &Path) -> Vec<Task> {
    read_dir_tasks(&done_dir(ws))
}

fn find_open(ws: &Path, id: &str) -> EaiResult<Task> {
    if !valid_id(id) {
        return Err(EaiError::config(format!(
            "invalid task id `{id}` (want T-<AGENT>-<n>)"
        )));
    }
    list_open(ws)
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| EaiError::config(format!("no open task {id}")))
}

fn next_id_from_ids<'a>(ids: impl IntoIterator<Item = &'a str>, agent: &str) -> EaiResult<String> {
    let token = agent_token(agent)?;
    let prefix = format!("T-{token}-");
    let max = ids
        .into_iter()
        .filter_map(|id| id.strip_prefix(&prefix).and_then(|n| n.parse::<u64>().ok()))
        .max()
        .unwrap_or(0);
    Ok(format!("{prefix}{}", max + 1))
}

/// Next `T-<AGENT>-<n>` across open and done tasks.
pub fn next_id(ws: &Path, agent: &str) -> EaiResult<String> {
    let (open, done) = std::thread::scope(|scope| {
        let open = scope.spawn(|| list_open(ws));
        let done = list_done(ws);
        (open.join().unwrap_or_default(), done)
    });
    let all: Vec<String> = open.into_iter().chain(done).map(|t| t.id).collect();
    next_id_from_ids(all.iter().map(String::as_str), agent)
}

/// A task to create; the id, author and timestamp are filled in by [`add`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewTask {
    pub title: String,
    pub goal: String,
    /// `s` | `m` | `l`.
    pub size: String,
    pub deps: Vec<String>,
    /// Acceptance argv; see [`accept_allowed`].
    pub accept: Vec<String>,
    /// Roadmap vector this delivers; must exist in `.agents/roadmap.json`.
    pub roadmap: Option<String>,
}

pub fn add(ws: &Path, agent: &str, new: NewTask) -> EaiResult<Task> {
    add_inner(ws, agent, new, false)
}

fn add_inner(ws: &Path, agent: &str, new: NewTask, authored: bool) -> EaiResult<Task> {
    let NewTask {
        title,
        goal,
        size,
        deps,
        accept: accept_cmd,
        roadmap,
    } = new;
    if let Some(vector) = &roadmap {
        validate_roadmap_link(ws, vector)?;
    }
    let (title, goal, size, deps) = (
        title.as_str(),
        goal.as_str(),
        size.as_str(),
        deps.as_slice(),
    );
    check_task_fields(title, size, &accept_cmd)?;
    let (open, done) = std::thread::scope(|scope| {
        let open = scope.spawn(|| list_open(ws));
        let done = list_done(ws);
        (open.join().unwrap_or_default(), done)
    });
    let known: std::collections::HashSet<String> =
        open.into_iter().chain(done).map(|t| t.id).collect();
    check_task_deps(deps, &known)?;
    let task = Task {
        id: next_id_from_ids(known.iter().map(String::as_str), agent)?,
        title: title.trim().to_string(),
        goal: goal.trim().to_string(),
        size: size.to_string(),
        deps: deps.to_vec(),
        accept: Accept { cmd: accept_cmd },
        roadmap,
        created_by: agent_token(agent)?,
        created_unix: now_unix(),
        authored,
        closed: None,
    };
    std::fs::create_dir_all(tasks_dir(ws))?;
    std::fs::write(
        tasks_dir(ws).join(format!("{}.json", task.id)),
        serde_json::to_vec_pretty(&task)?,
    )?;
    Ok(task)
}

/// The field-level gates every new task meets — title, size, acceptance
/// command — shared by `add` and the author path's batch pre-flight.
fn check_task_fields(title: &str, size: &str, accept_cmd: &[String]) -> EaiResult<()> {
    if title.trim().is_empty() {
        return Err(EaiError::config("task title must not be empty"));
    }
    if !matches!(size, "s" | "m" | "l") {
        return Err(EaiError::config("size must be s, m or l"));
    }
    if !accept_allowed(accept_cmd) {
        return Err(EaiError::config(
            "acceptance command is required and must start with cargo, susi or scripts/<file> \
             (a task file is not a shell)",
        ));
    }
    // The command runs as argv, never through a shell, so a shell-quoted
    // argument reaches the program with its quote characters still attached.
    // That used to be discovered only when `close` ran it — after the whole
    // gate — as an opaque exit code.
    if let Some(quoted) = accept_cmd
        .iter()
        .find(|a| a.contains('\'') || a.contains('"'))
    {
        return Err(EaiError::config(format!(
            "acceptance commands are executed as argv, not through a shell: `{quoted}` still \
             carries its quotes. Drop them — `--accept \"cargo nextest run -E binary(tasks_cli)\"` \
             rather than `-E 'binary(tasks_cli)'`"
        )));
    }
    Ok(())
}

/// Every dep must name a task the queue already knows.
fn check_task_deps(deps: &[String], known: &std::collections::HashSet<String>) -> EaiResult<()> {
    if let Some(bad) = deps.iter().find(|d| !known.contains(*d)) {
        return Err(EaiError::config(format!("unknown dependency {bad}")));
    }
    Ok(())
}

/// `VC-<digits>-<digits>`.
pub fn valid_vector_id(id: &str) -> bool {
    let mut p = id.splitn(3, '-');
    matches!(
        (p.next(), p.next(), p.next()),
        (Some("VC"), Some(a), Some(b))
            if !a.is_empty() && a.bytes().all(|c| c.is_ascii_digit())
                && !b.is_empty() && b.bytes().all(|c| c.is_ascii_digit())
    )
}

/// A roadmap vector as the coverage report needs it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Vector {
    pub id: String,
    pub priority: String,
    pub title: String,
    /// The vector's own status narrative — where it says whether the
    /// capability is delivered ("DELIVERED: …", "PARTIAL: …").
    pub progress: String,
    /// What mastery looks like — the bar a verification task checks.
    #[serde(default)]
    pub mastery_target: String,
}

/// Vectors from `.agents/roadmap.json` (`/vectors`), in file order.
pub fn roadmap_vectors(ws: &Path) -> EaiResult<Vec<Vector>> {
    let text = std::fs::read_to_string(ws.join(".agents").join("roadmap.json"))
        .map_err(|_| EaiError::config("no .agents/roadmap.json in this workspace"))?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    Ok(v["vectors"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Vector {
                        id: x["id"].as_str()?.to_string(),
                        priority: x["priority"].as_str().unwrap_or("-").to_string(),
                        title: x["vector"]
                            .as_str()
                            .or_else(|| x["mastery_target"].as_str())
                            .unwrap_or("")
                            .to_string(),
                        progress: x["progress"].as_str().unwrap_or("").to_string(),
                        mastery_target: x["mastery_target"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

fn validate_roadmap_link(ws: &Path, vector: &str) -> EaiResult<()> {
    if !valid_vector_id(vector) {
        return Err(EaiError::config(format!(
            "invalid roadmap vector `{vector}` (want VC-<n>-<n>)"
        )));
    }
    if !roadmap_vectors(ws)?.iter().any(|v| v.id == vector) {
        return Err(EaiError::config(format!(
            "roadmap vector {vector} does not exist in .agents/roadmap.json"
        )));
    }
    Ok(())
}

/// How many machine-authored tasks one agent may have open at once — a
/// system that can add work but not justify it is a runaway, so the
/// author path is bounded (T-DEEPSEEK-104).
pub const AUTHORED_OPEN_MAX: usize = 5;

/// A machine-authored roadmap vector: the same fields a human-written
/// `.agents/roadmap.json` entry carries, gated the same way.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewVector {
    /// `VC-<n>-<n>`, unique across the roadmap and the batch.
    pub id: String,
    /// `P0` | `P1` | `P2` — the roadmap's own vocabulary.
    pub priority: String,
    /// The one-line capability the vector names.
    pub vector: String,
    /// What mastery looks like — the bar a verification task checks.
    pub mastery_target: String,
    /// The roadmap `type` field (named `vtype` to keep the serde name).
    #[serde(rename = "type")]
    pub vtype: String,
    /// Vector ids this work builds on; each must already be on the
    /// roadmap or be authored earlier in the same batch.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// One authored batch: the rationale that justifies it plus the vectors
/// and tasks it produces. `susi tasks author --file` consumes this.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AuthoredSpec {
    /// Why this work exists — the review line a runaway cannot write.
    /// An empty rationale is refused before any artifact lands.
    pub rationale: String,
    #[serde(default)]
    pub vectors: Vec<NewVector>,
    #[serde(default)]
    pub tasks: Vec<NewTask>,
}

/// What an [`author`] call created, for the report the CLI prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoredReport {
    pub vectors: Vec<String>,
    pub tasks: Vec<String>,
}

/// The narrative every freshly-authored vector starts with — the same
/// "planned and queued" text the hand-written entries carry.
const AUTHORED_PROGRESS: &str =
    "PARTIAL: planned and queued; no implementation yet, and the queue holds the first task that \
     proves or refutes this.";

/// Author backlog artifacts from analysis: vectors into
/// `.agents/roadmap.json` and tasks into `.agents/tasks/`, both through
/// the same gates a human's edits would meet. The batch is checked as a
/// whole before anything lands — a half-written batch is never a state.
///
/// The bound is per author: an agent may hold at most
/// [`AUTHORED_OPEN_MAX`] open machine-authored tasks; the bound does not
/// count tasks a human added by hand.
///
/// # Errors
/// [`EaiError::config`] on an empty rationale, a rate-bound breach, an
/// invalid or duplicate vector, a vector type/priority outside the
/// roadmap's vocabulary, a dependency on an unknown vector or task, or
/// any gate [`add`] applies to a hand-written task. I/O failures surface
/// as [`EaiError::io`].
pub fn author(ws: &Path, agent: &str, spec: &AuthoredSpec) -> EaiResult<AuthoredReport> {
    if spec.rationale.trim().is_empty() {
        return Err(EaiError::config(
            "authored work needs a rationale — a record that cannot justify itself is a runaway",
        ));
    }
    let token = agent_token(agent)?;
    let authored_open = list_open(ws)
        .iter()
        .filter(|t| t.authored && t.created_by == token)
        .count();
    if authored_open + spec.tasks.len() > AUTHORED_OPEN_MAX {
        return Err(EaiError::config(format!(
            "authored backlog bound: {token} already holds {authored_open} open machine-authored \
             tasks; the bound is {AUTHORED_OPEN_MAX}"
        )));
    }

    // Validate every vector before any write — a rejected batch leaves
    // both files untouched.
    let roadmap_path = ws.join(".agents").join("roadmap.json");
    let text = std::fs::read_to_string(&roadmap_path)
        .map_err(|_| EaiError::config("no .agents/roadmap.json in this workspace"))?;
    let mut doc: serde_json::Value = serde_json::from_str(&text)?;
    let existing = roadmap_vectors(ws)?;
    let known: std::collections::BTreeSet<String> = existing.iter().map(|v| v.id.clone()).collect();
    let known_types: std::collections::BTreeSet<String> = doc["vectors"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v["type"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let batch_ids: std::collections::BTreeSet<String> =
        spec.vectors.iter().map(|v| v.id.clone()).collect();
    if batch_ids.len() != spec.vectors.len() {
        return Err(EaiError::config("the batch names one vector id twice"));
    }
    for v in &spec.vectors {
        if !valid_vector_id(&v.id) {
            return Err(EaiError::config(format!(
                "invalid vector id `{}` (want VC-<n>-<n>)",
                v.id
            )));
        }
        if known.contains(&v.id) {
            return Err(EaiError::config(format!(
                "vector {} already exists on the roadmap",
                v.id
            )));
        }
        if v.vector.trim().is_empty() || v.mastery_target.trim().is_empty() {
            return Err(EaiError::config(format!(
                "vector {} needs a name and a mastery target",
                v.id
            )));
        }
        if !matches!(v.priority.as_str(), "P0" | "P1" | "P2") {
            return Err(EaiError::config(format!(
                "vector {} priority must be P0, P1 or P2",
                v.id
            )));
        }
        if !known_types.contains(&v.vtype) {
            return Err(EaiError::config(format!(
                "vector {} type `{}` is not in the roadmap's vocabulary",
                v.id, v.vtype
            )));
        }
        for dep in &v.depends_on {
            if dep == &v.id {
                return Err(EaiError::config(format!(
                    "vector {} depends on itself",
                    v.id
                )));
            }
            if !known.contains(dep) && !batch_ids.contains(dep) {
                return Err(EaiError::config(format!(
                    "vector {} depends on unknown vector {dep}",
                    v.id
                )));
            }
        }
    }

    // The whole batch is checked before anything lands: task gates run
    // against the queue as it is now, with roadmap links resolving against
    // the existing vectors plus this batch's own — a half-written batch is
    // never a state.
    let known_tasks: std::collections::HashSet<String> = list_open(ws)
        .into_iter()
        .chain(list_done(ws))
        .map(|t| t.id)
        .collect();
    for t in &spec.tasks {
        if let Some(r) = &t.roadmap {
            if !valid_vector_id(r) || (!known.contains(r) && !batch_ids.contains(r)) {
                return Err(EaiError::config(format!(
                    "roadmap vector {r} does not exist in .agents/roadmap.json or this batch"
                )));
            }
        }
        check_task_fields(&t.title, &t.size, &t.accept)?;
        check_task_deps(&t.deps, &known_tasks)?;
    }

    if !spec.vectors.is_empty() {
        let arr = doc["vectors"]
            .as_array_mut()
            .ok_or_else(|| EaiError::config("roadmap.json has no vectors array"))?;
        for v in &spec.vectors {
            arr.push(serde_json::json!({
                "depends_on": v.depends_on,
                "id": v.id,
                "mastery_target": v.mastery_target,
                "priority": v.priority,
                "progress": AUTHORED_PROGRESS,
                "type": v.vtype,
                "vector": v.vector,
            }));
        }
        let mut body = serde_json::to_string_pretty(&doc)?;
        body.push('\n');
        crate::susi_config::atomic_write_bytes(&roadmap_path, body.as_bytes())
            .map_err(|e| EaiError::filesystem(format!("roadmap write: {e}")))?;
    }

    // Tasks go through `add` — the identical gates a hand-written task
    // meets — stamped authored so the bound and the review surface see
    // them.
    let mut tasks = Vec::new();
    for new in &spec.tasks {
        tasks.push(add_inner(ws, agent, new.clone(), true)?.id);
    }
    Ok(AuthoredReport {
        vectors: spec.vectors.iter().map(|v| v.id.clone()).collect(),
        tasks,
    })
}

/// Susi's own gap analysis as an authored batch: the coverage audit
/// already knows which vectors claim no mastery and hold no open task —
/// the same finding the board reports as a missing queue item. This
/// drafts a "Verify mastery" task for each through [`author`], so the
/// analysis and the gates are the same code a human's work meets, and
/// the bound still applies.
///
/// When more vectors are unqueued than the remaining authored budget,
/// the batch is *trimmed*, not refused: the bound shapes which rungs
/// land — highest roadmap priority first — rather than making the main
/// use case (a backlog longer than the bound) impossible.
pub fn author_unqueued(ws: &Path, agent: &str) -> EaiResult<AuthoredReport> {
    let vectors = roadmap_vectors(ws)?;
    let token = agent_token(agent)?;
    let authored_open = list_open(ws)
        .iter()
        .filter(|t| t.authored && t.created_by == token)
        .count();
    let remaining = AUTHORED_OPEN_MAX.saturating_sub(authored_open);
    let cov = roadmap_coverage(&vectors, &list_open(ws), &list_done(ws));
    let mut unqueued: Vec<&Coverage> = cov.iter().filter(|c| c.unqueued()).collect();
    unqueued.sort_by(|a, b| {
        a.vector
            .priority
            .cmp(&b.vector.priority)
            .then(a.vector.id.cmp(&b.vector.id))
    });
    let tasks: Vec<NewTask> = unqueued
        .iter()
        .take(remaining)
        .map(|c| NewTask {
            title: format!("Verify mastery: {}", c.vector.title),
            goal: c.vector.mastery_target.clone(),
            size: "m".into(),
            deps: vec![],
            accept: vec![
                "cargo".into(),
                "nextest".into(),
                "run".into(),
                "--locked".into(),
                "-E".into(),
                format!(
                    "test({}_mastery)",
                    c.vector.id.to_ascii_lowercase().replace('-', "_")
                ),
            ],
            roadmap: Some(c.vector.id.clone()),
        })
        .collect();
    author(
        ws,
        agent,
        &AuthoredSpec {
            rationale: "coverage audit: vectors that claim no mastery and hold no open task get \
                        their first verification task"
                .into(),
            vectors: vec![],
            tasks,
        },
    )
}

/// A distinct, reproducible failure a repair task gets drafted for — one
/// per failure *kind*, not one per occurrence (an intent that fails the
/// same way fifty times is one bug, not fifty).
struct FailureSignature {
    /// Stable identity used for dedup and the derived test slug.
    slug: String,
    title: String,
    goal: String,
}

/// Lowercase ASCII alphanumerics; every other run of characters collapses
/// to one `_`; bounded so a pasted goal cannot produce an unusable title.
fn failure_slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
        if out.len() >= 48 {
            break;
        }
    }
    out.trim_matches('_').to_string()
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Refused and failed missions from the durable mission trace — the same
/// append-only record `mission_trace::read_all` survives a restart on.
/// A governance refusal is repair-worthy too: one that recurs may mean
/// the policy is wrong, not just the request.
fn mission_failure_signatures(ws: &Path) -> Vec<FailureSignature> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for t in crate::susi_core::mission_trace::read_all(ws) {
        if t.succeeded() {
            continue;
        }
        let slug = failure_slug(&format!("{}_{}", t.outcome, t.goal));
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue; // same failure kind already represented in this batch
        }
        out.push(FailureSignature {
            slug,
            title: format!(
                "Repair: {} mission \"{}\"",
                t.outcome,
                truncate_chars(&t.goal, 60)
            ),
            goal: format!(
                "Mission `{}` ended {} and must be repaired, or its refusal justified as \
                 correct. Evidence: crates/susi-core/src/mission_trace.rs (mission_id={}).",
                t.goal, t.outcome, t.mission_id
            ),
        });
    }
    out
}

/// Susi's own failure analysis as an authored batch: a mission that did
/// not succeed — today's recorded source is the durable mission trace;
/// audit-log denials and roadmap-verdict drift are not read yet, see the
/// module-level note — drafts its own repair task through [`author`],
/// under the same gates and the same per-author bound as a human's
/// `tasks add`. A failure already named by an open or done task's title
/// is not re-authored, so a recurring failure stays one task, never one
/// per occurrence.
///
/// # Errors
/// As [`author`]: an over-bound batch is refused whole.
pub fn author_from_failures(ws: &Path, agent: &str) -> EaiResult<AuthoredReport> {
    let existing: std::collections::BTreeSet<String> = list_open(ws)
        .into_iter()
        .chain(list_done(ws))
        .map(|t| t.title)
        .collect();

    let tasks: Vec<NewTask> = mission_failure_signatures(ws)
        .into_iter()
        .filter(|f| !existing.contains(&f.title))
        .map(|f| NewTask {
            title: f.title,
            goal: f.goal,
            size: "m".into(),
            deps: vec![],
            accept: vec![
                "cargo".into(),
                "nextest".into(),
                "run".into(),
                "--locked".into(),
                "-E".into(),
                format!("test({}_repaired)", f.slug),
            ],
            roadmap: None,
        })
        .collect();

    author(
        ws,
        agent,
        &AuthoredSpec {
            rationale: "failure analysis: a mission that did not succeed drafts its own repair \
                        task, deduplicated by failure kind"
                .into(),
            vectors: vec![],
            tasks,
        },
    )
}

/// One vector's coverage by tasks.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Coverage {
    pub vector: Vector,
    pub open: Vec<String>,
    pub closed: Vec<String>,
}

impl Coverage {
    pub fn uncovered(&self) -> bool {
        self.open.is_empty() && self.closed.is_empty()
    }
    /// Every linked task is closed (and there is at least one). This is
    /// *coverage*, not mastery: it says the vector was worked on and the work
    /// was closed, which is how all 102 vectors read as delivered on 2026-10-02
    /// while a source review still found open gaps in ten of them.
    pub fn delivered(&self) -> bool {
        self.open.is_empty() && !self.closed.is_empty()
    }
    /// The vector's own narrative claims the capability is delivered.
    pub fn mastery_claimed(&self) -> bool {
        self.vector.progress.trim_start().starts_with("DELIVERED")
    }
    /// Not claimed delivered and nothing queued to change that: the work the
    /// queue would lose once the tasks already in it close.
    pub fn unqueued(&self) -> bool {
        !self.mastery_claimed() && self.open.is_empty()
    }
}

/// Join vectors with the tasks that name them.
pub fn roadmap_coverage(vectors: &[Vector], open: &[Task], done: &[Task]) -> Vec<Coverage> {
    let ids = |tasks: &[Task], v: &Vector| -> Vec<String> {
        tasks
            .iter()
            .filter(|t| t.roadmap.as_deref() == Some(v.id.as_str()))
            .map(|t| t.id.clone())
            .collect()
    };
    vectors
        .iter()
        .map(|v| Coverage {
            vector: v.clone(),
            open: ids(open, v),
            closed: ids(done, v),
        })
        .collect()
}

pub(super) fn git_bin() -> &'static Path {
    static GIT_BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    GIT_BIN.get_or_init(|| {
        #[cfg(windows)]
        if let Some(root) = cached_git_install_root() {
            for mingw in ["ucrt64", "mingw64", "clang64", "clangarm64", "mingw32"] {
                let candidate = root.join(mingw).join("bin").join("git.exe");
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        PathBuf::from("git")
    })
}

fn git(ws: &Path, args: &[&str]) -> EaiResult<String> {
    let out = Command::new(git_bin())
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

/// Feed one plumbing command while draining its output. This keeps large batch
/// reads from deadlocking on pipe capacity and shares the native Git path.
fn git_input(ws: &Path, args: &[&str], input: Vec<u8>) -> EaiResult<Vec<u8>> {
    let mut child = Command::new(git_bin())
        .args(args)
        .current_dir(ws)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| EaiError::process("git stdin"))?;
    let feeder = std::thread::spawn(move || {
        use std::io::Write;
        stdin.write_all(&input)
    });
    let out = child.wait_with_output()?;
    let fed = feeder
        .join()
        .map_err(|_| EaiError::process("git input writer panicked"))?;
    if !out.status.success() {
        return Err(EaiError::process(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    fed?;
    Ok(out.stdout)
}

/// Apply a fetched mirror transaction without overwriting a newer concurrent
/// fetch. Remote writes remain the queue relay's authoritative atomic push.
fn mirror_updates(
    ws: &Path,
    leases: &[queue_relay::Lease],
    updates: &[queue_relay::Update],
) -> EaiResult<()> {
    let mut input = String::from("start\n");
    for (name, value) in updates {
        let old = leases
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, old)| old.as_str())
            .ok_or_else(|| EaiError::config("mirror update has no expected ref"))?;
        match (value, old.is_empty()) {
            (Some(new), true) => input.push_str(&format!("create {name} {new}\n")),
            (Some(new), false) => input.push_str(&format!("update {name} {new} {old}\n")),
            (None, false) => input.push_str(&format!("delete {name} {old}\n")),
            (None, true) => {}
        }
    }
    input.push_str("prepare\ncommit\n");
    git_input(ws, &["update-ref", "--stdin"], input.into_bytes()).map(|_| ())
}

fn symbolic_head_branch(ws: &Path) -> Option<String> {
    let dot_git = ws.join(".git");
    if dot_git.is_dir() {
        if let Ok(head) = std::fs::read_to_string(dot_git.join("HEAD")) {
            let trimmed = head.trim();
            if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
                return Some(branch.to_string());
            }
            return None;
        }
    }
    git(ws, &["symbolic-ref", "-q", "--short", "HEAD"]).ok()
}

fn claim_ref(id: &str) -> String {
    format!("refs/claims/{id}")
}

/// The close receipt for `id`; see [`CloseRecord`].
fn closed_ref(id: &str) -> String {
    format!("refs/closed/{id}")
}

/// The record that `id` was given up deliberately; see [`Abandoned`].
fn abandoned_ref(id: &str) -> String {
    format!("refs/abandoned/{id}")
}

/// The merger's attestation that `id`'s tested head reached `main` lives on
/// `refs/merged/<id>`; see [`MergedTask`]. Rust only ever reads it — the
/// auto-merge job (`scripts/auto-merge-pr.sh`) is its only writer — so there is
/// no constructor here on purpose.
const MERGED_NAMESPACE: &str = "refs/merged/";

/// The record that `id`'s acceptance was re-run on the merged tree; see
/// [`VerifiedTask`].
fn verified_ref(id: &str) -> String {
    format!("refs/verified/{id}")
}

/// Mirror the remote's queue refs — claims, close receipts, abandonments,
/// merge attestations and their verifications — into local refs, dropping
/// whatever the remote no longer has: a released claim must not linger in
/// other clones and make a free task look taken, and a published receipt must
/// not linger either, or an agent would be blocked by work that already merged.
///
/// They are fetched together because every boundary that reads claims also
/// needs the debt they leave behind: a *released* claim must not hide the
/// accepted-but-unmerged work, and an owed merge must not hide the merge that
/// already landed.
fn sync_queue_refs(ws: &Path) -> EaiResult<()> {
    let r = remote();
    let heads = format!("+refs/heads/*:refs/remotes/{r}/*");
    git(
        ws,
        &[
            "fetch",
            "--quiet",
            "--prune",
            &r,
            &heads,
            "+refs/claims/*:refs/claims/*",
            "+refs/claim-agents/*:refs/claim-agents/*",
            "+refs/claim-generation/*:refs/claim-generation/*",
            "+refs/closed/*:refs/closed/*",
            "+refs/abandoned/*:refs/abandoned/*",
            "+refs/merged/*:refs/merged/*",
            "+refs/verified/*:refs/verified/*",
        ],
    )
    .map(|_| ())
}

/// Mirror the queue refs once, so a caller that goes on to read several of them
/// pays for one remote round-trip instead of one per reader. Every `*_synced`
/// reader below assumes this just ran; the plain readers (`claims`,
/// `unpublished_closes`, ...) still sync for themselves.
pub fn sync_queue(ws: &Path) -> EaiResult<()> {
    sync_queue_refs(ws)
}

/// Generic so claim blobs, close receipts, abandonments, merge attestations and
/// their verifications share one writer.
fn blob_of<T: Serialize>(ws: &Path, value: &T) -> EaiResult<String> {
    let body = serde_json::to_string(value)?;
    let mut child = Command::new(git_bin())
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(ws)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .ok_or_else(|| EaiError::process("git hash-object stdin"))?
            .write_all(body.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(EaiError::process("git hash-object failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Read one record ref, or `None` when the remote has none (or it is
/// unreadable — a corrupt record is not a reason to crash the queue).
/// Every record under `namespace`, keyed by ref name, in two git calls
/// whatever their number: the refs and their objects from `for-each-ref`,
/// then every body from one `cat-file --batch`. Reading records one by one
/// cost two process spawns each, and a spawn is slow (about 150 ms on
/// Windows): with 67 close receipts on the remote that was ten seconds of
/// every `workflow check`, paid by every agent at the start of every task.
fn records_batch<T: serde::de::DeserializeOwned>(
    ws: &Path,
    namespace: &str,
) -> EaiResult<Vec<(String, T)>> {
    let listed = git(
        ws,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            namespace,
        ],
    )?;
    let refs = listed
        .lines()
        .filter_map(|line| line.split_once(' '))
        .collect::<Vec<_>>();
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let input = refs
        .iter()
        .map(|(oid, _)| format!("{oid}\n"))
        .collect::<String>();
    let bytes = git_input(ws, &["cat-file", "--batch"], input.into_bytes())?;
    Ok(parse_batch(&bytes, &refs))
}

/// The bodies `git cat-file --batch` printed for `refs`, in order: per object
/// a `<oid> <type> <size>` line, the body, and a newline; `<oid> missing` for
/// an object that is not there. A body that is not the record's JSON is
/// skipped, as [`read_record`] skips it.
fn parse_batch<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    refs: &[(&str, &str)],
) -> Vec<(String, T)> {
    let mut out = Vec::new();
    let mut at = 0;
    for (_, name) in refs {
        let Some(line_end) = bytes
            .get(at..)
            .and_then(|rest| rest.iter().position(|b| *b == b'\n'))
        else {
            break;
        };
        let header = String::from_utf8_lossy(&bytes[at..at + line_end]).to_string();
        at += line_end + 1;
        let mut fields = header.split(' ');
        let _oid = fields.next();
        let kind = fields.next().unwrap_or("missing");
        if kind == "missing" {
            continue;
        }
        let Some(size) = fields.next().and_then(|s| s.parse::<usize>().ok()) else {
            break;
        };
        let Some(body) = bytes.get(at..at + size) else {
            break;
        };
        at += size + 1;
        if let Ok(record) = serde_json::from_slice::<T>(body) {
            out.push(((*name).to_string(), record));
        }
    }
    out
}

fn read_record<T: serde::de::DeserializeOwned>(ws: &Path, name: &str) -> EaiResult<Option<T>> {
    let Ok(body) = git(ws, &["cat-file", "-p", name]) else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&body).ok())
}

/// Write one record to one ref, compare-and-swap so a writer that got there
/// first is refused instead of silently overwritten. `raced` is what the caller
/// says when it loses that race.
fn put_record<T: Serialize>(ws: &Path, name: &str, value: &T, raced: &str) -> EaiResult<()> {
    let blob = blob_of(ws, value)?;
    let r = remote();
    let listed = git(ws, &["ls-remote", &r, name])?;
    let old = listed.split_whitespace().next().unwrap_or("");
    queue_relay::push(
        ws,
        &r,
        &[(name.to_string(), old.to_string())],
        &[(name.to_string(), Some(blob.clone()))],
        raced,
    )?;
    git(ws, &["update-ref", name, &blob])?;
    Ok(())
}

/// Drop a record ref on the remote and locally. Compare-and-swap: a record
/// re-pushed since we read it belongs to a newer fact and must survive.
fn clear_record(ws: &Path, name: &str) -> EaiResult<()> {
    clear_records(ws, &[name.to_string()])
}

fn clear_records(ws: &Path, names: &[String]) -> EaiResult<()> {
    if names.is_empty() {
        return Ok(());
    }
    let r = remote();
    let mut args = vec!["ls-remote", &r];
    for name in names {
        args.push(name.as_str());
    }
    let listed = git(ws, &args)?;
    let mut leases = Vec::new();
    let mut updates = Vec::new();
    for line in listed.lines() {
        let mut parts = line.split_whitespace();
        let Some(old) = parts.next() else { continue };
        let Some(refname) = parts.next() else {
            continue;
        };
        if !old.is_empty() && names.iter().any(|n| n == refname) {
            leases.push((refname.to_string(), old.to_string()));
            updates.push((refname.to_string(), None));
        }
    }
    if !updates.is_empty() {
        let first = &names[0];
        queue_relay::push(
            ws,
            &r,
            &leases,
            &updates,
            &format!("{first} was rewritten on {r} since it was read; refresh the queue and retry"),
        )?;
    }
    // Update the mirror through Git too: loose-file deletion bypasses ref
    // locking and can delete a newer ref installed by a concurrent fetch.
    let _ = mirror_updates(ws, &leases, &updates);
    Ok(())
}

fn read_claim(ws: &Path, id: &str) -> Option<(String, Claim)> {
    let refname = claim_ref(id);
    let bytes = git_input(
        ws,
        &["cat-file", "--batch"],
        format!("{refname}\n").into_bytes(),
    )
    .ok()?;
    let line_end = bytes.iter().position(|b| *b == b'\n')?;
    let header = std::str::from_utf8(&bytes[..line_end]).ok()?;
    let sha = header.split_whitespace().next()?.to_owned();
    let (_, claim) = parse_batch::<Claim>(&bytes, &[("", refname.as_str())]).pop()?;
    Some((sha, claim))
}

/// The close receipt for `id`, if the remote has one.
fn read_closed(ws: &Path, id: &str) -> EaiResult<Option<CloseRecord>> {
    read_record(ws, &closed_ref(id))
}

/// Whether the close of `id` is on `origin/main` — the only proof that the
/// accepted work is actually done. An unreachable origin is an error, never a
/// silent "published": the caller must not treat an unknown as a merge.
fn published_on_main(ws: &Path, id: &str) -> EaiResult<bool> {
    fetch_origin(ws)?;
    Ok(done_on_main(ws, id))
}

/// Every readable close receipt on the queue, with the id it was filed under.
///
/// One `cat-file --batch` reads them all. Reading each record with its own
/// `rev-parse` and `cat-file` cost two git processes per receipt, and on Windows
/// process start-up is most of that cost. Unreadable records are skipped, as
/// [`read_record`] skips them.
fn closed_receipts(ws: &Path) -> EaiResult<Vec<(String, CloseRecord)>> {
    Ok(records_batch::<CloseRecord>(ws, "refs/closed/")?
        .into_iter()
        .filter_map(|(name, record)| Some((name.strip_prefix("refs/closed/")?.to_string(), record)))
        .collect())
}

/// Refresh `origin` once for a loop that asks [`done_on_main`] per record.
///
/// Fetching per record cost one remote round-trip (seconds over SSH) for every
/// accepted close on the queue: `workflow check` took about three minutes with
/// sixty-odd receipts outstanding. The loop fetches here and reads the local
/// `origin/main` for each record.
fn fetch_origin(ws: &Path) -> EaiResult<()> {
    git(ws, &["fetch", "--quiet", "origin"]).map(|_| ())
}

/// Whether the cargo target in `ws` is one `scripts/seed-target.sh` fills: a
/// link (a junction on Windows) to the clone's old shared one, which a
/// worktree whose session began before seeding existed may have, and where a
/// sibling's build of a crate can stand in for this branch's; or a target that
/// is missing or holds nothing a build wrote (empty, or cargo's tag files
/// only), which is what a worktree just made has while a hook's background
/// copy of the snapshot fills it. A target that holds a build is this
/// worktree's own.
fn target_wants_seed(ws: &Path) -> bool {
    let target = ws.join("target");
    let Ok(meta) = target.symlink_metadata() else {
        return true;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    meta.is_dir()
        && std::fs::read_dir(&target).is_ok_and(|entries| {
            entries.filter_map(Result::ok).all(|entry| {
                matches!(
                    entry.file_name().to_str(),
                    Some("CACHEDIR.TAG" | ".rustc_info.json")
                )
            })
        })
}

/// Give the worktree at `ws` a cargo target of its own before a cargo
/// acceptance builds, when [`target_wants_seed`]: its
/// `scripts/seed-target.sh`, in the foreground, removes a link (the link
/// only), waits for a background copy that is running, or copies the warm
/// snapshot in. Without a wait, the acceptance would build cold beside the
/// copy, and the finished copy would be thrown away. A target that holds a
/// build, a checkout without the script, or a host with no shell to run it is
/// left as it is: the script never fails, and neither does this. The script
/// runs with this process's environment, as the acceptance does: with
/// `CARGO_TARGET_DIR` naming another directory, where the acceptance builds,
/// it copies nothing (and still replaces a link).
fn seed_own_target(ws: &Path) {
    let script = ws.join("scripts").join("seed-target.sh");
    if !script.is_file() || !target_wants_seed(ws) {
        return;
    }
    let mut command = posix_shell_command("bash");
    // No Git shell found: a bare `bash` there is WSL's, which cannot run a
    // script by its Windows path.
    #[cfg(windows)]
    if !Path::new(command.get_program()).is_absolute() {
        return;
    }
    // Its report goes to stderr, with the acceptance's own.
    let _ = command
        .arg(&script)
        .arg(ws)
        .current_dir(ws)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status();
}

/// For one release: the command that runs a cargo acceptance through the
/// branch's own `scripts/target-guard.sh --run -- <argv>`, as v0.21.23 ran
/// every one, in a worktree whose branch was cut before the seeding and has
/// not merged `origin/main` since. Such a branch has the real guard and no
/// `scripts/seed-target.sh`, and its `target/` is still the clone's shared
/// one, where a sibling's build of a crate can stand in for this branch's: the
/// guard drops exactly those builds and holds the target until the acceptance
/// ends. `None` for a branch that carries the seeding (it builds into a target
/// of its own, and its guard is a pass-through), for a checkout with no guard,
/// and where no Git shell can run one (the guard was an aid, never a gate).
/// Remove once no branch predates the seeding.
fn guarded_acceptance(ws: &Path, argv: &[String]) -> Option<Command> {
    if ws.join("scripts").join("seed-target.sh").is_file() {
        return None;
    }
    let script = ws.join("scripts").join("target-guard.sh");
    if !script.is_file() {
        return None;
    }
    let mut command = posix_shell_command("bash");
    // No Git shell found: a bare `bash` there is WSL's, which cannot run a
    // script by its Windows path.
    #[cfg(windows)]
    if !Path::new(command.get_program()).is_absolute() {
        return None;
    }
    command.arg(&script).arg("--run").arg("--").args(argv);
    Some(command)
}

/// The POSIX shell to run a repo script with: `name` (`sh` or `bash`).
///
/// Elsewhere that is just the name. On Windows a bare name is a trap: process
/// creation searches the system directories before PATH, so `bash` finds
/// WSL's `System32\bash.exe` (or the WindowsApps stub), which cannot run a
/// script given as a Windows path. So Git for Windows' own shell is used:
/// `git --exec-path` is `<git>/<mingw64|ucrt64>/libexec/git-core`, and the
/// shells are in `<git>/bin` and `<git>/usr/bin`, three levels up. A standard
/// Git install puts only `<git>/cmd` on PATH, so outside Git Bash
/// (PowerShell, the desktop app) no shell is on PATH at all.
///
/// That install comes first, for every name, and the PATH (skipping those
/// stubs) only after it: each MSYS runtime keeps its own process table, and
/// a host with two Git installs (Program Files' and scoop's, whose shim put
/// `sh` on PATH) ran `finish` under one and `tasks close` under the other,
/// so a lock one took (the retired target guard's) read the other's live
/// holder as dead. The Git that `git` is also runs the hooks a push starts.
/// Run the shell through [`posix_shell_command`], which also gives it the
/// POSIX userland.
#[must_use]
pub fn posix_shell(name: &str) -> std::path::PathBuf {
    posix_shell_on(name, std::env::var_os("PATH").as_deref())
}

/// A shell that cannot run a script by its Windows path: WSL's System32
/// `bash.exe` and the WindowsApps stub.
#[cfg(windows)]
fn is_shell_stub(path: &Path) -> bool {
    let lowered = path.to_string_lossy().to_ascii_lowercase();
    lowered.contains("system32") || lowered.contains("windowsapps")
}

/// [`posix_shell`] for a given PATH (the child's, which a test can narrow).
fn posix_shell_on(name: &str, path: Option<&std::ffi::OsStr>) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let exe = format!("{name}.exe");
        if let Some(root) = git_install_root(path) {
            // `bin/` first: those are Git's launchers, which set MSYSTEM and
            // put usr/bin and the mingw bin on PATH. usr/bin's shell is the
            // raw MSYS one; started from PowerShell it found no mkdir, sed or
            // dirname, so repo scripts silently did nothing and `workflow
            // sync` stopped at its first `dirname`.
            for dir in ["bin", "usr/bin"] {
                let candidate = root.join(dir).join(&exe);
                if candidate.is_file() && !is_shell_stub(&candidate) {
                    return candidate;
                }
            }
        }
        if let Some(path) = path {
            for dir in std::env::split_paths(path) {
                if is_shell_stub(&dir) {
                    continue;
                }
                let candidate = dir.join(&exe);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = path;
    std::path::PathBuf::from(name)
}

/// The root of the Git install that `git` (on `path`) runs from: three levels
/// above its `--exec-path`.
#[cfg(windows)]
fn cached_git_install_root() -> Option<std::path::PathBuf> {
    static ROOT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| git_install_root_uncached(std::env::var_os("PATH").as_deref()))
        .clone()
}

#[cfg(windows)]
fn git_install_root(path: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    if path == std::env::var_os("PATH").as_deref() {
        return cached_git_install_root();
    }
    git_install_root_uncached(path)
}

#[cfg(windows)]
fn git_install_root_uncached(path: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    let mut git = Command::new("git");
    if let Some(path) = path {
        git.env("PATH", path);
    }
    let out = git.arg("--exec-path").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let exec_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Path::new(&exec_path)
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
}

/// A command running [`posix_shell`] `name` with the POSIX userland on its
/// PATH, so a repo script finds mkdir, sed, find and dirname.
///
/// Git's `bin/` launchers set that PATH themselves. A raw MSYS shell
/// (`<git>/usr/bin/sh.exe`: the last resort, and what a PATH holding usr/bin
/// yields) does not, so its own directory and Git's mingw `bin` go first on
/// the child's PATH. Started outside Git Bash it otherwise ran scripts with
/// only `<git>/cmd` and System32 on PATH, where `find` is Windows' own.
#[must_use]
pub fn posix_shell_command(name: &str) -> Command {
    posix_shell_command_on(name, std::env::var_os("PATH"))
}

/// [`posix_shell_command`] for a given PATH (the child's, which a test can
/// narrow to what a standard Git install leaves outside Git Bash).
fn posix_shell_command_on(name: &str, path: Option<std::ffi::OsString>) -> Command {
    let shell = posix_shell_on(name, path.as_deref());
    let mut command = Command::new(&shell);
    #[cfg(windows)]
    let path = {
        let mut dirs = msys_userland(&shell);
        if dirs.is_empty() {
            path
        } else {
            if let Some(path) = &path {
                dirs.extend(std::env::split_paths(path));
            }
            std::env::join_paths(dirs).ok().or(path)
        }
    };
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command
}

/// The directories a raw MSYS shell (`<root>/usr/bin/<shell>.exe`) needs at
/// the front of PATH: Git's mingw `bin` (its git and curl) and `usr/bin` (the
/// coreutils), in the order Git's launchers put them. Empty for any other
/// shell, a launcher included.
#[cfg(windows)]
fn msys_userland(shell: &Path) -> Vec<std::path::PathBuf> {
    let named = |dir: &Path, name: &str| {
        dir.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
    };
    let Some(bin) = shell.parent().filter(|d| named(d, "bin")) else {
        return Vec::new();
    };
    let Some(root) = bin
        .parent()
        .filter(|d| named(d, "usr"))
        .and_then(Path::parent)
    else {
        return Vec::new();
    };
    let mut dirs: Vec<std::path::PathBuf> =
        ["ucrt64", "mingw64", "clang64", "clangarm64", "mingw32"]
            .iter()
            .map(|mingw| root.join(mingw).join("bin"))
            .filter(|dir| dir.is_dir())
            .collect();
    dirs.push(bin.to_path_buf());
    dirs
}

/// Whether an unobserved close is still *owed*. A receipt reports debt while
/// a merge could still publish something: a missing done file whose task is
/// still open, or a tested head that never reached `origin/main`. A receipt
/// whose head is already on `origin/main` for a task gone from the queue
/// paid what it can pay — the work shipped, no close can ever be observed,
/// and reporting it forever trains the swarm to ignore a red board. The
/// record itself is kept — settled is not deleted — so `release` still
/// refuses to drop it silently.
fn receipt_owed(ws: &Path, record: &CloseRecord) -> bool {
    !done_on_main(ws, &record.task) && receipt_owed_unfinished(ws, record)
}

fn receipt_owed_unfinished(ws: &Path, record: &CloseRecord) -> bool {
    !(head_on_main(ws, record) && !open_on_main(ws, &record.task))
}

/// The receipt's own promise is "until this head is on `origin/main`": the
/// tested head contained there is publication of the work itself. A head
/// that resolves nowhere is no evidence.
fn head_on_main(ws: &Path, record: &CloseRecord) -> bool {
    git(
        ws,
        &["merge-base", "--is-ancestor", &record.head, "origin/main"],
    )
    .is_ok()
}

/// [`done_on_main`] for the open side: whether the task is still queued.
fn open_on_main(ws: &Path, id: &str) -> bool {
    git(
        ws,
        &[
            "cat-file",
            "-e",
            &format!("origin/main:.agents/tasks/{id}.json"),
        ],
    )
    .is_ok()
}

/// When the done record on `origin/main` says `id` was closed, if it is there.
fn closed_on_main_at(ws: &Path, id: &str) -> Option<u64> {
    let body = git(
        ws,
        &["show", &format!("origin/main:.agents/tasks/done/{id}.json")],
    )
    .ok()?;
    let record: serde_json::Value = serde_json::from_str(&body).ok()?;
    record.get("closed")?.get("at_unix")?.as_u64()
}

/// [`published_on_main`] against the `origin/main` this clone already has.
fn done_on_main(ws: &Path, id: &str) -> bool {
    git(
        ws,
        &[
            "cat-file",
            "-e",
            &format!("origin/main:.agents/tasks/done/{id}.json"),
        ],
    )
    .is_ok()
}

/// Publish `refs/closed/<id>` before the task record moves: from here on the
/// task is accepted, and nothing may claim it — or let its agent claim anything
/// else — until this head is on `origin/main`. Pushing first means a failed push
/// leaves the task open and `close` is simply retried.
fn push_close_receipt(ws: &Path, id: &str, owner_sha: &str, claim: &Claim) -> EaiResult<String> {
    let head = git(ws, &["rev-parse", "HEAD"])?;
    let record = CloseRecord {
        task: id.to_string(),
        agent: claim.agent.clone(),
        head: head.clone(),
        closed_unix: now_unix(),
        scopes: claim.scopes.clone(),
    };
    let receipt_ref = closed_ref(id);
    let agent_ref = format!("refs/claim-agents/{}", claim.agent);
    // These refs came from close's queue snapshot. Stale expected values are
    // refused by the atomic push, never overwritten after a second lookup.
    let local_sha =
        |name: &str| git(ws, &["rev-parse", "--verify", "--quiet", name]).unwrap_or_default();
    let agent_old = local_sha(&agent_ref);
    if !agent_old.is_empty() && agent_old != owner_sha {
        return Err(EaiError::config("agent ownership changed before close"));
    }
    let mut retained = claim.clone();
    // Change the claim blob so Git sends an actual ref update: a no-op push
    // would not protect it from a reassignment after ref advertisement.
    retained.lease_until_unix = retained
        .lease_until_unix
        .checked_add(1)
        .ok_or_else(|| EaiError::config("claim lease cannot be retained"))?;
    let receipt_blob = blob_of(ws, &record)?;
    let claim_blob = claim_blob(ws, &retained)?;
    let updates = vec![
        (receipt_ref.clone(), Some(receipt_blob)),
        (claim_ref(id), Some(claim_blob.clone())),
        (agent_ref.clone(), Some(claim_blob)),
    ];
    let leases = vec![
        (receipt_ref.clone(), local_sha(&receipt_ref)),
        (claim_ref(id), owner_sha.to_string()),
        (agent_ref, agent_old),
    ];
    queue_relay::push(
        ws,
        &remote(),
        &leases,
        &updates,
        &format!(
            "{id}'s ownership or receipt changed during acceptance; refresh the queue and re-run the acceptance check"
        ),
    )?;
    if mirror_updates(ws, &leases, &updates).is_err() {
        // A concurrent fetch may already have installed this transaction or a
        // newer one. Refresh instead of overwriting its refs individually.
        sync_queue_refs(ws)?;
    }
    Ok(head)
}

/// Record that an accepted task was given up, on the shared remote.
fn push_abandoned(ws: &Path, id: &str, agent: &str, reason: &str) -> EaiResult<()> {
    let record = Abandoned {
        task: id.to_string(),
        agent: agent.to_string(),
        reason: reason.trim().to_string(),
        at_unix: now_unix(),
    };
    put_record(
        ws,
        &abandoned_ref(id),
        &record,
        &format!("{id} was abandoned concurrently; refresh the queue and retry"),
    )
}

/// A commit for a message: short when it looks like a sha.
fn short_head(head: &str) -> &str {
    head.get(..8).unwrap_or(head)
}

/// Committed work for `id` that no merge has published: a commit on this branch
/// whose message carries the task's trailer. An unreachable or missing
/// `origin/main` answers "no" — this is a guard against walking away silently,
/// not a freshness gate, and `ensure_synced` is what refuses a stale base.
fn has_unpublished_work(ws: &Path, id: &str) -> bool {
    let trailer = format!("Task: {id}");
    git(ws, &["log", "--format=%B", "origin/main..HEAD"])
        .map(|log| log.lines().any(|line| line.trim() == trailer))
        .unwrap_or(false)
}

/// Whether `agent` accepted `id` and the close is still not on `origin/main`:
/// the "keep waiting on a merge" state, in which an expired lease may be
/// re-adopted instead of taken over. Requires a synchronized receipt ref.
fn accepted_unpublished(ws: &Path, id: &str, agent: &str) -> EaiResult<bool> {
    match read_closed(ws, id)? {
        Some(record) if record.agent == agent => {
            Ok(!published_on_main(ws, id)? && receipt_owed(ws, &record))
        }
        _ => Ok(false),
    }
}

/// Accepted tasks this agent has not published, clearing the receipts whose
/// close has since appeared on `origin/main`. The debt ends exactly where the
/// merge is observed, so no separate cleanup can be forgotten.
///
/// `except` is the task being (re)claimed: re-adopting your *own* accepted
/// task is how `finish` keeps waiting after a lease lapsed, and that must not
/// be read as "start something new" — the debt still blocks every other task.
///
/// The caller must have synchronized first ([`sync_queue_refs`], which [`claims`]
/// does): the local receipt refs are what this reads.
#[cfg(test)]
fn owed_closes(ws: &Path, agent: &str, except: Option<&str>) -> EaiResult<Vec<String>> {
    let receipts = closed_receipts(ws)?;
    owed_closes_from_receipts(ws, &receipts, agent, except, &mut |ws| fetch_origin(ws))
}

fn main_task_paths(ws: &Path) -> Option<std::collections::HashSet<String>> {
    let listed = git(
        ws,
        &[
            "ls-tree",
            "--name-only",
            "-r",
            "origin/main",
            ".agents/tasks",
        ],
    )
    .ok()?;
    Some(listed.lines().map(str::to_string).collect())
}

fn owed_closes_from_receipts(
    ws: &Path,
    all_receipts: &[(String, CloseRecord)],
    agent: &str,
    except: Option<&str>,
    ensure_fetched: &mut dyn FnMut(&Path) -> EaiResult<()>,
) -> EaiResult<Vec<String>> {
    let mine: Vec<&(String, CloseRecord)> = all_receipts
        .iter()
        .filter(|(id, record)| except != Some(id.as_str()) && record.agent == agent)
        .collect();
    if mine.is_empty() {
        return Ok(Vec::new());
    }
    // One fetch for every receipt, not one each, and none when this agent has
    // nothing outstanding: the swarm's other receipts (dozens) are not this
    // agent's debt, and fetching for them cost every claim a round-trip.
    ensure_fetched(ws)?;
    let Some(main_paths) = main_task_paths(ws) else {
        let mut owed: Vec<String> = mine.into_iter().map(|(id, _)| id.clone()).collect();
        owed.sort();
        return Ok(owed);
    };
    let mut head_cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut to_clear = Vec::new();
    let mut owed = Vec::new();
    for (id, record) in mine {
        if main_paths.contains(&format!(".agents/tasks/done/{id}.json")) {
            to_clear.push(closed_ref(id));
        } else if main_paths.contains(&format!(".agents/tasks/{id}.json")) {
            owed.push(id.clone());
        } else {
            let on_main = *head_cache
                .entry(record.head.clone())
                .or_insert_with(|| head_on_main(ws, record));
            if !on_main {
                owed.push(id.clone());
            }
        }
    }
    if !to_clear.is_empty() {
        clear_records(ws, &to_clear)?;
    }
    owed.sort();
    Ok(owed)
}

/// Accepted tasks this agent has not published — read-only, for `workflow
/// check`. Unlike [`owed_closes`] it mutates nothing, so the checklist can
/// report a debt without clearing it behind the agent's back.
pub fn unpublished_closes(ws: &Path, agent: &str) -> EaiResult<Vec<CloseRecord>> {
    agent_token(agent)?;
    sync_queue_refs(ws)?;
    unpublished_closes_synced(ws, agent)
}

/// [`unpublished_closes`] for a caller that has just run [`sync_queue`] and
/// fetched `origin` (as `workflow check` does, in parallel): the same read,
/// without a fetch behind each receipt.
pub fn unpublished_closes_synced(ws: &Path, agent: &str) -> EaiResult<Vec<CloseRecord>> {
    let by = agent_token(agent)?;
    let mine: Vec<(String, CloseRecord)> = closed_receipts(ws)?
        .into_iter()
        .filter(|(_, record)| record.agent == by)
        .collect();
    if mine.is_empty() {
        return Ok(Vec::new());
    }
    let Some(main_paths) = main_task_paths(ws) else {
        let mut out: Vec<CloseRecord> = mine.into_iter().map(|(_, record)| record).collect();
        out.sort_by(|a, b| a.task.cmp(&b.task));
        return Ok(out);
    };
    let mut head_cache: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut out = mine
        .into_iter()
        .filter(|(id, record)| {
            if main_paths.contains(&format!(".agents/tasks/done/{id}.json")) {
                return false;
            }
            if main_paths.contains(&format!(".agents/tasks/{id}.json")) {
                return true;
            }
            let on_main = *head_cache
                .entry(record.head.clone())
                .or_insert_with(|| head_on_main(ws, record));
            !on_main
        })
        .map(|(_, record)| record)
        .collect::<Vec<_>>();
    out.sort_by(|a, b| a.task.cmp(&b.task));
    Ok(out)
}

/// Every merge attestation on the remote, as the merger wrote it.
pub fn merged_tasks(ws: &Path) -> EaiResult<Vec<MergedTask>> {
    sync_queue_refs(ws)?;
    records_under::<MergedTask>(ws, MERGED_NAMESPACE)
}

/// Every post-merge verification record on the remote.
pub fn verified_tasks(ws: &Path) -> EaiResult<Vec<VerifiedTask>> {
    sync_queue_refs(ws)?;
    records_under::<VerifiedTask>(ws, "refs/verified/")
}

/// Read every record under one namespace, keyed by the id in its ref name.
fn records_under<T: serde::de::DeserializeOwned>(ws: &Path, namespace: &str) -> EaiResult<Vec<T>> {
    Ok(records_batch::<T>(ws, namespace)?
        .into_iter()
        .map(|(_, record)| record)
        .collect())
}

/// Whether a merge attestation is real, re-derived from `origin/main` instead
/// of believed: the named merge commit must be contained in `main`, and it must
/// actually contain the tested head. A forged receipt would have to produce a
/// merge commit on `main` that contains a head somebody accepted — which is the
/// thing being attested.
#[must_use]
pub fn merged_is_real(ws: &Path, record: &MergedTask) -> bool {
    let contains = |a: &str, b: &str| git(ws, &["merge-base", "--is-ancestor", a, b]).is_ok();
    contains(&record.head, &record.merge) && contains(&record.merge, "origin/main")
}

/// Check a full object-ID attestation against one immutable main-history walk.
/// Being on main is insufficient: the tested head must precede this merge.
fn attestation_in_history(
    parents: &std::collections::HashMap<&str, Vec<&str>>,
    head: &str,
    merge: &str,
) -> bool {
    if !parents.contains_key(head) || !parents.contains_key(merge) {
        return false;
    }
    let mut pending = vec![merge];
    let mut seen = std::collections::HashSet::new();
    while let Some(commit) = pending.pop() {
        if commit == head {
            return true;
        }
        if seen.insert(commit) {
            if let Some(previous) = parents.get(commit) {
                pending.extend(previous.iter().copied());
            }
        }
    }
    false
}

/// One agent's standing in the queue: whoever accepted work still owes the
/// merge that publishes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub agent: String,
    /// Tasks it accepted whose merge is still not on `origin/main`.
    pub owed: Vec<String>,
    /// Tasks it accepted and then gave up, with the recorded reason.
    pub abandoned: Vec<String>,
}

/// The swarm's reconciliation of what agents said against what the remote has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditReport {
    /// Closed, published, and still not on `origin/main` — the outstanding debt.
    pub owed: Vec<CloseRecord>,
    /// Accepted then given up, with the reason (a permanent record).
    pub abandoned: Vec<Abandoned>,
    /// Real merge attestations: the tested head is on `main`.
    pub merged: Vec<MergedTask>,
    /// Attestations no verification has covered yet.
    pub unverified: Vec<MergedTask>,
    /// Merge attestations that do not survive [`merged_is_real`].
    pub forged: Vec<MergedTask>,
    /// Per-agent totals, the ranking input (Mandate 56).
    pub agents: Vec<AgentRecord>,
}

impl AuditReport {
    /// Nothing accepted is still unpublished or unmerged.
    #[must_use]
    pub fn clean(&self) -> bool {
        self.owed.is_empty()
    }
}

/// Reconcile the queue's remote facts: what agents accepted and abandoned, what
/// the merger attested, and what that means against `origin/main`.
///
/// This is the swarm-wide view `workflow check` cannot give — it answers "did
/// every agent that reported success actually publish it?", not "may *I* start
/// work?". Read-only: it reports, and the claim path is what clears.
pub fn audit(ws: &Path) -> EaiResult<AuditReport> {
    sync_queue_refs(ws)?;
    if remote() != DEFAULT_REMOTE {
        git(ws, &["fetch", "--quiet", "origin"])?;
    }

    let merged = records_under::<MergedTask>(ws, MERGED_NAMESPACE)?;
    let verified = records_under::<VerifiedTask>(ws, "refs/verified/")?;
    // Audits used four Git processes per attestation, repeating both ancestry
    // checks while partitioning real/forged records. Walk the snapshot once;
    // no persistent proof survives a new audit or an advance of main.
    let history = if merged.is_empty() {
        String::new()
    } else {
        git(
            ws,
            &[
                "rev-list",
                "--full-history",
                "--parents",
                "origin/main",
                "--",
            ],
        )?
    };
    let parents: std::collections::HashMap<&str, Vec<&str>> = history
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?, fields.collect()))
        })
        .collect();
    let full_id =
        |id: &str| matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit());
    let mut real = Vec::new();
    let mut forged = Vec::new();
    let mut checked = std::collections::HashMap::new();
    for record in merged {
        let canonical = full_id(&record.head) && full_id(&record.merge);
        let key = if canonical {
            (
                record.head.to_ascii_lowercase(),
                record.merge.to_ascii_lowercase(),
            )
        } else {
            (record.head.clone(), record.merge.clone())
        };
        let valid = *checked.entry(key.clone()).or_insert_with(|| {
            if canonical {
                attestation_in_history(&parents, &key.0, &key.1)
            } else {
                // Preserve legacy abbreviated IDs/ref spellings through Git.
                merged_is_real(ws, &record)
            }
        });
        if valid {
            real.push(record);
        } else {
            forged.push(record);
        }
    }
    let verified_ids: std::collections::HashSet<&str> =
        verified.iter().map(|v| v.task.as_str()).collect();
    let unverified: Vec<MergedTask> = real
        .iter()
        .filter(|m| !verified_ids.contains(m.task.as_str()))
        .cloned()
        .collect();

    let mut owed = Vec::new();
    for record in records_under::<CloseRecord>(ws, "refs/closed/")? {
        let landed = !receipt_owed(ws, &record) || real.iter().any(|m| m.task == record.task);
        if !landed {
            owed.push(record);
        }
    }
    owed.sort_by(|a, b| a.task.cmp(&b.task));

    let abandoned = records_under::<Abandoned>(ws, "refs/abandoned/")?;

    let mut agents: std::collections::BTreeMap<String, AgentRecord> = Default::default();
    for record in &owed {
        agents
            .entry(record.agent.clone())
            .or_insert_with(|| AgentRecord {
                agent: record.agent.clone(),
                owed: Vec::new(),
                abandoned: Vec::new(),
            })
            .owed
            .push(record.task.clone());
    }
    for record in &abandoned {
        agents
            .entry(record.agent.clone())
            .or_insert_with(|| AgentRecord {
                agent: record.agent.clone(),
                owed: Vec::new(),
                abandoned: Vec::new(),
            })
            .abandoned
            .push(format!("{}: {}", record.task, record.reason));
    }

    Ok(AuditReport {
        owed,
        abandoned,
        merged: real,
        unverified,
        forged,
        agents: agents.into_values().collect(),
    })
}

/// One acceptance re-run on the merged tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyOutcome {
    pub task: String,
    /// `passed`, `skipped:<why>` or `failed:<code>`.
    pub result: String,
    pub detail: String,
}

/// Re-run, on this tree, the acceptance of merge attestations that have none.
///
/// This answers the question `close` cannot: the acceptance passed on the
/// branch, but did the *merged* result still pass it, next to everyone else's
/// changes? Only `cargo` acceptances are re-run — they are hermetic by mandate
/// and availabile anywhere, while a host-specific `scripts/…` checker failing
/// on a bare runner would say nothing about the merge — and a task whose
/// acceptance cannot be re-run is recorded as skipped, with the reason.
///
/// `limit` bounds one run so a broken job cannot turn into an unbounded sweep.
/// A failure writes no verification record, so the next run retries it.
///
/// The sweep runs on every merge and shares the job with everything else, so
/// it is bounded in *time* as well as count: see
/// [`verify_merged_within`]. A single acceptance that never returns used to
/// hold the job until CI killed it, losing every record it had already earned.
pub fn verify_merged(ws: &Path, limit: usize) -> EaiResult<Vec<VerifyOutcome>> {
    verify_merged_within(ws, limit, Duration::from_secs(VERIFY_BUDGET_SECS))
}

/// Default wall-clock budget for one sweep.
///
/// The job that hosts this has `timeout-minutes: 90`, and a cold run pays for
/// `cargo run` building `susi` plus one test target per acceptance — 30–50
/// minutes on a shared runner. The budget is 20 minutes so that 5 (setup) + 50
/// (cold builds) + 20 (sweep) stays inside 90: a job cancelled at its timeout
/// records nothing, so the queue would never drain and main would never go
/// green. What does not fit is deferred to the next merge's run, and
/// `susi tasks audit` keeps reporting it.
pub const VERIFY_BUDGET_SECS: u64 = 20 * 60;

/// Ceiling on one acceptance, so a hang cannot consume the whole budget.
///
/// The heaviest acceptance here (`--test release_e2e_checks`, the whole root
/// package) takes a couple of minutes once its build is warm, so 12 is
/// generous for real work and short enough that one stuck test cannot eat the
/// sweep.
pub const VERIFY_ONE_TIMEOUT_SECS: u64 = 12 * 60;

/// [`verify_merged`] with an explicit total budget.
pub fn verify_merged_within(
    ws: &Path,
    limit: usize,
    budget: Duration,
) -> EaiResult<Vec<VerifyOutcome>> {
    let report = audit(ws)?;
    let done = done_dir(ws);
    let mut candidates: Vec<&MergedTask> = report
        .unverified
        .iter()
        .filter(|m| done.join(format!("{}.json", m.task)).is_file())
        .collect();
    candidates.sort_by_key(|m| m.merged_unix);
    candidates.truncate(limit);
    let deadline = Instant::now() + budget;

    let mut out = Vec::new();
    for record in candidates {
        let task: Task =
            serde_json::from_slice(&std::fs::read(done.join(format!("{}.json", record.task)))?)?;
        // One acceptance cannot outlive the sweep's remaining budget: the
        // check that would cross it is deferred whole, not started late.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            out.push(VerifyOutcome {
                task: record.task.clone(),
                result: "deferred:budget".to_string(),
                detail: format!(
                    "the sweep's {}s budget was spent before this acceptance ran",
                    budget.as_secs()
                ),
            });
            continue;
        }
        let per_check = remaining.min(Duration::from_secs(VERIFY_ONE_TIMEOUT_SECS));
        let result = verify_one(ws, &task, per_check);
        // A failure is not recorded, so the next run retries it; a pass, and a
        // skip with its reason, are facts worth keeping.
        if result.result != "passed" && !result.result.starts_with("skipped") {
            out.push(result);
            continue;
        }
        let value = result.result.clone();
        let detail = result.detail.clone();
        let verified = VerifiedTask {
            task: record.task.clone(),
            head: record.head.clone(),
            merge: record.merge.clone(),
            result: value.clone(),
            verified_unix: now_unix(),
        };
        let raced = format!(
            "{} was verified concurrently; the record already on the remote stands",
            record.task
        );
        // Losing this race is not a failure — someone else already recorded it.
        if put_record(ws, &verified_ref(&record.task), &verified, &raced).is_err()
            && read_record::<VerifiedTask>(ws, &verified_ref(&record.task))?.is_none()
        {
            return Err(EaiError::config(format!(
                "could not record the verification of {}",
                record.task
            )));
        }
        out.push(VerifyOutcome {
            task: record.task.clone(),
            result: value,
            detail,
        });
    }
    Ok(out)
}

/// Run one task's acceptance against this tree, killing it at `timeout`.
fn verify_one(ws: &Path, task: &Task, timeout: Duration) -> VerifyOutcome {
    let outcome = |result: &str, detail: String| VerifyOutcome {
        task: task.id.clone(),
        result: result.to_string(),
        detail,
    };
    if !accept_allowed(&task.accept.cmd) {
        return outcome(
            "skipped:not-allowed",
            "acceptance command is not allowed".into(),
        );
    }
    let program = task.accept.cmd.first().cloned().unwrap_or_default();
    if program != "cargo" {
        return outcome(
            "skipped:not-cargo",
            format!(
                "`{program}` acceptance is host-specific; the merged tree cannot re-run it faithfully"
            ),
        );
    }
    let started = Instant::now();
    match run_bounded(&program, &task.accept.cmd[1..], ws, timeout) {
        Bounded::Exited(out) if out.status.success() => {
            outcome("passed", task.accept.cmd.join(" "))
        }
        Bounded::Exited(out) => outcome(
            &format!("failed:{}", out.status.code().unwrap_or(-1)),
            format!(
                "{} :: {}",
                task.accept.cmd.join(" "),
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
            ),
        ),
        // Not a verdict on the merged tree: the acceptance did not finish, so
        // it is deferred and retried, and no verification record is written.
        Bounded::TimedOut => outcome(
            "deferred:timeout",
            format!(
                "{} did not finish within {}s; deferred to a later sweep",
                task.accept.cmd.join(" "),
                timeout.as_secs()
            ),
        ),
        Bounded::Failed(e) => outcome("failed:-1", format!("{program} could not run: {e}")),
    }
    .tap_elapsed(started)
}

/// One bounded child outcome.
enum Bounded {
    Exited(std::process::Output),
    TimedOut,
    Failed(std::io::Error),
}

impl VerifyOutcome {
    /// Append the wall time to the detail, so a sweep's log says where it went.
    fn tap_elapsed(mut self, started: Instant) -> Self {
        self.detail = format!("{} ({}s)", self.detail, started.elapsed().as_secs());
        self
    }
}

/// Run a command to completion or kill its process tree at `timeout`.
///
/// A plain `output()` has no deadline: an acceptance that waits on something
/// the runner does not have holds the whole sweep, and the job that runs the
/// sweep dies at its own timeout having written nothing.
///
/// Output goes to files rather than pipes on purpose. `cargo test` spawns
/// `susi`, which spawns agents and daemons; anything that outlives the child
/// while holding the pipe keeps it open, so `wait_with_output` blocks forever
/// even though the command already exited. The workflow adapter redirects to
/// files for exactly this reason.
fn run_bounded(program: &str, args: &[String], cwd: &Path, timeout: Duration) -> Bounded {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!("susi-verify-{}-{stamp}", std::process::id()));
    let out_path = base.with_extension("out");
    let err_path = base.with_extension("err");
    let discard = |e: std::io::Error| {
        let _ = std::fs::remove_file(&out_path);
        let _ = std::fs::remove_file(&err_path);
        Bounded::Failed(e)
    };
    let Ok(stdout) = std::fs::File::create(&out_path) else {
        let _ = std::fs::remove_file(&err_path);
        return Bounded::Failed(std::io::Error::other("create stdout capture"));
    };
    let Ok(stderr) = std::fs::File::create(&err_path) else {
        let _ = std::fs::remove_file(&out_path);
        return Bounded::Failed(std::io::Error::other("create stderr capture"));
    };
    let mut child = match Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return discard(e),
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return discard(e),
        }
        if Instant::now() >= deadline {
            kill_process_tree(&mut child);
            let _ = std::fs::remove_file(&out_path);
            let _ = std::fs::remove_file(&err_path);
            return Bounded::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stdout = std::fs::read(&out_path).unwrap_or_default();
    let stderr = std::fs::read(&err_path).unwrap_or_default();
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    Bounded::Exited(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Kill a child and anything it spawned.
fn kill_process_tree(child: &mut std::process::Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        // A cargo test spawns `susi`, which spawns agents; the group form
        // reaches them, the single-pid form is the fallback when the child is
        // not a group leader.
        let _ = Command::new("kill")
            .args(["-s", "KILL", &format!("-{pid}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The bounded runner must return once the child exits, even when something it
/// spawned still holds the output stream: a leaked agent or daemon is the
/// normal case for a `cargo test` acceptance, and waiting for EOF there is what
/// held the sweep until the job was killed.
#[cfg(unix)]
#[test]
fn a_leaked_grandchild_does_not_hold_the_acceptance_open() {
    let dir = std::env::temp_dir().join(format!("susi-verify-leak-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // `sh` exits immediately after spawning a child that inherits the output
    // stream and sleeps far longer than the test will wait.
    let script = "sleep 60 & echo done";
    let started = Instant::now();
    let outcome = run_bounded(
        "sh",
        &["-c".to_string(), script.to_string()],
        &dir,
        Duration::from_secs(20),
    );
    let elapsed = started.elapsed();
    match outcome {
        Bounded::Exited(out) => {
            assert_eq!(out.status.code(), Some(0));
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
        }
        Bounded::TimedOut => {
            panic!("the parent exited; the sweep must not wait on the leaked child")
        }
        Bounded::Failed(e) => panic!("sh could not run: {e}"),
    }
    assert!(
        elapsed < Duration::from_secs(15),
        "took {elapsed:?}, so it waited on the leaked grandchild"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Claims currently on the remote, keyed by task id.
pub fn claims(ws: &Path) -> EaiResult<Vec<Claim>> {
    sync_queue_refs(ws)?;
    claims_synced(ws)
}

/// [`claims`] for a caller that has just run [`sync_queue`].
pub fn claims_synced(ws: &Path) -> EaiResult<Vec<Claim>> {
    records_under::<Claim>(ws, "refs/claims/")
}

/// Refuse work on a stale base: an agent must hold every commit of
/// `origin/main` before claiming, so it starts from current rules and a current
/// queue. Unreachable remotes and missing integration refs fail closed.
pub fn ensure_synced(ws: &Path) -> EaiResult<()> {
    ensure_synced_for(ws, None)
}

/// How many accepted-but-unmerged closes one agent may have outstanding while
/// it claims more work (`SUSI_MERGES_IN_FLIGHT`, default 4). The merge
/// machinery lands them while the agent keeps working; one in flight made a
/// fast agent idle on a queue it cannot hurry. The bound still refuses the
/// pileup: success reported again and again and never published.
pub fn merges_in_flight() -> usize {
    std::env::var("SUSI_MERGES_IN_FLIGHT")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4)
}

/// [`ensure_synced`] with the task being claimed exempted from the
/// "unpublished completion" bound.
///
/// One accepted-but-unmerged close is the pipeline: the merge machinery lands
/// it while the agent takes the next task, so it is not a reason to refuse a
/// claim. A second outstanding close is a pileup of accepted-but-unproven
/// work, and that is refused. Re-adopting *your own* accepted task after a
/// lease lapsed is the other case where a close is legitimately in this branch
/// and not yet on main: `finish` re-adopts it to keep waiting. Exempting
/// exactly that id keeps the bound for every other claim, and keeps a lapsed
/// lease from stranding the work.
pub fn ensure_synced_for(ws: &Path, claiming: Option<&str>) -> EaiResult<()> {
    git(ws, &["fetch", "--quiet", "origin"])?;
    // The dependency gate below reads task files from this checkout, so an
    // uncommitted edit to a *tracked* one — deleting the task you depend on,
    // say — must not open it. Untracked additions are deliberately fine:
    // `tasks add` writes one, and it is committed with the work.
    let tampered = git(ws, &["status", "--porcelain", "--", ".agents/tasks/"])?
        .lines()
        .filter(|line| !line.starts_with("??"))
        .count();
    if tampered > 0 {
        return Err(EaiError::config(format!(
            "claim refused: {tampered} uncommitted change(s) to tracked task files — \
             commit them first (the queue and its dependencies are read from the checkout)"
        )));
    }
    let n = git(ws, &["rev-list", "--count", "HEAD..origin/main"])?;
    let unpublished = git(
        ws,
        &[
            "diff",
            "--name-only",
            "origin/main...HEAD",
            "--",
            ".agents/tasks/done/",
        ],
    )?;
    let reassuming = claiming.map(|id| format!(".agents/tasks/done/{id}.json"));
    let pending: Vec<&str> = unpublished
        .lines()
        .filter(|path| reassuming.as_deref() != Some(*path))
        .collect();
    if pending.len() > merges_in_flight() {
        return Err(EaiError::config(format!(
            "claim refused: {} completed tasks are still awaiting merge — publish one first \
             (at most {} may be in flight; SUSI_MERGES_IN_FLIGHT)",
            pending.len(),
            merges_in_flight()
        )));
    }
    match n.parse::<u64>() {
        Ok(0) | Err(_) => Ok(()),
        Ok(n) => {
            // In a busy swarm main moves between an agent's sync and its
            // claim most of the time, and every refusal was a manual
            // merge-and-retry loop. A clean tree whose merge with main has no
            // conflict is merged here, exactly as the advice below would have
            // had the agent do; anything else is still the agent's call.
            let dirty = git(ws, &["status", "--porcelain", "--untracked-files=no"])?;
            if dirty.trim().is_empty()
                && git(ws, &["merge-tree", "--write-tree", "HEAD", "origin/main"]).is_ok()
            {
                if git(ws, &["merge", "--no-edit", "--quiet", "origin/main"]).is_ok() {
                    eprintln!(
                        "merged origin/main ({n} commit(s)) before claiming: the merge was clean"
                    );
                    return Ok(());
                }
                let _ = git(ws, &["merge", "--abort"]);
            }
            Err(EaiError::config(format!(
                "claim refused: this worktree is {n} commit(s) behind origin/main. \
                 Sync first: `git merge origin/main` (then re-run the claim)"
            )))
        }
    }
}

/// Claim `id` for `agent`. Fails if another live claim exists or a dependency
/// is still open. An expired claim is taken over by compare-and-swap.
/// True when two scope sets reserve the same surface. A task record is one
/// file per id under `.agents/tasks`, so a claim on that directory (or on one
/// record) never conflicts with another task's own record — otherwise one
/// worker's directory-wide record scope would serialize the whole swarm.
fn scopes_conflict(left: &[String], right: &[String]) -> bool {
    left.iter().any(|a| {
        right.iter().any(|b| {
            (a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/")))
                && !(is_task_record(a) && is_task_record(b))
        })
    })
}

/// True for a task-record path: the directory itself or one record under it.
fn is_task_record(scope: &str) -> bool {
    scope == ".agents/tasks" || scope.starts_with(".agents/tasks/")
}

pub fn claim(ws: &Path, id: &str, agent: &str, hours: u64, now: u64) -> EaiResult<Claim> {
    claim_scoped(
        ws,
        id,
        agent,
        ClaimOptions {
            hours,
            now,
            scopes: &[],
        },
    )
}

/// Reserve declared files/directories together with task and agent ownership.
/// A global CAS ref makes overlapping-scope decisions atomic across clones.
pub struct ClaimOptions<'a> {
    pub hours: u64,
    pub now: u64,
    pub scopes: &'a [String],
}

pub fn claim_scoped(
    ws: &Path,
    id: &str,
    agent: &str,
    options: ClaimOptions<'_>,
) -> EaiResult<Claim> {
    // The generation ref is one global ref, so two agents claiming *different*
    // tasks at the same instant collide there: one push wins, the other loses
    // the compare-and-swap and is told to refresh and retry. Six agents
    // starting together is the normal case, not an edge — a live six-way test
    // saw five of six fail that way, for conflicts that were nobody's. The
    // retry belongs here, where the race is understood, not in every caller.
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match claim_once(
            ws,
            id,
            agent,
            ClaimOptions {
                hours: options.hours,
                now: options.now,
                scopes: options.scopes,
            },
        ) {
            Err(err) if attempt < CAS_ATTEMPTS && is_cas_race(&err) => cas_backoff(attempt, agent),
            other => return other,
        }
    }
}

/// How many times a lost compare-and-swap is retried before the caller is told.
/// With the backoff below that is about 13 s of retrying in the worst case: a
/// swarm claiming at one sync boundary all lose but one race each round, and
/// eight short tries (under 1.5 s) ran out under load.
const CAS_ATTEMPTS: u32 = 12;
/// Both messages below carry a phrase from here, so the retry matcher and the
/// text a user reads can never drift apart.
const CAS_PHRASE: &str = "concurrently with another agent";
const CAS_RENEW_PHRASE: &str = "concurrently with another claim";
/// How git words a lock held for a moment by another process: many agents
/// share one clone's ref store and push to the same remote refs. These are
/// races too, and retrying is the answer, not a failure for the agent.
const LOCK_PHRASES: &[&str] = &[
    "cannot lock ref",
    "unable to lock",
    "failed to lock",
    ".lock': file exists",
];

fn is_cas_race(err: &EaiError) -> bool {
    // git capitalises some of these ("Unable to create ... File exists.").
    let text = err.to_string().to_ascii_lowercase();
    text.contains(CAS_PHRASE)
        || text.contains(CAS_RENEW_PHRASE)
        || LOCK_PHRASES.iter().any(|phrase| text.contains(phrase))
}

/// Exponential backoff with jitter, capped at two seconds. The jitter changes
/// with every attempt: a seed fixed per process kept two colliding agents in
/// step on every retry, and the point is that they stop colliding.
fn cas_backoff(attempt: u32, agent: &str) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    let seed = nanos
        ^ u64::from(std::process::id()).wrapping_mul(2_654_435_761)
        ^ agent.bytes().map(u64::from).sum::<u64>()
        ^ u64::from(attempt).wrapping_mul(40_503);
    let base = 50u64.saturating_mul(1u64 << attempt.min(6)).min(2_000);
    let jitter = seed % (base / 2 + 1);
    std::thread::sleep(std::time::Duration::from_millis(base / 2 + jitter));
}

fn claim_once(ws: &Path, id: &str, agent: &str, options: ClaimOptions<'_>) -> EaiResult<Claim> {
    let ClaimOptions { hours, now, scopes } = options;
    for scope in scopes {
        if scope.is_empty()
            || scope.starts_with('/')
            || scope
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..")
        {
            return Err(EaiError::config(
                "scope must be a normalized repo-relative path",
            ));
        }
    }
    let task = find_open(ws, id)?;
    let open: std::collections::HashSet<String> = list_open(ws).into_iter().map(|t| t.id).collect();
    if let Some(dep) = task.deps.iter().find(|d| open.contains(*d)) {
        return Err(EaiError::config(format!(
            "{id} is blocked: dependency {dep} is still open"
        )));
    }
    let agent = agent_token(agent)?;
    let generation_ref = "refs/claim-generation/current";
    let agent_ref = format!("refs/claim-agents/{agent}");
    let r = remote();
    // `claims(ws)` runs `sync_queue_refs(ws)`, which mirrors `+refs/claims/*`,
    // `+refs/claim-agents/*` and `+refs/claim-generation/*` in a single atomic
    // `git fetch` snapshot, avoiding a separate preflight `ls-remote` round-trip.
    let live = claims(ws)?;
    let generation_old =
        git(ws, &["rev-parse", "--verify", "--quiet", generation_ref]).unwrap_or_default();
    let agent_old = git(ws, &["rev-parse", "--verify", "--quiet", &agent_ref]).unwrap_or_default();
    let all_receipts = closed_receipts(ws)?;
    let mut origin_fetched = r == DEFAULT_REMOTE;
    let mut ensure_origin_fetched = |ws: &Path| -> EaiResult<()> {
        if !origin_fetched {
            fetch_origin(ws)?;
            origin_fetched = true;
        }
        Ok(())
    };
    // An accepted task whose close is not yet on origin/main is still owned by
    // whoever accepted it, whatever happened to the claim: it may have been
    // released, or lapsed hours ago. The receipt on the shared remote is the
    // debt, and it binds the *agent*, not the worktree — a fresh clone cannot
    // launder it, which is what a claim lease alone could not express. One
    // owed merge is the pipeline: an agent whose close is queued may take the
    // next task while the merge machinery lands it. Two owed merges is the
    // pileup the receipt exists to refuse — success reported twice and never
    // published. The task being claimed here is exempt: re-adopting your own
    // accepted task is how `finish` keeps waiting, not a way to start
    // something new.
    let owed = owed_closes_from_receipts(
        ws,
        &all_receipts,
        &agent,
        Some(id),
        &mut ensure_origin_fetched,
    )?;
    if owed.len() > merges_in_flight() {
        let list = owed.join(", ");
        let first = owed.first().map(String::as_str).unwrap_or("");
        let cap = merges_in_flight();
        return Err(EaiError::config(format!(
            "{agent} owes merges for {list} — {cap} accepted tasks may await their merges while \
             the next is claimed, not more. Publish one: `susi workflow finish {first}` (or, to \
             give it up deliberately: `susi tasks release {first} --abandon <reason>`)"
        )));
    }
    let branch = symbolic_head_branch(ws);
    let owned = live
        .iter()
        .find(|c| c.task == id && c.agent == agent && !c.expired(now));
    let requested_scopes = owned.map_or_else(
        || scopes.to_vec(),
        |claim| {
            let mut union = claim.scopes.clone();
            for scope in scopes {
                if !union.contains(scope) {
                    union.push(scope.clone());
                }
            }
            union
        },
    );
    let held: Vec<&Claim> = live
        .iter()
        .filter(|c| !c.expired(now) && !owned.is_some_and(|own| own.task == c.task))
        .filter(|c| scopes_conflict(&requested_scopes, &c.scopes))
        .collect();
    if !held.is_empty() {
        // Refusing on a stale view of main would hold files a merge already
        // freed; one fetch, only when something overlaps.
        ensure_origin_fetched(ws)?;
    }
    for other in held {
        // `close` keeps the lease until the closing commit reaches main, and
        // `finish` releases the claim at the push — so a claim outlives its
        // merge whenever nothing released it (a session that ended, a wait
        // that never came back): hours of a 4 h lease on files that are on
        // main already. The reservation was for work on a branch; once that
        // work is on main the files are everyone's again, and a sibling's
        // next task must not wait out a lease that reserves nothing. Board
        // hygiene removes the ref once the lease lapses; this is the window
        // before that. The claim must predate that close: ids are minted from
        // a clone's own view of the queue, so a clone behind main can mint an
        // id main already closed, and a live claim on the re-minted id is
        // real work, not a leftover.
        if closed_on_main_at(ws, &other.task).is_some_and(|at| other.claimed_unix <= at) {
            continue;
        }
        return Err(EaiError::config(format!(
            "scope overlaps {} held by {}",
            other.task, other.agent
        )));
    }
    // Closed but not merged: the work is on a branch, not on main, so its
    // files are still taken. Only receipts that recorded their scopes count,
    // and only until the close is on `origin/main` (or the receipt is settled).
    let overlapping: Vec<&(String, CloseRecord)> = all_receipts
        .iter()
        .filter(|(task, receipt)| {
            task != id
                && !receipt.scopes.is_empty()
                && scopes_conflict(&requested_scopes, &receipt.scopes)
        })
        .collect();
    if !overlapping.is_empty() {
        // Refusing on a stale view of main would hold files a merge already
        // freed; one fetch, only when something overlaps.
        ensure_origin_fetched(ws)?;
    }
    for (task, receipt) in overlapping {
        if done_on_main(ws, task) || !receipt_owed(ws, receipt) {
            continue;
        }
        return Err(EaiError::config(format!(
            "scope overlaps {task}, closed by {} and not yet merged: its files stay reserved \
             until that merge lands on main. Claim other files, or wait for the merge",
            receipt.agent
        )));
    }
    if let Some(current) = owned {
        if current
            .branch
            .as_ref()
            .is_some_and(|held| Some(held) != branch.as_ref())
        {
            return Err(EaiError::config(format!(
                "{id} can only expand its scope from the branch that holds it"
            )));
        }
        let (old, mut expanded) = read_claim(ws, id)
            .ok_or_else(|| EaiError::config(format!("{id} claim changed; refresh and retry")))?;
        expanded.branch = branch;
        expanded.scopes = requested_scopes;
        expanded.lease_until_unix = now.saturating_add(hours.max(1).saturating_mul(3600));
        let blob = claim_blob(ws, &expanded)?;
        if !agent_old.is_empty() && agent_old != old {
            return Err(EaiError::config(
                "agent ownership changed; cannot expand claim scope",
            ));
        }
        queue_relay::push(
            ws,
            &r,
            &[
                (claim_ref(id), old),
                (agent_ref.clone(), agent_old),
                (generation_ref.to_string(), generation_old),
            ],
            &[
                (claim_ref(id), Some(blob.clone())),
                (agent_ref, Some(blob.clone())),
                (generation_ref.to_string(), Some(blob.clone())),
            ],
            &format!("{id} or agent {agent} was claimed {CAS_PHRASE}; refresh the queue and retry"),
        )?;
        git(ws, &["update-ref", &claim_ref(id), &blob])?;
        return Ok(expanded);
    }
    if let Some(other) = live.iter().find(|c| c.agent == agent && !c.expired(now)) {
        return Err(EaiError::config(format!(
            "{agent} already holds {}; complete or release it before claiming {id}",
            other.task
        )));
    }
    // A second atomic ref serializes claims by this agent even across clones.
    if !agent_old.is_empty() {
        if git(ws, &["cat-file", "-e", &format!("{agent_old}^{{blob}}")]).is_err() {
            git(ws, &["fetch", "--quiet", &r, &agent_ref])?;
        }
        let body = git(ws, &["cat-file", "-p", &agent_old])?;
        let owner: Claim = serde_json::from_str(&body)?;
        if !owner.expired(now) {
            return Err(EaiError::config(format!(
                "{agent} already holds {}; complete or release it first",
                owner.task
            )));
        }
    }
    let existing = read_claim(ws, id);
    if let Some((_, c)) = &existing {
        if !c.expired(now) {
            return Err(EaiError::config(format!(
                "{id} is claimed by {} until unix {}",
                c.agent, c.lease_until_unix
            )));
        }
    }
    let mine = Claim {
        branch,
        scopes: scopes.to_vec(),
        task: id.to_string(),
        agent,
        claimed_unix: now,
        lease_until_unix: now.saturating_add(hours.max(1).saturating_mul(3600)),
    };
    let blob = claim_blob(ws, &mine)?;
    let task_old = existing.as_ref().map_or("", |(sha, _)| sha.as_str());
    // A claim is one blob pushed to the task, agent and generation refs, so a
    // takeover of an expired claim leaves the previous holder's agent ref
    // pointing at a claim it no longer holds. Clear it in the same atomic push
    // — but only while it still points at exactly that claim: if that agent has
    // claimed something else since, its ref is its own bookkeeping, not ours.
    let stale_agent = match &existing {
        Some((sha, old)) if old.agent != mine.agent => {
            let stale = format!("refs/claim-agents/{}", old.agent);
            let current =
                git(ws, &["rev-parse", "--verify", "--quiet", &stale]).unwrap_or_default();
            (current == *sha).then(|| (stale, sha.clone()))
        }
        _ => None,
    };
    let mut leases: Vec<queue_relay::Lease> = vec![
        (claim_ref(id), task_old.to_string()),
        (agent_ref.clone(), agent_old),
    ];
    let mut updates: Vec<queue_relay::Update> = vec![
        (claim_ref(id), Some(blob.clone())),
        (agent_ref, Some(blob.clone())),
    ];
    // Unscoped claims reserve zero file paths (`scopes_conflict(&[], _)` is
    // always false), so they never contend on the global scope-generation ref.
    if !scopes.is_empty() {
        leases.push((generation_ref.to_string(), generation_old));
        updates.push((generation_ref.to_string(), Some(blob.clone())));
    }
    if let Some((stale, sha)) = stale_agent {
        leases.push((stale.clone(), sha));
        updates.push((stale, None));
    }
    queue_relay::push(
        ws,
        &r,
        &leases,
        &updates,
        &format!(
            "{id} or agent {} was claimed {CAS_PHRASE}; refresh the queue and retry",
            mine.agent
        ),
    )?;
    git(ws, &["update-ref", &claim_ref(id), &blob])?;
    Ok(mine)
}

fn claim_blob(ws: &Path, claim: &Claim) -> EaiResult<String> {
    blob_of(ws, claim)
}

/// Extend an owned live lease without relinquishing its task or scope locks.
pub fn renew(ws: &Path, id: &str, agent: &str, hours: u64, now: u64) -> EaiResult<Claim> {
    // Every agent renews at the same sync boundary, so renewals collide on the
    // global generation ref exactly as claims do.
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match renew_once(ws, id, agent, hours, now) {
            Err(err) if attempt < CAS_ATTEMPTS && is_cas_race(&err) => cas_backoff(attempt, agent),
            other => return other,
        }
    }
}

fn renew_once(ws: &Path, id: &str, agent: &str, hours: u64, now: u64) -> EaiResult<Claim> {
    if !valid_id(id) {
        return Err(EaiError::config("invalid task id"));
    }
    let r = remote();
    sync_queue_refs(ws)?;
    let (old, mut claim) =
        read_claim(ws, id).ok_or_else(|| EaiError::config("no claim to renew"))?;
    let branch = git(ws, &["symbolic-ref", "-q", "--short", "HEAD"]).ok();
    if claim.agent != agent_token(agent)?
        || claim
            .branch
            .as_ref()
            .is_some_and(|b| Some(b) != branch.as_ref())
    {
        return Err(EaiError::config(
            "renewal requires your live claim on this branch",
        ));
    }
    // A lapsed lease is normally fatal for renewal — the task is free for
    // another agent to take over, and it should be. The exception is an
    // accepted task whose close is still unpublished: `finish` is waiting on a
    // merge, the receipt on the shared remote already stops this agent from
    // starting anything else, and re-adopting the lease is the difference
    // between waiting it out and stranding the work for nobody to finish.
    if claim.expired(now) && !accepted_unpublished(ws, id, &claim.agent)? {
        return Err(EaiError::config(
            "renewal requires your live claim on this branch",
        ));
    }
    if claim.branch.is_none() {
        claim.branch = branch;
    }
    claim.lease_until_unix = now.saturating_add(hours.max(1).saturating_mul(3600));
    let blob = claim_blob(ws, &claim)?;
    let agent_ref = format!("refs/claim-agents/{}", claim.agent);
    let agent_old = git(ws, &["rev-parse", "--verify", "--quiet", &agent_ref]).unwrap_or_default();
    if !agent_old.is_empty() && agent_old != old {
        return Err(EaiError::config("agent ownership changed; cannot renew"));
    }
    // Renewing an existing lease never alters `claim.scopes`, so it does not
    // touch the global `refs/claim-generation/current` ref (saving a remote
    // `ls-remote` round-trip and eliminating false CAS collisions with other
    // agents' concurrent claims and renewals).
    queue_relay::push(
        ws,
        &r,
        &[
            (claim_ref(id), old),
            (agent_ref.clone(), agent_old.to_string()),
        ],
        &[
            (claim_ref(id), Some(blob.clone())),
            (agent_ref, Some(blob.clone())),
        ],
        &format!("renewal of {id} raced {CAS_RENEW_PHRASE}; retry"),
    )?;
    git(ws, &["update-ref", &claim_ref(id), &blob])?;
    Ok(claim)
}

/// Give up a claim (only your own unless `force`).
///
/// A task that was accepted but whose close is not yet on `origin/main` cannot
/// be released by accident: the claim is what keeps the unmerged work from
/// being redone. `--force` still frees the claim — that is the deliberate "move
/// my claim to another checkout" path — but it does *not* clear the debt, which
/// keeps blocking that agent's next claim until the merge lands. Only
/// [`abandon`] gives the obligation up, and it says why.
pub fn release(ws: &Path, id: &str, agent: &str, force: bool) -> EaiResult<()> {
    release_claim(ws, id, agent, force, None)
}

/// Give up an accepted task deliberately, recording why.
///
/// The receipt is replaced by an [`Abandoned`] record on `refs/abandoned/<id>`,
/// so the task stops blocking its agent *and* the decision stays visible on the
/// remote: a task dropped silently is indistinguishable from one still in
/// flight, which is the whole failure mode this closes.
pub fn abandon(ws: &Path, id: &str, agent: &str, reason: &str) -> EaiResult<()> {
    if reason.trim().is_empty() {
        return Err(EaiError::config(
            "abandoning a task needs a reason — it is recorded on the remote",
        ));
    }
    release_claim(ws, id, agent, false, Some(reason))
}

fn release_claim(
    ws: &Path,
    id: &str,
    agent: &str,
    force: bool,
    abandon_reason: Option<&str>,
) -> EaiResult<()> {
    if !valid_id(id) {
        return Err(EaiError::config(format!("invalid task id `{id}`")));
    }
    sync_queue_refs(ws)?;
    let by = agent_token(agent)?;
    let receipt = read_closed(ws, id)?;
    // The receipt outlives the claim on purpose: a released or lapsed claim
    // must not erase the fact that the accepted work is still unmerged. Only
    // observing the merge, or an explicit abandonment, clears it.
    if let Some(record) = &receipt {
        if done_on_main(ws, id) {
            clear_record(ws, &closed_ref(id))?;
        } else if let Some(reason) = abandon_reason {
            if record.agent != by && !force {
                return Err(EaiError::config(format!(
                    "{id} was accepted by {}, not {by} — only its agent can abandon it",
                    record.agent
                )));
            }
            push_abandoned(ws, id, &record.agent, reason)?;
            clear_record(ws, &closed_ref(id))?;
        } else if !force {
            return Err(EaiError::config(format!(
                "{id} was accepted by {} (head {}) and its close is not on origin/main, so the \
                 accepted work is not done anywhere but this branch. Publish it: \
                 `susi workflow finish {id}` — or give it up deliberately: \
                 `susi tasks release {id} --abandon <reason>`",
                record.agent,
                short_head(&record.head)
            )));
        }
    }
    let Some((sha, c)) = read_claim(ws, id) else {
        // The claim ref is gone but the agent ref can still name it — a release
        // that raced, or a ref deleted by hand. Left in place it says this agent
        // already holds the task, so it can never claim it again.
        let stale_agent = format!("refs/claim-agents/{}", agent_token(agent)?);
        let listed = git(ws, &["ls-remote", &remote(), &stale_agent])?;
        let stale_sha = listed.split_whitespace().next().unwrap_or("");
        if !stale_sha.is_empty() {
            let body = git(ws, &["cat-file", "-p", stale_sha])?;
            if let Ok(parsed) = serde_json::from_str::<Claim>(&body) {
                if parsed.task == id {
                    let _ = queue_relay::push(
                        ws,
                        &remote(),
                        &[(stale_agent.clone(), stale_sha.to_string())],
                        &[(stale_agent.clone(), None)],
                        "the agent ref moved on since it was read; nothing to clean",
                    );
                }
            }
        }
        return Ok(());
    };
    if !force && c.agent != by {
        return Err(EaiError::config(format!(
            "{id} is claimed by {}, not you",
            c.agent
        )));
    }
    // Ownership is also *where*: with a shared or colliding agent token
    // (`codex-1` and `codex1` both tokenise to `CODEX1`), the token alone would
    // let one worker free another's live claim — and freeing it lets the task be
    // redone by someone else. Releasing from a different checkout is a recovery
    // action, so it stays possible, but explicitly: `--force`.
    if !force {
        if let Some(claimed_on) = c.branch.as_deref() {
            let here = git(ws, &["symbolic-ref", "-q", "--short", "HEAD"]).ok();
            if here.as_deref() != Some(claimed_on) {
                return Err(EaiError::config(format!(
                    "{id} was claimed on branch '{claimed_on}', not '{}' — release it from there, or pass --force to free it anyway",
                    here.as_deref().unwrap_or("(detached HEAD)")
                )));
            }
        }
    }
    // Work that was never accepted is still work. A claim whose branch carries
    // commits for this task that no merge has published cannot be dropped
    // silently, or "the agent moved on" and "the agent walked away" look the
    // same on the board. Checked after ownership, so an agent from the wrong
    // checkout sees the branch diagnostic; `--force` stays the explicit override
    // for moving a claim (re-scoping, a recreated worktree).
    if receipt.is_none() && abandon_reason.is_none() && !force && has_unpublished_work(ws, id) {
        return Err(EaiError::config(format!(
            "{id} has committed work on this branch that no merge has published — releasing the \
             claim would drop it silently. Give it up deliberately: `susi tasks release {id} \
             --abandon <reason>` (recorded on refs/abandoned/{id}), or publish it: \
             `susi workflow finish {id}`"
        )));
    }
    let agent_ref = format!("refs/claim-agents/{}", c.agent);
    // A reason given for work that was never accepted is still a fact worth
    // keeping: without this, abandoning an unfinished task left no trace at
    // all — only abandoning an *accepted* one did.
    if receipt.is_none() {
        if let Some(reason) = abandon_reason {
            push_abandoned(ws, id, &c.agent, reason)?;
        }
    }
    let agent_old = git(ws, &["rev-parse", "--verify", "--quiet", &agent_ref]).unwrap_or_default();
    let mut leases: Vec<queue_relay::Lease> = vec![(claim_ref(id), sha.clone())];
    let mut updates: Vec<queue_relay::Update> = vec![(claim_ref(id), None)];
    // Legacy claims have no agent ref; never delete a newer agent lease.
    if agent_old == sha {
        leases.push((agent_ref.clone(), sha.clone()));
        updates.push((agent_ref, None));
    }
    queue_relay::push(
        ws,
        &remote(),
        &leases,
        &updates,
        &format!(
            "{id}'s claim changed on the remote since it was read; refresh the queue and retry"
        ),
    )?;
    let _ = git(ws, &["update-ref", "-d", &claim_ref(id)]);
    Ok(())
}

/// Total passed tests across every `test result: ok. N passed` line.
fn tests_passed(cargo_output: &str) -> u64 {
    cargo_output
        .lines()
        .filter(|l| l.trim_start().starts_with("test result: ok."))
        .filter_map(|l| {
            l.split("ok. ")
                .nth(1)?
                .split(" passed")
                .next()?
                .trim()
                .parse::<u64>()
                .ok()
        })
        .sum()
}

/// Run the acceptance check; on success move the task to `done/` (recording
/// who, when and at which commit), retaining ownership until merge. Returns the record.
pub fn close(ws: &Path, id: &str, agent: &str) -> EaiResult<Task> {
    let mut task = find_open(ws, id)?;
    let by = agent_token(agent)?;
    sync_queue_refs(ws).map_err(|_| {
        EaiError::config(format!(
            "close refused: cannot reach origin to check whether {id} is already closed elsewhere"
        ))
    })?;
    let (owner_sha, owner_claim) = read_claim(ws, id).ok_or_else(|| {
        EaiError::config(format!("{id} requires a live owned claim before closing"))
    })?;
    let branch = symbolic_head_branch(ws);
    if owner_claim.agent != by
        || owner_claim.expired(now_unix())
        || owner_claim
            .branch
            .as_ref()
            .is_some_and(|b| Some(b) != branch.as_ref())
    {
        return Err(EaiError::config(format!(
            "{id} is claimed by {}, not {by} on this branch, or its lease expired",
            owner_claim.agent
        )));
    }
    ensure_open_on_main_synced(ws, id)?;
    if !accept_allowed(&task.accept.cmd) {
        return Err(EaiError::config("task acceptance command is not allowed"));
    }
    // `finish` runs the gate immediately before close; when that run provably
    // covered a cargo-test acceptance (same package or the workspace), the
    // result here would be byte-identical, so finish names the covered task in
    // SUSI_ACCEPTANCE_COVERED rather than paying for a second identical suite.
    // The value is `<id>@<worktree>` — bound to one worktree so a stale export
    // cannot skip acceptance elsewhere. Cargo-shaped acceptances only: a
    // scripts/ checker asserts things the gate never ran. Setting the env by
    // hand skips like --no-verify does — the post-merge verify job re-runs the
    // acceptance on the merged tree either way, which is the authoritative one.
    let covered = std::env::var("SUSI_ACCEPTANCE_COVERED")
        .ok()
        .is_some_and(|v| {
            let (cid, dir) = v.split_once('@').unwrap_or((v.as_str(), ""));
            cid == id
                && std::fs::canonicalize(dir).ok().as_deref()
                    == std::fs::canonicalize(ws).ok().as_deref()
        })
        && task.accept.cmd.first().is_some_and(|p| p == "cargo")
        && task
            .accept
            .cmd
            .get(1)
            .is_some_and(|a| a == "test" || a == "nextest");
    if covered {
        eprintln!(
            "gate covered the acceptance `{}` — not re-running it",
            task.accept.cmd.join(" ")
        );
    } else {
        // A cargo acceptance builds into this worktree's own target, so it
        // runs as it is: no other worktree's build is there to stand in for
        // this one's. A worktree still linked to the old shared target gets
        // its own first, and one whose target a hook is still copying waits
        // for that copy. A branch cut before the seeding, with no seeder yet,
        // still builds in the shared target: for one release it runs through
        // that branch's own guard, as it always did.
        let cargo = task.accept.cmd.first().is_some_and(|p| p == "cargo");
        if cargo {
            seed_own_target(ws);
        }
        let guarded = cargo
            .then(|| guarded_acceptance(ws, &task.accept.cmd))
            .flatten()
            .and_then(|mut command| command.current_dir(ws).output().ok());
        let out = match guarded {
            Some(out) => out,
            None => acceptance_command(&task.accept.cmd)
                .current_dir(ws)
                .output()
                .map_err(|e| {
                    // A check that does not exist yet is "not done", not an I/O crash.
                    EaiError::process(format!(
                        "{id} is not done: acceptance command `{}` could not run ({e})",
                        task.accept.cmd.join(" ")
                    ))
                })?,
        };
        // Show the check's own output: the closer should see what proved it.
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        print!("{}", String::from_utf8_lossy(&out.stdout));
        if !out.status.success() {
            return Err(EaiError::process(format!(
                "{id} is not done: acceptance check `{}` exited {:?}",
                task.accept.cmd.join(" "),
                out.status.code()
            )));
        }
        // `cargo test <filter>` exits 0 when the filter matches nothing; that
        // proves nothing, so a test-based check must have run at least one test.
        if task.accept.cmd.first().is_some_and(|p| p == "cargo")
            && task.accept.cmd.get(1).is_some_and(|a| a == "test")
        {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if tests_passed(&text) == 0 {
                return Err(EaiError::process(format!(
                    "{id} is not done: `{}` ran no tests (a filter that matches nothing passes vacuously)",
                    task.accept.cmd.join(" ")
                )));
            }
        }
    }
    // Acceptance may outlive a lease; check whether the lease expired before
    // mutating the queue (a non-expired lease cannot be taken over by another
    // agent).
    if owner_claim.expired(now_unix()) {
        return Err(EaiError::config(format!(
            "{id} is claimed by {}, not {by} on this branch, or its lease expired",
            owner_claim.agent
        )));
    }
    // Publish the receipt before the record moves. From here the task is
    // accepted and its agent may not claim anything else until this head is on
    // origin/main; a failed push leaves the task open, so close is retried
    // rather than leaving an acceptance nobody can see.
    let head = push_close_receipt(ws, id, &owner_sha, &owner_claim)?;
    task.closed = Some(Closed {
        at_unix: now_unix(),
        commit: head,
        by,
    });
    std::fs::create_dir_all(done_dir(ws))?;
    std::fs::write(
        done_dir(ws).join(format!("{id}.json")),
        serde_json::to_vec_pretty(&task)?,
    )?;
    std::fs::remove_file(tasks_dir(ws).join(format!("{id}.json")))?;
    // Keep the lease until the closing commit reaches main. Releasing here
    // allowed another current agent to reclaim the still-open remote task.

    Ok(task)
}

fn acceptance_command(argv: &[String]) -> Command {
    let program = argv.first().map(String::as_str).unwrap_or_default();
    #[cfg(windows)]
    {
        let interpreter = match Path::new(program).extension().and_then(|ext| ext.to_str()) {
            Some("sh") => Some("sh"),
            _ => None,
        };
        if let Some(executable) = interpreter {
            let mut command = Command::new(executable);
            command.arg(program).args(&argv[1..]);
            return command;
        }
        if Path::new(program).extension().and_then(|ext| ext.to_str()) == Some("py") {
            // Use Git Bash as the parent so Python inherits its POSIX utility
            // path too; checkers may invoke grep/sed as part of acceptance.
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    "export PYTHONIOENCODING=utf-8; exec python3 \"$@\"",
                    "susi-acceptance",
                ])
                .arg(program)
                .args(&argv[1..]);
            return command;
        }
    }
    let mut command = Command::new(program);
    command.args(&argv[1..]);
    command
}

/// Refuse to close a task `origin/main` already has in `done/`.
///
/// The queue is shared state, but `close` only ever read the worktree. An agent
/// that claimed a task before another branch's close merged, and closed it
/// afterwards without re-syncing, wrote a second `done/<id>.json` on divergent
/// history: this repository has four such ids (T-CLAUDE-17, T-CODEX-35,
/// T-INTELLIBITZ-14, T-INTELLIBITZ-15), and `.agents/tasks/**` has no merge
/// driver, so each is a hand-resolved add/delete conflict in which one record
/// silently wins. Fails closed — an unreachable origin refuses the close rather
/// than risking a second one.
fn ensure_open_on_main_synced(ws: &Path, id: &str) -> EaiResult<()> {
    if remote() != DEFAULT_REMOTE {
        git(ws, &["fetch", "--quiet", "origin"]).map_err(|_| {
            EaiError::config(format!(
                "close refused: cannot reach origin to check whether {id} is already closed elsewhere"
            ))
        })?;
    }
    if done_on_main(ws, id) {
        return Err(EaiError::config(format!(
            "{id} is already closed on origin/main — merge origin/main instead of closing it \
             twice (if your work is still needed, open a new task)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_contention_is_a_race_to_retry_and_other_failures_are_not() {
        for text in [
            "claim T-A-1 changed concurrently with another agent",
            "git push failed: ! [remote rejected] refs/claims/T-A-1 (cannot lock ref 'refs/claims/T-A-1': is at abc but expected def)",
            "git fetch failed: error: Unable to lock refs/claims/T-A-1",
            "git update-ref failed: fatal: Unable to create '/repo/.git/refs/claims/T-A-1.lock': File exists.",
        ] {
            assert!(is_cas_race(&EaiError::process(text)), "{text}");
        }
        for text in [
            "invalid task id",
            "git fetch failed: could not read from remote repository",
            "scope overlaps T-B-2 held by B",
        ] {
            assert!(!is_cas_race(&EaiError::process(text)), "{text}");
        }
    }

    /// On Windows a bare `bash` is WSL's System32 stub, which cannot run a
    /// script by its Windows path; the resolver must find Git's shell.
    #[cfg(windows)]
    #[test]
    fn the_posix_shell_on_windows_is_never_the_wsl_stub() {
        for name in ["sh", "bash"] {
            let shell = posix_shell(name);
            let lowered = shell.to_string_lossy().to_ascii_lowercase();
            assert!(!lowered.contains("system32"), "{}", shell.display());
            assert!(!lowered.contains("windowsapps"), "{}", shell.display());
            if Command::new("git").arg("--version").output().is_ok() {
                assert!(
                    shell.is_absolute() && shell.is_file(),
                    "{}",
                    shell.display()
                );
            }
        }
    }

    /// `sh` and `bash` both come from the Git install `git` runs from, ahead
    /// of whatever PATH offers: on a host with two installs, scoop's shim put
    /// its `sh` first on PATH, so `finish` ran under scoop's runtime and
    /// `tasks close` (bash, found under `git`) under Program Files'. Each
    /// runtime keeps its own process table, and the target guard's lock read
    /// the other's live holder as dead and took it.
    #[cfg(windows)]
    #[test]
    fn sh_and_bash_come_from_the_git_install_git_runs_from() {
        let Some(git_root) = git_install_root(std::env::var_os("PATH").as_deref()) else {
            return; // no git: nothing to find a shell in
        };
        if !git_root.join("bin").join("bash.exe").is_file()
            || !git_root.join("bin").join("sh.exe").is_file()
        {
            return; // not Git for Windows' layout
        }
        // Another install's shells, first on PATH (as a scoop shim is).
        let decoy = std::env::temp_dir().join(format!("susi-decoy-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&decoy);
        std::fs::create_dir_all(&decoy).unwrap();
        for name in ["sh.exe", "bash.exe"] {
            std::fs::write(decoy.join(name), b"not a shell").unwrap();
        }
        let mut dirs = vec![decoy.clone()];
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        let path = std::env::join_paths(dirs).unwrap();
        for name in ["sh", "bash"] {
            let shell = posix_shell_on(name, Some(&path));
            assert_eq!(
                shell.parent(),
                Some(git_root.join("bin").as_path()),
                "{name} must come from {}, not {}",
                git_root.display(),
                shell.display()
            );
        }
        let _ = std::fs::remove_dir_all(&decoy);
    }

    /// Outside Git Bash (PowerShell, the desktop app) a standard Git install
    /// leaves only `<git>\cmd` and System32 on PATH. The shell picked there
    /// must still find the POSIX userland: Git's raw `usr/bin` shell, picked
    /// first, found no mkdir, so the target guard stopped at its first line
    /// and did nothing (and `workflow sync` stopped at its first `dirname`).
    #[cfg(windows)]
    #[test]
    fn the_posix_shell_finds_coreutils_with_only_gits_cmd_on_path() {
        let Ok(out) = Command::new("git").arg("--exec-path").output() else {
            return; // no git: nothing to find a shell in
        };
        let exec_path = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        let Some(git_root) = exec_path.ancestors().nth(3).map(Path::to_path_buf) else {
            return;
        };
        if !git_root.join("cmd").join("git.exe").is_file() {
            return; // not Git for Windows' layout
        }
        let windows = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
        let path = std::env::join_paths([
            git_root.join("cmd"),
            windows.join("System32"),
            windows.clone(),
        ])
        .unwrap();
        let dir = std::env::temp_dir().join(format!("susi-shell-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // External tools only, and `find` is the one System32 also has.
        std::fs::write(
            dir.join("probe.sh"),
            "mkdir -p made/deep && touch made/deep/ok && find made -name ok | sed 's/^/found /' > seen\n",
        )
        .unwrap();
        for name in ["sh", "bash"] {
            let _ = std::fs::remove_dir_all(dir.join("made"));
            let _ = std::fs::remove_file(dir.join("seen"));
            let status = posix_shell_command_on(name, Some(path.clone()))
                .arg(dir.join("probe.sh"))
                .current_dir(&dir)
                .stdin(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "{name} could not run the probe script");
            assert_eq!(
                std::fs::read_to_string(dir.join("seen"))
                    .unwrap_or_default()
                    .trim(),
                "found made/deep/ok",
                "{name} must run scripts with Git's mkdir, find and sed"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The raw MSYS shell gets its userland put first on the child's PATH, in
    /// the launchers' order; a launcher (or any other shell) gets nothing.
    #[cfg(windows)]
    #[test]
    fn a_raw_msys_shell_is_given_its_userland_and_a_launcher_is_not() {
        let root = std::env::temp_dir().join(format!("susi-msys-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["usr/bin", "bin", "ucrt64/bin"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        assert_eq!(
            msys_userland(&root.join("usr").join("bin").join("sh.exe")),
            [
                root.join("ucrt64").join("bin"),
                root.join("usr").join("bin")
            ]
        );
        assert!(msys_userland(&root.join("bin").join("sh.exe")).is_empty());
        assert!(msys_userland(Path::new(r"C:\tools\sh.exe")).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_batch_of_records_is_read_in_order_skipping_missing_and_unreadable() {
        let good = br#"{"task":"T-A-1","agent":"a","head":"h1","closed_unix":1}"#;
        let other = br#"{"task":"T-B-2","agent":"b","head":"h2","closed_unix":2}"#;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(format!("o1 blob {}\n", good.len()).as_bytes());
        bytes.extend_from_slice(good);
        bytes.extend_from_slice(b"\no2 missing\no3 blob 8\nnot json\n");
        bytes.extend_from_slice(format!("o4 blob {}\n", other.len()).as_bytes());
        bytes.extend_from_slice(other);
        bytes.push(b'\n');
        let refs = [
            ("o1", "refs/closed/T-A-1"),
            ("o2", "refs/closed/T-GONE-1"),
            ("o3", "refs/closed/T-BAD-1"),
            ("o4", "refs/closed/T-B-2"),
        ];
        let read = parse_batch::<CloseRecord>(&bytes, &refs);
        assert_eq!(
            read.iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["refs/closed/T-A-1", "refs/closed/T-B-2"]
        );
        assert_eq!(read[1].1.agent, "b");
    }

    struct Repos {
        root: PathBuf,
    }
    impl Repos {
        /// A bare "server" plus two clones sharing it, each with an initial commit.
        fn new(tag: &str) -> (Self, PathBuf, PathBuf) {
            let root =
                std::env::temp_dir().join(format!("susi-tasks-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let run = |dir: &Path, args: &[&str]| {
                let out = Command::new(git_bin())
                    .args(args)
                    .current_dir(dir)
                    .env("GIT_AUTHOR_NAME", "t")
                    .env("GIT_AUTHOR_EMAIL", "t@t")
                    .env("GIT_COMMITTER_NAME", "t")
                    .env("GIT_COMMITTER_EMAIL", "t@t")
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            };
            let bare = root.join("server.git");
            std::fs::create_dir_all(&bare).unwrap();
            run(&bare, &["init", "--bare", "--quiet"]);
            // Tiny disposable repositories never need background maintenance.
            // Keep real Git transport/CAS checks; omit its housekeeping forks.
            let maintenance =
                "\n[gc]\n\tauto = 0\n[maintenance]\n\tauto = false\n[receive]\n\tautogc = false\n";
            let server_config = bare.join("config");
            let mut cfg = std::fs::read_to_string(&server_config).unwrap();
            cfg.push_str(maintenance);
            std::fs::write(server_config, cfg).unwrap();
            let bare_url = bare.to_string_lossy().replace('\\', "/");
            let mut clones = Vec::new();
            for name in ["a", "b"] {
                let dir = root.join(name);
                std::fs::create_dir_all(&dir).unwrap();
                run(&dir, &["init", "--quiet"]);
                // Repo-local identity, gpgsign off, and remote `origin` written
                // directly to `.git/config` instead of spawning 5 git processes
                // per clone across every test fixture.
                let cfg_path = dir.join(".git").join("config");
                let mut cfg = std::fs::read_to_string(&cfg_path).unwrap_or_default();
                cfg.push_str(&format!(
                    "\n[user]\n\tname = test\n\temail = test@example.test\n\
                     [commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n\
                     [remote \"origin\"]\n\turl = {bare_url}\n\
                     \tfetch = +refs/heads/*:refs/remotes/origin/*\n"
                ));
                cfg.push_str(maintenance);
                std::fs::write(&cfg_path, cfg).unwrap();
                run(&dir, &["commit", "--allow-empty", "-m", "init", "--quiet"]);
                clones.push(dir);
            }
            let b = clones.pop().unwrap();
            let a = clones.pop().unwrap();
            (Self { root }, a, b)
        }
    }
    impl Drop for Repos {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// `add` with the old positional shape, to keep the cases readable.
    macro_rules! addt {
        ($ws:expr, $agent:expr, $title:expr, $goal:expr, $size:expr, $deps:expr, $accept:expr $(,)?) => {
            add(
                $ws,
                $agent,
                NewTask {
                    title: $title.to_string(),
                    goal: $goal.to_string(),
                    size: $size.to_string(),
                    deps: $deps.to_vec(),
                    accept: $accept,
                    roadmap: None,
                },
            )
        };
    }

    fn true_cmd() -> Vec<String> {
        vec!["cargo".into(), "--version".into()]
    }

    #[test]
    fn mirror_batch_refuses_a_newer_ref_without_partially_creating_others() {
        let (_r, a, _) = Repos::new("mirror-batch-race");
        let old = blob_of(&a, &serde_json::json!({"value": "old"})).unwrap();
        let newer = blob_of(&a, &serde_json::json!({"value": "newer"})).unwrap();
        let requested = blob_of(&a, &serde_json::json!({"value": "requested"})).unwrap();
        git(&a, &["update-ref", "refs/closed/T-A-1", &newer]).unwrap();
        let result = mirror_updates(
            &a,
            &[
                ("refs/closed/T-A-1".into(), old),
                ("refs/closed/T-B-1".into(), String::new()),
            ],
            &[
                ("refs/closed/T-A-1".into(), Some(requested.clone())),
                ("refs/closed/T-B-1".into(), Some(requested)),
            ],
        );
        assert!(result.is_err());
        assert_eq!(git(&a, &["rev-parse", "refs/closed/T-A-1"]).unwrap(), newer);
        assert!(git(&a, &["rev-parse", "--verify", "refs/closed/T-B-1"]).is_err());
    }

    #[test]
    fn close_receipt_atomically_retains_the_claim_and_agent_index() {
        let (_r, a, _) = Repos::new("atomic-close-owner");
        let task = addt!(&a, "claude", "atomic close", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &task.id, "claude", 4, now_unix()).unwrap();
        sync_queue(&a).unwrap();
        let (old, owner) = read_claim(&a, &task.id).unwrap();
        push_close_receipt(&a, &task.id, &old, &owner).unwrap();
        let (retained, updated) = read_claim(&a, &task.id).unwrap();
        assert_ne!(
            retained, old,
            "a real update must bind receipt publication to ownership"
        );
        assert_eq!(updated.lease_until_unix, owner.lease_until_unix + 1);
        assert_eq!(
            git(&a, &["rev-parse", "refs/claim-agents/CLAUDE"]).unwrap(),
            retained
        );
        assert_eq!(read_closed(&a, &task.id).unwrap().unwrap().agent, "CLAUDE");
        git(&a, &["pack-refs", "--all", "--prune"]).unwrap();
        assert_eq!(claims_synced(&a).unwrap(), vec![updated]);
    }

    #[test]
    fn close_receipt_refuses_ownership_changed_during_acceptance() {
        let (_r, a, b) = Repos::new("atomic-close-race");
        let task = addt!(&a, "claude", "atomic close", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &task.id, "claude", 4, now_unix()).unwrap();
        sync_queue(&a).unwrap();
        let (old, owner) = read_claim(&a, &task.id).unwrap();
        // Another worker takes the claim while this worker runs acceptance.
        let mut other = owner.clone();
        other.agent = "OTHER".to_string();
        let newer = blob_of(&b, &other).unwrap();
        git(
            &b,
            &[
                "push",
                "--quiet",
                "--force",
                "origin",
                &format!("{newer}:{}", claim_ref(&task.id)),
            ],
        )
        .unwrap();
        let err = push_close_receipt(&a, &task.id, &old, &owner).unwrap_err();
        assert!(
            err.to_string().contains("changed during acceptance"),
            "{err}"
        );
        assert!(git(&b, &["ls-remote", "origin", &closed_ref(&task.id)])
            .unwrap()
            .is_empty());
        assert!(git(&b, &["ls-remote", "origin", &claim_ref(&task.id)])
            .unwrap()
            .starts_with(&newer));
    }

    /// Publish this clone's commits on `origin/main`, the way a merge would:
    /// `published_on_main` looks for the close record in that tree.
    fn publish(ws: &Path) {
        git(ws, &["add", "-A"]).unwrap();
        git(ws, &["commit", "--quiet", "-m", "merge the branch"]).unwrap();
        git(ws, &["push", "--quiet", "origin", "HEAD:main"]).unwrap();
        git(ws, &["fetch", "--quiet", "origin"]).unwrap();
    }

    /// Write one record ref the way the merger or the verifier would, so the
    /// readers are exercised against what production pushes.
    fn put_ref(ws: &Path, name: &str, value: &serde_json::Value) {
        let blob = blob_of(ws, value).unwrap();
        git(
            ws,
            &["push", "--quiet", "origin", &format!("{blob}:{name}")],
        )
        .unwrap();
        git(ws, &["update-ref", name, &blob]).unwrap();
    }

    /// Seed a fresh producer snapshot in one real remote and local transaction.
    fn put_new_refs(ws: &Path, records: Vec<(String, serde_json::Value)>) {
        let mut leases = Vec::new();
        let mut updates = Vec::new();
        for (name, value) in records {
            leases.push((name.clone(), String::new()));
            updates.push((name, Some(blob_of(ws, &value).unwrap())));
        }
        queue_relay::push(ws, "origin", &leases, &updates, "fixture refs raced").unwrap();
        mirror_updates(ws, &leases, &updates).unwrap();
    }

    /// Rewrite the acceptance in a `done/` record: a task that passed on its
    /// branch, and does (or cannot) run later on the merged tree.
    fn rewrite_accept(ws: &Path, id: &str, argv: serde_json::Value) {
        let path = done_dir(ws).join(format!("{id}.json"));
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        doc["accept"]["cmd"] = argv;
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    }

    fn write_roadmap(ws: &Path, ids: &[(&str, &str)]) {
        std::fs::create_dir_all(ws.join(".agents")).unwrap();
        let vectors: Vec<serde_json::Value> = ids
            .iter()
            .map(
                |(id, p)| serde_json::json!({"id": id, "priority": p, "vector": format!("v {id}")}),
            )
            .collect();
        std::fs::write(
            ws.join(".agents/roadmap.json"),
            serde_json::json!({"vectors": vectors}).to_string(),
        )
        .unwrap();
    }

    /// The same helper, with each vector's own status narrative — which is
    /// where a vector says whether it is delivered.
    fn write_roadmap_with_progress(ws: &Path, ids: &[(&str, &str, &str)]) {
        std::fs::create_dir_all(ws.join(".agents")).unwrap();
        let vectors: Vec<serde_json::Value> = ids
            .iter()
            .map(|(id, p, progress)| {
                serde_json::json!({"id": id, "priority": p, "vector": format!("v {id}"),
                                   "progress": progress})
            })
            .collect();
        std::fs::write(
            ws.join(".agents/roadmap.json"),
            serde_json::json!({"vectors": vectors}).to_string(),
        )
        .unwrap();
    }

    /// Coverage says the vector was worked on and the work was closed; mastery
    /// says the capability is proven. They were the same number until this:
    /// all 102 vectors read as delivered while ten still had open gaps.
    #[test]
    fn roadmap_coverage_is_not_mastery() {
        let (r, a, _) = Repos::new("rmmastery");
        write_roadmap_with_progress(
            &a,
            &[
                ("VC-201-001", "P0", "DELIVERED: verified by a real test"),
                (
                    "VC-201-002",
                    "P1",
                    "PARTIAL: the mechanism landed, the capability did not",
                ),
            ],
        );
        let claimed = linked(&a, "VC-201-001").unwrap();
        let partial = linked(&a, "VC-201-002").unwrap();
        claim(&a, &claimed.id, "claude", 1, now_unix()).unwrap();
        close(&a, &claimed.id, "claude").unwrap();
        // The vector's coverage is what this test reads, not the merge: give the
        // accepted tasks up explicitly rather than pretending they published.
        abandon(&a, &claimed.id, "claude", "coverage fixture").unwrap();
        claim(&a, &partial.id, "claude", 1, now_unix()).unwrap();
        close(&a, &partial.id, "claude").unwrap();
        abandon(&a, &partial.id, "claude", "coverage fixture").unwrap();

        let cov = roadmap_coverage(
            &roadmap_vectors(&a).unwrap(),
            &list_open(&a),
            &list_done(&a),
        );
        // Both are "delivered" by coverage: every linked task is closed.
        assert!(cov[0].delivered() && cov[1].delivered());
        // Only one of them claims the capability.
        assert!(cov[0].mastery_claimed() && !cov[1].mastery_claimed());
        // And the unclaimed one is the queue gap: nothing is left to close it.
        assert!(!cov[0].unqueued() && cov[1].unqueued());
        drop(r);
    }

    fn linked(ws: &Path, vector: &str) -> EaiResult<Task> {
        add(
            ws,
            "claude",
            NewTask {
                title: "t".into(),
                goal: String::new(),
                size: "s".into(),
                deps: vec![],
                accept: true_cmd(),
                roadmap: Some(vector.to_string()),
            },
        )
    }

    #[test]
    fn roadmap_link_is_validated_against_roadmap_json() {
        let (r, a, _) = Repos::new("rmlink");
        assert!(linked(&a, "VC-201-001").is_err(), "no roadmap.json yet");
        write_roadmap(&a, &[("VC-201-001", "P0")]);
        let t = linked(&a, "VC-201-001").unwrap();
        assert_eq!(t.roadmap.as_deref(), Some("VC-201-001"));
        assert!(linked(&a, "VC-201-999")
            .unwrap_err()
            .to_string()
            .contains("does not exist"));
        assert!(linked(&a, "vc-1")
            .unwrap_err()
            .to_string()
            .contains("invalid roadmap vector"));
        assert!(
            valid_vector_id("VC-200-001")
                && !valid_vector_id("VC--1")
                && !valid_vector_id("VC-1-x")
        );
        drop(r);
    }

    #[test]
    fn roadmap_link_coverage_joins_open_and_closed_tasks() {
        let (r, a, _) = Repos::new("rmcov");
        write_roadmap(
            &a,
            &[
                ("VC-201-001", "P0"),
                ("VC-201-002", "P1"),
                ("VC-201-003", "P1"),
            ],
        );
        let t1 = linked(&a, "VC-201-001").unwrap();
        let t2 = linked(&a, "VC-201-001").unwrap();
        let t3 = linked(&a, "VC-201-002").unwrap();
        claim(&a, &t2.id, "claude", 1, now_unix()).unwrap();
        close(&a, &t2.id, "claude").unwrap();
        abandon(&a, &t2.id, "claude", "coverage fixture").unwrap();
        let cov = roadmap_coverage(
            &roadmap_vectors(&a).unwrap(),
            &list_open(&a),
            &list_done(&a),
        );
        assert_eq!(cov.len(), 3);
        assert_eq!(
            (cov[0].open.clone(), cov[0].closed.clone()),
            (vec![t1.id.clone()], vec![t2.id.clone()])
        );
        assert!(!cov[0].uncovered() && !cov[0].delivered());
        assert_eq!(cov[1].open, vec![t3.id.clone()]);
        assert!(
            cov[2].uncovered(),
            "a vector nobody linked a task to is uncovered"
        );
        // Delivered once every linked task is closed.
        claim(&a, &t1.id, "claude", 1, now_unix()).unwrap();
        close(&a, &t1.id, "claude").unwrap();
        let cov = roadmap_coverage(
            &roadmap_vectors(&a).unwrap(),
            &list_open(&a),
            &list_done(&a),
        );
        assert!(cov[0].delivered());
        drop(r);
    }

    #[test]
    fn missing_accept_command_is_reported_as_not_done() {
        let (r, a, _) = Repos::new("missing");
        let t = addt!(
            &a,
            "claude",
            "needs a script",
            "",
            "s",
            &[],
            vec!["scripts/does-not-exist-yet.sh".to_string()]
        )
        .unwrap();
        claim(&a, &t.id, "claude", 1, now_unix()).unwrap();
        let err = close(&a, &t.id, "claude").unwrap_err().to_string();
        assert!(err.contains("is not done"), "{err}");
        assert!(
            err.contains("could not run") || err.contains("exited"),
            "{err}"
        );
        assert!(err.contains("scripts/does-not-exist-yet.sh"), "{err}");
        assert_eq!(list_open(&a).len(), 1);
        drop(r);
    }

    #[test]
    fn tests_passed_sums_result_lines_and_treats_zero_as_vacuous() {
        let out = "running 2 tests\ntest result: ok. 2 passed; 0 failed; 0 ignored\n\
                   running 0 tests\ntest result: ok. 0 passed; 0 failed\n\
                   test result: ok. 3 passed; 0 failed";
        assert_eq!(tests_passed(out), 5);
        assert_eq!(
            tests_passed("running 0 tests\ntest result: ok. 0 passed; 0 failed"),
            0
        );
        assert_eq!(tests_passed("no test output at all"), 0);
    }

    #[test]
    fn a_cargo_test_check_that_matches_nothing_does_not_close_the_task() {
        let (r, a, _) = Repos::new("vacuous");
        // A one-test crate, so `cargo test nomatch` exits 0 having run nothing.
        // Empty `[workspace]` keeps cargo from walking up into the parent
        // checkout's Cargo.toml when `TMP` lives under `.git/susi-hermetic`.
        std::fs::write(
            a.join("Cargo.toml"),
            "[package]\nname = \"vac\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::create_dir_all(a.join("src")).unwrap();
        std::fs::write(a.join("src/lib.rs"), "#[test]\nfn real() {}\n").unwrap();
        let cmd = |filter: &str| {
            let mut cmd = ["cargo", "test", "--offline", "--quiet", filter]
                .map(String::from)
                .to_vec();
            cmd.extend([
                "--target-dir".into(),
                a.join("accept-target").display().to_string(),
            ]);
            cmd
        };
        let vacuous = addt!(&a, "claude", "vacuous", "", "s", &[], cmd("nomatch")).unwrap();
        claim(&a, &vacuous.id, "claude", 1, now_unix()).unwrap();
        let err = close(&a, &vacuous.id, "claude").unwrap_err().to_string();
        assert!(err.contains("ran no tests"), "{err}");
        assert_eq!(list_open(&a).len(), 1);

        let real = addt!(&a, "claude", "real", "", "s", &[], cmd("real")).unwrap();
        release(&a, &vacuous.id, "claude", false).unwrap();
        claim(&a, &real.id, "claude", 1, now_unix()).unwrap();
        close(&a, &real.id, "claude").unwrap();
        assert_eq!(list_done(&a).len(), 1);
        drop(r);
    }

    #[test]
    fn ids_and_acceptance_commands_are_validated() {
        assert!(valid_id("T-CLAUDE-3"));
        assert!(valid_id("T-A1-10"));
        for bad in [
            "T-claude-3",
            "T--3",
            "T-CLAUDE-",
            "T-CLAUDE-x",
            "X-A-1",
            "T-A-1/../x",
            "../T-A-1",
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
        let c = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(accept_allowed(&c(&["cargo", "test", "-p", "x"])));
        assert!(accept_allowed(&c(&["scripts/check.sh"])));
        assert!(!accept_allowed(&c(&["rm", "-rf", "/"])));
        assert!(!accept_allowed(&c(&["scripts/../evil.sh"])));
        assert!(!accept_allowed(&c(&["bash", "-c", "x"])));
        assert!(!accept_allowed(&[]));
    }

    #[test]
    fn add_numbers_per_agent_and_rejects_bad_input() {
        let (r, a, _) = Repos::new("add");
        let t1 = addt!(&a, "claude", "one", "", "s", &[], true_cmd()).unwrap();
        let t2 = addt!(
            &a,
            "claude",
            "two",
            "",
            "m",
            std::slice::from_ref(&t1.id),
            true_cmd()
        )
        .unwrap();
        let d1 = addt!(&a, "devin", "three", "", "l", &[], true_cmd()).unwrap();
        assert_eq!(
            (t1.id.as_str(), t2.id.as_str(), d1.id.as_str()),
            ("T-CLAUDE-1", "T-CLAUDE-2", "T-DEVIN-1")
        );
        assert!(addt!(&a, "claude", "", "", "s", &[], true_cmd()).is_err());
        assert!(addt!(&a, "claude", "x", "", "xl", &[], true_cmd()).is_err());
        assert!(addt!(&a, "claude", "x", "", "s", &[], vec![]).is_err());
        assert!(addt!(&a, "claude", "x", "", "s", &["T-NOPE-9".into()], true_cmd()).is_err());
        assert_eq!(list_open(&a).len(), 3);
        drop(r);
    }

    #[test]
    fn renewal_keeps_scope_ownership_and_rejects_other_agents() {
        let (_r, a, _) = Repos::new("renewal");
        let task = addt!(&a, "claude", "job", "", "s", &[], true_cmd()).unwrap();
        let now = now_unix();
        claim_scoped(
            &a,
            &task.id,
            "claude",
            ClaimOptions {
                hours: 1,
                now,
                scopes: &["src".into()],
            },
        )
        .unwrap();
        assert!(renew(&a, &task.id, "devin", 1, now + 10).is_err());
        let renewed = renew(&a, &task.id, "claude", 2, now + 10).unwrap();
        assert_eq!(renewed.lease_until_unix, now + 10 + 7200);
        assert_eq!(renewed.scopes, vec!["src"]);
        assert!(renew(&a, &task.id, "claude", 1, now + 8000).is_err());
        release(&a, &task.id, "claude", false).unwrap();
    }

    #[test]
    fn task_record_scopes_do_not_serialize_the_swarm() {
        let wide = [".agents/tasks".to_string()];
        let one = [".agents/tasks/T-X-1.json".to_string()];
        assert!(!scopes_conflict(&wide, &one));
        assert!(!scopes_conflict(&one, &wide));
        let other = [".agents/tasks/T-Y-2.json".to_string()];
        assert!(!scopes_conflict(&one, &other));
        let code = ["crates/a".to_string()];
        let code_file = ["crates/a/src/lib.rs".to_string()];
        assert!(scopes_conflict(&code, &code_file));
        let cli = ["src/cli".to_string()];
        let cli_file = ["src/cli/keys.rs".to_string()];
        assert!(scopes_conflict(&cli, &cli_file));
    }
    #[test]
    fn scopes_reject_nested_overlap_and_allow_disjoint_work() {
        let (_r, a, b) = Repos::new("scopes");
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        let second = addt!(&b, "devin", "second", "", "s", &[], true_cmd()).unwrap();
        let now = now_unix();
        claim_scoped(
            &a,
            &first.id,
            "claude",
            ClaimOptions {
                hours: 1,
                now,
                scopes: &["src/cli".into()],
            },
        )
        .unwrap();
        assert!(claim_scoped(
            &b,
            &second.id,
            "devin",
            ClaimOptions {
                hours: 1,
                now,
                scopes: &["src/cli/tasks.rs".into()]
            }
        )
        .is_err());
        assert!(claim_scoped(
            &b,
            &second.id,
            "devin",
            ClaimOptions {
                hours: 1,
                now,
                scopes: &["src".into()]
            }
        )
        .is_err());
        assert!(claim_scoped(
            &b,
            &second.id,
            "devin",
            ClaimOptions {
                hours: 1,
                now,
                scopes: &["../src".into()]
            }
        )
        .is_err());
        claim_scoped(
            &b,
            &second.id,
            "devin",
            ClaimOptions {
                hours: 1,
                now,
                scopes: &["src/client".into()],
            },
        )
        .unwrap();
        // The commit checker enforces the reservation at the write boundary.
        std::fs::create_dir_all(a.join("src/cli")).unwrap();
        std::fs::write(a.join("src/cli/test.rs"), "allowed").unwrap();
        git(&a, &["add", "src/cli/test.rs"]).unwrap();
        let checker =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check-task-scope.py");
        let checked = || {
            Command::new("python3")
                .arg(&checker)
                .arg(&first.id)
                .current_dir(&a)
                .output()
                .unwrap()
                .status
                .success()
        };
        assert!(checked());
        std::fs::write(a.join("outside.rs"), "outside scope").unwrap();
        git(&a, &["add", "outside.rs"]).unwrap();
        assert!(!checked());
        release(&a, &first.id, "claude", false).unwrap();
        release(&b, &second.id, "devin", false).unwrap();
    }

    /// A live six-way test on the real remote found this: the generation ref is
    /// one global ref, so six agents claiming six *different* tasks at the same
    /// instant collided on it — one won, five were told to refresh and retry for
    /// a conflict that was nobody's. Six starting together is the normal case.
    #[test]
    fn six_agents_claiming_six_different_tasks_at_once_all_win() {
        let (_r, a, _b) = Repos::new("six-way");
        let ids: Vec<String> = (1..=6)
            .map(|_| {
                addt!(&a, "claude", "task", "", "s", &[], true_cmd())
                    .unwrap()
                    .id
            })
            .collect();
        let now = now_unix();
        let ws: &Path = &a;
        let results: Vec<EaiResult<Claim>> = std::thread::scope(|scope| {
            let handles: Vec<_> = ids
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let agent = format!("AGENT{i}");
                    scope.spawn(move || {
                        claim_scoped(
                            ws,
                            id,
                            &agent,
                            ClaimOptions {
                                hours: 1,
                                now,
                                scopes: &[],
                            },
                        )
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let failures: Vec<String> = results
            .iter()
            .filter_map(|r| r.as_ref().err().map(std::string::ToString::to_string))
            .collect();
        assert!(
            failures.is_empty(),
            "six disjoint claims must all win, nothing else was claimed: {failures:?}"
        );
        assert_eq!(claims(&a).unwrap().len(), 6);
    }

    /// The same race for *one* task must still have exactly one winner, with the
    /// retry in place: mutual exclusion is the property, not speed.
    #[test]
    fn six_agents_racing_for_one_task_still_produce_one_winner() {
        let (_r, a, _b) = Repos::new("six-race");
        let task = addt!(&a, "claude", "one", "", "s", &[], true_cmd()).unwrap();
        let now = now_unix();
        let ws: &Path = &a;
        let results: Vec<EaiResult<Claim>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..6)
                .map(|i| {
                    let agent = format!("AGENT{i}");
                    let id = task.id.clone();
                    scope.spawn(move || {
                        claim_scoped(
                            ws,
                            &id,
                            &agent,
                            ClaimOptions {
                                hours: 1,
                                now,
                                scopes: &[],
                            },
                        )
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let winners = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(winners, 1, "exactly one agent wins one task");
    }

    #[test]
    fn one_agent_cannot_claim_two_tasks_across_clones() {
        let (_r, a, b) = Repos::new("agent-race");
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        let second = addt!(&b, "claude", "second", "", "s", &[], true_cmd()).unwrap();
        // Distinct ids are needed: both clones start with their own counter.
        let second_id = "T-DEVIN-1";
        let mut second = second;
        second.id = second_id.into();
        std::fs::write(
            tasks_dir(&b).join(format!("{second_id}.json")),
            serde_json::to_vec(&second).unwrap(),
        )
        .unwrap();
        let now = now_unix();
        let (left, right) = std::thread::scope(|scope| {
            let left = scope.spawn(|| claim(&a, &first.id, "worker", 1, now));
            let right = scope.spawn(|| claim(&b, second_id, "worker", 1, now));
            (left.join().unwrap(), right.join().unwrap())
        });
        assert_ne!(
            left.is_ok(),
            right.is_ok(),
            "exactly one atomic agent lease wins"
        );
        let winner = left.or(right).unwrap();
        let ws = if winner.task == first.id { &a } else { &b };
        release(ws, &winner.task, "worker", false).unwrap();
        assert!(claims(ws).unwrap().is_empty());
        assert!(git(ws, &["ls-remote", "origin", "refs/claim-agents/*"])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn only_one_of_two_racing_agents_gets_the_claim() {
        let (r, a, b) = Repos::new("race");
        let t = addt!(&a, "claude", "job", "", "s", &[], true_cmd()).unwrap();
        // Share the task file with the second clone (as a merged commit would).
        std::fs::create_dir_all(tasks_dir(&b)).unwrap();
        std::fs::copy(
            tasks_dir(&a).join(format!("{}.json", t.id)),
            tasks_dir(&b).join(format!("{}.json", t.id)),
        )
        .unwrap();
        let now = now_unix();
        let first = claim(&a, &t.id, "claude", 4, now).unwrap();
        assert_eq!(first.agent, "CLAUDE");
        let second = claim(&b, &t.id, "devin", 4, now).unwrap_err().to_string();
        assert!(second.contains("claimed by CLAUDE"), "{second}");
        let seen = claims(&b).unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].agent, "CLAUDE");
        drop(r);
    }

    #[test]
    fn an_owned_claim_can_atomically_expand_its_scope_on_the_same_branch() {
        let (_r, a, b) = Repos::new("expand-scope");
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        let second = addt!(&b, "devin", "second", "", "s", &[], true_cmd()).unwrap();
        let now = now_unix();
        claim_scoped(
            &a,
            &first.id,
            "claude",
            ClaimOptions {
                hours: 1,
                now,
                scopes: &["src".into()],
            },
        )
        .unwrap();

        let expanded = claim_scoped(
            &a,
            &first.id,
            "claude",
            ClaimOptions {
                hours: 2,
                now: now + 10,
                scopes: &["tests".into(), "src".into()],
            },
        )
        .unwrap();
        assert_eq!(expanded.scopes, vec!["src", "tests"]);
        assert_eq!(expanded.lease_until_unix, now + 10 + 7200);
        assert_eq!(
            claims(&a)
                .unwrap()
                .into_iter()
                .find(|claim| claim.task == first.id)
                .unwrap()
                .scopes,
            vec!["src", "tests"]
        );

        assert!(claim_scoped(
            &b,
            &second.id,
            "devin",
            ClaimOptions {
                hours: 1,
                now: now + 20,
                scopes: &["tests/ui".into()],
            }
        )
        .is_err());
    }

    #[test]
    fn a_stale_claim_is_taken_over_and_release_is_owner_only() {
        let (r, a, b) = Repos::new("lease");
        let t = addt!(&a, "claude", "job", "", "s", &[], true_cmd()).unwrap();
        std::fs::create_dir_all(tasks_dir(&b)).unwrap();
        std::fs::copy(
            tasks_dir(&a).join(format!("{}.json", t.id)),
            tasks_dir(&b).join(format!("{}.json", t.id)),
        )
        .unwrap();
        let t0 = 1_000_000;
        claim(&a, &t.id, "claude", 1, t0).unwrap();
        // Lease (1h) lapsed: another agent may take it.
        let taken = claim(&b, &t.id, "devin", 1, t0 + 3_601).unwrap();
        assert_eq!(taken.agent, "DEVIN");
        // Only the holder releases without force.
        assert!(release(&a, &t.id, "claude", false)
            .unwrap_err()
            .to_string()
            .contains("not you"));
        release(&b, &t.id, "devin", false).unwrap();
        assert!(claims(&a).unwrap().is_empty());
        drop(r);
    }

    /// Releasing is how a claim stops protecting work, so it is bound to the
    /// branch the claim was taken on: two workers that share an agent token
    /// (same `SUSI_AGENT`, or `codex-1`/`codex1` both tokenising to `CODEX1`)
    /// must not be able to free each other's live claim. Recovery from a lost
    /// worktree stays possible, deliberately, through `--force`.
    #[test]
    fn release_is_bound_to_the_branch_that_claimed_unless_forced() {
        let (r, a, _b) = Repos::new("release-branch");
        let t = addt!(&a, "claude", "job", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &t.id, "claude", 4, now_unix()).unwrap();

        git(&a, &["switch", "--quiet", "-c", "elsewhere"]).unwrap();
        let err = release(&a, &t.id, "claude", false).unwrap_err().to_string();
        assert!(err.contains("branch"), "{err}");
        // Still held: the claim was not freed.
        assert!(claims(&a).unwrap().iter().any(|c| c.task == t.id));

        release(&a, &t.id, "claude", true).unwrap();
        assert!(claims(&a).unwrap().is_empty());
        drop(r);
    }

    /// Taking over an expired claim used to leave the previous holder's
    /// `refs/claim-agents/<AGENT>` pointing at the claim it no longer holds.
    #[test]
    fn taking_over_an_expired_claim_clears_the_previous_holder() {
        let (r, a, b) = Repos::new("takeover-ref");
        let t = addt!(&a, "claude", "job", "", "s", &[], true_cmd()).unwrap();
        std::fs::create_dir_all(tasks_dir(&b)).unwrap();
        std::fs::copy(
            tasks_dir(&a).join(format!("{}.json", t.id)),
            tasks_dir(&b).join(format!("{}.json", t.id)),
        )
        .unwrap();
        let t0 = 2_000_000;
        claim(&a, &t.id, "claude", 1, t0).unwrap();
        assert!(git(&a, &["ls-remote", "origin", "refs/claim-agents/*"])
            .unwrap()
            .contains("refs/claim-agents/CLAUDE"));

        claim(&b, &t.id, "devin", 1, t0 + 3_601).unwrap();
        let refs = git(&b, &["ls-remote", "origin", "refs/claim-agents/*"]).unwrap();
        assert!(
            !refs.contains("refs/claim-agents/CLAUDE"),
            "the dispossessed holder's ref must not outlive its claim: {refs}"
        );
        assert!(refs.contains("refs/claim-agents/DEVIN"), "{refs}");
        drop(r);
    }

    /// The review feared a dispossessed holder would then be locked out of
    /// every new claim by its own stale agent ref. It is not: that ref can only
    /// hold an expired lease (a live claim is only replaced by a takeover after
    /// expiry), so the same-agent check lets it through. This pins that down.
    #[test]
    fn a_dispossessed_agent_can_still_claim_another_task() {
        let (r, a, b) = Repos::new("dispossessed");
        let x = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        // `b` needs the task file too, to be able to take the claim over.
        std::fs::create_dir_all(tasks_dir(&b)).unwrap();
        std::fs::copy(
            tasks_dir(&a).join(format!("{}.json", x.id)),
            tasks_dir(&b).join(format!("{}.json", x.id)),
        )
        .unwrap();
        let t0 = 3_000_000;
        claim(&a, &x.id, "claude", 1, t0).unwrap();
        claim(&b, &x.id, "devin", 1, t0 + 3_601).unwrap();

        // CLAUDE lost the claim but holds nothing now, so it can take new work.
        let y = addt!(&a, "claude", "second", "", "s", &[], true_cmd()).unwrap();
        let got = claim(&a, &y.id, "claude", 1, now_unix()).unwrap();
        assert_eq!(got.agent, "CLAUDE");
        drop(r);
    }

    /// The dependency gate reads task files from the checkout, so an
    /// uncommitted edit to a *tracked* one must not open it — deleting the task
    /// you depend on is otherwise a way to unblock yourself without doing the
    /// dependency. This lives in `ensure_synced`, which the CLI claims through
    /// (`tasks_cli.rs`) and the autonomous builder calls directly; untracked
    /// additions stay fine, because that is what `tasks add` writes.
    #[test]
    fn ensure_synced_refuses_uncommitted_changes_to_tracked_task_files() {
        let (r, a, _b) = Repos::new("queue-dirty");
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        git(&a, &["add", "-A"]).unwrap();
        git(&a, &["commit", "--quiet", "-m", "queue"]).unwrap();
        git(&a, &["push", "--quiet", "origin", "HEAD:main"]).unwrap();
        git(&a, &["fetch", "--quiet", "origin"]).unwrap();

        // A new, untracked task file is the normal `add` flow: allowed.
        addt!(
            &a,
            "claude",
            "second",
            "",
            "s",
            std::slice::from_ref(&first.id),
            true_cmd()
        )
        .unwrap();
        ensure_synced(&a).unwrap();

        // Deleting a tracked task file in the working tree is not.
        std::fs::remove_file(tasks_dir(&a).join(format!("{}.json", first.id))).unwrap();
        let err = ensure_synced(&a).unwrap_err().to_string();
        assert!(
            err.contains("uncommitted change(s) to tracked task files"),
            "{err}"
        );
        drop(r);
    }

    #[test]
    fn a_task_with_an_open_dependency_cannot_be_claimed() {
        let (r, a, _) = Repos::new("deps");
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        let second = addt!(
            &a,
            "claude",
            "second",
            "",
            "s",
            std::slice::from_ref(&first.id),
            true_cmd(),
        )
        .unwrap();
        let err = claim(&a, &second.id, "claude", 1, now_unix())
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked"), "{err}");
        drop(r);
    }

    /// A link at `link` to the directory `to`: a junction on Windows (what
    /// the old linker made, and what any user can create), a symlink elsewhere.
    fn link_dir(to: &Path, link: &Path) {
        #[cfg(windows)]
        {
            let native = |p: &Path| p.to_string_lossy().replace('/', "\\");
            let out = Command::new("cmd")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(native(link))
                .arg(native(to))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "mklink /J: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        #[cfg(not(windows))]
        std::os::unix::fs::symlink(to, link).unwrap();
    }

    /// `tasks close` runs a cargo acceptance as itself. Worktrees used to
    /// share one cargo target and the acceptance ran inside
    /// `scripts/target-guard.sh --run`; that script is retired (kept one
    /// release for the installed susi), and a checkout that carries the
    /// seeding must not have it run. A worktree whose `target/` is still a
    /// link to the old shared target gets its own first
    /// (`scripts/seed-target.sh`), and what the link pointed at is never
    /// touched. So does one whose target is missing or empty: a hook's
    /// background copy may be filling it, and the seeder waits for that copy
    /// rather than let the acceptance build cold beside it. A target that
    /// holds a build runs no seeder at all.
    #[test]
    fn close_runs_a_cargo_acceptance_unwrapped_after_seeding_a_linked_target() {
        if cfg!(windows) && !Path::new(posix_shell_command("bash").get_program()).is_absolute() {
            return; // no Git shell on this host: seeding is skipped by design
        }
        let (r, a, _) = Repos::new("unwrapped");
        std::fs::create_dir_all(a.join("scripts")).unwrap();
        // A guard that fails, and leaves a mark, if anything runs it.
        std::fs::write(
            a.join("scripts").join("target-guard.sh"),
            "touch \"$(git rev-parse --show-toplevel)/guard-ran\"\nexit 97\n",
        )
        .unwrap();
        // A seeder that replaces a link with a real directory, as the real
        // one does (the link only), says whom it was run for, and counts its
        // runs outside the target.
        std::fs::write(
            a.join("scripts").join("seed-target.sh"),
            "printf '%s\\n' \"$1\" >> \"$1/seed-runs\"\n\
             [ ! -L \"$1/target\" ] || rm -f \"$1/target\"\n\
             [ -d \"$1/target\" ] || mkdir \"$1/target\"\n\
             printf '%s' \"$1\" > \"$1/target/seeded-for\"\n",
        )
        .unwrap();
        let shared = a.parent().unwrap().join("shared-target");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("sentinel"), "another worktree's build").unwrap();
        link_dir(&shared, &a.join("target"));
        let t = addt!(&a, "claude", "unwrapped", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &t.id, "claude", 1, now_unix()).unwrap();
        close(&a, &t.id, "claude").expect("a passing cargo acceptance closes the task");
        assert!(
            !a.join("guard-ran").exists(),
            "the acceptance must not run through the retired guard"
        );
        let target = a.join("target");
        let meta = target.symlink_metadata().unwrap();
        assert!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "the linked target was replaced by one of the worktree's own"
        );
        assert!(
            target.join("seeded-for").is_file(),
            "the worktree's own seeder ran before the acceptance"
        );
        assert_eq!(
            std::fs::read_to_string(shared.join("sentinel")).unwrap(),
            "another worktree's build",
            "what the link pointed at is never touched"
        );
        let runs = || {
            std::fs::read_to_string(a.join("seed-runs"))
                .unwrap_or_default()
                .lines()
                .count()
        };
        assert_eq!(runs(), 1);
        // A target that holds a build is this worktree's own: no seeder.
        seed_own_target(&a);
        assert_eq!(runs(), 1, "a target holding a build runs no seeder");
        // Empty, or with only cargo's tag files, or missing: a background
        // copy may be filling it, and the seeder waits for that copy.
        std::fs::remove_file(target.join("seeded-for")).unwrap();
        seed_own_target(&a);
        assert_eq!(runs(), 2, "an empty target is seeded");
        assert!(target.join("seeded-for").is_file());
        std::fs::remove_file(target.join("seeded-for")).unwrap();
        std::fs::write(target.join("CACHEDIR.TAG"), "Signature").unwrap();
        seed_own_target(&a);
        assert_eq!(runs(), 3, "cargo's tag file alone is no build");
        std::fs::remove_dir_all(&target).unwrap();
        seed_own_target(&a);
        assert_eq!(runs(), 4, "a missing target is seeded");
        assert!(target.join("seeded-for").is_file());
        drop(r);
    }

    /// For one release a branch cut before the seeding, which has not merged
    /// `origin/main` since, still has the real `scripts/target-guard.sh` and
    /// no `scripts/seed-target.sh`, and its `target/` is the clone's shared
    /// one. The new `tasks close` runs its cargo acceptance through that
    /// guard, as v0.21.23 did: run plainly, it could test a sibling's build
    /// of a crate in place of this branch's. A branch that carries the
    /// seeding never runs its guard (the pass-through), nor does a checkout
    /// with no guard.
    #[test]
    fn an_old_branchs_cargo_acceptance_runs_inside_its_own_guard() {
        if Command::new(posix_shell("bash"))
            .arg("--version")
            .output()
            .is_err()
        {
            return; // no bash on this host: the acceptance runs unguarded by design
        }
        if cfg!(windows) && !Path::new(posix_shell_command("bash").get_program()).is_absolute() {
            return; // no Git shell: the acceptance runs unguarded by design
        }
        let (r, a, _) = Repos::new("oldguard");
        std::fs::create_dir_all(a.join("scripts")).unwrap();
        // The old branch's guard: it runs the acceptance, inside, and leaves
        // a mark; it calls external tools, as the real one does.
        std::fs::write(
            a.join("scripts").join("target-guard.sh"),
            "#!/usr/bin/env bash\n\
             [ \"$1\" = --run ] || exit 9\n\
             shift\n\
             [ \"$1\" != -- ] || shift\n\
             mkdir -p \"$(git rev-parse --show-toplevel)/guard\" && touch \"$(git rev-parse --show-toplevel)/guard/ran\"\n\
             exec \"$@\"\n",
        )
        .unwrap();
        let shared = a.parent().unwrap().join("shared-target");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("sentinel"), "another worktree's build").unwrap();
        link_dir(&shared, &a.join("target"));
        let argv = ["git".to_string(), "--version".to_string()];
        let out = guarded_acceptance(&a, &argv)
            .expect("an old branch runs its acceptance through its own guard")
            .current_dir(&a)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).starts_with("git version"),
            "the acceptance itself must run, inside the guard"
        );
        assert!(a.join("guard").join("ran").is_file(), "the guard must run");
        std::fs::remove_dir_all(a.join("guard")).unwrap();
        // Through `close`: the cargo acceptance passes inside the guard, and
        // nothing converts the link (that branch has no seeder to do it).
        let t = addt!(&a, "claude", "old branch", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &t.id, "claude", 1, now_unix()).unwrap();
        close(&a, &t.id, "claude").expect("a passing cargo acceptance closes the task");
        assert!(
            a.join("guard").join("ran").is_file(),
            "close must run an old branch's cargo acceptance inside its guard"
        );
        assert!(a
            .join("target")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(shared.join("sentinel")).unwrap(),
            "another worktree's build"
        );
        // A branch that carries the seeding: its guard is the pass-through,
        // and is never run.
        std::fs::write(a.join("scripts").join("seed-target.sh"), "exit 0\n").unwrap();
        assert!(guarded_acceptance(&a, &argv).is_none());
        // A checkout without the guard runs its acceptance as it is.
        let bare = a.parent().unwrap().join("no-guard");
        std::fs::create_dir_all(&bare).unwrap();
        assert!(guarded_acceptance(&bare, &argv).is_none());
        drop(r);
    }

    #[test]
    fn close_requires_the_acceptance_check_to_pass() {
        let (r, a, _) = Repos::new("close");
        let failing = addt!(
            &a,
            "claude",
            "nope",
            "",
            "s",
            &[],
            vec!["cargo".into(), "definitely-not-a-subcommand".into()],
        )
        .unwrap();
        claim(&a, &failing.id, "claude", 1, now_unix()).unwrap();
        let err = close(&a, &failing.id, "claude").unwrap_err().to_string();
        assert!(err.contains("not done"), "{err}");
        assert_eq!(
            list_open(&a).len(),
            1,
            "a failed check must leave the task open"
        );
        assert!(list_done(&a).is_empty());

        let passing = addt!(&a, "claude", "yes", "", "s", &[], true_cmd()).unwrap();
        release(&a, &failing.id, "claude", false).unwrap();
        claim(&a, &passing.id, "claude", 1, now_unix()).unwrap();
        let done = close(&a, &passing.id, "claude").unwrap();
        assert_eq!(done.closed.as_ref().unwrap().by, "CLAUDE");
        assert!(!done.closed.unwrap().commit.is_empty());
        assert_eq!(list_open(&a).len(), 1);
        assert_eq!(list_done(&a).len(), 1);
        // Publication retains ownership until the closing commit is merged:
        // releasing by accident is refused while the close is unpublished.
        assert!(claims(&a).unwrap().iter().any(|c| c.task == passing.id));
        let err = release(&a, &passing.id, "claude", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("--abandon"), "{err}");
        assert!(
            claims(&a).unwrap().iter().any(|c| c.task == passing.id),
            "a refused release must leave the claim alone"
        );
        publish(&a);
        release(&a, &passing.id, "claude", false).unwrap();
        assert!(claims(&a).unwrap().is_empty());
        drop(r);
    }

    /// `finish` names the task it gated in `SUSI_ACCEPTANCE_COVERED` so close
    /// does not run the same cargo test twice. A covered acceptance that would
    /// fail if run still closes; a coverage flag naming a *different* task —
    /// or a non-cargo acceptance — still runs the check.
    #[test]
    fn a_covered_acceptance_is_skipped_but_only_for_the_named_task() {
        let (r, a, _) = Repos::new("covered-accept");
        let t = addt!(
            &a,
            "claude",
            "covered",
            "",
            "s",
            &[],
            vec![
                "cargo".into(),
                "test".into(),
                "--manifest-path".into(),
                "Cargo.toml".into(),
            ],
        )
        .unwrap();
        claim(&a, &t.id, "claude", 1, now_unix()).unwrap();
        // `cargo test` cannot pass in this fixture — no Cargo.toml — so a
        // successful close proves the run was skipped. The flag is bound to
        // `<id>@<worktree>`: the id alone is not unique across fixtures.
        std::env::set_var(
            "SUSI_ACCEPTANCE_COVERED",
            format!("{}@{}", t.id, a.display()),
        );
        let done = close(&a, &t.id, "claude");
        std::env::remove_var("SUSI_ACCEPTANCE_COVERED");
        assert!(done.is_ok(), "{:?}", done.err());
        assert_eq!(list_done(&a).len(), 1);
        // The closed task keeps its claim until the merge lands; the
        // pipeline's release frees it while the receipt keeps the debt.
        release(&a, &t.id, "claude", true).unwrap();

        let t2 = addt!(
            &a,
            "claude",
            "not-covered",
            "",
            "s",
            &[],
            vec![
                "cargo".into(),
                "test".into(),
                "--manifest-path".into(),
                "Cargo.toml".into(),
            ],
        )
        .unwrap();
        claim(&a, &t2.id, "claude", 1, now_unix()).unwrap();
        // Same task-id shape but a different id, same worktree: not covered.
        std::env::set_var(
            "SUSI_ACCEPTANCE_COVERED",
            format!("{}@{}", t.id, a.display()),
        );
        let err = close(&a, &t2.id, "claude");
        std::env::remove_var("SUSI_ACCEPTANCE_COVERED");
        assert!(err.unwrap_err().to_string().contains("not done"));
        assert_eq!(list_open(&a).len(), 1);
        drop(r);
    }

    /// `workflow check` reads the claims and the unpublished-close debt from one
    /// queue sync. The `*_synced` readers must answer from what that sync
    /// mirrored and fetch nothing of their own — the identical second fetch
    /// behind the first cost ~4 s of SSH setup — while the plain readers keep
    /// syncing for every other caller.
    #[test]
    fn the_synced_readers_use_the_mirror_and_the_plain_readers_still_fetch() {
        let (r, a, b) = Repos::new("synced-readers");
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &first.id, "claude", 4, now_unix()).unwrap();
        close(&a, &first.id, "claude").unwrap();

        sync_queue(&b).unwrap();
        assert_eq!(
            claims_synced(&b).unwrap().len(),
            claims(&b).unwrap().len(),
            "the same claims whether the reader fetched or not"
        );
        let synced = unpublished_closes_synced(&b, "claude").unwrap();
        assert_eq!(synced.len(), 1, "the debt is read from the mirror");
        assert_eq!(
            synced[0].task,
            unpublished_closes(&b, "claude").unwrap()[0].task
        );

        // A claim lands on the remote after the sync. The synced reader must not
        // go and fetch it; the plain one must.
        let second = addt!(&a, "other", "second", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &second.id, "other", 4, now_unix()).unwrap();
        assert!(
            !claims_synced(&b)
                .unwrap()
                .iter()
                .any(|c| c.task == second.id),
            "a synced reader that fetches again defeats the point of syncing once"
        );
        assert!(
            claims(&b).unwrap().iter().any(|c| c.task == second.id),
            "a plain reader must still sync for itself"
        );
        // An unusable agent is refused before anything is read, synced or not.
        assert!(unpublished_closes_synced(&b, "  ").is_err());
        drop(r);
    }

    /// The pipeline bound: an agent whose accepted tasks still await their
    /// merges may claim the next one — nobody idles on a merge queue they
    /// cannot hurry — up to `merges_in_flight()`; one more is the pileup the
    /// receipt exists to refuse. Neither releasing the claim nor a fresh clone
    /// launders the debt, and each merge that lands frees a slot.
    #[test]
    fn merges_in_flight_pipeline_up_to_the_bound_and_one_more_is_refused() {
        let (r, a, b) = Repos::new("owed-clone");
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &first.id, "claude", 4, now_unix()).unwrap();
        close(&a, &first.id, "claude").unwrap();

        // The acceptance is a fact on the remote: who, and which head.
        let listed = git(&a, &["ls-remote", "origin", "refs/closed/*"]).unwrap();
        assert!(
            listed.contains(&format!("refs/closed/{}", first.id)),
            "{listed}"
        );
        let owed = unpublished_closes(&b, "claude").unwrap();
        assert_eq!(owed.len(), 1, "a fresh clone sees the debt");
        assert_eq!(owed[0].agent, "CLAUDE");

        // Freeing the claim does not free the agent — the receipt outlives it,
        // which is exactly what lets `finish` hand a queued merge off without
        // losing the record.
        release(&b, &first.id, "claude", true).unwrap();
        assert!(claims(&b).unwrap().is_empty());
        assert_eq!(
            unpublished_closes(&b, "claude").unwrap().len(),
            1,
            "the receipt must outlive the claim"
        );

        // `next_id` counts the queue this clone can see; give it the close
        // record so the next task is a different id (as a sync would).
        std::fs::create_dir_all(done_dir(&b)).unwrap();
        std::fs::copy(
            done_dir(&a).join(format!("{}.json", first.id)),
            done_dir(&b).join(format!("{}.json", first.id)),
        )
        .unwrap();

        // The first complete lifecycle above proves receipt publication and
        // release. Seed the intermediate accepted history as one fixture
        // transaction, then exercise the real claim at the configured boundary
        // and one past it. Replaying that same lifecycle four more times made
        // this single boundary test launch hundreds of processes.
        let mut later = Vec::new();
        let mut leases = Vec::new();
        let mut updates = Vec::new();
        let head = git(&b, &["rev-parse", "HEAD"]).unwrap();
        for n in 1..merges_in_flight() {
            let mut next =
                addt!(&b, "claude", format!("next-{n}"), "", "s", &[], true_cmd()).unwrap();
            let receipt = CloseRecord {
                task: next.id.clone(),
                agent: "CLAUDE".into(),
                head: head.clone(),
                closed_unix: now_unix(),
                scopes: Vec::new(),
            };
            let name = closed_ref(&next.id);
            leases.push((name.clone(), String::new()));
            updates.push((name, Some(blob_of(&b, &receipt).unwrap())));
            next.closed = Some(Closed {
                at_unix: receipt.closed_unix,
                commit: head.clone(),
                by: "CLAUDE".into(),
            });
            std::fs::write(
                done_dir(&b).join(format!("{}.json", next.id)),
                serde_json::to_vec(&next).unwrap(),
            )
            .unwrap();
            std::fs::remove_file(tasks_dir(&b).join(format!("{}.json", next.id))).unwrap();
            later.push(next.id);
        }
        queue_relay::push(&b, "origin", &leases, &updates, "fixture receipts raced").unwrap();
        if merges_in_flight() > 0 {
            let boundary = addt!(&b, "claude", "at the bound", "", "s", &[], true_cmd()).unwrap();
            claim(&b, &boundary.id, "claude", 4, now_unix())
                .unwrap_or_else(|e| panic!("merges in flight must not stall the queue: {e}"));
            close(&b, &boundary.id, "claude").unwrap();
            release(&b, &boundary.id, "claude", true).unwrap();
            later.push(boundary.id);
        }

        // The bound is full: the next claim is the pileup the receipts refuse.
        let over = addt!(&b, "claude", "over", "", "s", &[], true_cmd()).unwrap();
        let err = claim(&b, &over.id, "claude", 4, now_unix())
            .unwrap_err()
            .to_string();
        assert!(err.contains("owes merges"), "{err}");
        assert!(err.contains(&first.id), "{err}");

        // The first merge lands: a slot frees.
        publish(&a);
        let got = claim(&b, &over.id, "claude", 4, now_unix()).unwrap();
        assert_eq!(got.task, over.id);

        // The rest land too: the receipts clear themselves at the next
        // boundary (`owed_closes` prunes the published ones it observes).
        // `publish` fast-forwards main, so the later closes land through the
        // clone that published the first.
        for id in &later {
            std::fs::copy(
                done_dir(&b).join(format!("{id}.json")),
                done_dir(&a).join(format!("{id}.json")),
            )
            .unwrap();
        }
        publish(&a);
        git(&b, &["fetch", "--quiet", "origin"]).unwrap();
        sync_queue_refs(&b).unwrap();
        assert!(owed_closes(&b, "CLAUDE", None).unwrap().is_empty());
        assert!(
            git(&b, &["ls-remote", "origin", "refs/closed/*"])
                .unwrap()
                .is_empty(),
            "observing the merges clears the receipts"
        );
        drop(r);
    }

    /// A closed task's files stay reserved until its close is on main. `finish`
    /// releases the claim when the branch run is green, before the merge; a
    /// second agent editing the same files in that window met the conflict
    /// only at the merge gate, after both had moved on.
    #[test]
    fn a_closed_but_unmerged_task_keeps_its_files_reserved_until_it_merges() {
        let (r, a, b) = Repos::new("closed-scopes");
        let scoped =
            |scopes: &[&str]| -> Vec<String> { scopes.iter().map(|s| (*s).to_string()).collect() };
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        let src = scoped(&["src/x.rs"]);
        claim_scoped(
            &a,
            &first.id,
            "claude",
            ClaimOptions {
                hours: 4,
                now: now_unix(),
                scopes: &src,
            },
        )
        .unwrap();
        close(&a, &first.id, "claude").unwrap();
        release(&a, &first.id, "claude", true).unwrap();

        // Another agent, another task, on a clone that sees the queue.
        std::fs::create_dir_all(done_dir(&b)).unwrap();
        std::fs::copy(
            done_dir(&a).join(format!("{}.json", first.id)),
            done_dir(&b).join(format!("{}.json", first.id)),
        )
        .unwrap();
        let second = addt!(&b, "gemini", "second", "", "s", &[], true_cmd()).unwrap();
        let wide = scoped(&["src"]);
        let err = claim_scoped(
            &b,
            &second.id,
            "gemini",
            ClaimOptions {
                hours: 4,
                now: now_unix(),
                scopes: &wide,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not yet merged"), "{err}");
        assert!(err.contains(&first.id), "{err}");

        // Files the closed task never reserved are free.
        let docs = scoped(&["docs"]);
        claim_scoped(
            &b,
            &second.id,
            "gemini",
            ClaimOptions {
                hours: 4,
                now: now_unix(),
                scopes: &docs,
            },
        )
        .unwrap_or_else(|e| panic!("unrelated files must stay claimable: {e}"));
        release(&b, &second.id, "gemini", false).unwrap();

        // The merge lands: the files are free again.
        publish(&a);
        claim_scoped(
            &b,
            &second.id,
            "gemini",
            ClaimOptions {
                hours: 4,
                now: now_unix(),
                scopes: &wide,
            },
        )
        .unwrap_or_else(|e| panic!("a merged close must free its files: {e}"));
        drop(r);
    }

    /// A claim can outlive its merge: `close` keeps the lease until the close
    /// is on main, and nothing released it (the session ended, the wait never
    /// came back). Its files are on main — the reservation reserves nothing —
    /// so a sibling's claim on them is not refused for the rest of a 4 h lease.
    #[test]
    fn a_live_claim_on_a_task_merged_to_main_reserves_nothing() {
        let (r, a, b) = Repos::new("merged-claim");
        let scoped =
            |scopes: &[&str]| -> Vec<String> { scopes.iter().map(|s| (*s).to_string()).collect() };
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        let src = scoped(&["src/x.rs"]);
        claim_scoped(
            &a,
            &first.id,
            "claude",
            ClaimOptions {
                hours: 4,
                now: now_unix(),
                scopes: &src,
            },
        )
        .unwrap();
        close(&a, &first.id, "claude").unwrap();
        // The merge lands with the claim still live — never released.
        publish(&a);

        std::fs::create_dir_all(done_dir(&b)).unwrap();
        std::fs::copy(
            done_dir(&a).join(format!("{}.json", first.id)),
            done_dir(&b).join(format!("{}.json", first.id)),
        )
        .unwrap();
        let second = addt!(&b, "gemini", "second", "", "s", &[], true_cmd()).unwrap();
        let wide = scoped(&["src"]);
        claim_scoped(
            &b,
            &second.id,
            "gemini",
            ClaimOptions {
                hours: 4,
                now: now_unix(),
                scopes: &wide,
            },
        )
        .unwrap_or_else(|e| panic!("a merged task's live claim must not reserve its files: {e}"));
        release(&b, &second.id, "gemini", false).unwrap();

        // An unmerged live claim still does (a third agent: claude still holds
        // the merged task's claim, and one live claim per agent is the rule).
        let third = addt!(&a, "devin", "third", "", "s", &[], true_cmd()).unwrap();
        let docs = scoped(&["docs/x.md"]);
        claim_scoped(
            &a,
            &third.id,
            "devin",
            ClaimOptions {
                hours: 4,
                now: now_unix(),
                scopes: &docs,
            },
        )
        .unwrap();
        let err = claim_scoped(
            &b,
            &second.id,
            "gemini",
            ClaimOptions {
                hours: 4,
                now: now_unix(),
                scopes: &scoped(&["docs"]),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("scope overlaps"), "{err}");
        assert!(err.contains(&third.id), "{err}");
        drop(r);
    }

    /// A lease that lapsed on your own accepted task must not strand the work:
    /// `finish` re-adopts it to keep waiting. While the re-adopted claim is
    /// live, another claim is still refused — one live claim at a time.
    #[test]
    fn a_lapsed_lease_can_readopt_its_own_accepted_task() {
        let (r, a, _b) = Repos::new("readopt");
        let t = addt!(&a, "claude", "job", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &t.id, "claude", 1, now_unix()).unwrap();
        close(&a, &t.id, "claude").unwrap();

        // The lease lapses while the pull request waits: renew re-adopts it,
        // because the task is accepted and its receipt still has not merged.
        let later = now_unix() + 3_601;
        let again = renew(&a, &t.id, "claude", 1, later).unwrap();
        assert_eq!(
            again.task, t.id,
            "finish must be able to readopt its own task"
        );
        assert!(again.lease_until_unix > later);

        let other = addt!(&a, "claude", "other", "", "s", &[], true_cmd()).unwrap();
        let err = claim(&a, &other.id, "claude", 1, later)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already holds"), "{err}");
        drop(r);
    }

    /// Giving an accepted task up is allowed, but never silent: the refusal
    /// names the way out, `--force` frees the claim without clearing the debt,
    /// and abandoning records why on the remote.
    #[test]
    fn abandoning_an_unpublished_acceptance_is_explicit_and_recorded() {
        let (r, a, _b) = Repos::new("abandon");
        let t = addt!(&a, "claude", "job", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &t.id, "claude", 4, now_unix()).unwrap();
        close(&a, &t.id, "claude").unwrap();

        let err = release(&a, &t.id, "claude", false).unwrap_err().to_string();
        assert!(err.contains("--abandon"), "{err}");
        assert!(claims(&a).unwrap().iter().any(|c| c.task == t.id));

        // `--force` frees the claim (the recovery path), not the obligation.
        release(&a, &t.id, "claude", true).unwrap();
        assert!(claims(&a).unwrap().is_empty());
        assert_eq!(unpublished_closes(&a, "claude").unwrap().len(), 1);

        assert!(
            abandon(&a, &t.id, "claude", "   ").is_err(),
            "a reason is required"
        );
        // Another agent cannot abandon work it did not accept.
        let err = abandon(&a, &t.id, "devin", "not mine")
            .unwrap_err()
            .to_string();
        assert!(err.contains("only its agent"), "{err}");
        abandon(&a, &t.id, "claude", "superseded by T-CLAUDE-2").unwrap();

        assert!(unpublished_closes(&a, "claude").unwrap().is_empty());
        let recorded = git(&a, &["ls-remote", "origin", "refs/abandoned/*"]).unwrap();
        assert!(
            recorded.contains(&format!("refs/abandoned/{}", t.id)),
            "{recorded}"
        );
        let next = addt!(&a, "claude", "next", "", "s", &[], true_cmd()).unwrap();
        assert_eq!(
            claim(&a, &next.id, "claude", 4, now_unix()).unwrap().task,
            next.id
        );
        drop(r);
    }

    /// Walking away from committed work is allowed, but not silent: it needs a
    /// reason and leaves an abandonment on the remote. A claim with nothing
    /// behind it is still free to release.
    #[test]
    fn releasing_before_acceptance_requires_a_recorded_reason() {
        let (r, a, _b) = Repos::new("release-wip");
        // `origin/main` has to exist for "committed work, unmerged" to mean
        // anything; the harness's clones do not push a main until asked.
        git(&a, &["push", "--quiet", "origin", "HEAD:main"]).unwrap();
        git(&a, &["fetch", "--quiet", "origin"]).unwrap();

        let t = addt!(&a, "claude", "job", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &t.id, "claude", 4, now_unix()).unwrap();
        // Nothing committed for the task: freeing the claim is not walking away.
        release(&a, &t.id, "claude", false).unwrap();
        assert!(claims(&a).unwrap().is_empty());

        claim(&a, &t.id, "claude", 4, now_unix()).unwrap();
        git(&a, &["add", "-A"]).unwrap();
        git(
            &a,
            &[
                "commit",
                "--quiet",
                "-m",
                "feat: wip",
                "-m",
                &format!("Task: {}", t.id),
            ],
        )
        .unwrap();
        let err = release(&a, &t.id, "claude", false).unwrap_err().to_string();
        assert!(err.contains("--abandon"), "{err}");
        assert!(
            claims(&a).unwrap().iter().any(|c| c.task == t.id),
            "a refused release must leave the claim alone"
        );

        // A reason records it on the remote and frees the claim.
        abandon(&a, &t.id, "claude", "superseded before acceptance").unwrap();
        assert!(claims(&a).unwrap().is_empty());
        let recorded = git(&a, &["ls-remote", "origin", "refs/abandoned/*"]).unwrap();
        assert!(
            recorded.contains(&format!("refs/abandoned/{}", t.id)),
            "{recorded}"
        );
        drop(r);
    }

    /// A merge attestation is evidence only when `origin/main` agrees with it.
    #[test]
    fn a_merge_attestation_is_re_derived_from_main() {
        let (r, a, _b) = Repos::new("attest-real");
        git(&a, &["commit", "--allow-empty", "-m", "the tested head"]).unwrap();
        let head = git(&a, &["rev-parse", "HEAD"]).unwrap();
        git(&a, &["commit", "--allow-empty", "-m", "the merge on main"]).unwrap();
        let merge = git(&a, &["rev-parse", "HEAD"]).unwrap();
        git(&a, &["push", "--quiet", "origin", "HEAD:main"]).unwrap();
        git(&a, &["fetch", "--quiet", "origin"]).unwrap();

        let real = MergedTask {
            task: "T-CLAUDE-1".into(),
            head: head.clone(),
            merge: merge.clone(),
            pr: 7,
            merged_unix: 1,
        };
        assert!(
            merged_is_real(&a, &real),
            "the head is on main through {merge}"
        );

        // A merge commit that never reached main proves nothing, however it
        // reads.
        git(&a, &["commit", "--allow-empty", "-m", "local only"]).unwrap();
        let local = git(&a, &["rev-parse", "HEAD"]).unwrap();
        let forged = MergedTask {
            merge: local,
            ..real.clone()
        };
        assert!(!merged_is_real(&a, &forged));

        let duplicate = MergedTask {
            task: "T-CLAUDE-2".into(),
            ..real.clone()
        };
        let unpublished = MergedTask {
            task: "T-CLAUDE-3".into(),
            ..forged.clone()
        };
        put_new_refs(
            &a,
            [&real, &duplicate, &unpublished]
                .into_iter()
                .map(|record| {
                    (
                        format!("refs/merged/{}", record.task),
                        serde_json::to_value(record).unwrap(),
                    )
                })
                .collect(),
        );
        let before = audit(&a).unwrap();
        assert_eq!(
            before.merged.len(),
            2,
            "duplicate attestations must retain both tasks"
        );
        assert_eq!(before.forged, vec![unpublished.clone()]);

        // Nor does a head the named merge does not contain.
        let unrelated = MergedTask {
            head: merge.clone(),
            merge,
            ..real.clone()
        };
        git(&a, &["commit", "--allow-empty", "-m", "after"]).unwrap();
        let after = git(&a, &["rev-parse", "HEAD"]).unwrap();
        git(&a, &["push", "--quiet", "origin", "HEAD:main"]).unwrap();
        git(&a, &["fetch", "--quiet", "origin"]).unwrap();
        let ahead = MergedTask {
            task: "T-CLAUDE-4".into(),
            head: after,
            ..unrelated
        };
        assert!(
            !merged_is_real(&a, &ahead),
            "a later head is not in that merge"
        );
        let abbreviated = MergedTask {
            task: "T-CLAUDE-5".into(),
            head: real.head[..12].to_string(),
            merge: real.merge[..12].to_string(),
            ..real
        };
        put_new_refs(
            &a,
            [&ahead, &abbreviated]
                .into_iter()
                .map(|record| {
                    (
                        format!("refs/merged/{}", record.task),
                        serde_json::to_value(record).unwrap(),
                    )
                })
                .collect(),
        );
        let after = audit(&a).unwrap();
        assert_eq!(
            after.merged.len(),
            4,
            "a new audit must observe main advancing and retain legacy IDs"
        );
        assert!(after.merged.contains(&unpublished));
        assert_eq!(
            after.forged,
            vec![ahead],
            "both commits are on main, but this merge cannot attest a later head"
        );
        drop(r);
    }

    /// The swarm-wide question: did every agent that reported success publish
    /// it? The audit answers per agent, and a recorded abandonment is visible
    /// rather than silent.
    #[test]
    fn the_audit_names_who_owes_a_merge_and_what_was_given_up() {
        let (r, a, b) = Repos::new("audit");
        let first = addt!(&a, "claude", "first", "", "s", &[], true_cmd()).unwrap();
        claim(&a, &first.id, "claude", 4, now_unix()).unwrap();
        close(&a, &first.id, "claude").unwrap();

        let second = addt!(&b, "devin", "second", "", "s", &[], true_cmd()).unwrap();
        claim(&b, &second.id, "devin", 4, now_unix()).unwrap();
        close(&b, &second.id, "devin").unwrap();
        abandon(&b, &second.id, "devin", "superseded by T-DEVIN-2").unwrap();

        let report = audit(&a).unwrap();
        assert!(!report.clean(), "an unpublished acceptance is not clean");
        assert_eq!(report.owed.len(), 1, "{report:?}");
        assert_eq!(report.owed[0].task, first.id);
        assert_eq!(report.abandoned.len(), 1, "{report:?}");
        assert!(report.abandoned[0].reason.contains("superseded"));
        let claude = report
            .agents
            .iter()
            .find(|x| x.agent == "CLAUDE")
            .expect("the audit must attribute the debt to its agent");
        assert_eq!(claude.owed, vec![first.id.clone()]);
        let devin = report
            .agents
            .iter()
            .find(|x| x.agent == "DEVIN")
            .expect("an abandonment is on the record too");
        assert_eq!(devin.owed.len(), 0);
        assert_eq!(devin.abandoned.len(), 1);

        // The merge lands: the debt clears, the abandonment stays.
        publish(&a);
        let report = audit(&b).unwrap();
        assert!(report.clean(), "{report:?}");
        assert_eq!(report.abandoned.len(), 1);
        drop(r);
    }

    /// A close whose tested head is already on `origin/main` for a task gone
    /// from the queue paid what it can pay — no merge could ever put a done
    /// file there. It must not report as debt forever; but settled is not
    /// deleted: the receipt stays until an observed close or a deliberate
    /// abandon, because deleting an unobserved acceptance is exactly what the
    /// record exists to prevent.
    #[test]
    fn a_settled_receipt_is_not_owed_but_the_record_stays() {
        let (r, a, b) = Repos::new("settled");
        let _ = std::fs::write(a.join("work.rs"), "fn work() {}\n");
        publish(&a);
        let main_head = git(&a, &["rev-parse", "origin/main"]).unwrap();
        // The agent field is the token form `agent_token` produces — a
        // lowercase name would filter every receipt out of `unpublished_closes`
        // and the assertions below would pass on an empty list.
        let receipt = |id: &str, head: &str| {
            put_ref(
                &a,
                &format!("refs/closed/{id}"),
                &serde_json::json!({
                    "task": id,
                    "agent": "CLAUDE",
                    "head": head,
                    "closed_unix": now_unix(),
                }),
            );
        };

        // The evaporated task: head on main, no open or done file anywhere.
        receipt("T-SETTLED-1", &main_head);
        assert!(
            !published_on_main(&a, "T-SETTLED-1").unwrap(),
            "no done file was observed — the record stays"
        );
        assert!(
            owed_closes(&a, "CLAUDE", None).unwrap().is_empty(),
            "nothing a merge could still publish remains — not owed"
        );
        assert!(
            unpublished_closes(&b, "claude").unwrap().is_empty(),
            "the board must not warn on a debt nobody can still pay"
        );
        assert!(
            git(&b, &["ls-remote", "origin", "refs/closed/T-SETTLED-1"])
                .unwrap()
                .contains("refs/closed/T-SETTLED-1"),
            "settled keeps the record — it does not delete it"
        );
        assert!(
            audit(&a).unwrap().owed.is_empty(),
            "the audit must not owe a debt the merged head already paid"
        );

        // The same published head with the task still open is real debt: the
        // bookkeeping close must still land or the task is worked twice.
        std::fs::create_dir_all(a.join(".agents/tasks")).unwrap();
        let _ = std::fs::write(a.join(".agents/tasks/T-SETTLED-2.json"), "{}");
        publish(&a);
        receipt("T-SETTLED-2", &main_head);
        assert!(
            !unpublished_closes(&a, "claude").unwrap().is_empty(),
            "an open task's close is still owed — the claim protects it"
        );

        // A head that never reached main is owed even when the task is
        // missing: a branch containing it can still be merged.
        let _ = std::fs::write(a.join("side.rs"), "fn side() {}\n");
        git(&a, &["add", "-A"]).unwrap();
        git(&a, &["commit", "--quiet", "-m", "unpublished work"]).unwrap();
        let stranded = git(&a, &["rev-parse", "HEAD"]).unwrap();
        receipt("T-SETTLED-3", &stranded);
        assert!(
            unpublished_closes(&a, "claude")
                .unwrap()
                .iter()
                .any(|c| c.task == "T-SETTLED-3"),
            "a head that is not an ancestor of main is still owed"
        );

        // A head that resolves nowhere is no evidence either.
        receipt("T-SETTLED-4", &"0".repeat(40));
        assert!(
            unpublished_closes(&a, "claude")
                .unwrap()
                .iter()
                .any(|c| c.task == "T-SETTLED-4"),
            "a head that does not resolve is not evidence"
        );
        drop(r);
    }

    /// Post-merge verification: the acceptance is re-run on the merged tree,
    /// a pass is recorded, a host-specific command is skipped *with its
    /// reason*, and a failure is recorded nowhere so the next run retries it.
    #[test]
    fn acceptance_is_re_verified_on_the_merged_tree() {
        let (r, a, _b) = Repos::new("verify");
        let mut ids = Vec::new();
        for (agent, title) in [("claude", "ok"), ("devin", "broken"), ("gemini", "scripts")] {
            let mut task = addt!(&a, agent, title, "", "s", &[], true_cmd()).unwrap();
            if ids.is_empty() {
                claim(&a, &task.id, agent, 4, now_unix()).unwrap();
                close(&a, &task.id, agent).unwrap();
            } else {
                // One actual acceptance establishes the lifecycle. The sweep
                // consumes committed done records and merger attestations;
                // replaying two more claims/acceptances only retests setup.
                task.closed = Some(Closed {
                    at_unix: now_unix(),
                    commit: git(&a, &["rev-parse", "HEAD"]).unwrap(),
                    by: agent_token(agent).unwrap(),
                });
                std::fs::write(
                    done_dir(&a).join(format!("{}.json", task.id)),
                    serde_json::to_vec(&task).unwrap(),
                )
                .unwrap();
                std::fs::remove_file(tasks_dir(&a).join(format!("{}.json", task.id))).unwrap();
            }
            ids.push(task.id);
        }
        let (ok, broken, scripts) = (&ids[0], &ids[1], &ids[2]);
        rewrite_accept(
            &a,
            broken,
            serde_json::json!(["cargo", "definitely-not-a-subcommand"]),
        );
        rewrite_accept(
            &a,
            scripts,
            serde_json::json!(["scripts/check-roadmap-progress.sh"]),
        );
        publish(&a);
        let head = git(&a, &["rev-parse", "HEAD"]).unwrap();
        put_new_refs(
            &a,
            [(ok, 1u64), (broken, 2), (scripts, 3)]
                .into_iter()
                .map(|(id, pr)| {
                    (
                        format!("refs/merged/{id}"),
                        serde_json::json!({
                            "task": id, "head": head, "merge": head, "pr": pr, "merged_unix": 1
                        }),
                    )
                })
                .collect(),
        );

        let outcomes = verify_merged(&a, 5).unwrap();
        assert_eq!(outcomes.len(), 3, "{outcomes:?}");
        let result = |id: &str| {
            outcomes
                .iter()
                .find(|o| o.task == id)
                .map(|o| o.result.clone())
                .unwrap_or_default()
        };
        assert_eq!(result(ok), "passed");
        assert!(result(broken).starts_with("failed"), "{outcomes:?}");
        assert_eq!(result(scripts), "skipped:not-cargo");

        let recorded = verified_tasks(&a).unwrap();
        assert_eq!(recorded.len(), 2, "a failure is not recorded: {recorded:?}");
        assert!(recorded
            .iter()
            .any(|v| v.task == *ok && v.result == "passed"));
        assert!(recorded
            .iter()
            .any(|v| v.task == *scripts && v.result == "skipped:not-cargo"));
        assert!(!recorded.iter().any(|v| v.task == *broken));

        // A recorded verification is not re-run; the failure is retried, which
        // is what keeps a broken merge visible until it is fixed forward.
        let retried = verify_merged(&a, 5).unwrap();
        assert_eq!(retried.len(), 1, "{retried:?}");
        assert_eq!(retried[0].task, *broken);
        assert!(retried[0].result.starts_with("failed"));
        drop(r);
    }

    /// A sweep that runs out of budget defers the rest instead of holding the
    /// job: a job killed at its own timeout records nothing, so the queue would
    /// never drain and main would never go green.
    #[test]
    fn a_spent_budget_defers_the_rest_of_the_sweep() {
        let (r, a, _b) = Repos::new("verify-budget");
        let mut ids = Vec::new();
        for agent in ["claude", "devin"] {
            let task = addt!(&a, agent, "job", "", "s", &[], true_cmd()).unwrap();
            claim(&a, &task.id, agent, 4, now_unix()).unwrap();
            close(&a, &task.id, agent).unwrap();
            ids.push(task.id);
        }
        publish(&a);
        let head = git(&a, &["rev-parse", "HEAD"]).unwrap();
        for id in &ids {
            put_ref(
                &a,
                &format!("refs/merged/{id}"),
                &serde_json::json!({
                    "task": id, "head": head, "merge": head, "pr": 1, "merged_unix": 1
                }),
            );
        }

        let outcomes = verify_merged_within(&a, 5, Duration::ZERO).unwrap();
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        for outcome in &outcomes {
            assert_eq!(outcome.result, "deferred:budget", "{outcomes:?}");
        }
        assert!(
            verified_tasks(&a).unwrap().is_empty(),
            "a deferred acceptance is not a verdict and must not be recorded"
        );
        // The work is still outstanding, so the next sweep sees it again.
        let next = verify_merged_within(&a, 5, Duration::from_secs(60)).unwrap();
        assert_eq!(next.len(), 2, "{next:?}");
        assert!(next.iter().all(|o| o.result == "passed"), "{next:?}");
        drop(r);
    }

    #[test]
    fn close_refuses_a_task_someone_else_holds() {
        let (r, a, b) = Repos::new("other");
        let t = addt!(&a, "claude", "job", "", "s", &[], true_cmd()).unwrap();
        std::fs::create_dir_all(tasks_dir(&b)).unwrap();
        std::fs::copy(
            tasks_dir(&a).join(format!("{}.json", t.id)),
            tasks_dir(&b).join(format!("{}.json", t.id)),
        )
        .unwrap();
        claim(&a, &t.id, "claude", 4, now_unix()).unwrap();
        let err = close(&b, &t.id, "devin").unwrap_err().to_string();
        assert!(err.contains("claimed by CLAUDE"), "{err}");
        drop(r);
    }

    #[test]
    fn a_shell_quoted_acceptance_command_is_refused_when_the_task_is_added() {
        let (_r, a, _b) = Repos::new("accept-quotes");
        let task = |accept: Vec<String>| NewTask {
            title: "check something".into(),
            goal: String::new(),
            size: "s".into(),
            deps: Vec::new(),
            accept,
            roadmap: None,
        };
        // Quotes are not shell syntax here: they reach the program literally, so
        // the failure only surfaced at `close`, as an opaque exit code.
        let quoted = task(vec![
            "cargo".into(),
            "nextest".into(),
            "run".into(),
            "-E".into(),
            "'binary(x)'".into(),
        ]);
        let err = add(&a, "test", quoted).unwrap_err().to_string();
        assert!(
            err.contains("executed as argv, not through a shell"),
            "{err}"
        );
        let plain = task(vec![
            "cargo".into(),
            "nextest".into(),
            "run".into(),
            "-E".into(),
            "binary(x)".into(),
        ]);
        assert!(add(&a, "test", plain).is_ok());
    }

    // ---- T-DEEPSEEK-233 / VC-202-008: author repair tasks from failures ---

    fn failure_ws(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("susi-author-failures-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".agents/tasks")).unwrap();
        std::fs::write(root.join(".agents/roadmap.json"), r#"{"vectors": []}"#).unwrap();
        root
    }

    fn record_mission(ws: &Path, mission_id: &str, goal: &str, outcome: &str) {
        crate::susi_core::mission_trace::MissionTrace::new(mission_id, goal, outcome, "swarm")
            .emit(ws)
            .unwrap();
    }

    #[test]
    fn self_authored_failure_backlog_drafts_a_repair_task_per_failure_kind() {
        let ws = failure_ws("drafts");
        record_mission(&ws, "m-1", "deploy the api", "FAILED");
        record_mission(&ws, "m-2", "deploy the api", "FAILED"); // same kind: not a second task
        record_mission(&ws, "m-3", "rotate the cluster key", "BLOCKED");
        record_mission(&ws, "m-4", "migrate the database", "COMPLETE"); // succeeded: no task

        let report = author_from_failures(&ws, "deepseek").unwrap();
        assert_eq!(
            report.tasks.len(),
            2,
            "one task per distinct failure kind, not per occurrence"
        );
        let open = list_open(&ws);
        assert_eq!(open.len(), 2);
        assert!(open
            .iter()
            .any(|t| t.title.contains("FAILED") && t.title.contains("deploy the api")));
        assert!(open
            .iter()
            .any(|t| t.title.contains("BLOCKED") && t.title.contains("rotate the cluster key")));
        assert!(open.iter().all(|t| t.authored));
    }

    #[test]
    fn self_authored_failure_backlog_does_not_redraft_an_already_queued_failure() {
        let ws = failure_ws("dedup");
        record_mission(&ws, "m-1", "deploy the api", "FAILED");
        author_from_failures(&ws, "deepseek").unwrap();
        assert_eq!(list_open(&ws).len(), 1);

        // The same failure kind recurs; it must not grow the queue again.
        record_mission(&ws, "m-2", "deploy the api", "FAILED");
        let report = author_from_failures(&ws, "deepseek").unwrap();
        assert!(
            report.tasks.is_empty(),
            "already-queued failure kind is not re-authored"
        );
        assert_eq!(list_open(&ws).len(), 1);
    }

    #[test]
    fn self_authored_failure_backlog_is_bound_like_any_other_authored_work() {
        let ws = failure_ws("bound");
        for i in 0..(AUTHORED_OPEN_MAX + 1) {
            record_mission(
                &ws,
                &format!("m-{i}"),
                &format!("distinct goal {i}"),
                "FAILED",
            );
        }
        let err = author_from_failures(&ws, "deepseek")
            .unwrap_err()
            .to_string();
        assert!(err.contains("bound"), "{err}");
        assert_eq!(
            list_open(&ws).len(),
            0,
            "an over-bound batch writes nothing"
        );
    }

    #[test]
    fn self_authored_failure_backlog_accept_targets_a_derivable_regression_test() {
        let ws = failure_ws("accept");
        record_mission(&ws, "m-1", "deploy the api", "FAILED");
        author_from_failures(&ws, "deepseek").unwrap();
        let drafted = &list_open(&ws)[0];
        assert!(drafted.accept.cmd.last().unwrap().ends_with("_repaired)"));
        assert_eq!(drafted.accept.cmd[0], "cargo");
    }

    #[test]
    fn self_authored_failure_backlog_wiring_is_on_the_cli_path() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../src/cli/tasks_cli.rs"
        ))
        .unwrap();
        for needle in ["from_failures", "author_from_failures("] {
            assert!(src.contains(needle), "tasks_cli.rs is missing `{needle}`");
        }
    }
}

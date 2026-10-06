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
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let mut out: Vec<Task> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
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

/// Next `T-<AGENT>-<n>` across open and done tasks.
pub fn next_id(ws: &Path, agent: &str) -> EaiResult<String> {
    let token = agent_token(agent)?;
    let prefix = format!("T-{token}-");
    let max = list_open(ws)
        .into_iter()
        .chain(list_done(ws))
        .filter_map(|t| {
            t.id.strip_prefix(&prefix)
                .and_then(|n| n.parse::<u64>().ok())
        })
        .max()
        .unwrap_or(0);
    Ok(format!("{prefix}{}", max + 1))
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
    let known: std::collections::HashSet<String> = list_open(ws)
        .into_iter()
        .chain(list_done(ws))
        .map(|t| t.id)
        .collect();
    check_task_deps(deps, &known)?;
    let task = Task {
        id: next_id(ws, agent)?,
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

fn git(ws: &Path, args: &[&str]) -> EaiResult<String> {
    let out = Command::new("git")
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
    git(
        ws,
        &[
            "fetch",
            "--quiet",
            "--prune",
            &r,
            "+refs/claims/*:refs/claims/*",
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
    let mut child = Command::new("git")
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
fn read_record<T: serde::de::DeserializeOwned>(ws: &Path, name: &str) -> EaiResult<Option<T>> {
    let Ok(sha) = git(ws, &["rev-parse", "--verify", "--quiet", name]) else {
        return Ok(None);
    };
    let body = git(ws, &["cat-file", "-p", &sha])?;
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
    git(
        ws,
        &[
            "push",
            "--quiet",
            &format!("--force-with-lease={name}:{old}"),
            &r,
            &format!("{blob}:{name}"),
        ],
    )
    .map_err(|_| EaiError::config(raced.to_string()))?;
    git(ws, &["update-ref", name, &blob])?;
    Ok(())
}

/// Drop a record ref on the remote and locally. Compare-and-swap: a record
/// re-pushed since we read it belongs to a newer fact and must survive.
fn clear_record(ws: &Path, name: &str) -> EaiResult<()> {
    let r = remote();
    let listed = git(ws, &["ls-remote", &r, name])?;
    let old = listed.split_whitespace().next().unwrap_or("");
    if !old.is_empty() {
        git(
            ws,
            &[
                "push",
                "--quiet",
                &format!("--force-with-lease={name}:{old}"),
                &r,
                &format!(":{name}"),
            ],
        )?;
    }
    let _ = git(ws, &["update-ref", "-d", name]);
    Ok(())
}

fn read_claim(ws: &Path, id: &str) -> Option<(String, Claim)> {
    let sha = git(ws, &["rev-parse", "--verify", "--quiet", &claim_ref(id)]).ok()?;
    let body = git(ws, &["cat-file", "-p", &sha]).ok()?;
    Some((sha, serde_json::from_str(&body).ok()?))
}

/// The close receipt for `id`, if the remote has one.
fn read_closed(ws: &Path, id: &str) -> EaiResult<Option<CloseRecord>> {
    read_record(ws, &closed_ref(id))
}

/// Whether the close of `id` is on `origin/main` — the only proof that the
/// accepted work is actually done. An unreachable origin is an error, never a
/// silent "published": the caller must not treat an unknown as a merge.
fn published_on_main(ws: &Path, id: &str) -> EaiResult<bool> {
    git(ws, &["fetch", "--quiet", "origin"])?;
    Ok(done_on_main(ws, id))
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
fn push_close_receipt(ws: &Path, id: &str, agent: &str) -> EaiResult<()> {
    let record = CloseRecord {
        task: id.to_string(),
        agent: agent.to_string(),
        head: git(ws, &["rev-parse", "HEAD"])?,
        closed_unix: now_unix(),
    };
    put_record(
        ws,
        &closed_ref(id),
        &record,
        &format!(
            "{id} was accepted and published by another worker concurrently; refresh the queue \
             and re-run the acceptance check"
        ),
    )
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
        Some(record) if record.agent == agent => Ok(!published_on_main(ws, id)?),
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
fn owed_closes(ws: &Path, agent: &str, except: Option<&str>) -> EaiResult<Vec<String>> {
    let refs = git(ws, &["for-each-ref", "--format=%(refname)", "refs/closed/"])?;
    let mut owed = Vec::new();
    for name in refs.lines() {
        let Some(id) = name.strip_prefix("refs/closed/") else {
            continue;
        };
        if except == Some(id) {
            continue;
        }
        let Some(record) = read_closed(ws, id)? else {
            continue;
        };
        if record.agent != agent {
            continue;
        }
        if published_on_main(ws, id)? {
            clear_record(ws, &closed_ref(id))?;
        } else {
            owed.push(id.to_string());
        }
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

/// [`unpublished_closes`] for a caller that has just run [`sync_queue`]: the
/// same read, without a second identical fetch behind the first.
pub fn unpublished_closes_synced(ws: &Path, agent: &str) -> EaiResult<Vec<CloseRecord>> {
    let by = agent_token(agent)?;
    let refs = git(ws, &["for-each-ref", "--format=%(refname)", "refs/closed/"])?;
    let mut out = Vec::new();
    for name in refs.lines() {
        let Some(id) = name.strip_prefix("refs/closed/") else {
            continue;
        };
        let Some(record) = read_closed(ws, id)? else {
            continue;
        };
        if record.agent == by && !published_on_main(ws, id)? {
            out.push(record);
        }
    }
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
    let refs = git(ws, &["for-each-ref", "--format=%(refname)", namespace])?;
    let mut out = Vec::new();
    for name in refs.lines() {
        if let Some(record) = read_record::<T>(ws, name)? {
            out.push(record);
        }
    }
    Ok(out)
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
    // One fetch for the whole report: the per-task lookup below would otherwise
    // hit the remote once per receipt.
    git(ws, &["fetch", "--quiet", "origin"])?;

    let merged = records_under::<MergedTask>(ws, MERGED_NAMESPACE)?;
    let verified = records_under::<VerifiedTask>(ws, "refs/verified/")?;
    let real: Vec<MergedTask> = merged
        .iter()
        .filter(|m| merged_is_real(ws, m))
        .cloned()
        .collect();
    let forged: Vec<MergedTask> = merged
        .iter()
        .filter(|m| !merged_is_real(ws, m))
        .cloned()
        .collect();
    let verified_ids: std::collections::HashSet<&str> =
        verified.iter().map(|v| v.task.as_str()).collect();
    let unverified: Vec<MergedTask> = real
        .iter()
        .filter(|m| !verified_ids.contains(m.task.as_str()))
        .cloned()
        .collect();

    let mut owed = Vec::new();
    for record in records_under::<CloseRecord>(ws, "refs/closed/")? {
        let landed = done_on_main(ws, &record.task) || real.iter().any(|m| m.task == record.task);
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
pub fn verify_merged(ws: &Path, limit: usize) -> EaiResult<Vec<VerifyOutcome>> {
    let report = audit(ws)?;
    let done = done_dir(ws);
    let mut candidates: Vec<&MergedTask> = report
        .unverified
        .iter()
        .filter(|m| done.join(format!("{}.json", m.task)).is_file())
        .collect();
    candidates.sort_by_key(|m| m.merged_unix);
    candidates.truncate(limit);

    let mut out = Vec::new();
    for record in candidates {
        let task: Task =
            serde_json::from_slice(&std::fs::read(done.join(format!("{}.json", record.task)))?)?;
        let result = verify_one(ws, &task);
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

/// Run one task's acceptance against this tree.
fn verify_one(ws: &Path, task: &Task) -> VerifyOutcome {
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
    match Command::new(&program)
        .args(&task.accept.cmd[1..])
        .current_dir(ws)
        .output()
    {
        Ok(out) if out.status.success() => outcome("passed", task.accept.cmd.join(" ")),
        Ok(out) => outcome(
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
        Err(e) => outcome("failed:-1", format!("{} could not run: {e}", program)),
    }
}

/// Claims currently on the remote, keyed by task id.
pub fn claims(ws: &Path) -> EaiResult<Vec<Claim>> {
    sync_queue_refs(ws)?;
    claims_synced(ws)
}

/// [`claims`] for a caller that has just run [`sync_queue`].
pub fn claims_synced(ws: &Path) -> EaiResult<Vec<Claim>> {
    let refs = git(ws, &["for-each-ref", "--format=%(refname)", "refs/claims/"])?;
    Ok(refs
        .lines()
        .filter_map(|r| r.strip_prefix("refs/claims/"))
        .filter_map(|id| read_claim(ws, id).map(|(_, c)| c))
        .collect())
}

/// Refuse work on a stale base: an agent must hold every commit of
/// `origin/main` before claiming, so it starts from current rules and a current
/// queue. Unreachable remotes and missing integration refs fail closed.
pub fn ensure_synced(ws: &Path) -> EaiResult<()> {
    ensure_synced_for(ws, None)
}

/// [`ensure_synced`] with the task being claimed exempted from the
/// "unpublished completion" rule.
///
/// Re-adopting *your own* accepted task after a lease lapsed is the one case
/// where the close is legitimately in this branch and not yet on main: `finish`
/// re-adopts it to keep waiting. Exempting exactly that id keeps the rule for
/// every other claim, and keeps a lapsed lease from stranding the work.
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
    if !pending.is_empty() {
        return Err(EaiError::config(
            "claim refused: publish and merge the completed task before starting another",
        ));
    }
    match n.parse::<u64>() {
        Ok(0) | Err(_) => Ok(()),
        Ok(n) => Err(EaiError::config(format!(
            "claim refused: this worktree is {n} commit(s) behind origin/main. \
             Sync first: `git merge origin/main` (then re-run the claim)"
        ))),
    }
}

/// Claim `id` for `agent`. Fails if another live claim exists or a dependency
/// is still open. An expired claim is taken over by compare-and-swap.
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
const CAS_ATTEMPTS: u32 = 8;
/// Both messages below carry a phrase from here, so the retry matcher and the
/// text a user reads can never drift apart.
const CAS_PHRASE: &str = "concurrently with another agent";
const CAS_RENEW_PHRASE: &str = "concurrently with another claim";

fn is_cas_race(err: &EaiError) -> bool {
    let text = err.to_string();
    text.contains(CAS_PHRASE) || text.contains(CAS_RENEW_PHRASE)
}

/// Spread the retries across processes: identical sleeps would keep the swarm
/// in lockstep, and the point is that they stop colliding.
fn cas_backoff(attempt: u32, agent: &str) {
    let seed = u64::from(std::process::id()) + agent.bytes().map(u64::from).sum::<u64>();
    let jitter = (seed % 97) + 11;
    std::thread::sleep(std::time::Duration::from_millis(
        u64::from(attempt) * 25 + jitter,
    ));
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
    let generation_ref = "refs/claim-generation/current";
    // Read the generation BEFORE the snapshot; any concurrent successful
    // claim invalidates our transaction, including a claim on a different task.
    let generation = git(ws, &["ls-remote", &remote(), generation_ref])?;
    let generation_old = generation.split_whitespace().next().unwrap_or("");
    let task = find_open(ws, id)?;
    let open: std::collections::HashSet<String> = list_open(ws).into_iter().map(|t| t.id).collect();
    if let Some(dep) = task.deps.iter().find(|d| open.contains(*d)) {
        return Err(EaiError::config(format!(
            "{id} is blocked: dependency {dep} is still open"
        )));
    }
    let agent = agent_token(agent)?;
    let live = claims(ws)?;
    // An accepted task whose close is not yet on origin/main is still owned by
    // whoever accepted it, whatever happened to the claim: it may have been
    // released, or lapsed hours ago. The receipt on the shared remote is the
    // debt, and it blocks the *agent*, not the worktree — a fresh clone cannot
    // launder it, which is what a claim lease alone could not express. The task
    // being claimed here is exempt: re-adopting your own accepted task is how
    // `finish` keeps waiting, not a way to start something new.
    let owed = owed_closes(ws, &agent, Some(id))?;
    if !owed.is_empty() {
        let list = owed.join(", ");
        let first = owed.first().map(String::as_str).unwrap_or("");
        return Err(EaiError::config(format!(
            "{agent} owes a merge for {list} — an accepted task is not done until its close is \
             on origin/main, and the claim is what stops another agent redoing it. Publish it: \
             `susi workflow finish {first}` (or, to give it up deliberately: \
             `susi tasks release {first} --abandon <reason>`)"
        )));
    }
    for other in live.iter().filter(|c| !c.expired(now)) {
        if scopes.iter().any(|a| {
            other.scopes.iter().any(|b| {
                a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
            })
        }) {
            return Err(EaiError::config(format!(
                "scope overlaps {} held by {}",
                other.task, other.agent
            )));
        }
    }
    if let Some(other) = live.iter().find(|c| c.agent == agent && !c.expired(now)) {
        return Err(EaiError::config(format!(
            "{agent} already holds {}; complete or release it before claiming {id}",
            other.task
        )));
    }
    // A second atomic ref serializes claims by this agent even across clones.
    let agent_ref = format!("refs/claim-agents/{agent}");
    let listed = git(ws, &["ls-remote", &remote(), &agent_ref])?;
    let agent_old = listed.split_whitespace().next().unwrap_or("").to_string();
    if !agent_old.is_empty() {
        git(ws, &["fetch", "--quiet", &remote(), &agent_ref])?;
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
        branch: git(ws, &["symbolic-ref", "-q", "--short", "HEAD"]).ok(),
        scopes: scopes.to_vec(),
        task: id.to_string(),
        agent,
        claimed_unix: now,
        lease_until_unix: now.saturating_add(hours.max(1).saturating_mul(3600)),
    };
    let blob = claim_blob(ws, &mine)?;
    let target = format!("{blob}:{}", claim_ref(id));
    let r = remote();
    let task_old = existing.as_ref().map_or("", |(sha, _)| sha.as_str());
    let task_lease = format!("--force-with-lease={}:{}", claim_ref(id), task_old);
    let agent_lease = format!("--force-with-lease={agent_ref}:{agent_old}");
    let agent_target = format!("{blob}:{agent_ref}");
    let generation_lease = format!("--force-with-lease={generation_ref}:{generation_old}");
    let generation_target = format!("{blob}:{generation_ref}");
    // A claim is one blob pushed to the task, agent and generation refs, so a
    // takeover of an expired claim leaves the previous holder's agent ref
    // pointing at a claim it no longer holds. Clear it in the same atomic push
    // — but only while it still points at exactly that claim: if that agent has
    // claimed something else since, its ref is its own bookkeeping, not ours.
    let stale_agent = match &existing {
        Some((sha, old)) if old.agent != mine.agent => {
            let stale = format!("refs/claim-agents/{}", old.agent);
            let listed = git(ws, &["ls-remote", &r, &stale])?;
            let current = listed.split_whitespace().next().unwrap_or("");
            (current == sha).then(|| (stale, sha.clone()))
        }
        _ => None,
    };
    let mut args: Vec<String> = vec![
        "push".into(),
        "--quiet".into(),
        "--atomic".into(),
        task_lease,
        agent_lease,
        generation_lease,
    ];
    if let Some((stale, sha)) = &stale_agent {
        args.push(format!("--force-with-lease={stale}:{sha}"));
    }
    args.push(r);
    args.push(target);
    args.push(agent_target);
    args.push(generation_target);
    if let Some((stale, _)) = &stale_agent {
        args.push(format!(":{stale}"));
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    git(ws, &argv).map_err(|_| {
        EaiError::config(format!(
            "{id} or agent {} was claimed {CAS_PHRASE}; refresh the queue and retry",
            mine.agent
        ))
    })?;
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
    let generation_ref = "refs/claim-generation/current";
    let generation = git(ws, &["ls-remote", &r, generation_ref])?;
    let generation_old = generation.split_whitespace().next().unwrap_or("");
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
    let agent_list = git(ws, &["ls-remote", &r, &agent_ref])?;
    let agent_old = agent_list.split_whitespace().next().unwrap_or("");
    if !agent_old.is_empty() && agent_old != old {
        return Err(EaiError::config("agent ownership changed; cannot renew"));
    }
    git(
        ws,
        &[
            "push",
            "--quiet",
            "--atomic",
            &format!("--force-with-lease={}:{}", claim_ref(id), old),
            &format!("--force-with-lease={agent_ref}:{agent_old}"),
            &format!("--force-with-lease={generation_ref}:{generation_old}"),
            &r,
            &format!("{blob}:{}", claim_ref(id)),
            &format!("{blob}:{agent_ref}"),
            &format!("{blob}:{generation_ref}"),
        ],
    )
    .map_err(|_| EaiError::config(format!("renewal of {id} raced {CAS_RENEW_PHRASE}; retry")))?;
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
        if published_on_main(ws, id)? {
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
    let lease = format!("--force-with-lease={}:{sha}", claim_ref(id));
    let agent_ref = format!("refs/claim-agents/{}", c.agent);
    // A reason given for work that was never accepted is still a fact worth
    // keeping: without this, abandoning an unfinished task left no trace at
    // all — only abandoning an *accepted* one did.
    if receipt.is_none() {
        if let Some(reason) = abandon_reason {
            push_abandoned(ws, id, &c.agent, reason)?;
        }
    }
    let listed = git(ws, &["ls-remote", &remote(), &agent_ref])?;
    let agent_old = listed.split_whitespace().next().unwrap_or("");
    let deletion = format!(":{}", claim_ref(id));
    if agent_old == sha {
        let agent_lease = format!("--force-with-lease={agent_ref}:{sha}");
        git(
            ws,
            &[
                "push",
                "--quiet",
                "--atomic",
                &lease,
                &agent_lease,
                &remote(),
                &deletion,
                &format!(":{agent_ref}"),
            ],
        )?;
    } else {
        // Legacy claims have no agent ref; never delete a newer agent lease.
        git(ws, &["push", "--quiet", &lease, &remote(), &deletion])?;
    }
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
    let verify_owner = || -> EaiResult<()> {
        let claim = claims(ws)?
            .into_iter()
            .find(|c| c.task == id)
            .ok_or_else(|| {
                EaiError::config(format!("{id} requires a live owned claim before closing"))
            })?;
        let branch = git(ws, &["symbolic-ref", "-q", "--short", "HEAD"]).ok();
        if claim.agent != by
            || claim.expired(now_unix())
            || claim
                .branch
                .as_ref()
                .is_some_and(|b| Some(b) != branch.as_ref())
        {
            return Err(EaiError::config(format!(
                "{id} is claimed by {}, not {by} on this branch, or its lease expired",
                claim.agent
            )));
        }
        Ok(())
    };
    verify_owner()?;
    ensure_open_on_main(ws, id)?;
    if !accept_allowed(&task.accept.cmd) {
        return Err(EaiError::config("task acceptance command is not allowed"));
    }
    let out = Command::new(&task.accept.cmd[0])
        .args(&task.accept.cmd[1..])
        .current_dir(ws)
        .output()
        .map_err(|e| {
            // A check that does not exist yet is "not done", not an I/O crash.
            EaiError::process(format!(
                "{id} is not done: acceptance command `{}` could not run ({e})",
                task.accept.cmd.join(" ")
            ))
        })?;
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
    // Acceptance may outlive a lease; re-check before mutating the queue.
    verify_owner()?;
    // Publish the receipt before the record moves. From here the task is
    // accepted and its agent may not claim anything else until this head is on
    // origin/main; a failed push leaves the task open, so close is retried
    // rather than leaving an acceptance nobody can see.
    push_close_receipt(ws, id, &by)?;
    task.closed = Some(Closed {
        at_unix: now_unix(),
        commit: git(ws, &["rev-parse", "HEAD"]).unwrap_or_default(),
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
fn ensure_open_on_main(ws: &Path, id: &str) -> EaiResult<()> {
    git(ws, &["fetch", "--quiet", "origin"]).map_err(|_| {
        EaiError::config(format!(
            "close refused: cannot reach origin to check whether {id} is already closed elsewhere"
        ))
    })?;
    if git(
        ws,
        &[
            "cat-file",
            "-e",
            &format!("origin/main:.agents/tasks/done/{id}.json"),
        ],
    )
    .is_ok()
    {
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
                let out = Command::new("git")
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
            let mut clones = Vec::new();
            for name in ["a", "b"] {
                let dir = root.join(name);
                std::fs::create_dir_all(&dir).unwrap();
                run(&dir, &["init", "--quiet"]);
                // Repo-local identity: `run` sets the env vars, but a test that
                // commits through the shared `git` helper has none, and the CI
                // runner has no global identity either (a test must never need
                // the developer's ~/.gitconfig).
                run(&dir, &["config", "user.name", "test"]);
                run(&dir, &["config", "user.email", "test@example.test"]);
                run(&dir, &["remote", "add", "origin", bare.to_str().unwrap()]);
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
        assert!(err.contains("could not run"), "{err}");
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
        std::fs::write(
            a.join("Cargo.toml"),
            "[package]\nname = \"vac\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
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

    /// The lie this closes: an agent accepts a task, never publishes the merge,
    /// and starts the next one. Neither releasing the claim nor moving to a
    /// fresh clone may launder that — the receipt is on the shared remote.
    #[test]
    fn an_unpublished_acceptance_blocks_the_next_claim_across_clones() {
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

        // Freeing the claim does not free the agent.
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

        let second = addt!(&b, "claude", "second", "", "s", &[], true_cmd()).unwrap();
        let err = claim(&b, &second.id, "claude", 4, now_unix())
            .unwrap_err()
            .to_string();
        assert!(err.contains("owes a merge"), "{err}");
        assert!(err.contains(&first.id), "{err}");

        // The merge lands: the receipt clears itself at the next boundary.
        publish(&a);
        let got = claim(&b, &second.id, "claude", 4, now_unix()).unwrap();
        assert_eq!(got.task, second.id);
        assert!(
            git(&b, &["ls-remote", "origin", "refs/closed/*"])
                .unwrap()
                .is_empty(),
            "observing the merge clears the receipt"
        );
        drop(r);
    }

    /// A lease that lapsed on your own accepted task must not strand the work:
    /// `finish` re-adopts it to keep waiting. The debt still blocks every
    /// *other* task, so re-adoption is not a way to start something new.
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
        assert!(err.contains("owes a merge"), "{err}");
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

        // Nor does a head the named merge does not contain.
        let unrelated = MergedTask {
            head: merge.clone(),
            merge,
            ..real
        };
        git(&a, &["commit", "--allow-empty", "-m", "after"]).unwrap();
        let after = git(&a, &["rev-parse", "HEAD"]).unwrap();
        git(&a, &["push", "--quiet", "origin", "HEAD:main"]).unwrap();
        git(&a, &["fetch", "--quiet", "origin"]).unwrap();
        let ahead = MergedTask {
            head: after,
            ..unrelated
        };
        assert!(
            !merged_is_real(&a, &ahead),
            "a later head is not in that merge"
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

    /// Post-merge verification: the acceptance is re-run on the merged tree,
    /// a pass is recorded, a host-specific command is skipped *with its
    /// reason*, and a failure is recorded nowhere so the next run retries it.
    #[test]
    fn acceptance_is_re_verified_on_the_merged_tree() {
        let (r, a, _b) = Repos::new("verify");
        let mut ids = Vec::new();
        for (agent, title) in [("claude", "ok"), ("devin", "broken"), ("gemini", "scripts")] {
            let task = addt!(&a, agent, title, "", "s", &[], true_cmd()).unwrap();
            claim(&a, &task.id, agent, 4, now_unix()).unwrap();
            close(&a, &task.id, agent).unwrap();
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
        for (id, pr) in [(ok, 1u64), (broken, 2), (scripts, 3)] {
            put_ref(
                &a,
                &format!("refs/merged/{id}"),
                &serde_json::json!({
                    "task": id, "head": head, "merge": head, "pr": pr, "merged_unix": 1
                }),
            );
        }

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

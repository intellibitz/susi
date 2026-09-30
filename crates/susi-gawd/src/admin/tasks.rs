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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<Closed>,
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
    if title.trim().is_empty() {
        return Err(EaiError::config("task title must not be empty"));
    }
    if !matches!(size, "s" | "m" | "l") {
        return Err(EaiError::config("size must be s, m or l"));
    }
    if !accept_allowed(&accept_cmd) {
        return Err(EaiError::config(
            "acceptance command is required and must start with cargo, susi or scripts/<file> \
             (a task file is not a shell)",
        ));
    }
    let known: std::collections::HashSet<String> = list_open(ws)
        .into_iter()
        .chain(list_done(ws))
        .map(|t| t.id)
        .collect();
    if let Some(bad) = deps.iter().find(|d| !known.contains(*d)) {
        return Err(EaiError::config(format!("unknown dependency {bad}")));
    }
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
        closed: None,
    };
    std::fs::create_dir_all(tasks_dir(ws))?;
    std::fs::write(
        tasks_dir(ws).join(format!("{}.json", task.id)),
        serde_json::to_vec_pretty(&task)?,
    )?;
    Ok(task)
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
    /// Every linked task is closed (and there is at least one).
    pub fn delivered(&self) -> bool {
        self.open.is_empty() && !self.closed.is_empty()
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

/// Mirror the remote's claims into local `refs/claims/*`, dropping local
/// claims the remote no longer has — a released claim must not linger in
/// other clones and make a free task look taken.
fn sync_claims(ws: &Path) -> EaiResult<()> {
    let r = remote();
    let listed = git(ws, &["ls-remote", &r, "refs/claims/*"])?;
    if listed.is_empty() {
        let stale = git(ws, &["for-each-ref", "--format=%(refname)", "refs/claims/"])?;
        for name in stale.lines() {
            let _ = git(ws, &["update-ref", "-d", name]);
        }
        return Ok(());
    }
    git(
        ws,
        &[
            "fetch",
            "--quiet",
            "--prune",
            &r,
            "+refs/claims/*:refs/claims/*",
        ],
    )
    .map(|_| ())
}

fn read_claim(ws: &Path, id: &str) -> Option<(String, Claim)> {
    let sha = git(ws, &["rev-parse", "--verify", "--quiet", &claim_ref(id)]).ok()?;
    let body = git(ws, &["cat-file", "-p", &sha]).ok()?;
    Some((sha, serde_json::from_str(&body).ok()?))
}

/// Claims currently on the remote, keyed by task id.
pub fn claims(ws: &Path) -> EaiResult<Vec<Claim>> {
    sync_claims(ws)?;
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
    git(ws, &["fetch", "--quiet", "origin"])?;
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
    if !unpublished.is_empty() {
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
    git(
        ws,
        &[
            "push",
            "--quiet",
            "--atomic",
            &task_lease,
            &agent_lease,
            &generation_lease,
            &r,
            &target,
            &agent_target,
            &generation_target,
        ],
    )
    .map_err(|_| {
        EaiError::config(format!(
            "{id} or agent {} was claimed concurrently; refresh the queue and retry",
            mine.agent
        ))
    })?;
    git(ws, &["update-ref", &claim_ref(id), &blob])?;
    Ok(mine)
}

fn claim_blob(ws: &Path, claim: &Claim) -> EaiResult<String> {
    let body = serde_json::to_string(claim)?;
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

/// Extend an owned live lease without relinquishing its task or scope locks.
pub fn renew(ws: &Path, id: &str, agent: &str, hours: u64, now: u64) -> EaiResult<Claim> {
    if !valid_id(id) {
        return Err(EaiError::config("invalid task id"));
    }
    let r = remote();
    let generation_ref = "refs/claim-generation/current";
    let generation = git(ws, &["ls-remote", &r, generation_ref])?;
    let generation_old = generation.split_whitespace().next().unwrap_or("");
    sync_claims(ws)?;
    let (old, mut claim) =
        read_claim(ws, id).ok_or_else(|| EaiError::config("no claim to renew"))?;
    let branch = git(ws, &["symbolic-ref", "-q", "--short", "HEAD"]).ok();
    if claim.agent != agent_token(agent)?
        || claim.expired(now)
        || claim
            .branch
            .as_ref()
            .is_some_and(|b| Some(b) != branch.as_ref())
    {
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
    )?;
    git(ws, &["update-ref", &claim_ref(id), &blob])?;
    Ok(claim)
}

/// Give up a claim (only your own unless `force`).
pub fn release(ws: &Path, id: &str, agent: &str, force: bool) -> EaiResult<()> {
    if !valid_id(id) {
        return Err(EaiError::config(format!("invalid task id `{id}`")));
    }
    sync_claims(ws)?;
    let Some((sha, c)) = read_claim(ws, id) else {
        return Ok(());
    };
    if !force && c.agent != agent_token(agent)? {
        return Err(EaiError::config(format!(
            "{id} is claimed by {}, not you",
            c.agent
        )));
    }
    let lease = format!("--force-with-lease={}:{sha}", claim_ref(id));
    let agent_ref = format!("refs/claim-agents/{}", c.agent);
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
        release(&a, &t2.id, "claude", false).unwrap();
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
        // Publication retains ownership until the closing commit is merged.
        assert!(claims(&a).unwrap().iter().any(|c| c.task == passing.id));
        release(&a, &passing.id, "claude", false).unwrap();
        assert!(claims(&a).unwrap().is_empty());
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
}

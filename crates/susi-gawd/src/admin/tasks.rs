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

/// Claim `id` for `agent`. Fails if another live claim exists or a dependency
/// is still open. An expired claim is taken over by compare-and-swap.
pub fn claim(ws: &Path, id: &str, agent: &str, hours: u64, now: u64) -> EaiResult<Claim> {
    let task = find_open(ws, id)?;
    let open: std::collections::HashSet<String> = list_open(ws).into_iter().map(|t| t.id).collect();
    if let Some(dep) = task.deps.iter().find(|d| open.contains(*d)) {
        return Err(EaiError::config(format!(
            "{id} is blocked: dependency {dep} is still open"
        )));
    }
    let agent = agent_token(agent)?;
    sync_claims(ws)?;
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
        task: id.to_string(),
        agent,
        claimed_unix: now,
        lease_until_unix: now + hours.max(1) * 3600,
    };
    let body = serde_json::to_string(&mine)?;
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
    let blob = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let target = format!("{blob}:{}", claim_ref(id));
    let r = remote();
    let pushed = match &existing {
        // Take over an expired lease only if nobody re-claimed it meanwhile.
        Some((old, _)) => {
            let lease = format!("--force-with-lease={}:{old}", claim_ref(id));
            git(ws, &["push", "--quiet", &lease, &r, &target])
        }
        None => git(ws, &["push", "--quiet", &r, &target]),
    };
    pushed.map_err(|_| EaiError::config(format!("{id} was claimed by someone else first")))?;
    Ok(mine)
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
    git(
        ws,
        &[
            "push",
            "--quiet",
            &lease,
            &remote(),
            &format!(":{}", claim_ref(id)),
        ],
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
/// who, when and at which commit) and release the claim. Returns the record.
pub fn close(ws: &Path, id: &str, agent: &str) -> EaiResult<Task> {
    let mut task = find_open(ws, id)?;
    let by = agent_token(agent)?;
    if let Some(c) = claims(ws)?.into_iter().find(|c| c.task == id) {
        if !c.expired(now_unix()) && c.agent != by {
            return Err(EaiError::config(format!(
                "{id} is claimed by {}, not {by}",
                c.agent
            )));
        }
    }
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
    let _ = release(ws, id, agent, true);
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
            ["cargo", "test", "--offline", "--quiet", filter]
                .map(String::from)
                .to_vec()
        };
        let vacuous = addt!(&a, "claude", "vacuous", "", "s", &[], cmd("nomatch")).unwrap();
        claim(&a, &vacuous.id, "claude", 1, now_unix()).unwrap();
        let err = close(&a, &vacuous.id, "claude").unwrap_err().to_string();
        assert!(err.contains("ran no tests"), "{err}");
        assert_eq!(list_open(&a).len(), 1);

        let real = addt!(&a, "claude", "real", "", "s", &[], cmd("real")).unwrap();
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
        claim(&a, &passing.id, "claude", 1, now_unix()).unwrap();
        let done = close(&a, &passing.id, "claude").unwrap();
        assert_eq!(done.closed.as_ref().unwrap().by, "CLAUDE");
        assert!(!done.closed.unwrap().commit.is_empty());
        assert_eq!(list_open(&a).len(), 1);
        assert_eq!(list_done(&a).len(), 1);
        // Closing released the claim.
        assert!(claims(&a).unwrap().iter().all(|c| c.task != passing.id));
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

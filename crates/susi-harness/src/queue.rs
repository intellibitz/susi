//! Turn the roadmap queue into a mission plan.
//!
//! The normal mission mints a task per plan entry. Draining the roadmap queue
//! needs the opposite: choose tasks that are already dependency-ready, group
//! ones whose scopes do not overlap (so their pull requests cannot conflict),
//! and emit a plan whose entries name the existing task ids. The adapter then
//! claims each task in its own worktree and runs the acceptance the task
//! already declares.

use crate::supervisor::{Plan, PlanTask};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use susi_error::{EaiError, EaiResult};

#[derive(Debug, Deserialize)]
struct TaskRecord {
    id: String,
    title: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    accept: Option<Acceptance>,
    #[serde(default)]
    roadmap: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    created_unix: u64,
}

/// The queue stores acceptance as argv; a hand-written task may still hold the
/// original string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Acceptance {
    Argv {
        #[serde(default)]
        cmd: Vec<String>,
    },
    Text(String),
}

impl Acceptance {
    fn command(&self) -> Option<String> {
        match self {
            Self::Argv { cmd } if !cmd.is_empty() => Some(cmd.join(" ")),
            Self::Argv { .. } => None,
            Self::Text(text) if !text.trim().is_empty() => Some(text.clone()),
            Self::Text(_) => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Roadmap {
    #[serde(default)]
    vectors: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
struct Vector {
    id: String,
    #[serde(default)]
    priority: String,
}

/// Build a mission plan from dependency-ready queue tasks.
///
/// `limit` bounds the batch; tasks are taken in priority order (P0 first, then
/// smaller first) and a task whose scopes overlap an already chosen task is
/// skipped, because two workers editing the same tree would conflict on merge.
///
/// # Errors
///
/// Returns `EaiError::io` when the queue cannot be read, `EaiError::config`
/// when a task record is malformed or no candidate is ready.
#[allow(clippy::too_many_arguments)] // the queue's selection inputs, one per filter
pub fn plan_batch(
    repo: &Path,
    mission: &str,
    limit: usize,
    workers: &[String],
    only_roadmap: Option<&str>,
    claimed: &[String],
) -> EaiResult<Plan> {
    let limit = limit.max(1);
    let tasks_dir = repo.join(".agents").join("tasks");
    let done = read_ids(&tasks_dir.join("done"))?;
    let records = read_records(&tasks_dir)?;
    let priorities = roadmap_priorities(repo);
    let total = records.len();
    let crates = crate_names(repo);
    let mut ready = Vec::new();
    let (mut already_done, mut no_accept, mut inferred, mut blocked, mut filtered) =
        (0, 0, 0, 0, 0);
    for mut task in records {
        if done.contains(&task.id) {
            already_done += 1;
            continue;
        }
        if let Some(roadmap) = only_roadmap {
            if task.roadmap.as_deref() != Some(roadmap) {
                filtered += 1;
                continue;
            }
        }
        if task.accept.as_ref().and_then(Acceptance::command).is_none() {
            no_accept += 1;
            continue;
        }
        if task.deps.iter().any(|dep| !done.contains(dep)) {
            blocked += 1;
            continue;
        }
        if task.scopes.is_empty() {
            // Legacy roadmap records were authored without scopes: scopes only
            // ever lived on claims, and these tasks never had one. Infer a
            // conservative reservation from the goal instead of leaving the
            // whole queue unclaimable; a wrong guess fails the claim or the
            // acceptance, it never silently widens a commit.
            task.scopes = infer_scopes(repo, &task, &crates);
            inferred += 1;
        }
        ready.push(task);
    }
    let mut records = ready;

    // Scopes a live claim already reserves, resolved from the claimed task's
    // own record, so the plan never names a task whose claim must fail.
    let claimed_scopes: Vec<Vec<String>> = claimed
        .iter()
        .filter_map(|id| records.iter().find(|task| &task.id == id))
        .map(|task| {
            if task.scopes.is_empty() {
                infer_scopes(repo, task, &crates)
            } else {
                task.scopes.clone()
            }
        })
        .collect();

    records.sort_by(|left, right| {
        priority_rank(left, &priorities)
            .cmp(&priority_rank(right, &priorities))
            .then_with(|| size_rank(left).cmp(&size_rank(right)))
            .then_with(|| right.created_unix.cmp(&left.created_unix))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut selected: Vec<&TaskRecord> = Vec::new();
    for task in &records {
        if claimed_scopes
            .iter()
            .any(|scopes| scopes_overlap(&task.scopes, scopes))
        {
            blocked += 1;
            continue;
        }
        if selected
            .iter()
            .any(|other| scopes_overlap(&task.scopes, &other.scopes))
        {
            continue;
        }
        selected.push(task);
        if selected.len() == limit {
            break;
        }
    }
    if selected.is_empty() {
        return Err(EaiError::config(format!(
            "no dependency-ready queue task has a free scope (open {total}, ready {}, already-done {already_done}, no-accept {no_accept}, inferred-scope {inferred}, dep-blocked {blocked}, roadmap-filtered {filtered}, done ids {})",
            records.len(),
            done.len()
        )));
    }

    let tasks = selected
        .iter()
        .enumerate()
        .map(|(index, task)| PlanTask {
            key: slug(&task.id),
            title: task.title.clone(),
            accept: task
                .accept
                .as_ref()
                .and_then(Acceptance::command)
                .unwrap_or_default(),
            scopes: task.scopes.clone(),
            model: if workers.is_empty() {
                None
            } else {
                Some(workers[index % workers.len()].clone())
            },
            task: Some(task.id.clone()),
            prompt: prompt_for(task),
        })
        .collect::<Vec<_>>();
    let plan = Plan {
        mission: mission.to_string(),
        tasks,
    };
    plan.validate()?;
    Ok(plan)
}

fn prompt_for(task: &TaskRecord) -> String {
    let goal = if task.goal.trim().is_empty() {
        todo_goal(&task.title)
    } else {
        task.goal.clone()
    };
    format!(
        "Implement the queued task .agents/tasks/{id}.json. Read that file first. \
Goal: {goal} \
Work only inside the scopes it declares and make its acceptance command pass. \
If the goal names work outside those scopes, stop and report the gap instead of widening the change.",
        id = task.id,
        goal = goal.trim()
    )
}

fn todo_goal(title: &str) -> String {
    format!("Deliver \"{title}\".")
}

fn priority_rank(task: &TaskRecord, priorities: &BTreeMap<String, String>) -> u8 {
    match task.roadmap.as_deref().and_then(|id| priorities.get(id)) {
        Some(priority) if priority.eq_ignore_ascii_case("P0") => 0,
        Some(priority) if priority.eq_ignore_ascii_case("P1") => 1,
        Some(priority) if priority.eq_ignore_ascii_case("P2") => 2,
        _ => 3,
    }
}

fn size_rank(task: &TaskRecord) -> u8 {
    match task.size.as_deref() {
        Some("s") => 0,
        Some("m") => 1,
        Some("l") => 2,
        _ => 3,
    }
}

fn slug(id: &str) -> String {
    let slug: String = id
        .trim_start_matches("T-")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    slug.chars().take(48).collect()
}

/// True when two scope lists could touch the same file.
fn scopes_overlap(left: &[String], right: &[String]) -> bool {
    left.iter()
        .any(|a| right.iter().any(|b| paths_overlap(a, b)))
}

fn paths_overlap(left: &str, right: &str) -> bool {
    let left = normalize(left);
    let right = normalize(right);
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string()
}

/// The workspace crate names, used to spot a crate a goal names.
fn crate_names(repo: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(repo.join("crates")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// Infer a conservative scope set for a legacy task that declares none.
///
/// The goal usually names its crate or paths. Keywords map the broad areas
/// (GMCP, engine, daemon, …) to the crate that owns them. Metadata paths are
/// always included so the worker can record a verdict or its own task file.
/// The result is reduced to the shallowest paths: a claim on `crates/x` covers
/// `crates/x/src/y`, and reserving both would only make the batch planner
/// treat two tasks as conflicting.
fn infer_scopes(repo: &Path, task: &TaskRecord, crates: &[String]) -> Vec<String> {
    let text = format!(
        "{} {} {}",
        task.title,
        task.goal,
        task.roadmap.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    let mut scopes: BTreeSet<String> = BTreeSet::new();
    for token in text.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '`' | '"' | '\'' | ',' | '(' | ')' | '[' | ']' | ':' | ';' | '*'
            )
    }) {
        let token = token.trim_end_matches(['.', '!', '?', '/']);
        if token.len() <= 7 || token.contains("..") {
            continue;
        }
        if [
            "crates/", "tests/", "docs/", "deploy/", "scripts/", ".agents/",
        ]
        .iter()
        .any(|prefix| token.starts_with(prefix))
        {
            scopes.insert(token.to_string());
        }
    }
    for krate in crates {
        if text.contains(krate) {
            scopes.insert(format!("crates/{krate}"));
        }
    }
    let keywords: &[(&[&str], &[&str])] = &[
        (
            &["gmcp", "mcp"],
            &[
                "crates/susi-gmcp",
                "tests/gmcp_requirements.rs",
                "docs/gmcp",
            ],
        ),
        (
            &["gawd", "brain", "swarm"],
            &["crates/susi-gawd", "crates/susi-gawd-swarm"],
        ),
        (&["gemi", "engine"], &["crates/susi-gemi"]),
        (
            &["model", "provider", "inference", "embedding"],
            &["crates/susi-vendor-models"],
        ),
        (&["daemon"], &["crates/susi-daemon"]),
        (&["server"], &["crates/susi-server"]),
        (&["sandbox"], &["crates/susi-sandbox"]),
        (&["a2a"], &["crates/susi-gawd-a2a"]),
        (
            &[
                "harness",
                "orchestr",
                "gateway",
                "native worker",
                "budget",
                "cockpit",
                "supervisor",
            ],
            &["crates/susi-harness"],
        ),
    ];
    let gmcp = text.contains("gmcp") || text.contains("mcp");
    // A BYOK goal says "per-key budget caps", which would otherwise select the
    // harness crate; its real homes are the model provider, the keys CLI and
    // its own acceptance test.
    let byok = text.contains("byok") || text.contains("bring your own key");
    for (needles, paths) in keywords {
        // An MCP task talks about "model requests" without owning the model
        // provider crate; the GMCP mapping already names its real homes.
        if gmcp && needles.contains(&"model") {
            continue;
        }
        if byok && needles.contains(&"budget") {
            continue;
        }
        if needles.iter().any(|needle| text.contains(needle)) {
            for path in *paths {
                if path_exists_or_parent(repo, path) {
                    scopes.insert((*path).to_string());
                }
            }
        }
    }
    if byok {
        for path in ["crates/susi-vendor-models", "src/cli"] {
            scopes.insert((*path).to_string());
        }
        scopes.insert("tests/byok_enhancements.rs".to_string());
    }
    if let Some(roadmap) = &task.roadmap {
        scopes.insert(format!(".agents/roadmap-verdicts/{roadmap}.json"));
        // A GMCP requirement task must leave its evidence on record, and the
        // closure updates the verdict. Reserve both so a compliant worker
        // commit is not refused as out of scope.
        scopes.insert(format!(".agents/gmcp/evidence/{roadmap}.jsonl"));
    }
    scopes.insert(format!(".agents/tasks/{}.json", task.id));
    reduce_scopes(scopes)
}

/// True when the path exists, or its parent directory does, so a scope can name
/// a file the worker is about to create.
fn path_exists_or_parent(repo: &Path, path: &str) -> bool {
    let target = repo.join(path);
    if target.exists() {
        return true;
    }
    target.parent().is_some_and(|parent| parent.is_dir())
}

fn reduce_scopes(scopes: BTreeSet<String>) -> Vec<String> {
    let mut all: Vec<String> = scopes.into_iter().collect();
    all.sort_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)));
    let mut reduced: Vec<String> = Vec::new();
    'each: for scope in all {
        for parent in &reduced {
            if scope == *parent || scope.starts_with(&format!("{parent}/")) {
                continue 'each;
            }
        }
        reduced.push(scope);
    }
    reduced
}

fn read_ids(dir: &Path) -> EaiResult<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
        Err(error) => {
            return Err(EaiError::io(format!(
                "read task dir {}: {error}",
                dir.display()
            )))
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| EaiError::io(format!("read task dir: {error}")))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            ids.insert(stem.to_string());
        }
    }
    Ok(ids)
}

fn read_records(dir: &Path) -> EaiResult<Vec<TaskRecord>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(EaiError::io(format!(
                "read task dir {}: {error}",
                dir.display()
            )))
        }
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| EaiError::io(format!("read task dir: {error}")))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|error| EaiError::io(format!("read {}: {error}", path.display())))?;
        let record: TaskRecord = serde_json::from_str(&text)
            .map_err(|error| EaiError::io(format!("parse {}: {error}", path.display())))?;
        records.push(record);
    }
    Ok(records)
}

fn roadmap_priorities(repo: &Path) -> BTreeMap<String, String> {
    let path = repo.join(".agents").join("roadmap.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    let Ok(roadmap) = serde_json::from_str::<Roadmap>(&text) else {
        return BTreeMap::new();
    };
    roadmap
        .vectors
        .into_iter()
        .map(|vector| (vector.id, vector.priority))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_task(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".agents/tasks/done")).unwrap();
        std::fs::write(
            dir.path().join(".agents/roadmap.json"),
            r#"{"vectors":[
                {"id":"VC-A","priority":"P0"},
                {"id":"VC-B","priority":"P1"}
            ]}"#,
        )
        .unwrap();
        write_task(
            &dir.path().join(".agents/tasks/done"),
            "T-DEP.json",
            r#"{"id":"T-DEP","title":"dep","scopes":["x"]}"#,
        );
        write_task(
            &dir.path().join(".agents/tasks"),
            "T-A.json",
            r#"{
                "id":"T-A","title":"alpha","deps":["T-DEP"],"roadmap":"VC-A",
                "scopes":["crates/susi-gmcp"],"size":"m","created_unix":3,
                "accept":{"cmd":["cargo","test","-p","susi-gmcp","req_a"]}
            }"#,
        );
        write_task(
            &dir.path().join(".agents/tasks"),
            "T-B.json",
            r#"{
                "id":"T-B","title":"beta","roadmap":"VC-B",
                "scopes":["crates/susi-gmcp/src/lib.rs"],"size":"s","created_unix":2,
                "accept":"cargo test -p susi-gmcp req_b"
            }"#,
        );
        write_task(
            &dir.path().join(".agents/tasks"),
            "T-C.json",
            r#"{
                "id":"T-C","title":"blocked","deps":["T-MISSING"],"roadmap":"VC-A",
                "scopes":["crates/other"],"accept":"cargo test -p other req_c"
            }"#,
        );
        write_task(
            &dir.path().join(".agents/tasks"),
            "T-D.json",
            r#"{
                "id":"T-D","title":"delta","roadmap":"VC-A",
                "scopes":["crates/disjoint"],"size":"s","created_unix":1,
                "accept":{"cmd":["cargo","test","-p","disjoint","req_d"]}
            }"#,
        );
        dir
    }

    #[test]
    fn a_batch_takes_ready_disjoint_tasks_and_names_their_ids() {
        let dir = repo();
        let plan = plan_batch(
            dir.path(),
            "roadmap-batch",
            2,
            &["deepseek-flash".to_string(), "gemini-flash".to_string()],
            None,
            &[],
        )
        .unwrap();
        assert_eq!(plan.mission, "roadmap-batch");
        let ids: Vec<&str> = plan
            .tasks
            .iter()
            .filter_map(|task| task.task.as_deref())
            .collect();
        assert_eq!(ids, vec!["T-D", "T-A"]);
        assert_eq!(plan.tasks[0].accept, "cargo test -p disjoint req_d");
        assert_eq!(plan.tasks[0].model.as_deref(), Some("deepseek-flash"));
        assert_eq!(plan.tasks[1].model.as_deref(), Some("gemini-flash"));
        assert!(plan.tasks.iter().all(|task| task.task.is_some()));
    }

    #[test]
    fn overlapping_or_blocked_tasks_are_skipped() {
        let dir = repo();
        let plan = plan_batch(dir.path(), "m", 10, &[], None, &[]).unwrap();
        let ids: Vec<&str> = plan
            .tasks
            .iter()
            .filter_map(|task| task.task.as_deref())
            .collect();
        assert!(!ids.contains(&"T-B"), "T-B overlaps T-A: {ids:?}");
        assert!(!ids.contains(&"T-C"), "T-C is dependency-blocked: {ids:?}");
    }

    #[test]
    fn a_roadmap_filter_narrows_the_batch() {
        let dir = repo();
        let plan = plan_batch(dir.path(), "m", 10, &[], Some("VC-B"), &[]).unwrap();
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].task.as_deref(), Some("T-B"));
    }

    #[test]
    fn a_legacy_task_gets_scopes_inferred_from_its_goal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".agents/tasks/done")).unwrap();
        std::fs::create_dir_all(dir.path().join("crates/susi-gmcp")).unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("crates/susi-gmcp/Cargo.toml"), "").unwrap();
        write_task(
            &dir.path().join(".agents/tasks"),
            "T-GMCP.json",
            r#"{
                "id":"T-GMCP","title":"GMCP contract","roadmap":"VC-X",
                "goal":"Add the GMCP contract test in tests/gmcp_requirements.rs",
                "accept":{"cmd":["cargo","test","--test","gmcp_requirements","req"]}
            }"#,
        );
        let plan = plan_batch(dir.path(), "m", 1, &[], Some("VC-X"), &[]).unwrap();
        let task = &plan.tasks[0];
        assert_eq!(task.task.as_deref(), Some("T-GMCP"));
        assert!(
            task.scopes.iter().any(|scope| scope == "crates/susi-gmcp"),
            "{:?}",
            task.scopes
        );
        assert!(
            task.scopes
                .iter()
                .any(|scope| scope == "tests/gmcp_requirements.rs"),
            "{:?}",
            task.scopes
        );
        assert!(
            task.scopes
                .iter()
                .any(|scope| scope == ".agents/roadmap-verdicts/VC-X.json"),
            "{:?}",
            task.scopes
        );
    }

    #[test]
    fn no_ready_task_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(plan_batch(dir.path(), "m", 2, &[], None, &[]).is_err());
    }
    #[test]
    fn a_roadmap_task_reserves_its_evidence_and_verdict() {
        let dir = std::env::temp_dir();
        let task = TaskRecord {
            id: "T-X-1".into(),
            title: "gmcp operations".into(),
            goal: "gmcp operations evidence".into(),
            deps: Vec::new(),
            scopes: Vec::new(),
            accept: None,
            roadmap: Some("VC-203-005".into()),
            size: Some("l".into()),
            created_unix: 0,
        };
        let scopes = infer_scopes(&dir, &task, &[]);
        assert!(scopes.contains(&".agents/gmcp/evidence/VC-203-005.jsonl".to_string()));
        assert!(scopes.contains(&".agents/roadmap-verdicts/VC-203-005.json".to_string()));
    }
    #[test]
    fn byok_work_infers_its_own_crates_not_the_harness() {
        let task = TaskRecord {
            id: "T-BYOK-1".into(),
            title: "[BYOK] Complete Bring Your Own Key enhancements".into(),
            goal: "dynamic request-level headers, encrypted at-rest storage, unified susi keys CLI, and per-key budget caps".into(),
            deps: Vec::new(),
            scopes: Vec::new(),
            accept: None,
            roadmap: None,
            size: Some("m".into()),
            created_unix: 0,
        };
        let scopes = infer_scopes(&std::env::temp_dir(), &task, &[]);
        assert!(scopes.contains(&"crates/susi-vendor-models".to_string()));
        assert!(scopes.contains(&"src/cli".to_string()));
        assert!(scopes.contains(&"tests/byok_enhancements.rs".to_string()));
        assert!(!scopes.contains(&"crates/susi-harness".to_string()));
    }

    #[test]
    fn a_claimed_task_and_its_overlapping_neighbour_are_not_planned() {
        let dir = repo();
        let plan = plan_batch(dir.path(), "m", 10, &[], None, &["T-A".to_string()]).unwrap();
        let ids: Vec<&str> = plan
            .tasks
            .iter()
            .filter_map(|task| task.task.as_deref())
            .collect();
        assert!(!ids.contains(&"T-A"), "the claimed task is not free");
        assert!(
            !ids.contains(&"T-B"),
            "T-B overlaps the claimed task's scopes"
        );
        assert!(ids.contains(&"T-D"), "disjoint work stays available");
    }
}

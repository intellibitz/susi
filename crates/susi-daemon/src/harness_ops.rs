//! The self-build harness over the socket: the `daemon.harness` plane topic
//! behind the MCP `harness_status`, `harness_budget`, `harness_models`,
//! `harness_plan`, `harness_author`, `harness_run` and `harness_runs` tools
//! (zero-UI plan B4 group 1). Every action calls
//! [`susi_harness::operator`], the library `susi harness` calls.
//!
//! The socket has no caller working directory, so every path is absolute
//! and `repo` names the target repository (the CLI's working directory).
//! A run is long: `run` starts it on a daemon thread and returns at once;
//! `runs` and `status` (with the run's `state`) report its progress.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use susi_harness::fleet;
use susi_harness::operator::{self, ModelsMode};
use susi_harness::supervisor::Plan;

type R<T> = Result<T, String>;

fn msg(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn number(payload: &Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(Value::as_u64)
}

/// An absolute path argument (the socket has no caller working directory).
fn absolute(payload: &Value, key: &str) -> R<Option<PathBuf>> {
    let Some(text) = field(payload, key) else {
        return Ok(None);
    };
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err(format!(
            "{key} must be an absolute path (the socket has no caller working directory)"
        ));
    }
    Ok(Some(path))
}

fn required_path(payload: &Value, key: &str) -> R<PathBuf> {
    absolute(payload, key)?.ok_or_else(|| format!("{key} is required"))
}

/// The repository the action reads (the CLI's working directory): `repo`
/// when given, else the substrate home.
fn repo_or_home(payload: &Value) -> R<PathBuf> {
    Ok(absolute(payload, "repo")?.unwrap_or_else(susi_paths::SusiDirs::substrate_home))
}

fn with_notes(doc: R<Value>, notes: Vec<String>) -> R<Value> {
    let mut doc = doc?;
    if let Some(map) = doc.as_object_mut() {
        map.insert("notes".into(), json!(notes));
        return Ok(doc);
    }
    Ok(json!({ "result": doc, "notes": notes }))
}

/// One mission the daemon is running or ran.
#[derive(Debug, Clone)]
struct RunRecord {
    state: PathBuf,
    started_unix_ms: u64,
    notes: Vec<String>,
    /// `None` while running; the report (or error) and budget line once done.
    outcome: Option<Value>,
}

fn runs() -> &'static Mutex<BTreeMap<String, RunRecord>> {
    static RUNS: OnceLock<Mutex<BTreeMap<String, RunRecord>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn run_view(mission: &str, record: &RunRecord) -> Value {
    json!({
        "mission": mission,
        "state": record.state,
        "started_unix_ms": record.started_unix_ms,
        "running": record.outcome.is_none(),
        "notes": record.notes,
        "outcome": record.outcome,
    })
}

/// The missions this daemon has started, newest state last.
pub fn list_runs() -> Value {
    let runs = runs().lock().unwrap_or_else(|e| e.into_inner());
    Value::Array(runs.iter().map(|(m, r)| run_view(m, r)).collect())
}

/// The `susi harness run` options for a socket request, with the CLI's
/// defaults resolved against `repo` (the CLI resolves them against its
/// working directory, which is the repository).
pub fn run_options(payload: &Value) -> R<operator::RunOptions> {
    let repo = required_path(payload, "repo")?;
    let text = |key: &str, default: &str| field(payload, key).unwrap_or(default).to_string();
    let path = |key: &str, default: &str| -> R<PathBuf> {
        Ok(absolute(payload, key)?.unwrap_or_else(|| repo.join(default)))
    };
    // A socket run always drives this daemon's own binary: the CLI's
    // `--susi` is a local operator choice, and carrying it over the socket
    // would let any bearer holder launch an arbitrary program per task.
    if payload.get("susi").is_some_and(|v| !v.is_null()) {
        return Err(
            "susi cannot be chosen over the socket; a socket run drives the daemon's own binary"
                .to_string(),
        );
    }
    let susi = crate::supervisor::reexec_path()
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)
        .map_err(|e| format!("locate the susi binary the adapter drives: {e}"))?;
    Ok(operator::RunOptions {
        plan: required_path(payload, "plan")?,
        state: path("state", "supervisor-state.json")?,
        gateway: field(payload, "gateway")
            .ok_or("gateway is required")?
            .to_string(),
        model: text("model", "auto"),
        timeout: number(payload, "timeout").unwrap_or(900),
        parallel: number(payload, "parallel").map_or(1, |n| n.max(1) as usize),
        prefix: text("prefix", "harness"),
        workers: text("workers", "auto"),
        budget_usd: text("budget_usd", "5"),
        catalogue: path("catalogue", "deploy/harness/prices.json")?,
        daily_usd: text("daily_usd", "25"),
        ledger: absolute(payload, "ledger")?.unwrap_or_else(|| fleet::default_ledger(&repo)),
        controls: path("controls", "out/control.jsonl")?,
        rsi_log: path("rsi_log", ".agents/rsi-loop.jsonl")?,
        attempts: number(payload, "attempts").map_or(3, |n| n as usize),
        adapter_state: path("adapter_state", "harness-state")?,
        susi,
        repo,
    })
}

/// Finalizes a mission's record when its worker unwinds without having
/// recorded an outcome, so a panicked run never reads as running forever
/// (which would also refuse every later run of that mission).
struct FinalizeOnUnwind<'a> {
    mission: &'a str,
}

impl Drop for FinalizeOnUnwind<'_> {
    fn drop(&mut self) {
        let mut table = runs().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = table.get_mut(self.mission)
            && record.outcome.is_none()
        {
            record.outcome = Some(json!({
                "result": { "error": "worker panicked before recording an outcome" },
                "budget_line": Value::Null,
            }));
        }
    }
}

/// Run one mission's work and record its notes and outcome; the record is
/// finalized even when `work` panics.
fn run_worker(mission: &str, work: impl FnOnce() -> (Vec<String>, Value)) {
    let _guard = FinalizeOnUnwind { mission };
    let (notes, outcome) = work();
    let mut table = runs().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(record) = table.get_mut(mission) {
        record.notes = notes;
        record.outcome = Some(outcome);
    }
}

/// Start `susi harness run` on a daemon thread; refuses a mission this
/// daemon is still running.
pub fn start_run(opts: operator::RunOptions) -> R<Value> {
    let plan = Plan::load(&opts.plan).map_err(|e| format!("load {}: {e}", opts.plan.display()))?;
    let mission = plan.mission.clone();
    // The per-mission default state file sits in the repository (the CLI's
    // working directory).
    let state = operator::run_state_path(&opts, &mission, &opts.repo);
    {
        let mut table = runs().lock().unwrap_or_else(|e| e.into_inner());
        if table.get(&mission).is_some_and(|r| r.outcome.is_none()) {
            return Err(format!(
                "mission {mission} is already running in this daemon"
            ));
        }
        table.insert(
            mission.clone(),
            RunRecord {
                state: state.clone(),
                started_unix_ms: operator::now_unix_ms(),
                notes: Vec::new(),
                outcome: None,
            },
        );
    }
    let thread_mission = mission.clone();
    // A one-shot worker on the supervision registry (VC-202-010): status-
    // visible, and never restarted — a mission resumes from its state file
    // on the operator's next run instead.
    let mut job = Some(move || {
        run_worker(&thread_mission, || {
            let repo = opts.repo.clone();
            let mut notes = Vec::new();
            let outcome = operator::run(&opts, &repo, &mut |line: String| notes.push(line));
            let report = match outcome.report {
                Ok(report) => {
                    let failure = operator::mission_failure(&report);
                    json!({ "report": report, "failure": failure })
                }
                Err(error) => json!({ "error": error.to_string() }),
            };
            let outcome = json!({ "result": report, "budget_line": outcome.budget_line });
            (notes, outcome)
        });
    });
    crate::service_supervision::Supervisor::global().spawn_oneshot(
        &format!("harness-run:{mission}"),
        0,
        move || job.take(),
    );
    Ok(json!({ "mission": mission, "state": state, "started": true }))
}

fn models_mode(name: Option<&str>) -> R<ModelsMode> {
    match name.unwrap_or("registry") {
        "registry" => Ok(ModelsMode::Registry),
        "verify" => Ok(ModelsMode::Verify),
        "discover" => Ok(ModelsMode::Discover),
        "available" => Ok(ModelsMode::Available),
        other => Err(format!(
            "unknown models mode `{other}` (expected registry, verify, discover or available)"
        )),
    }
}

/// Dispatch one `daemon.harness` request: `{action, ..}`.
pub fn dispatch(payload: &Value) -> R<Value> {
    if let Some(error) = susi_harness::providers::Registry::overlay_error() {
        return Err(error.to_string());
    }
    let mut notes = Vec::new();
    let mut sink = |line: String| notes.push(line);
    let doc = match field(payload, "action").ok_or("action is required")? {
        "status" => operator::status(
            required_path(payload, "ledger")?,
            absolute(payload, "events")?,
            absolute(payload, "state")?,
            absolute(payload, "plan")?,
        )
        .map_err(msg),
        "budget" => {
            let repo = repo_or_home(payload)?;
            let ledger =
                absolute(payload, "ledger")?.unwrap_or_else(|| fleet::default_ledger(&repo));
            Ok(operator::budget(&ledger, &repo, &mut sink))
        }
        "models" => {
            let mode = models_mode(field(payload, "mode"))?;
            let provider = field(payload, "provider");
            if provider.is_some() && !matches!(mode, ModelsMode::Verify | ModelsMode::Discover) {
                return Err("provider requires mode verify or discover".to_string());
            }
            let repo = repo_or_home(payload)?;
            operator::models(mode, provider, &repo, &mut sink)
                .map(|doc| {
                    if mode == ModelsMode::Verify {
                        json!({ "report": doc.clone(), "failed": operator::verify_failures(&doc) })
                    } else {
                        doc
                    }
                })
                .map_err(msg)
        }
        "plan" => {
            let repo = required_path(payload, "repo")?;
            let opts = operator::PlanQueueOptions {
                out: required_path(payload, "out")?,
                mission: field(payload, "mission")
                    .unwrap_or("roadmap-batch")
                    .to_string(),
                limit: number(payload, "limit").map_or(1, |n| n as usize),
                workers: field(payload, "workers").unwrap_or("auto").to_string(),
                roadmap: field(payload, "roadmap").map(str::to_string),
                gateway: field(payload, "gateway").map(str::to_string),
                repo: repo.clone(),
            };
            operator::plan_queue(&opts, &repo, &mut sink).map_err(msg)
        }
        "author" => {
            let repo = required_path(payload, "repo")?;
            let opts = operator::AuthorOptions {
                gateway: field(payload, "gateway")
                    .ok_or("gateway is required")?
                    .to_string(),
                model: field(payload, "model").unwrap_or("auto").to_string(),
                roadmap: absolute(payload, "roadmap")?
                    .unwrap_or_else(|| repo.join(".agents/roadmap.json")),
                vector: field(payload, "vector")
                    .ok_or("vector is required")?
                    .to_string(),
                out: required_path(payload, "out")?,
                max_tasks: number(payload, "max_tasks").map_or(3, |n| n as usize),
                workers: field(payload, "workers").unwrap_or("auto").to_string(),
            };
            operator::author(&opts, &repo, &mut sink).map_err(msg)
        }
        "run" => start_run(run_options(payload)?),
        "runs" => Ok(list_runs()),
        other => Err(format!("unknown harness action `{other}`")),
    };
    with_notes(doc, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-harness-ops-{tag}-{}-{}",
            std::process::id(),
            operator::now_unix_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dispatch_refuses_relative_paths_and_missing_arguments() {
        let refused = |payload: Value| dispatch(&payload).unwrap_err();
        assert!(refused(json!({})).contains("action"));
        assert!(refused(json!({"action": "nope"})).contains("unknown harness action"));
        assert!(refused(json!({"action": "status"})).contains("ledger"));
        assert!(refused(json!({"action": "status", "ledger": "rel.jsonl"})).contains("absolute"));
        assert!(refused(json!({"action": "plan"})).contains("repo"));
        assert!(refused(json!({"action": "models", "mode": "bogus"})).contains("unknown models"));
        assert!(
            refused(json!({"action": "models", "provider": "x"})).contains("verify or discover")
        );
        let repo = temp_dir("refuse");
        let repo_text = repo.display().to_string();
        assert!(refused(json!({"action": "author", "repo": repo_text})).contains("gateway"));
        assert!(
            refused(json!({"action": "run", "repo": repo_text, "plan": "p.json"}))
                .contains("absolute")
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn status_and_registry_answer_from_local_state() {
        let repo = temp_dir("status");
        let ledger = repo.join("ledger.jsonl");
        std::fs::write(&ledger, "").unwrap();
        let status = dispatch(&json!({"action": "status", "ledger": ledger})).unwrap();
        assert!(status.is_object());
        let registry = dispatch(&json!({"action": "models"})).unwrap();
        assert!(registry["providers"].is_array());
        assert!(dispatch(&json!({"action": "runs"})).unwrap()["result"].is_array());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn run_options_resolve_cli_defaults_against_the_repository() {
        let repo = temp_dir("opts");
        let plan = repo.join("plan.json");
        let opts = run_options(&json!({
            "repo": repo, "plan": plan, "gateway": "http://127.0.0.1:9180/v1"
        }))
        .unwrap();
        assert_eq!(opts.catalogue, repo.join("deploy/harness/prices.json"));
        assert_eq!(opts.controls, repo.join("out/control.jsonl"));
        assert_eq!(opts.attempts, 3);
        assert!(opts.adapter_state.starts_with(&repo));
        // A plan that does not exist is refused before a thread starts.
        assert!(start_run(opts).unwrap_err().contains("load"));
        // A caller cannot choose the binary a socket run drives.
        let refused = run_options(&json!({
            "repo": repo, "plan": plan, "gateway": "http://127.0.0.1:9180/v1",
            "susi": repo.join("evil.exe"),
        }))
        .unwrap_err();
        assert!(refused.contains("susi cannot be chosen"), "{refused}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_panicking_worker_still_finalizes_its_record() {
        let mission = format!("panic-test-{}", operator::now_unix_ms());
        runs().lock().unwrap().insert(
            mission.clone(),
            RunRecord {
                state: PathBuf::from("state.json"),
                started_unix_ms: 0,
                notes: Vec::new(),
                outcome: None,
            },
        );
        let thread_mission = mission.clone();
        let joined = std::thread::spawn(move || {
            run_worker(&thread_mission, || panic!("injected run failure"));
        })
        .join();
        assert!(joined.is_err(), "the injected panic propagates");
        let table = runs().lock().unwrap_or_else(|e| e.into_inner());
        let outcome = table[&mission].outcome.clone().expect("finalized");
        assert!(
            outcome["result"]["error"]
                .as_str()
                .unwrap()
                .contains("panicked")
        );
        drop(table);
        // A normal worker records its own outcome.
        run_worker(&mission, || (vec!["n".into()], json!({"result": "ok"})));
        let table = runs().lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(table[&mission].outcome, Some(json!({"result": "ok"})));
        assert_eq!(table[&mission].notes, vec!["n".to_string()]);
    }
}

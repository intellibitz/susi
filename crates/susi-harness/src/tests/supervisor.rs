use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

struct FakeExecutor {
    calls: Mutex<Vec<String>>,
    failing: Vec<String>,
    active: AtomicUsize,
    peak: AtomicUsize,
    /// Keys preparing right now.
    in_flight: Mutex<Vec<String>>,
    /// Every pair of keys that prepared at the same time.
    together: Mutex<Vec<(String, String)>>,
}

impl FakeExecutor {
    fn new(failing: Vec<String>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            failing,
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            in_flight: Mutex::new(Vec::new()),
            together: Mutex::new(Vec::new()),
        }
    }

    fn ran_together(&self, a: &str, b: &str) -> bool {
        self.together
            .lock()
            .unwrap()
            .iter()
            .any(|(x, y)| (x == a && y == b) || (x == b && y == a))
    }
}

impl TaskExecutor for FakeExecutor {
    type Prepared = String;

    fn prepare(&self, task: &PlanTask) -> EaiResult<String> {
        self.calls.lock().unwrap().push(task.key.clone());
        {
            let mut in_flight = self.in_flight.lock().unwrap();
            let mut together = self.together.lock().unwrap();
            for other in in_flight.iter() {
                together.push((other.clone(), task.key.clone()));
            }
            in_flight.push(task.key.clone());
        }
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.in_flight
            .lock()
            .unwrap()
            .retain(|key| key != &task.key);
        Ok(task.key.clone())
    }

    fn land(&self, key: String) -> EaiResult<TaskStatus> {
        if self.failing.contains(&key) {
            Ok(TaskStatus::Failed {
                reason: "no".into(),
            })
        } else {
            Ok(TaskStatus::Done {
                task: format!("T-{key}"),
            })
        }
    }
}

fn plan() -> Plan {
    Plan {
        mission: "m-1".into(),
        tasks: ["a", "b", "c"]
            .iter()
            .map(|key| PlanTask {
                key: (*key).into(),
                title: format!("task {key}"),
                accept: "cargo test -p susi-harness".into(),
                // Disjoint scopes, so the tasks may share a wave.
                scopes: vec![format!("docs/{key}.md")],
                model: None,
                task: None,
                deps: Vec::new(),
                prompt: "do it".into(),
            })
            .collect(),
    }
}

fn spec(key: &str, scopes: &[&str], deps: &[&str]) -> PlanTask {
    PlanTask {
        key: key.into(),
        title: format!("task {key}"),
        accept: "cargo test -p susi-harness".into(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        model: None,
        task: None,
        deps: deps.iter().map(|dep| (*dep).to_string()).collect(),
        prompt: "do it".into(),
    }
}

fn mission(tasks: Vec<PlanTask>) -> Plan {
    Plan {
        mission: "m-wave".into(),
        tasks,
    }
}

#[test]
fn overlapping_scopes_run_in_sequential_waves_and_both_land() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = FakeExecutor::new(vec![]);
    let plan = mission(vec![
        spec("core", &["crates/susi-core"], &[]),
        spec("lib", &["Crates\\Susi-Core\\src\\lib.rs"], &[]),
        spec("defs", &["crates/susi-core-defs"], &[]),
    ]);
    let report = Supervisor::new(dir.path().join("state.json"), &executor)
        .with_parallelism(4)
        .run(&plan)
        .unwrap();
    assert_eq!(report.done, vec!["core", "defs", "lib"]);
    assert!(report.failed.is_empty());
    assert!(!executor.ran_together("core", "lib"));
    // The sibling crate is disjoint and shares the first wave.
    assert!(executor.ran_together("core", "defs"));
    assert_eq!(*executor.calls.lock().unwrap().last().unwrap(), "lib");
}

#[test]
fn disjoint_scopes_prepare_concurrently_in_one_wave() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = FakeExecutor::new(vec![]);
    let plan = mission(vec![
        spec("a", &["docs/a.md"], &[]),
        spec("b", &["docs/b.md"], &[]),
    ]);
    let report = Supervisor::new(dir.path().join("state.json"), &executor)
        .with_parallelism(2)
        .run(&plan)
        .unwrap();
    assert_eq!(report.done, vec!["a", "b"]);
    assert!(executor.ran_together("a", "b"));
}

#[test]
fn a_dependent_waits_for_its_dependency_and_fails_when_it_fails() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = FakeExecutor::new(vec!["base".into()]);
    let plan = mission(vec![
        spec("leaf", &["docs/leaf.md"], &["mid"]),
        spec("base", &["docs/base.md"], &[]),
        spec("mid", &["docs/mid.md"], &["base"]),
        spec("free", &["docs/free.md"], &[]),
    ]);
    let state = dir.path().join("state.json");
    let report = Supervisor::new(&state, &executor)
        .with_parallelism(4)
        .run(&plan)
        .unwrap();
    assert_eq!(report.done, vec!["free"]);
    // The failure cascades in dependency order: base, then mid, then leaf.
    assert_eq!(
        report.failed,
        vec![
            ("base".to_string(), "no".to_string()),
            (
                "mid".to_string(),
                "dependency base did not complete".to_string()
            ),
            (
                "leaf".to_string(),
                "dependency mid did not complete".to_string()
            ),
        ]
    );
    // Neither dependent was ever prepared.
    let mut calls = executor.calls.lock().unwrap().clone();
    calls.sort();
    assert_eq!(calls, vec!["base", "free"]);
    // The failures are persisted, so a rerun retries the chain in order.
    let retry = FakeExecutor::new(vec![]);
    let second = Supervisor::new(&state, &retry)
        .with_parallelism(4)
        .run(&plan)
        .unwrap();
    assert_eq!(second.skipped, vec!["free"]);
    assert_eq!(*retry.calls.lock().unwrap(), vec!["base", "mid", "leaf"]);
    assert_eq!(second.done, vec!["base", "mid", "leaf"]);
}

#[test]
fn a_landed_dependency_from_an_earlier_run_unblocks_its_dependent() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let first = mission(vec![spec("base", &["docs/base.md"], &[])]);
    let executor = FakeExecutor::new(vec![]);
    Supervisor::new(&state, &executor).run(&first).unwrap();
    let second = mission(vec![
        spec("base", &["docs/base.md"], &[]),
        spec("next", &["docs/next.md"], &["base"]),
    ]);
    let report = Supervisor::new(&state, &executor).run(&second).unwrap();
    assert_eq!(report.skipped, vec!["base"]);
    assert_eq!(report.done, vec!["next"]);
}

#[test]
fn a_plan_rejects_unknown_deps_cycles_and_bad_scopes() {
    let unknown = r#"{"mission":"m","tasks":[
        {"key":"a","title":"t","accept":"cargo test","scopes":["x"],"prompt":"p","deps":["ghost"]}
    ]}"#;
    let error = Plan::from_json(unknown).unwrap_err().to_string();
    assert!(error.contains("ghost"), "{error}");

    let cycle = r#"{"mission":"m","tasks":[
        {"key":"a","title":"t","accept":"cargo test","scopes":["x"],"prompt":"p","deps":["b"]},
        {"key":"b","title":"t","accept":"cargo test","scopes":["y"],"prompt":"p","deps":["a"]}
    ]}"#;
    let error = Plan::from_json(cycle).unwrap_err().to_string();
    assert!(error.contains("dependency cycle"), "{error}");

    let escape = r#"{"mission":"m","tasks":[
        {"key":"a","title":"t","accept":"cargo test","scopes":["../x"],"prompt":"p"}
    ]}"#;
    assert!(Plan::from_json(escape).is_err());

    // A plan without `deps` still parses: the field is optional.
    let legacy = r#"{"mission":"m","tasks":[
        {"key":"a","title":"t","accept":"cargo test","scopes":["x"],"prompt":"p"},
        {"key":"b","title":"t","accept":"cargo test","scopes":["y"],"prompt":"p","deps":["a"]}
    ]}"#;
    let plan = Plan::from_json(legacy).unwrap();
    assert!(plan.tasks[0].deps.is_empty());
    assert_eq!(plan.tasks[1].deps, vec!["a"]);
}

#[test]
fn run_rejects_a_cyclic_plan_built_without_validation() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = FakeExecutor::new(vec![]);
    let plan = mission(vec![
        spec("a", &["docs/a.md"], &["b"]),
        spec("b", &["docs/b.md"], &["a"]),
    ]);
    assert!(Supervisor::new(dir.path().join("state.json"), &executor)
        .run(&plan)
        .is_err());
    assert!(executor.calls.lock().unwrap().is_empty());
}

#[test]
fn a_run_lands_every_task_and_persists_state() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = FakeExecutor::new(vec![]);
    let supervisor = Supervisor::new(dir.path().join("state.json"), &executor);
    let report = supervisor.run(&plan()).unwrap();
    assert_eq!(report.done, vec!["a", "b", "c"]);
    assert!(report.failed.is_empty());
    assert!(report.skipped.is_empty());
    assert!(dir.path().join("state.json").is_file());
}

#[test]
fn a_rerun_skips_what_landed() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = FakeExecutor::new(vec![]);
    let supervisor = Supervisor::new(dir.path().join("state.json"), &executor);
    supervisor.run(&plan()).unwrap();
    let second = supervisor.run(&plan()).unwrap();
    assert_eq!(second.skipped, vec!["a", "b", "c"]);
    assert_eq!(executor.calls.lock().unwrap().len(), 3);
}

#[test]
fn a_failed_task_is_retried_on_the_next_run() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let failing = FakeExecutor::new(vec!["b".into()]);
    let state = dir.path().join("state.json");
    let first = Supervisor::new(&state, &failing).run(&plan()).unwrap();
    assert_eq!(first.done, vec!["a", "c"]);
    assert_eq!(first.failed.len(), 1);

    let retry = FakeExecutor::new(vec![]);
    let second = Supervisor::new(&state, &retry).run(&plan()).unwrap();
    assert_eq!(second.done, vec!["b"]);
    assert_eq!(second.skipped, vec!["a", "c"]);
}

#[test]
fn tasks_prepare_in_parallel_and_land_in_order() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = FakeExecutor::new(vec![]);
    let supervisor = Supervisor::new(dir.path().join("state.json"), &executor).with_parallelism(2);
    let report = supervisor.run(&plan()).unwrap();
    assert_eq!(report.done, vec!["a", "b", "c"]);
    let peak = executor.peak.load(Ordering::SeqCst);
    assert!(
        peak >= 2,
        "expected two prepares in flight, peak was {peak}"
    );
}

/// A task that finds no model it can pay for stops the mission; its
/// batch-mates were prepared beside it.
struct ExhaustingExecutor {
    exhausting: String,
    exhausted: std::sync::atomic::AtomicBool,
    landed: Mutex<Vec<String>>,
}

impl TaskExecutor for ExhaustingExecutor {
    type Prepared = String;

    fn prepare(&self, task: &PlanTask) -> EaiResult<String> {
        if task.key == self.exhausting {
            self.exhausted.store(true, Ordering::SeqCst);
        }
        Ok(task.key.clone())
    }

    fn land(&self, key: String) -> EaiResult<TaskStatus> {
        if key == self.exhausting {
            return Ok(TaskStatus::Failed {
                reason: "no model can run".into(),
            });
        }
        self.landed.lock().unwrap().push(key.clone());
        Ok(TaskStatus::Done {
            task: format!("T-{key}"),
        })
    }

    fn budget_exhausted(&self) -> bool {
        self.exhausted.load(Ordering::SeqCst)
    }
}

/// The stop broke out after the batch's first task, so a batch-mate whose
/// work had passed was never landed: its commit stayed unmerged and its
/// claim held, and the report called it unstarted.
#[test]
fn a_stop_for_a_top_up_still_lands_the_work_its_batch_prepared() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = ExhaustingExecutor {
        exhausting: "a".into(),
        exhausted: std::sync::atomic::AtomicBool::new(false),
        landed: Mutex::new(Vec::new()),
    };
    let report = Supervisor::new(dir.path().join("state.json"), &executor)
        .with_parallelism(2)
        .run(&plan())
        .unwrap();
    assert!(report.budget_exhausted);
    assert_eq!(report.done, vec!["b"]);
    assert_eq!(*executor.landed.lock().unwrap(), vec!["b"]);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.unstarted, vec!["c"]);
}

#[test]
fn a_plan_is_validated() {
    let duplicate = r#"{
        "mission": "m",
        "tasks": [
            {"key":"a","title":"t","accept":"cargo test","scopes":["x"],"prompt":"p"},
            {"key":"a","title":"t","accept":"cargo test","scopes":["x"],"prompt":"p"}
        ]
    }"#;
    assert!(Plan::from_json(duplicate).is_err());

    let no_scopes = r#"{
        "mission": "m",
        "tasks": [{"key":"a","title":"t","accept":"cargo test","prompt":"p"}]
    }"#;
    assert!(Plan::from_json(no_scopes).is_err());
    assert!(Plan::from_json("{}").is_err());
}

fn phases(events: &[LifecycleEvent]) -> Vec<&str> {
    events.iter().map(|event| event.phase.as_str()).collect()
}

#[test]
fn a_prepared_task_earns_its_lifecycle_events() {
    let accepted = work_events("T-1", true, true, "ok".into(), 10);
    assert_eq!(phases(&accepted), vec!["implemented", "gated"]);

    let rejected = work_events("T-1", true, false, "tests red".into(), 10);
    assert_eq!(phases(&rejected), vec!["implemented", "defect"]);
    assert_eq!(rejected[1].outcome.as_deref(), Some("not-accepted"));
    assert_eq!(rejected[1].detail.as_deref(), Some("tests red"));

    let nothing = work_events("T-1", false, false, "no work".into(), 10);
    assert_eq!(phases(&nothing), vec!["defect"]);
}

struct StubRunner {
    outcome: RunOutcome,
}

impl Runner for StubRunner {
    fn run(&self, _config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        Ok(self.outcome.clone())
    }
}

#[test]
fn supervisor_discovers_cli_fleet_and_reports_socket_endpoints() {
    let bin_dir = susi_paths::tempfile::tempdir().unwrap();
    std::fs::write(bin_dir.path().join("claude.exe"), b"").unwrap();
    std::fs::write(bin_dir.path().join("codex.exe"), b"").unwrap();
    std::fs::write(bin_dir.path().join("agy.exe"), b"").unwrap();

    let fleet = discover_cli_fleet_in(&[bin_dir.path().to_path_buf()]);
    assert_eq!(fleet.len(), 7);
    assert!(fleet.iter().find(|t| t.kind == "claude").unwrap().installed);
    assert!(fleet.iter().find(|t| t.kind == "codex").unwrap().installed);
    assert!(
        fleet
            .iter()
            .find(|t| t.kind == "antigravity")
            .unwrap()
            .installed
    );
    assert!(!fleet.iter().find(|t| t.kind == "qwen").unwrap().installed);

    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = FakeExecutor::new(vec![]);
    let sup = Supervisor::new(dir.path().join("state.json"), &executor).with_parallelism(3);
    let status = sup.adaptive_status_report();
    assert_eq!(status.cli_fleet.len(), 7);
    assert_eq!(status.socket_endpoints.gmcp_port, 9090);
    assert_eq!(status.socket_endpoints.gemi_port, 9091);
    assert_eq!(status.socket_endpoints.gawd_port, 9094);
    assert!(status
        .socket_endpoints
        .gmcp_http_url
        .contains("127.0.0.1:9090"));
    assert!(status
        .socket_endpoints
        .gemi_openai_url
        .contains("127.0.0.1:9091/v1/chat/completions"));
    assert!(status
        .socket_endpoints
        .gawd_a2a_url
        .contains("127.0.0.1:9094"));
    assert!(status.embedded_workflow_ready);
}

#[test]
fn supervisor_plans_embedded_workflow_and_executes_adaptive_task_with_failover() {
    use crate::adaptive::AdaptiveStrategy;

    let dir = susi_paths::tempfile::tempdir().unwrap();
    let executor = FakeExecutor::new(vec![]);
    let sup = Supervisor::new(dir.path().join("state.json"), &executor).with_parallelism(2);

    let nodes = vec![
        WorkflowNode {
            key: "t-core".to_string(),
            deps: vec![],
            scopes: vec!["crates/susi-core".to_string()],
        },
        WorkflowNode {
            key: "t-harness".to_string(),
            deps: vec![],
            scopes: vec!["crates/susi-harness".to_string()],
        },
        WorkflowNode {
            key: "t-core-sub".to_string(),
            deps: vec!["t-core".to_string()],
            scopes: vec!["crates/susi-core/src/lib.rs".to_string()],
        },
    ];
    let plan = sup.plan_embedded_workflow("wave-3", nodes).unwrap();
    assert_eq!(plan.wave_id, "wave-3");
    assert_eq!(plan.waves.len(), 2);
    assert_eq!(plan.waves[0], vec!["t-core", "t-harness"]);
    assert_eq!(plan.waves[1], vec!["t-core-sub"]);
    assert!(plan.blocked.is_empty());

    // Reject cyclic dependencies.
    let cyclic = vec![
        WorkflowNode {
            key: "a".to_string(),
            deps: vec!["b".to_string()],
            scopes: vec!["docs/a.md".to_string()],
        },
        WorkflowNode {
            key: "b".to_string(),
            deps: vec!["a".to_string()],
            scopes: vec!["docs/b.md".to_string()],
        },
    ];
    assert!(sup.plan_embedded_workflow("wave-bad", cyclic).is_err());

    // A missing CLI can fail over before any turn starts.
    let cli_fail = StubRunner {
        outcome: RunOutcome::Failed {
            message: "subscription CLI is not installed".to_string(),
        },
    };
    let payg_ok = StubRunner {
        outcome: RunOutcome::Succeeded {
            final_text: "completed on PAYG BYOK".to_string(),
            reported_cost_usd: Some(0.0125),
        },
    };
    let adaptive = AdaptiveRunner::new(payg_ok, cli_fail, AdaptiveStrategy::PreferCliSubThenPayg)
        .with_fallback_payg_model("deepseek-flash");

    let req = AdaptiveTaskRequest {
        mission: "m-adaptive".to_string(),
        task: "t-1".to_string(),
        model: "cli:claude:opus".to_string(),
        prompt: "implement feature".to_string(),
        workspace: dir.path().to_path_buf(),
        state_dir: dir.path().join("task-state"),
        gateway_url: "http://127.0.0.1:9180".to_string(),
        timeout_secs: 30,
    };
    let outcome = sup.execute_adaptive_task_with(&adaptive, &req);
    assert!(outcome.succeeded);
    assert_eq!(outcome.primary_mode, "monthly-sub-cli");
    assert_eq!(outcome.primary_model, "cli:claude:opus");
    assert_eq!(outcome.failover_mode.as_deref(), Some("payg-byok"));
    assert_eq!(outcome.failover_model.as_deref(), Some("deepseek-flash"));
    assert_eq!(
        outcome.final_text.as_deref(),
        Some("completed on PAYG BYOK")
    );
    assert_eq!(outcome.reported_cost_usd, Some(0.0125));
}

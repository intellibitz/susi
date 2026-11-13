use super::*;
use crate::event::read_events;
use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
use crate::pricing::{ModelPrice, TokenUsage};

struct FakeRunner {
    failing: Vec<String>,
    identical_failure: bool,
    calls: Mutex<Vec<String>>,
}

impl Runner for FakeRunner {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        self.calls.lock().unwrap().push(config.model.clone());
        if self.failing.contains(&config.model) {
            let message = if self.identical_failure {
                "acceptance failed".to_string()
            } else {
                format!("{} failed", config.model)
            };
            return Ok(RunOutcome::Failed { message });
        }
        Ok(RunOutcome::Succeeded {
            final_text: format!("{} ok", config.model),
            reported_cost_usd: None,
        })
    }
}

fn runner(failing: Vec<&str>) -> FakeRunner {
    FakeRunner {
        failing: failing.into_iter().map(str::to_string).collect(),
        identical_failure: false,
        calls: Mutex::new(Vec::new()),
    }
}

fn runner_with_identical_failures(failing: Vec<&str>) -> FakeRunner {
    FakeRunner {
        failing: failing.into_iter().map(str::to_string).collect(),
        identical_failure: true,
        calls: Mutex::new(Vec::new()),
    }
}

fn catalogue() -> PriceCatalogue {
    let mut models = std::collections::BTreeMap::new();
    models.insert(
        "cheap".to_string(),
        ModelPrice {
            input_miss: MicroUsd::from_micros(100_000),
            input_hit: MicroUsd::from_micros(10_000),
            output: MicroUsd::from_micros(200_000),
        },
    );
    models.insert(
        "capable".to_string(),
        ModelPrice {
            input_miss: MicroUsd::from_micros(1_000_000),
            input_hit: MicroUsd::from_micros(100_000),
            output: MicroUsd::from_micros(2_000_000),
        },
    );
    PriceCatalogue {
        schema: crate::catalog::PRICE_SCHEMA.to_string(),
        models,
    }
}

fn setup_git_board(dir: &Path) -> PathBuf {
    let board = dir.join("board.git");
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let run_git = |cwd: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_git(
        dir,
        &["init", "--quiet", "--bare", &board.to_string_lossy()],
    );
    run_git(&repo, &["init", "--quiet"]);
    run_git(&repo, &["config", "user.name", "Test"]);
    run_git(&repo, &["config", "user.email", "test@example.com"]);
    run_git(&repo, &["config", "commit.gpgsign", "false"]);
    run_git(&repo, &["config", "susi.agent", "harness-test"]);
    run_git(
        &repo,
        &["config", "susi.board.remote-url", &board.to_string_lossy()],
    );
    std::fs::write(repo.join("README.md"), "seed\n").unwrap();
    run_git(&repo, &["add", "README.md"]);
    run_git(&repo, &["commit", "--quiet", "-m", "init"]);
    repo
}

fn mission(dir: &Path, max_parallel: usize, candidates: Vec<&str>) -> MissionConfig {
    let repo = setup_git_board(dir);
    MissionConfig {
        id: "m-1".into(),
        tasks: vec![
            TaskSpec {
                id: "t-1".into(),
                prompt: "one".into(),
                workspace: repo.clone(),
            },
            TaskSpec {
                id: "t-2".into(),
                prompt: "two".into(),
                workspace: repo.clone(),
            },
            TaskSpec {
                id: "t-3".into(),
                prompt: "three".into(),
                workspace: repo,
            },
        ],
        candidates: candidates.into_iter().map(str::to_string).collect(),
        max_parallel,
        timeout: Duration::from_secs(5),
        gateway_url: "http://127.0.0.1:1".into(),
        state_dir: dir.to_path_buf(),
        ledger_path: dir.join("ledger.jsonl"),
        budget: None,
        policy: Policy::default(),
        usage: UsageProfile::agent(),
        catalogue: None,
    }
}

#[test]
fn a_missing_state_root_is_created() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let mut config = mission(dir.path(), 1, vec!["cheap"]);
    config.state_dir = dir.path().join("nested").join("state");
    let report = run_mission(&config, &runner(vec![])).unwrap();
    assert!(report.events_path.exists());
    let events = read_events(&report.events_path).unwrap();
    assert_eq!(events.len(), 8);
    assert_eq!(
        events.first().map(|event| event.kind),
        Some(EventKind::MissionStarted)
    );
    assert_eq!(
        events.last().map(|event| event.kind),
        Some(EventKind::MissionFinished)
    );
}

#[test]
fn every_task_runs_and_succeeds() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let config = mission(dir.path(), 2, vec!["cheap"]);
    let report = run_mission(&config, &runner(vec![])).unwrap();
    assert_eq!(report.succeeded, 3);
    assert_eq!(report.failed, 0);
    assert!(report
        .results
        .iter()
        .all(|result| result.final_text() == Some("cheap ok")));
}

#[test]
fn a_failing_model_falls_to_the_next_candidate() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let config = mission(dir.path(), 1, vec!["cheap", "capable"]);
    let report = run_mission(&config, &runner(vec!["cheap"])).unwrap();
    assert_eq!(report.succeeded, 3);
    let first = &report.results[0];
    assert_eq!(first.attempts.len(), 2);
    assert_eq!(first.attempts[0].model, "cheap");
    assert_eq!(first.attempts[1].model, "capable");
    assert_eq!(first.final_text(), Some("capable ok"));
}

#[test]
fn an_exhausted_ladder_fails_the_task() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let config = mission(dir.path(), 1, vec!["cheap", "capable"]);
    let report = run_mission(&config, &runner(vec!["cheap", "capable"])).unwrap();
    assert_eq!(report.failed, 3);
    assert_eq!(report.results[0].attempts.len(), 2);
    assert!(!report.results[0].succeeded);
    assert!(matches!(
        report.results[0].stopped,
        Some(TaskStop::Ladder(StopReason::Exhausted))
    ));
}

#[test]
fn the_same_failure_on_two_models_stops_the_task_early() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let config = mission(dir.path(), 1, vec!["cheap", "capable", "third"]);
    let runner = runner_with_identical_failures(vec!["cheap", "capable", "third"]);
    let report = run_mission(&config, &runner).unwrap();
    assert_eq!(report.stuck, 3);
    assert_eq!(report.results[0].attempts.len(), 2);
    assert!(matches!(
        report.results[0].stopped,
        Some(TaskStop::Ladder(StopReason::Stuck { .. }))
    ));
    assert!(runner
        .calls
        .lock()
        .unwrap()
        .iter()
        .all(|model| model != "third"));
}

#[test]
fn a_catalogue_ranks_candidates_by_expected_cost() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let mut config = mission(dir.path(), 1, vec!["capable", "cheap"]);
    config.catalogue = Some(catalogue());
    let runner = runner(vec!["capable"]);
    let report = run_mission(&config, &runner).unwrap();
    let first = &report.results[0];
    assert_eq!(first.attempts[0].model, "cheap");
    assert_eq!(first.final_text(), Some("cheap ok"));
    assert!(runner
        .calls
        .lock()
        .unwrap()
        .iter()
        .all(|model| model != "capable"));
}

#[test]
fn a_budget_refuses_the_attempt_instead_of_running_it() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let mut config = mission(dir.path(), 1, vec!["cheap"]);
    config.catalogue = Some(catalogue());
    config.budget = Some(Budget::new(MicroUsd::from_micros(1)));
    let runner = runner(vec![]);
    let report = run_mission(&config, &runner).unwrap();
    assert_eq!(report.succeeded, 0);
    assert!(report.budget_exhausted);
    assert!(runner.calls.lock().unwrap().is_empty());
    assert!(matches!(
        report.results[0].attempts[0].outcome,
        AttemptOutcome::Skipped { .. }
    ));
    let events = read_events(&report.events_path).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == EventKind::BudgetExhausted));
}

#[test]
fn a_budget_without_a_catalogue_is_refused() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let mut config = mission(dir.path(), 1, vec!["cheap"]);
    config.budget = Some(Budget::new(MicroUsd::from_micros(1)));
    assert!(run_mission(&config, &runner(vec![])).is_err());
}

#[test]
fn the_mission_total_comes_from_the_ledger() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let ledger = dir.path().join("ledger.jsonl");
    let entry = LedgerEntry::settle(Dispatch {
        mission: "m-1".into(),
        task: "t-1".into(),
        attempt: 1,
        model: "cheap".into(),
        provider: "deepseek".into(),
        usage: TokenUsage::new(1_000_000, 0, 0),
        price: ModelPrice {
            input_miss: MicroUsd::from_micros(150_000),
            input_hit: MicroUsd::ZERO,
            output: MicroUsd::ZERO,
        },
        started_unix_ms: 0,
        ended_unix_ms: 1,
        outcome: Outcome::Completed,
    });
    append_entry(&ledger, &entry).unwrap();
    let mut config = mission(dir.path(), 1, vec!["cheap"]);
    config.ledger_path = ledger;
    let report = run_mission(&config, &runner(vec![])).unwrap();
    assert_eq!(report.total_cost, MicroUsd::from_micros(150_000));
}

/// A crash cut a row inside a multi-byte character and the next row went
/// onto the torn line. The strict reader failed the whole file, and the
/// mission reported $0 spent; only the torn row may be lost.
#[test]
fn the_mission_total_counts_every_row_around_a_torn_line() {
    use std::io::Write;
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let ledger = dir.path().join("ledger.jsonl");
    let charge = |task: &str| {
        LedgerEntry::settle(Dispatch {
            mission: "m-1".into(),
            task: task.into(),
            attempt: 1,
            model: "cheap".into(),
            provider: "deepseek".into(),
            usage: TokenUsage::new(1_000_000, 0, 0),
            price: ModelPrice {
                input_miss: MicroUsd::from_micros(150_000),
                input_hit: MicroUsd::ZERO,
                output: MicroUsd::ZERO,
            },
            started_unix_ms: 0,
            ended_unix_ms: 1,
            outcome: Outcome::Completed,
        })
    };
    append_entry(&ledger, &charge("before")).unwrap();
    let torn = r#"{"mission":"m-1","task":"torn","outcome":{"rejected":{"reason":"gate “red”"}}}"#;
    let cut = torn.find('“').unwrap() + 1;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&ledger)
        .unwrap();
    file.write_all(&torn.as_bytes()[..cut]).unwrap();
    drop(file);
    append_entry(&ledger, &charge("after")).unwrap();
    let mut config = mission(dir.path(), 1, vec!["cheap"]);
    config.ledger_path = ledger;
    config.catalogue = Some(catalogue());
    let report = run_mission(&config, &runner(vec![])).unwrap();
    assert_eq!(report.total_cost, MicroUsd::from_micros(300_000));
}

struct PanickingOnTaskRunner {
    panic_task: String,
}

impl Runner for PanickingOnTaskRunner {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        assert!(
            config.task != self.panic_task,
            "simulated panic on {}",
            config.task
        );
        Ok(RunOutcome::Succeeded {
            final_text: format!("{} completed {}", config.model, config.task),
            reported_cost_usd: None,
        })
    }
}

#[test]
fn panicking_runner_is_caught_releases_budget_and_lets_sibling_tasks_succeed() {
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let mut config = mission(dir.path(), 2, vec!["cheap"]);
    let cat = catalogue();
    let single_estimate =
        Budget::new(MicroUsd::from_micros(1_000_000)).estimate(cat.price("cheap").unwrap(), "one");
    // Budget fits at most 2 concurrent attempts; if t-1 leaked its reservation on panic,
    // subsequent tasks under a 1-attempt budget would be starved.
    config.catalogue = Some(cat);
    config.budget = Some(Budget::new(single_estimate));
    config.max_parallel = 1;

    let runner = PanickingOnTaskRunner {
        panic_task: "t-1".to_string(),
    };
    let report = run_mission(&config, &runner).unwrap();
    assert_eq!(report.succeeded, 2);
    assert_eq!(report.failed, 1);
    assert!(
        !report.budget_exhausted,
        "panicking task must release its budget reservation"
    );

    let t1 = report.results.iter().find(|r| r.task == "t-1").unwrap();
    assert!(!t1.succeeded);
    assert!(matches!(
        &t1.attempts[0].outcome,
        AttemptOutcome::Error { message } if message.contains("runner panicked:") && message.contains("simulated panic on t-1")
    ));

    let t2 = report.results.iter().find(|r| r.task == "t-2").unwrap();
    assert!(t2.succeeded);
    let t3 = report.results.iter().find(|r| r.task == "t-3").unwrap();
    assert!(t3.succeeded);
}

use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Default)]
struct NativeState {
    calls: Vec<String>,
    commits: usize,
    changed: bool,
    dirty: bool,
    acceptance_failure: bool,
    zero_tests: bool,
    integration_failure: bool,
    prior_changes: bool,
    dropped_tests: bool,
    rewritten: bool,
    snapshot: Vec<u8>,
}
struct NativeRunner {
    directory: PathBuf,
    state: std::sync::Arc<Mutex<NativeState>>,
}
fn native_claim_fixture(directory: &Path, name: &str, scopes: &[String]) {
    let claims = directory.join(".git/susi-local-workflow/claims");
    std::fs::create_dir_all(&claims).unwrap();
    std::fs::create_dir_all(directory.join("target")).unwrap();
    std::fs::write(
        claims.join(format!("{name}.claim")),
        format!(
            "susi-local-claim-v2\n{name}\n{}\n{}\n{}\n",
            directory.display(),
            "a".repeat(40),
            scopes.join("\n")
        ),
    )
    .unwrap();
}
fn native_target(directory: &Path) -> SusiTarget<NativeRunner> {
    SusiTarget {
        repo: directory.into(),
        susi_program: "susi".into(),
        runner: NativeRunner {
            directory: directory.into(),
            state: Default::default(),
        },
    }
}
fn task_snapshot(directory: &Path) -> Vec<u8> {
    let mut paths: Vec<_> = std::fs::read_dir(directory.join(".agents/tasks"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .flat_map(|p| std::fs::read(p).unwrap_or_default())
        .collect()
}
impl CommandRunner for NativeRunner {
    fn run(&self, program: &Path, args: &[String], _cwd: &Path) -> EaiResult<RunOutput> {
        let mut state = self.state.lock().unwrap();
        let call = format!("{} {}", program.display(), args.join(" "));
        state.calls.push(call.clone());
        if args.first().map(String::as_str) == Some("local") {
            match args.get(2).map(String::as_str) {
                Some("start") => {
                    let scopes: Vec<_> = args[4..]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| pair[1].to_ascii_lowercase())
                        .collect();
                    native_claim_fixture(&self.directory, &args[3], &scopes);
                    return Ok(ok(&format!(
                        "Worktree: {}\nTarget: {}\n",
                        self.directory.display(),
                        self.directory.join("target").display()
                    )));
                }
                Some("commit") => {
                    state.commits += 1;
                    state.dirty = false;
                    state.snapshot = task_snapshot(&self.directory);
                }
                Some("integrate") if state.integration_failure => {
                    return Ok(RunOutput {
                        status: 1,
                        stdout: String::new(),
                        stderr: "candidate verification failed".into(),
                    })
                }
                Some("extend" | "integrate" | "release") => {}
                _ => panic!("unexpected native operation {call}"),
            }
            return Ok(ok("native coordinator verified operation"));
        }
        if program == Path::new("git") {
            if state.changed
                && state.dropped_tests
                && args.first().map(String::as_str) == Some("show")
            {
                return Ok(ok("#[test]\nfn one() {}\n#[test]\nfn two() {}\n"));
            }
            if state.changed && state.rewritten && args.iter().any(|a| a == "--numstat") {
                return Ok(ok(
                    "95\t13764\ttests/gmcp_requirements.rs\n3\t1\tdocs/harness.md\n",
                ));
            }
            if args.first().map(String::as_str) == Some("rev-parse") {
                return Ok(ok(&format!("{:040x}", state.commits + 1)));
            }
            if args.first().map(String::as_str) == Some("status") {
                return Ok(ok(
                    if state.dirty || state.snapshot != task_snapshot(&self.directory) {
                        " M docs/harness.md\n"
                    } else {
                        ""
                    },
                ));
            }
            if args.first().map(String::as_str) == Some("log") {
                return Ok(ok(if state.changed && state.commits > 1 {
                    "commit accepted edit\n"
                } else {
                    ""
                }));
            }
            if args.iter().any(|a| a == "diff") && args.iter().any(|a| a == "--name-only") {
                return Ok(ok(if state.prior_changes {
                    "docs/prior.md\n"
                } else if state.changed && state.dropped_tests {
                    "tests/gmcp_requirements.rs\n"
                } else {
                    ""
                }));
            }
            return Ok(ok(""));
        }
        if program == Path::new("cargo") {
            if args.first().map(String::as_str) == Some("test") {
                if state.acceptance_failure {
                    return Ok(RunOutput {
                        status: 1,
                        stdout: String::new(),
                        stderr: "actual acceptance failure".into(),
                    });
                }
                return Ok(ok(if state.zero_tests {
                    "test result: ok. 0 passed; 0 failed"
                } else {
                    "test result: ok. 1 passed; 0 failed"
                }));
            }
            return Ok(ok(""));
        }
        panic!("unexpected external dependency {call}");
    }
}
struct NativeExec {
    state: std::sync::Arc<Mutex<NativeState>>,
    outcome: RunOutcome,
    edits: bool,
}
impl Runner for NativeExec {
    fn run(&self, _config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        let mut state = self.state.lock().unwrap();
        state.changed = self.edits;
        state.dirty = self.edits;
        Ok(self.outcome.clone())
    }
}

type Call = (PathBuf, Vec<String>, PathBuf);

struct FakeRunner {
    calls: Mutex<Vec<Call>>,
    replies: Mutex<VecDeque<RunOutput>>,
}

impl FakeRunner {
    fn with(replies: Vec<RunOutput>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        }
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, program: &Path, args: &[String], cwd: &Path) -> EaiResult<RunOutput> {
        self.calls
            .lock()
            .unwrap()
            .push((program.to_path_buf(), args.to_vec(), cwd.to_path_buf()));
        Ok(self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(RunOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            }))
    }
}

fn ok(stdout: &str) -> RunOutput {
    RunOutput {
        status: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn target(replies: Vec<RunOutput>) -> SusiTarget<FakeRunner> {
    SusiTarget {
        repo: PathBuf::from("repo"),
        susi_program: PathBuf::from("susi"),
        runner: FakeRunner::with(replies),
    }
}

#[test]
fn start_worktree_reads_the_native_worktree_field() {
    let target = target(vec![ok("Worktree: E:/src/wt\nTarget: E:/src/wt/target\n")]);
    assert_eq!(
        target.start_worktree("demo").unwrap(),
        PathBuf::from("E:/src/wt")
    );
    let calls = target.runner.calls.lock().unwrap();
    assert_eq!(
        calls[0].1,
        [
            "local",
            "workflow",
            "start",
            "demo",
            "--scope",
            ".agents/tasks/T-LOCAL-DEMO-1.json",
            "--scope",
            ".agents/tasks/done/T-LOCAL-DEMO-1.json"
        ]
    );
}

/// Native creation supplies a private target without invoking a shell seeder.
#[test]
fn start_worktree_uses_native_private_target_without_shell_seeding() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
    std::fs::write(
        dir.path().join("scripts/seed-target.sh"),
        "must not execute",
    )
    .unwrap();
    let target = native_target(dir.path());
    target.start_worktree("demo").unwrap();
    assert!(dir.path().join("target").is_dir());
    assert!(!target_is_link(dir.path()));
    let calls = &target.runner.state.lock().unwrap().calls;
    assert_eq!(calls.len(), 1);
    assert!(calls[0].starts_with("susi local workflow start demo --scope"));
}

#[test]
fn add_task_returns_the_id_and_claim_reserves_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    target
        .start_scoped_worktree("demo", &["docs".into()], None)
        .unwrap();
    let id = target
        .add_task(dir.path(), "proof", "cargo test -p susi-harness")
        .unwrap();
    assert_eq!(id, "T-LOCAL-DEMO-1");
    target.claim(dir.path(), &id, &["docs".into()]).unwrap();
    let record: Value =
        serde_json::from_slice(&std::fs::read(task_file(dir.path(), &id).unwrap()).unwrap())
            .unwrap();
    assert_eq!(record["owner"], "demo");
    assert_eq!(record["status"], "claimed");
    assert_eq!(
        target.acceptance_argv(dir.path(), &id).unwrap(),
        ["cargo", "test", "-p", "susi-harness"]
    );
}

#[test]
fn acceptance_is_read_from_the_task_file_and_run_in_the_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = dir.path().join(".agents").join("tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::write(
        tasks.join("T-1.json"),
        r#"{"id":"T-1","accept":"cargo test -p susi-harness"}"#,
    )
    .unwrap();
    let target = target(vec![ok("tests pass")]);
    let argv = target.acceptance_argv(dir.path(), "T-1").unwrap();
    assert_eq!(argv, vec!["cargo", "test", "-p", "susi-harness"]);
    let output = target.accept(dir.path(), "T-1").unwrap();
    assert_eq!(output.stdout, "tests pass");
    let calls = target.runner.calls.lock().unwrap();
    assert_eq!(calls[0].0, PathBuf::from("cargo"));
    assert_eq!(
        calls[0].1,
        vec![
            "test".to_string(),
            "-p".to_string(),
            "susi-harness".to_string()
        ]
    );
    assert_eq!(calls[0].2, dir.path());
}

#[test]
fn acceptance_argv_is_read_from_the_queue_shape() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = dir.path().join(".agents").join("tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    std::fs::write(
        tasks.join("T-2.json"),
        r#"{"accept":{"cmd":["cargo","test","-p","susi-harness"]}}"#,
    )
    .unwrap();
    let argv = target(vec![]).acceptance_argv(dir.path(), "T-2").unwrap();
    assert_eq!(argv, vec!["cargo", "test", "-p", "susi-harness"]);
}

#[test]
fn a_failed_command_is_an_error_with_its_tail() {
    let target = target(vec![RunOutput {
        status: 1,
        stdout: String::new(),
        stderr: "no such task".to_string(),
    }]);
    let error = target.start_worktree("demo").unwrap_err();
    assert!(error.to_string().contains("no such task"));
}

#[test]
fn failed_native_integration_retains_candidate_and_claim() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    target.start_worktree("demo").unwrap();
    let id = target.add_task(dir.path(), "proof", "cargo test").unwrap();
    target.runner.state.lock().unwrap().integration_failure = true;
    assert!(target
        .finish(dir.path(), &id)
        .unwrap_err()
        .to_string()
        .contains("candidate retained"));
    let state = target.runner.state.lock().unwrap();
    assert!(state
        .calls
        .iter()
        .any(|call| call == "susi local workflow integrate demo"));
    assert!(!state
        .calls
        .iter()
        .any(|call| call.contains("workflow release")));
    assert!(local_claim(dir.path()).is_ok());
    let record: Value =
        serde_json::from_slice(&std::fs::read(task_file(dir.path(), &id).unwrap()).unwrap())
            .unwrap();
    assert_eq!(record["integration"], "pending_native_receipt");
}

/// A second worktree with its own native claim, beside the target's own.
fn second_worktree(name: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let id = local_task_id(name);
    // `require_scope` lowercases the path, not the claim: store lowercase.
    let scopes: Vec<String> = task_scopes(&id)
        .iter()
        .map(|scope| scope.to_ascii_lowercase())
        .chain([format!("docs/{name}.md")])
        .collect();
    native_claim_fixture(dir.path(), name, &scopes);
    (dir, id)
}

#[test]
fn finish_batch_integrates_every_claim_in_one_call_then_releases_each() {
    let one = tempfile::tempdir().unwrap();
    let target = native_target(one.path());
    target.start_worktree("one").unwrap();
    let first = target.add_task(one.path(), "first", "cargo test").unwrap();
    let (two, second) = second_worktree("two");
    assert_eq!(
        target.add_task(two.path(), "second", "cargo test").unwrap(),
        second
    );

    target
        .finish_batch(&[(one.path(), first.as_str()), (two.path(), second.as_str())])
        .unwrap();

    let state = target.runner.state.lock().unwrap();
    let integrations: Vec<&String> = state
        .calls
        .iter()
        .filter(|call| call.contains("workflow integrate"))
        .collect();
    assert_eq!(integrations, vec!["susi local workflow integrate one two"]);
    let integrate_at = state
        .calls
        .iter()
        .position(|call| call.contains("workflow integrate"))
        .unwrap();
    let releases: Vec<(usize, &String)> = state
        .calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.contains("workflow release"))
        .collect();
    assert_eq!(
        releases
            .iter()
            .map(|(_, call)| call.as_str())
            .collect::<Vec<_>>(),
        vec![
            "susi local workflow release one",
            "susi local workflow release two"
        ]
    );
    assert!(releases.iter().all(|(at, _)| *at > integrate_at));
    for (dir, id) in [(one.path(), &first), (two.path(), &second)] {
        assert!(dir.join(format!(".agents/tasks/done/{id}.json")).is_file());
    }
}

#[test]
fn a_failed_batch_integration_releases_nothing() {
    let one = tempfile::tempdir().unwrap();
    let target = native_target(one.path());
    target.start_worktree("one").unwrap();
    let first = target.add_task(one.path(), "first", "cargo test").unwrap();
    let (two, second) = second_worktree("two");
    target.add_task(two.path(), "second", "cargo test").unwrap();
    target.runner.state.lock().unwrap().integration_failure = true;

    let error = target
        .finish_batch(&[(one.path(), first.as_str()), (two.path(), second.as_str())])
        .unwrap_err()
        .to_string();
    assert!(error.contains("candidates retained"), "{error}");
    let state = target.runner.state.lock().unwrap();
    assert!(!state
        .calls
        .iter()
        .any(|call| call.contains("workflow release")));
    assert!(local_claim(one.path()).is_ok());
    assert!(local_claim(two.path()).is_ok());
    assert!(target.finish_batch(&[]).is_err());
}

#[test]
fn private_candidate_done_record_preserves_real_queue_dependency_contract() {
    let candidate = tempfile::tempdir().unwrap();
    let primary = tempfile::tempdir().unwrap();
    let target = native_target(candidate.path());
    target
        .start_scoped_worktree("demo", &["docs/first.md".into()], None)
        .unwrap();
    let id = target
        .add_task(candidate.path(), "first", "cargo test")
        .unwrap();
    let open = task_file(candidate.path(), &id).unwrap();
    let original = std::fs::read(&open).unwrap();
    let dependent = serde_json::json!({"id":"T-DEPENDENT-1","title":"dependent","deps":[id],"scopes":["docs/second.md"],"accept":{"cmd":["cargo","test"]}});
    for repo in [primary.path(), candidate.path()] {
        std::fs::create_dir_all(repo.join(".agents/tasks/done")).unwrap();
        std::fs::write(
            repo.join(".agents/tasks/T-DEPENDENT-1.json"),
            serde_json::to_vec(&dependent).unwrap(),
        )
        .unwrap();
    }
    std::fs::write(
        primary.path().join(format!(".agents/tasks/{id}.json")),
        &original,
    )
    .unwrap();
    let plan = |repo: &Path| {
        crate::queue::plan_batch(repo, "contract", 2, &["worker".into()], None, &[]).unwrap()
    };
    assert_eq!(
        plan(primary.path()).tasks[0].task.as_deref(),
        Some(id.as_str())
    );
    target.runner.state.lock().unwrap().integration_failure = true;
    assert!(target.finish(candidate.path(), &id).is_err());
    assert!(!open.exists());
    assert!(candidate
        .path()
        .join(format!(".agents/tasks/done/{id}.json"))
        .is_file());
    // A second close validates the same artifact and does not overwrite it.
    target.close(candidate.path(), &id).unwrap();
    let candidate_plan = plan(candidate.path());
    assert_eq!(candidate_plan.tasks.len(), 1);
    assert_eq!(
        candidate_plan.tasks[0].task.as_deref(),
        Some("T-DEPENDENT-1")
    );
    // Failed native integration has never published that artifact to the
    // primary queue, so its dependency remains blocked there.
    let primary_plan = plan(primary.path());
    assert_eq!(primary_plan.tasks.len(), 1);
    assert_eq!(primary_plan.tasks[0].task.as_deref(), Some(id.as_str()));
    assert_eq!(
        std::fs::read(primary.path().join(format!(".agents/tasks/{id}.json"))).unwrap(),
        original
    );
    assert!(!primary
        .path()
        .join(format!(".agents/tasks/done/{id}.json"))
        .exists());
}

#[test]
fn task_admission_preserves_existing_records_and_requires_native_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    assert!(target
        .add_task(dir.path(), "no owner", "cargo test")
        .is_err());
    target.start_worktree("demo").unwrap();
    let id = target
        .add_task(dir.path(), "original", "cargo test")
        .unwrap();
    let path = task_file(dir.path(), &id).unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(target
        .add_task(dir.path(), "overwrite", "cargo test")
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(task_file(dir.path(), "T-../../escape").is_err());
}

#[test]
fn acceptance_preserves_quoted_arguments_and_rejects_shell_expressions() {
    assert_eq!(
        parse_acceptance(r#"cargo test "a b" --manifest-path "C:\work tree\Cargo.toml""#).unwrap(),
        [
            "cargo",
            "test",
            "a b",
            "--manifest-path",
            r"C:\work tree\Cargo.toml"
        ]
    );
    for value in [
        "cargo test && cargo run",
        "cargo test; next",
        "cargo test | tee",
        "cargo test `echo x`",
        "cargo test \"open",
    ] {
        assert!(parse_acceptance(value).is_err(), "{value}");
    }
}

#[test]
fn linked_task_directory_is_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    target.start_worktree("demo").unwrap();
    std::fs::create_dir_all(dir.path().join(".agents")).unwrap();
    link_dir(elsewhere.path(), &dir.path().join(".agents/tasks"));
    assert!(target.add_task(dir.path(), "unsafe", "cargo test").is_err());
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

struct FakeExec {
    outcome: RunOutcome,
    prompts: Mutex<Vec<String>>,
}

impl Runner for FakeExec {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        self.prompts.lock().unwrap().push(config.prompt.clone());
        Ok(self.outcome.clone())
    }
}

#[test]
fn a_prepared_run_claims_commits_accepts_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    let exec = NativeExec {
        state: target.runner.state.clone(),
        outcome: RunOutcome::Succeeded {
            final_text: "done".into(),
            reported_cost_usd: None,
        },
        edits: true,
    };
    let report = run_task(
        &target,
        &spec_for(dir.path(), None, false),
        &exec,
        &SusiRunPolicy::unmetered(),
    )
    .unwrap();
    assert!(report.accepted && report.committed);
    assert_eq!(report.task, "T-LOCAL-REUSED-1");
    assert_eq!(report.attempts.len(), 1);
    land(&target, &report).unwrap();
    let calls = &target.runner.state.lock().unwrap().calls;
    assert!(calls
        .iter()
        .any(|s| s == "susi local workflow integrate reused"));
    assert!(calls
        .iter()
        .any(|s| s == "susi local workflow release reused"));
    assert!(!calls.iter().any(|s| s.contains("origin")
        || s.contains(".sh")
        || s.contains("susi tasks")
        || s.contains("susi workflow")));
}

#[test]
fn an_existing_queue_task_is_claimed_not_minted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".agents/tasks")).unwrap();
    std::fs::write(
        dir.path().join(".agents/tasks/T-EXIST-1.json"),
        r#"{"id":"T-EXIST-1","accept":{"cmd":["cargo","test"]}}"#,
    )
    .unwrap();
    let target = native_target(dir.path());
    let exec = NativeExec {
        state: target.runner.state.clone(),
        outcome: RunOutcome::Succeeded {
            final_text: "done".into(),
            reported_cost_usd: None,
        },
        edits: true,
    };
    let report = run_task(
        &target,
        &spec_for(dir.path(), Some("T-EXIST-1"), false),
        &exec,
        &SusiRunPolicy::unmetered(),
    )
    .unwrap();
    assert_eq!(report.task, "T-EXIST-1");
    assert!(report.accepted);
    assert!(!dir
        .path()
        .join(".agents/tasks/T-LOCAL-REUSED-1.json")
        .exists());
    assert!(target
        .runner
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|s| s.contains("Record T-EXIST-1")));
}

#[test]
fn an_agent_failure_is_not_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    let exec = NativeExec {
        state: target.runner.state.clone(),
        outcome: RunOutcome::Failed {
            message: "provider failed".into(),
        },
        edits: false,
    };
    let report = run_task(
        &target,
        &spec_for(dir.path(), None, false),
        &exec,
        &SusiRunPolicy::unmetered(),
    )
    .unwrap();
    assert!(!report.accepted);
    assert!(!target
        .runner
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|s| s.contains("workflow integrate")));
}

#[test]
fn a_record_only_commit_is_not_work() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    let exec = NativeExec {
        state: target.runner.state.clone(),
        outcome: RunOutcome::Succeeded {
            final_text: "no edits".into(),
            reported_cost_usd: None,
        },
        edits: false,
    };
    let report = run_task(
        &target,
        &spec_for(dir.path(), None, false),
        &exec,
        &SusiRunPolicy::unmetered(),
    )
    .unwrap();
    assert!(!report.accepted);
    assert_eq!(target.runner.state.lock().unwrap().commits, 1);
}

#[test]
fn a_missing_acceptance_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    assert!(target(vec![]).acceptance_argv(dir.path(), "T-404").is_err());
}
#[test]
fn commit_formats_before_it_stages() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    target.start_worktree("demo").unwrap();
    target.add_task(dir.path(), "record", "cargo test").unwrap();
    assert!(target.commit_work(dir.path(), "T-LOCAL-DEMO-1").unwrap());
    let calls = &target.runner.state.lock().unwrap().calls;
    let fmt = calls.iter().position(|s| s == "cargo fmt --all").unwrap();
    let commit = calls
        .iter()
        .position(|s| s.starts_with("susi local workflow commit demo"))
        .unwrap();
    assert!(fmt < commit);
    assert!(!calls
        .iter()
        .any(|s| s.starts_with("git commit") || s.starts_with("git add")));
}
#[test]
fn gate_lints_the_packages_the_scopes_touch() {
    let target = target(vec![ok("")]);
    target
        .gate(
            Path::new("wt"),
            &[
                "crates/susi-gmcp/src/lib.rs".to_string(),
                "xtask/src/main.rs".to_string(),
                "Cargo.lock".to_string(),
            ],
        )
        .unwrap();
    let calls = target.runner.calls.lock().unwrap();
    let first = |name: &str| {
        calls.iter().position(|(program, args, _)| {
            program == Path::new("cargo") && args.first().map(String::as_str) == Some(name)
        })
    };
    // The worktree's target is its own: nothing to clean, and a clean
    // would only recompile what the seed brought.
    assert!(
        first("clean").is_none(),
        "the gate must not clean its own target: {calls:?}"
    );
    let clippy_at = first("clippy").expect("cargo clippy must run");
    assert_eq!(calls.len(), 1, "the gate is one clippy run: {calls:?}");
    let args = &calls[clippy_at].1;
    assert!(args.contains(&"susi-gmcp".to_string()));
    assert!(args.contains(&"xtask".to_string()));
    assert!(args.contains(&"susi".to_string()));
    assert!(args.contains(&"-D".to_string()));
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

/// A shared target is refused without deleting another agent's build output.
#[test]
fn gate_refuses_shared_target_without_touching_other_agent_output() {
    let dir = tempfile::tempdir().unwrap();
    let shared = tempfile::tempdir().unwrap();
    std::fs::write(shared.path().join("keep"), "other agent output").unwrap();
    link_dir(shared.path(), &dir.path().join("target"));
    let target = target(Vec::new());
    assert!(target
        .gate(dir.path(), &["crates/susi-core".into()])
        .is_err());
    assert!(target.runner.calls.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(shared.path().join("keep")).unwrap(),
        "other agent output"
    );
}

/// Native ownership refusal preserves existing directories and files.
#[test]
fn native_start_refusal_preserves_existing_work() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "preserve prior work").unwrap();
    let target = target(vec![RunOutput {
        status: 1,
        stdout: String::new(),
        stderr: "destination already exists; refusing to reuse it".into(),
    }]);
    assert!(target.start_worktree("demo").is_err());
    assert_eq!(target.runner.calls.lock().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
        "preserve prior work"
    );
}

#[test]
fn git_status_returns_the_porcelain_output() {
    let target = target(vec![ok(" M src/lib.rs\n")]);
    let status = target.git_status(Path::new("wt")).unwrap();
    assert_eq!(status, " M src/lib.rs\n");
}
#[test]
fn native_start_refusal_is_not_retried_destructively() {
    let target = target(vec![RunOutput {
        status: 1,
        stdout: String::new(),
        stderr: "agent demo already has a claim".into(),
    }]);
    let error = target.start_worktree("demo").unwrap_err().to_string();
    assert!(error.contains("preserved"));
    assert_eq!(
        target.runner.calls.lock().unwrap().len(),
        1,
        "ownership refusal cannot trigger destructive retry"
    );
}
#[test]
fn a_zero_test_acceptance_is_refused() {
    assert!(SusiTarget::<FakeRunner>::is_test_command(&[
        "cargo".to_string(),
        "test".to_string(),
        "--test".to_string(),
        "x".to_string()
    ]));
    assert!(!SusiTarget::<FakeRunner>::is_test_command(&[
        "scripts/check.sh".to_string()
    ]));
    assert!(!SusiTarget::<FakeRunner>::ran_zero_tests(
        "test result: ok. 3 passed; 0 failed"
    ));
    assert!(SusiTarget::<FakeRunner>::ran_zero_tests(
        "test result: ok. 0 passed; 0 failed"
    ));
    assert!(!SusiTarget::<FakeRunner>::ran_zero_tests("tests pass"));
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// A gateway row for `model` that ended at `at_ms`.
fn gateway_row(model: &str, at_ms: u64, outcome: Outcome) -> LedgerEntry {
    LedgerEntry::settle(Dispatch {
        mission: "m".to_string(),
        task: "T".to_string(),
        attempt: 1,
        model: model.to_string(),
        provider: "deepseek".to_string(),
        usage: TokenUsage::default(),
        price: crate::pricing::ModelPrice::default(),
        started_unix_ms: at_ms,
        ended_unix_ms: at_ms,
        outcome,
    })
}

fn one_model_task(dir: &Path) -> SusiTaskSpec {
    SusiTaskSpec {
        worktree_name: "one".into(),
        title: "docs".into(),
        accept: "cargo test -p susi-harness".into(),
        scopes: vec!["docs/harness.md".into()],
        prompt: "write docs".into(),
        gateway_url: "http://127.0.0.1:1".into(),
        model: "deepseek-flash".into(),
        alternates: vec!["deepseek-flash".into()],
        timeout: Duration::from_secs(5),
        state_dir: dir.join("state"),
        task: None,
        precheck: false,
        net_deletion_allowance: DEFAULT_NET_DELETION_ALLOWANCE,
    }
}

/// The observed failure: a fleet of one model, a 429 that outlived the
/// same-model retry, and every later task of the mission failed in
/// seconds with "the attempt cap was reached" and no call made.
#[test]
fn a_single_model_whose_last_call_was_rate_limited_still_gets_an_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("ledger.jsonl");
    let now = now_unix_ms();
    append_entry(
        &ledger,
        &gateway_row("deepseek-flash", now - 2, Outcome::Completed),
    )
    .unwrap();
    append_entry(
        &ledger,
        &gateway_row(
            "deepseek-flash",
            now - 1,
            Outcome::ModelFailed {
                class: crate::ledger::FailureClass::RateLimited,
            },
        ),
    )
    .unwrap();
    let exec = FakeExec {
        outcome: RunOutcome::Failed {
            message: "tests red".into(),
        },
        prompts: Mutex::new(Vec::new()),
    };
    let target = native_target(dir.path());
    let policy = SusiRunPolicy {
        ledger_path: &ledger,
        mission: "m",
        ..SusiRunPolicy::unmetered()
    };
    let report = run_task(&target, &one_model_task(dir.path()), &exec, &policy).unwrap();
    assert_eq!(report.attempts.len(), 1, "the only model must be tried");
    assert_eq!(report.attempts[0].model, "deepseek-flash");
    assert!(!report.budget_exhausted);
    assert_eq!(exec.prompts.lock().unwrap().len(), 1);
}

#[test]
fn a_ladder_whose_every_key_was_refused_stops_the_mission_without_a_call() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("ledger.jsonl");
    // Refused just now: too recent to probe, so no call leaves the test.
    append_entry(
        &ledger,
        &gateway_row(
            "deepseek-flash",
            now_unix_ms(),
            Outcome::ModelFailed {
                class: crate::ledger::FailureClass::NoCredit,
            },
        ),
    )
    .unwrap();
    let exec = FakeExec {
        outcome: RunOutcome::Succeeded {
            final_text: "done".into(),
            reported_cost_usd: None,
        },
        prompts: Mutex::new(Vec::new()),
    };
    let target = native_target(dir.path());
    let policy = SusiRunPolicy {
        ledger_path: &ledger,
        mission: "m",
        ..SusiRunPolicy::unmetered()
    };
    let report = run_task(&target, &one_model_task(dir.path()), &exec, &policy).unwrap();
    assert!(report.attempts.is_empty());
    assert!(exec.prompts.lock().unwrap().is_empty());
    assert!(
        report.budget_exhausted,
        "the mission stops instead of failing every task"
    );
    let RunOutcome::Failed { message } = &report.agent else {
        panic!("expected a failure, got {:?}", report.agent);
    };
    assert!(message.contains("was refused"), "{message}");
}

#[test]
fn an_order_rebuilt_after_a_refusal_drops_that_provider() {
    // The gateway records a refused provider's failure; the order rebuilt
    // from the ledger must not offer the next attempt to a sibling of the
    // same provider.
    let ladder = vec!["claude-sonnet".to_string(), "deepseek-pro".to_string()];
    let at = 1_000_000;
    let entries = vec![gateway_row(
        "claude-sonnet",
        at,
        Outcome::ModelFailed {
            class: crate::ledger::FailureClass::NoCredit,
        },
    )];
    assert_eq!(
        order_from("claude-sonnet", &ladder, &entries, at + 1),
        vec!["deepseek-pro"],
        "an unfunded provider is not offered again"
    );
}

#[test]
fn a_transient_failure_moves_a_model_back_only_for_the_cooldown() {
    let ladder = vec!["deepseek-flash".to_string(), "deepseek-pro".to_string()];
    let at = 1_000_000;
    let entries = vec![gateway_row(
        "deepseek-flash",
        at,
        Outcome::ModelFailed {
            class: crate::ledger::FailureClass::Timeout,
        },
    )];
    assert_eq!(
        order_from("deepseek-flash", &ladder, &entries, at + 1),
        vec!["deepseek-pro", "deepseek-flash"]
    );
    assert_eq!(
        order_from(
            "deepseek-flash",
            &ladder,
            &entries,
            at + crate::policy::COOLDOWN_MS + 1
        ),
        vec!["deepseek-flash", "deepseek-pro"]
    );
}

/// An ancestry check performs no fetch, cleanup, or retry reset.
#[test]
fn sync_checks_local_ancestry_without_discarding_edits() {
    let dir = tempfile::tempdir().unwrap();
    native_claim_fixture(dir.path(), "demo", &["docs".into()]);
    let target = target(vec![ok("")]);
    target.sync(dir.path()).unwrap();
    let calls = argv_of(&target);
    assert_eq!(calls.len(), 1);
    assert!(calls[0].starts_with("git merge-base --is-ancestor "));
    assert!(!calls[0].contains("origin"));
}

/// Any other refusal is the caller's to see: retrying would hide it.
#[test]
fn sync_reports_a_refusal_that_is_not_leftovers() {
    let dir = tempfile::tempdir().unwrap();
    native_claim_fixture(dir.path(), "demo", &["docs".into()]);
    let target = target(vec![RunOutput {
        status: 1,
        stdout: String::new(),
        stderr: String::new(),
    }]);
    assert!(target
        .sync(dir.path())
        .unwrap_err()
        .to_string()
        .contains("preserve"));
    assert_eq!(target.runner.calls.lock().unwrap().len(), 1);
}
fn argv_of(target: &SusiTarget<FakeRunner>) -> Vec<String> {
    target
        .runner
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(program, args, _)| {
            format!(
                "{} {}",
                program
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().to_string()),
                args.join(" ")
            )
        })
        .collect()
}

fn spec_for(dir: &Path, task: Option<&str>, precheck: bool) -> SusiTaskSpec {
    SusiTaskSpec {
        worktree_name: "reused".into(),
        title: "docs".into(),
        accept: "cargo test -p susi-harness".into(),
        scopes: vec!["docs/harness.md".into()],
        prompt: "write docs".into(),
        gateway_url: "http://127.0.0.1:1".into(),
        model: "deepseek-flash".into(),
        alternates: Vec::new(),
        timeout: Duration::from_secs(5),
        state_dir: dir.join("state"),
        task: task.map(str::to_string),
        precheck,
        net_deletion_allowance: DEFAULT_NET_DELETION_ALLOWANCE,
    }
}

#[test]
fn test_attributes_are_counted_the_way_the_gate_counts_them() {
    let text = "#[test]\nfn a() {}\n  #[tokio::test]\n  async fn b() {}\n\
#[tokio::test(flavor = \"multi_thread\")]\nasync fn c() {}\n#[test_case(1)]\nfn d() {}\n\
// a #[test] inside a comment is not the attribute on its own line\n#[cfg(test)]\nmod tests {}\n";
    assert_eq!(count_test_attributes(text), 3);
    assert_eq!(count_test_attributes(""), 0);
}

/// The observed failure: `workflow start` reused the worktree an earlier
/// run left behind, whose branch carried that worker's commit replacing
/// the acceptance file. The precheck ran there, saw the lone test pass and
/// closed the task with zero model calls. The branch is put back on
/// `the native claim base` before the sync, the claim and anything that judges it.
#[test]
fn reused_worktree_ahead_with_code_is_preserved_and_refused() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    target.runner.state.lock().unwrap().prior_changes = true;
    target
        .start_scoped_worktree("demo", &["docs".into()], None)
        .unwrap();
    assert!(target.reset_reused_worktree(dir.path()).is_err());
    assert!(!target
        .runner
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|s| s.contains("reset") || s.contains("clean")));
}

/// A record-only branch is left alone: nothing to reset, the sync runs
/// first as before.
#[test]
fn worktree_matching_native_base_is_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    target.start_worktree("demo").unwrap();
    assert!(target.reset_reused_worktree(dir.path()).unwrap().is_empty());
    let calls = &target.runner.state.lock().unwrap().calls;
    assert!(calls.iter().any(|s| s.contains(&"a".repeat(40))));
    assert!(!calls
        .iter()
        .any(|s| s.contains("the native claim base") || s.contains("reset")));
}

/// Even with the reset in place, the precheck itself proves the tree it
/// judges equals the native claim base plus the record; a tree with code ahead is
/// never closed as "already passes on the untouched base".
#[test]
fn precheck_refuses_code_ahead_of_native_base() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    target.runner.state.lock().unwrap().prior_changes = true;
    let exec = NativeExec {
        state: target.runner.state.clone(),
        outcome: RunOutcome::Succeeded {
            final_text: "unused".into(),
            reported_cost_usd: None,
        },
        edits: false,
    };
    assert!(run_task(
        &target,
        &spec_for(dir.path(), None, true),
        &exec,
        &SusiRunPolicy::unmetered()
    )
    .is_err());
    assert!(!target
        .runner
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|s| s.contains("integrate")));
}

/// The precheck still closes a task whose acceptance passes on a tree that
/// equals the native claim base plus the record.
#[test]
fn the_precheck_closes_a_task_that_passes_on_the_untouched_base() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    let exec = NativeExec {
        state: target.runner.state.clone(),
        outcome: RunOutcome::Failed {
            message: "must not run".into(),
        },
        edits: false,
    };
    let report = run_task(
        &target,
        &spec_for(dir.path(), None, true),
        &exec,
        &SusiRunPolicy::unmetered(),
    )
    .unwrap();
    assert!(report.accepted);
    assert_eq!(report.model, "precheck");
    assert!(target
        .runner
        .state
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|s| s == "susi local workflow integrate reused"));
}

/// The worker replaced the acceptance file, keeping its one new test and
/// dropping the others, and reported success. Before anything is
/// committed the adapter counts the tests of every changed .rs file
/// against the base, rejects the attempt as a failure the ladder reads,
/// and resets the tree so the next model is not handed the deletion.
#[test]
fn an_attempt_that_deletes_tests_is_rejected_before_it_is_committed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tests")).unwrap();
    std::fs::write(
        dir.path().join("tests/gmcp_requirements.rs"),
        "#[test]\nfn one() {}\n",
    )
    .unwrap();
    let target = native_target(dir.path());
    target.runner.state.lock().unwrap().dropped_tests = true;
    let exec = NativeExec {
        state: target.runner.state.clone(),
        outcome: RunOutcome::Succeeded {
            final_text: "done".into(),
            reported_cost_usd: None,
        },
        edits: true,
    };
    let report = run_task(
        &target,
        &spec_for(dir.path(), None, false),
        &exec,
        &SusiRunPolicy::unmetered(),
    )
    .unwrap();
    assert!(!report.accepted && !report.committed);
    assert!(report.attempts[0].outcome.contains("deletes tests"));
    let calls = &target.runner.state.lock().unwrap().calls;
    assert!(!calls
        .iter()
        .any(|s| s.starts_with("cargo fmt") || s.contains("workflow integrate")));
    assert!(calls.iter().any(|s| s.starts_with("git reset --hard")));
    assert!(dir
        .path()
        .join("state/T-LOCAL-REUSED-1/attempt-1/rejection.txt")
        .is_file());
}

/// A test moved between two changed files keeps the total, so the
/// attempt is judged on its acceptance like any other.
#[test]
fn a_test_moved_between_changed_files_is_not_a_deletion() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "#[test]\nfn a() {}\n").unwrap();
    std::fs::write(
        dir.path().join("b.rs"),
        "#[test]\nfn b() {}\n#[test]\nfn c() {}\n",
    )
    .unwrap();
    let target = target(vec![
        ok("a.rs\nb.rs\n"),
        ok("#[test]\nfn a() {}\n#[test]\nfn c() {}\n"),
        ok("#[test]\nfn b() {}\n"),
    ]);
    assert_eq!(
        target
            .tests_dropped_unless(dir.path(), "base123", false)
            .unwrap(),
        None
    );
}

/// A deleted file's tests count as gone; a file new since the base held
/// none.
#[test]
fn a_deleted_test_file_is_a_deletion_and_a_new_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("new.rs"), "#[test]\nfn n() {}\n").unwrap();
    let target = target(vec![
        ok("gone.rs\nnew.rs\n"),
        ok("#[test]\nfn g1() {}\n#[test]\nfn g2() {}\n"),
        RunOutput {
            status: 128,
            stdout: String::new(),
            stderr: "fatal: path 'new.rs' does not exist in 'base123'".to_string(),
        },
    ]);
    let reason = target
        .tests_dropped_unless(dir.path(), "base123", false)
        .unwrap()
        .expect("two tests became one");
    assert!(
        reason.contains("gone.rs: 2 test(s) at the base, 0 now"),
        "{reason}"
    );
    assert!(!reason.contains("new.rs:"), "{reason}");
}

/// The operator's deliberate override, the one the finish gate honours,
/// lets the deletion through without a diff.
#[test]
fn the_test_removal_override_skips_the_check() {
    let dir = tempfile::tempdir().unwrap();
    let target = target(vec![]);
    assert_eq!(
        target
            .tests_dropped_unless(dir.path(), "base123", true)
            .unwrap(),
        None
    );
    assert!(target.runner.calls.lock().unwrap().is_empty());
}
/// The observed failure: a worker told to append one test replaced the
/// 13,859-line acceptance file with 95 lines, and the acceptance (only
/// the new test) passed. The attempt is rejected before it is committed,
/// the message names the file and the counts, the tree goes back to the
/// record, and the ladder reads a failure.
#[test]
fn an_attempt_that_rewrites_a_file_is_rejected_before_it_is_committed() {
    let dir = tempfile::tempdir().unwrap();
    let target = native_target(dir.path());
    target.runner.state.lock().unwrap().rewritten = true;
    let exec = NativeExec {
        state: target.runner.state.clone(),
        outcome: RunOutcome::Succeeded {
            final_text: "done".into(),
            reported_cost_usd: None,
        },
        edits: true,
    };
    let report = run_task(
        &target,
        &spec_for(dir.path(), None, false),
        &exec,
        &SusiRunPolicy::unmetered(),
    )
    .unwrap();
    assert!(!report.accepted && !report.committed);
    assert!(report.attempts[0].outcome.contains("rewrites files"));
    let calls = &target.runner.state.lock().unwrap().calls;
    assert!(!calls
        .iter()
        .any(|s| s.starts_with("cargo fmt") || s.contains("workflow integrate")));
    assert!(calls.iter().any(|s| s.starts_with("git reset --hard")));
    let saved = std::fs::read_to_string(
        dir.path()
            .join("state/T-LOCAL-REUSED-1/attempt-1/rejection.txt"),
    )
    .unwrap();
    assert!(saved.contains("13764"));
}

/// An edit that removes somewhat more than it adds — dead code, a
/// shortened test — stays within the allowance and is judged on its
/// acceptance; a binary file's counts are not lines.
#[test]
fn a_change_within_the_deletion_allowance_is_not_a_rewrite() {
    let target = target(vec![ok(
        "10\t150\tcrates/x/src/lib.rs\n40\t0\tcrates/x/src/new.rs\n-\t-\tassets/a.png\n",
    )]);
    assert_eq!(
        target
            .rewrite_beyond(Path::new("wt"), "base123", DEFAULT_NET_DELETION_ALLOWANCE)
            .unwrap(),
        None,
        "150 deleted against 50 added is 100 net, under 200"
    );
    let tight = SusiTarget {
        repo: PathBuf::from("repo"),
        susi_program: PathBuf::from("susi"),
        runner: FakeRunner::with(vec![ok("10\t150\tcrates/x/src/lib.rs\n")]),
    };
    let reason = tight
        .rewrite_beyond(Path::new("wt"), "base123", 100)
        .unwrap()
        .expect("140 net over an allowance of 100");
    assert!(
        reason.contains("crates/x/src/lib.rs: 150 line(s) deleted, 140 net"),
        "{reason}"
    );
}

/// The allowance is the operator's to raise for a deliberate rewrite;
/// anything that is not a number is the default.
#[test]
fn the_deletion_allowance_is_read_from_the_environment_value() {
    assert_eq!(
        parse_net_deletion_allowance(None),
        DEFAULT_NET_DELETION_ALLOWANCE
    );
    assert_eq!(
        parse_net_deletion_allowance(Some("")),
        DEFAULT_NET_DELETION_ALLOWANCE
    );
    assert_eq!(
        parse_net_deletion_allowance(Some("lots")),
        DEFAULT_NET_DELETION_ALLOWANCE
    );
    assert_eq!(parse_net_deletion_allowance(Some(" 20000 ")), 20_000);
    assert_eq!(parse_net_deletion_allowance(Some("0")), 0);
}

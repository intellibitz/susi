use super::*;
use crate::executor::{ExecutorConfig, RunOutcome, Runner};
use crate::recursive::{
    AuthorityGrant, CancellationToken, CapabilityContract, ContainerRole, RecursiveTaskNode,
    RECURSIVE_CONTRACT_SCHEMA,
};
use crate::supervisor::{
    build_adaptive_supervisor_report, discover_verified_container_contracts_in,
    execute_recursive_tree_with, RecursiveTreeRun,
};

#[test]
fn registry_json_lists_every_model_once_per_provider() {
    let doc = registry_json();
    let providers = doc["providers"].as_array().unwrap();
    let flat = doc["models"].as_array().unwrap();
    let nested: usize = providers
        .iter()
        .map(|p| p["models"].as_array().unwrap().len())
        .sum();
    assert_eq!(nested, flat.len());
    assert!(doc["aliases"].is_object() || doc["aliases"].is_array());
}

#[test]
fn verify_failures_names_only_failed_providers() {
    let report = json!({"ok": false, "providers": [
        {"provider": "a", "ok": true},
        {"provider": "b", "ok": false},
    ]});
    assert_eq!(verify_failures(&report), vec!["b".to_string()]);
    assert!(verify_failures(&json!({"ok": true, "providers": []})).is_empty());
}

// `budget` and `run` read provider balances from their endpoints
// (`fleet::gather_budgets`), so unit tests stay on the pure helpers.
#[test]
fn spent_today_sums_only_today_and_reports_torn_lines() {
    let dir = std::env::temp_dir().join(format!("susi-harness-op-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ledger = dir.join("ledger.jsonl");
    let now = 86_400_000 * 20_000 + 5;
    std::fs::write(
        &ledger,
        format!(
            "{}\n{}\nnot json\n",
            json!({"started_unix_ms": now, "cost": 7}),
            json!({"started_unix_ms": 5, "cost": 100}),
        ),
    )
    .unwrap();
    let mut seen = Vec::new();
    let spent = spent_today(&ledger, now, &mut |n: String| seen.push(n));
    assert_eq!(spent.micros(), 7);
    assert_eq!(seen.len(), 1, "the torn line is reported: {seen:?}");
    assert_eq!(resolve(&dir, Path::new("ledger.jsonl")), ledger);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_state_is_per_mission_by_default() {
    let opts = RunOptions {
        plan: "plan.json".into(),
        repo: ".".into(),
        state: "supervisor-state.json".into(),
        gateway: "http://127.0.0.1:1".into(),
        model: "auto".into(),
        timeout: 1,
        parallel: 1,
        prefix: "h".into(),
        workers: "auto".into(),
        budget_usd: "1".into(),
        catalogue: "c.json".into(),
        daily_usd: "1".into(),
        ledger: "l.jsonl".into(),
        controls: "c.jsonl".into(),
        rsi_log: "r.jsonl".into(),
        attempts: 1,
        adapter_state: "s".into(),
        susi: "susi".into(),
    };
    assert_eq!(
        run_state_path(&opts, "m1", Path::new("/w")),
        Path::new("/w").join("supervisor-m1.json")
    );
    let mut named = opts.clone();
    named.state = "mine.json".into();
    assert_eq!(
        run_state_path(&named, "m1", Path::new("/w")),
        Path::new("mine.json")
    );
}

#[test]
fn mission_failure_and_mission_alternates_and_tail_behave_deterministically() {
    let mut report = SupervisorReport {
        mission: "m-op".into(),
        done: vec!["t1".into()],
        failed: Vec::new(),
        skipped: Vec::new(),
        unstarted: Vec::new(),
        budget_exhausted: false,
        paused: false,
        aborted: false,
    };
    assert_eq!(mission_failure(&report), None);
    report.failed.push(("t2".into(), "reason".into()));
    assert_eq!(
        mission_failure(&report).as_deref(),
        Some("mission m-op failed 1 task(s)")
    );

    let fleet = Fleet {
        orchestrator: Some("gemini-pro".into()),
        workers: vec!["deepseek-flash".into()],
        fixers: vec!["claude-sonnet".into()],
        ladder: vec!["deepseek-flash".into(), "claude-sonnet".into()],
        standby: vec!["claude-sonnet".into(), "gpt-sol".into()],
        left_off: Vec::new(),
    };
    assert_eq!(
        mission_alternates(&fleet),
        vec![
            "deepseek-flash".to_string(),
            "claude-sonnet".to_string(),
            "gpt-sol".to_string()
        ]
    );
    assert_eq!(tail("short", 10), "short");
    assert_eq!(tail("abcdef", 3), "def");
    assert!(now_unix_ms() > 0);
}

#[test]
fn supervisor_wires_recursive_contracts_and_executes_nested_driver_engine_tree() {
    struct FakeRunner;
    impl Runner for FakeRunner {
        fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
            Ok(RunOutcome::Succeeded {
                final_text: format!("completed:{}:{}", config.model, config.task),
                reported_cost_usd: Some(0.0),
            })
        }
    }

    let scratch = std::env::temp_dir().join(format!(
        "susi-harness-rec-sup-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    std::fs::create_dir_all(&scratch).unwrap();

    let contracts = discover_verified_container_contracts_in(&[]);
    assert_eq!(contracts[0].component_id, "susi-harness");
    assert!(contracts[0].verified_available);

    let doc = recursive_contracts().unwrap();
    assert!(doc.as_array().unwrap().len() >= 8);
    let adaptive_report = build_adaptive_supervisor_report(20);
    assert!(!adaptive_report.recursive_contracts.is_empty());

    let mut test_contracts = contracts;
    test_contracts.push(CapabilityContract {
        contract_version: RECURSIVE_CONTRACT_SCHEMA.to_string(),
        component_id: "cli:claude".to_string(),
        role: ContainerRole::DriverAndEngine,
        capabilities: ["code_edit", "wave_plan"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        verified_available: true,
    });

    let tree = RecursiveTaskNode {
        task_key: "driver_root".to_string(),
        component_id: "susi-harness".to_string(),
        required_capabilities: vec!["wave_plan".to_string()],
        prompt: "coordinate".to_string(),
        grant: AuthorityGrant {
            allowed_scopes: vec!["crates".to_string()],
            allowed_capabilities: ["wave_plan", "code_edit"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            allow_network: false,
            allow_git_publish: false,
            max_depth: 3,
            budget_cap: MicroUsd::from_micros(500),
            timeout_ms: 10_000,
        },
        child_waves: vec![vec![RecursiveTaskNode {
            task_key: "leaf_edit".to_string(),
            component_id: "cli:claude".to_string(),
            required_capabilities: vec!["code_edit".to_string()],
            prompt: "implement".to_string(),
            grant: AuthorityGrant {
                allowed_scopes: vec!["crates/susi-core".to_string()],
                allowed_capabilities: ["code_edit"].into_iter().map(str::to_string).collect(),
                allow_network: false,
                allow_git_publish: false,
                max_depth: 1,
                budget_cap: MicroUsd::from_micros(200),
                timeout_ms: 5_000,
            },
            child_waves: Vec::new(),
        }]],
    };

    let cancel = CancellationToken::new();
    let run = RecursiveTreeRun {
        sandbox_root: &scratch,
        gateway_url: "http://127.0.0.1:9185",
        default_model: "deepseek-flash",
        root_run_id: "mission_rec",
        node: &tree,
        base_port_offset: 100,
        cancel: &cancel,
    };
    let receipt = execute_recursive_tree_with(&run, FakeRunner, &test_contracts).unwrap();
    assert!(receipt.verify_integrity());
    assert_eq!(receipt.effective_role, ContainerRole::Driver);
    assert_eq!(receipt.children.len(), 1);
    assert_eq!(receipt.children[0].effective_role, ContainerRole::Engine);
    assert_eq!(
        receipt.children[0].summary,
        "completed:cli:claude:leaf_edit"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// `submit_main` makes the publishing clone a witness of the storage remote
/// (`crate::storage_guard`): the record follows main, and history that left
/// the remote behind this clone's back comes back as a note, not a failure.
#[test]
fn a_harness_submission_witnesses_the_remote_and_notes_a_loss() {
    let root = std::env::temp_dir().join(format!("susi-harness-guard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let git = |dir: &Path, args: &[&str]| -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=Guard Test",
                "-c",
                "user.email=guard@test.invalid",
            ])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=.no-test-hooks",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join(".absent-fixture-config"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&root, &["init", "-q", "--bare", "remote.git"]);
    git(&work, &["init", "-q", "-b", "main"]);
    git(
        &work,
        &[
            "remote",
            "add",
            "origin",
            root.join("remote.git").to_str().unwrap(),
        ],
    );
    let commit = |text: &str| -> String {
        std::fs::write(work.join("source.txt"), text).unwrap();
        git(&work, &["add", "source.txt"]);
        git(&work, &["commit", "-q", "-m", text]);
        git(&work, &["rev-parse", "HEAD"])
    };
    let seed = commit("seed");
    let other = commit("another agent's work");
    git(&work, &["push", "-q", "origin", "main"]);
    let config = crate::git_governor::SubmitMainConfig {
        repo: work.clone(),
        rev: "HEAD".to_owned(),
        remote: "origin".to_owned(),
        require_task_trailer: false,
        adopted_host_commits: Default::default(),
        dry_run: false,
        max_attempts: 1,
    };
    let mut notes = Vec::new();
    submit_main(&config, &mut |line| notes.push(line)).unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    let record = "refs/susi-guard/origin/heads/main";
    assert_eq!(git(&work, &["rev-parse", record]), other);

    // A client with no hook rewinds main; the next submission comes from a
    // branch that never contained the dropped commit.
    git(
        &work,
        &[
            "push",
            "-q",
            "--force",
            "origin",
            &format!("{seed}:refs/heads/main"),
        ],
    );
    git(&work, &["reset", "-q", "--hard", &seed]);
    commit("later work");
    notes.clear();
    submit_main(&config, &mut |line| notes.push(line)).unwrap();
    assert!(
        notes.iter().any(|n| n.contains("REWOUND refs/heads/main")),
        "{notes:?}"
    );
    assert_eq!(git(&work, &["rev-parse", record]), other);
    let _ = std::fs::remove_dir_all(&root);
}

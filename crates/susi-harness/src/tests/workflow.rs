use super::*;

struct Task {
    key: String,
    deps: Vec<String>,
    scopes: Vec<String>,
}

fn task(key: &str, deps: &[&str], scopes: &[&str]) -> Task {
    Task {
        key: key.into(),
        deps: deps.iter().map(|dep| (*dep).to_string()).collect(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
    }
}

impl WorkflowTask for Task {
    fn key(&self) -> &str {
        &self.key
    }
    fn deps(&self) -> &[String] {
        &self.deps
    }
    fn scopes(&self) -> &[String] {
        &self.scopes
    }
}

fn keys<T: WorkflowTask>(tasks: &[T]) -> Vec<&str> {
    tasks.iter().map(WorkflowTask::key).collect()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn normalize_scope_canonicalises_separators_case_and_dots() {
    assert_eq!(
        normalize_scope(" .\\Crates\\Susi-Core\\ ").unwrap(),
        "crates/susi-core"
    );
    assert_eq!(normalize_scope("./docs//a.md/").unwrap(), "docs/a.md");
    assert_eq!(normalize_scope("././x").unwrap(), "x");
}

#[test]
fn normalize_scope_rejects_empty_dot_parent_and_absolute() {
    for raw in [
        "", "  ", ".", "./", "..", "../x", "a/../b", "a/./b", "/etc", "\\x", "C:/x",
    ] {
        assert!(normalize_scope(raw).is_err(), "{raw:?} should be rejected");
    }
}

#[test]
fn scopes_overlap_on_equality_and_component_prefixes_only() {
    assert!(scopes_overlap("crates/susi-core", "crates/susi-core"));
    assert!(scopes_overlap(
        "crates/susi-core",
        "crates/susi-core/src/lib.rs"
    ));
    assert!(scopes_overlap(
        "crates/susi-core/src/lib.rs",
        "crates/susi-core"
    ));
    assert!(!scopes_overlap("crates/susi-core", "crates/susi-core-defs"));
    assert!(!scopes_overlap("crates/susi-core-defs", "crates/susi-core"));
    assert!(!scopes_overlap("docs/a.md", "docs/b.md"));
}

#[test]
fn task_scopes_disjoint_normalizes_and_fails_closed() {
    assert!(!task_scopes_disjoint(
        &strings(&["Crates\\Susi-Core"]),
        &strings(&["crates/susi-core/src/lib.rs"])
    ));
    assert!(task_scopes_disjoint(
        &strings(&["crates/susi-core", "docs/a.md"]),
        &strings(&["crates/susi-core-defs", "docs/b.md"])
    ));
    assert!(!task_scopes_disjoint(
        &strings(&["../escape"]),
        &strings(&["docs/b.md"])
    ));
}

#[test]
fn validate_dependencies_rejects_unknown_and_cycles() {
    let fine = [task("a", &[], &["x"]), task("b", &["a"], &["y"])];
    assert!(validate_dependencies(&fine).is_ok());

    let unknown = [task("a", &["ghost"], &["x"])];
    let error = validate_dependencies(&unknown).unwrap_err().to_string();
    assert!(error.contains("ghost"), "{error}");

    let cycle = [
        task("a", &["c"], &["x"]),
        task("b", &["a"], &["y"]),
        task("c", &["b"], &["z"]),
    ];
    let error = validate_dependencies(&cycle).unwrap_err().to_string();
    assert!(error.contains("dependency cycle"), "{error}");
    assert!(error.contains("a -> c -> b -> a"), "{error}");

    let selfish = [task("a", &["a"], &["x"])];
    let error = validate_dependencies(&selfish).unwrap_err().to_string();
    assert!(error.contains("a -> a"), "{error}");
}

#[test]
fn overlapping_scopes_are_deferred_to_later_waves_in_plan_order() {
    let tasks = [
        task("a", &[], &["crates/susi-core"]),
        task("b", &[], &["crates/susi-core/src/lib.rs"]),
        task("c", &[], &["crates/susi-core-defs"]),
        task("d", &[], &["docs/d.md"]),
    ];
    let schedule = WorkflowSchedule::build(&tasks, &BTreeSet::new(), &[], 4).unwrap();
    assert_eq!(schedule.waves.len(), 2);
    assert_eq!(schedule.waves[0].index, 0);
    assert_eq!(keys(&schedule.waves[0].tasks), vec!["a", "c", "d"]);
    assert_eq!(schedule.waves[1].index, 1);
    assert_eq!(keys(&schedule.waves[1].tasks), vec!["b"]);
    assert!(schedule.blocked.is_empty());
}

#[test]
fn waves_respect_max_parallel_and_dependencies() {
    let tasks = [
        task("a", &[], &["a"]),
        task("b", &[], &["b"]),
        task("c", &["a"], &["c"]),
        task("d", &["c", "b"], &["d"]),
    ];
    let schedule = WorkflowSchedule::build(&tasks, &BTreeSet::new(), &[], 2).unwrap();
    let waves: Vec<Vec<&str>> = schedule
        .waves
        .iter()
        .map(|wave| keys(&wave.tasks))
        .collect();
    assert_eq!(waves, vec![vec!["a", "b"], vec!["c"], vec!["d"]]);

    let landed: BTreeSet<String> = ["a".to_string()].into();
    let resumed = WorkflowSchedule::build(&tasks, &landed, &[], 2).unwrap();
    assert_eq!(keys(&resumed.waves[0].tasks), vec!["b", "c"]);
}

#[test]
fn active_claims_block_overlapping_tasks_and_their_dependents() {
    let tasks = [
        task("a", &[], &["crates/susi-core/src"]),
        task("b", &[], &["docs/b.md"]),
        task("c", &["a"], &["docs/c.md"]),
    ];
    let claimed = strings(&["crates/susi-core"]);
    let schedule = WorkflowSchedule::build(&tasks, &BTreeSet::new(), &claimed, 4).unwrap();
    assert_eq!(schedule.waves.len(), 1);
    assert_eq!(keys(&schedule.waves[0].tasks), vec!["b"]);
    assert_eq!(keys(&schedule.blocked), vec!["a", "c"]);
}

#[test]
fn next_wave_reports_an_invalid_scope() {
    let tasks = [task("a", &[], &["../escape"])];
    let pending: Vec<&Task> = tasks.iter().collect();
    assert!(next_wave(&pending, &BTreeSet::new(), &[], 2).is_err());
}

fn write_claim(claims: &Path, name: &str, worktree: &Path, scopes: &[&str]) {
    std::fs::write(
        claims.join(format!("{name}.claim")),
        format!(
            "{CLAIM_HEADER}\n{name}\n{}\n{}\n{}\n",
            worktree.display(),
            "b".repeat(40),
            scopes.join("\n")
        ),
    )
    .unwrap();
}

#[test]
fn inspect_repo_workflow_reads_claims_slots_and_head_without_locking() {
    let repo = tempfile::tempdir().unwrap();
    let state = repo.path().join(".git/susi-local-workflow");
    let claims = state.join("claims");
    let slots = state.join("slots");
    std::fs::create_dir_all(&claims).unwrap();
    std::fs::create_dir_all(slots.join("1")).unwrap();
    std::fs::create_dir_all(slots.join("2")).unwrap();
    write_claim(
        &claims,
        "zeta",
        &repo.path().join("wt-zeta"),
        &["crates/susi-harness", ".agents/tasks/t-z.json"],
    );
    write_claim(&claims, "alpha", &repo.path().join("wt-alpha"), &["docs"]);
    std::fs::write(claims.join("notes.txt"), "ignored").unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(slots.join("1.lock"))
        .unwrap();
    held.lock().unwrap();
    std::fs::write(slots.join("1/owner"), "pid=4242\n").unwrap();
    std::fs::write(
        slots.join("2/owner"),
        "pid=7\nagent=antigravity\nscope=crates/susi-harness\n",
    )
    .unwrap();
    std::fs::create_dir_all(slots.join("3")).unwrap();
    let head = "c".repeat(40);
    std::fs::create_dir_all(repo.path().join(".git/refs/susi/integration")).unwrap();
    std::fs::write(
        repo.path().join(".git/refs/susi/integration/current"),
        format!("{head}\n"),
    )
    .unwrap();

    let snapshot = inspect_repo_workflow(repo.path()).unwrap();
    held.unlock().unwrap();

    assert_eq!(
        snapshot
            .claims
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "zeta"]
    );
    let zeta = &snapshot.claims[1];
    assert_eq!(zeta.worktree, repo.path().join("wt-zeta"));
    assert_eq!(zeta.base, "b".repeat(40));
    assert_eq!(
        zeta.scopes,
        strings(&["crates/susi-harness", ".agents/tasks/t-z.json"])
    );
    assert_eq!(
        snapshot.active_slots,
        vec![
            SlotRecord {
                agent: "1".into(),
                pid: Some(4242),
                scope_key: None,
            },
            SlotRecord {
                agent: "antigravity".into(),
                pid: Some(7),
                scope_key: Some("crates/susi-harness".into()),
            },
        ]
    );
    assert_eq!(snapshot.integration_head.as_deref(), Some(head.as_str()));
    assert_eq!(
        snapshot.claimed_scopes(),
        strings(&["docs", "crates/susi-harness", ".agents/tasks/t-z.json"])
    );
}

#[test]
fn inspect_repo_workflow_handles_empty_state_and_packed_refs() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    assert_eq!(
        inspect_repo_workflow(repo.path()).unwrap(),
        WorkflowSnapshot::default()
    );
    let head = "d".repeat(40);
    std::fs::write(
        repo.path().join(".git/packed-refs"),
        format!("# pack-refs with: peeled\n{head} {INTEGRATION_REF}\n"),
    )
    .unwrap();
    assert_eq!(
        inspect_repo_workflow(repo.path())
            .unwrap()
            .integration_head
            .as_deref(),
        Some(head.as_str())
    );
}

#[test]
fn inspect_repo_workflow_rejects_a_malformed_claim() {
    let repo = tempfile::tempdir().unwrap();
    let claims = repo.path().join(".git/susi-local-workflow/claims");
    std::fs::create_dir_all(&claims).unwrap();
    std::fs::write(claims.join("bad.claim"), "susi-local-claim-v1\nbad\n").unwrap();
    assert!(inspect_repo_workflow(repo.path()).is_err());
    assert!(inspect_repo_workflow(&repo.path().join("missing")).is_err());
}

#[test]
fn an_agent_publishes_on_its_own_branch_and_nowhere_else() {
    let policy = IntegrationPolicy::default();
    assert_eq!(
        policy.publish_branch("Antigravity").ok(),
        Some("local-agent/antigravity".to_string())
    );
    assert!(policy
        .check_push_target("antigravity", "local-agent/antigravity")
        .is_ok());
    assert!(policy
        .check_push_target("antigravity", "refs/heads/local-agent/antigravity")
        .is_ok());
    assert_eq!(
        policy.check_push_target("antigravity", "main"),
        Err(PolicyViolation::GateOnlyRef("main".into()))
    );
    assert_eq!(
        policy.check_push_target("antigravity", "refs/heads/main"),
        Err(PolicyViolation::GateOnlyRef("refs/heads/main".into()))
    );
    assert_eq!(
        policy.check_push_target("antigravity", "refs/susi/integration/current"),
        Err(PolicyViolation::CoordinatorRef(
            "refs/susi/integration/current".into()
        ))
    );
    assert_eq!(
        policy.check_push_target("antigravity", "local-agent/gemini"),
        Err(PolicyViolation::ForeignBranch {
            target: "local-agent/gemini".into(),
            own: "local-agent/antigravity".into(),
        })
    );
    assert_eq!(
        policy.check_push_target("antigravity", "feature/x"),
        Err(PolicyViolation::NotAnAgentBranch("feature/x".into()))
    );
    assert!(matches!(
        policy.publish_branch("bad name/"),
        Err(PolicyViolation::BadAgentName(_))
    ));
}

#[test]
fn fast_main_policy_grants_autonomous_main_freedom_while_protecting_releases_and_foreign_branches()
{
    let policy = IntegrationPolicy::fast_main();
    assert_eq!(policy.mode, PublicationMode::FastMainStream);
    assert_eq!(policy.gate_cycle_secs, 0);
    assert!(policy.check_push_target("carbon", "main").is_ok());
    assert!(policy
        .check_push_target("carbon", "refs/heads/main")
        .is_ok());
    assert!(policy
        .check_push_target("carbon", "local-agent/carbon")
        .is_ok());
    assert!(policy
        .check_push_target("carbon", "refs/heads/local-agent/carbon")
        .is_ok());

    assert_eq!(
        policy.check_push_target("carbon", "HEAD"),
        Err(PolicyViolation::GateOnlyRef("HEAD".into()))
    );
    assert_eq!(
        policy.check_push_target("carbon", "refs/susi/integration/current"),
        Err(PolicyViolation::CoordinatorRef(
            "refs/susi/integration/current".into()
        ))
    );
    assert_eq!(
        policy.check_push_target("carbon", "refs/heads/release/v0.21.55"),
        Err(PolicyViolation::ProtectedReleaseRef(
            "refs/heads/release/v0.21.55".into()
        ))
    );
    assert_eq!(
        policy.check_push_target("carbon", "refs/tags/v0.21.55"),
        Err(PolicyViolation::ProtectedReleaseRef(
            "refs/tags/v0.21.55".into()
        ))
    );
    assert_eq!(
        policy.check_push_target("carbon", "local-agent/grok"),
        Err(PolicyViolation::ForeignBranch {
            target: "local-agent/grok".into(),
            own: "local-agent/carbon".into(),
        })
    );

    let brief = policy.brief_lines("carbon").unwrap();
    assert_eq!(brief.len(), 6);
    assert!(brief[0].contains("zero dependency on any single coordinator"));
    assert!(brief[0].contains("`local-agent/carbon`"));
    assert!(policy
        .check_composed_task_trailers([
            "feat: x\n\nTask: T-CARBONDRIVER-1",
            "fix: y\n\nTask: T-GROKLABEL-1"
        ])
        .is_ok());
    assert!(policy
        .check_composed_task_trailers(["feat: untagged"])
        .is_err());
}

#[test]
fn every_commit_in_a_batch_names_the_agents_task() {
    let policy = IntegrationPolicy::default();
    let good = [
        "refactor: x\n\nTask: T-GEMINI-1\n",
        "fix: y\n\nTask: t-gemini-2",
    ];
    assert!(policy.check_commit_trailers("gemini", good).is_ok());
    assert_eq!(
        policy.check_commit_trailers("gemini", ["[iter 320/400] delete crate"]),
        Err(PolicyViolation::MissingTaskTrailer {
            subject: "[iter 320/400] delete crate".into()
        })
    );
    assert_eq!(
        policy.check_commit_trailers("gemini", ["chore\n\nTask: T-LEAN-1"]),
        Err(PolicyViolation::ForeignTaskTrailer {
            subject: "chore".into(),
            trailer: "T-LEAN-1".into()
        })
    );
}

#[test]
fn core_deletes_only_after_the_destination_is_proven() {
    let policy = IntegrationPolicy::default();
    let module = "crates/susi-gemi/src/engines/alpha.rs";
    let mut proof = MigrationProof {
        module: module.into(),
        ..MigrationProof::default()
    };
    assert_eq!(
        policy.check_migration(&proof),
        Err(PolicyViolation::DeletionBeforeDestination {
            module: module.into()
        })
    );
    proof.destination_commit = Some("c128ef95".into());
    assert!(matches!(
        policy.check_migration(&proof),
        Err(PolicyViolation::DestinationUnproven { missing, .. })
            if missing.contains("not declared")
    ));
    proof.destination_declared = true;
    proof.destination_compiles = true;
    assert!(matches!(
        policy.check_migration(&proof),
        Err(PolicyViolation::DestinationUnproven { missing, .. })
            if missing.contains("tests")
    ));
    proof.destination_tests_pass = true;
    assert!(policy.check_migration(&proof).is_ok());
    proof.ports_kept = vec![("gemi /v1/chat/completions local engine".into(), None)];
    assert!(matches!(
        policy.check_migration(&proof),
        Err(PolicyViolation::StubbedCapability { .. })
    ));
    proof.ports_kept = vec![(
        "gemi /v1/chat/completions local engine".into(),
        Some("susi-gpu-fabric plug over GEMI 9091".into()),
    )];
    assert!(policy.check_migration(&proof).is_ok());
}

#[test]
fn worker_briefs_carry_the_rules_and_plans_cannot_share_a_branch() {
    let policy = IntegrationPolicy::default();
    let lines = policy.brief_lines("Gemini").unwrap_or_default();
    assert_eq!(lines.len(), 6);
    assert!(lines[0].contains("`local-agent/gemini`"));
    assert!(lines[1].contains("Task: T-GEMINI-<n>"));
    assert!(lines[4].contains("20 minutes"));
    let ok = [task("alpha", &[], &["a"]), task("beta", &[], &["b"])];
    assert!(policy.validate_plan(&ok).is_ok());
    let clash = [task("Alpha", &[], &["a"]), task("alpha", &[], &["b"])];
    assert!(policy.validate_plan(&clash).is_err());
    let bad = [task("no spaces", &[], &["a"])];
    assert!(policy.validate_plan(&bad).is_err());
}

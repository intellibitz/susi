use super::*;
use std::sync::atomic::AtomicUsize;
use susi_error::EaiResult;

struct MockContainer {
    contract: CapabilityContract,
    cost: MicroUsd,
    calls: Arc<AtomicUsize>,
    fail_with: Option<String>,
    cancel_on_run: Option<CancellationToken>,
}

impl ContainerComponent for MockContainer {
    fn contract(&self) -> &CapabilityContract {
        &self.contract
    }

    fn execute_leaf(&self, ctx: &EngineTurnContext<'_>) -> Result<LeafExecutionOutput, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(tok) = &self.cancel_on_run {
            tok.cancel();
        }
        if ctx.cancel.is_cancelled() {
            return Err("cancelled during leaf execution".to_string());
        }
        if let Some(err) = &self.fail_with {
            return Err(err.clone());
        }
        let mut hasher = Sha256::new();
        hasher.update(ctx.run_id.as_bytes());
        hasher.update(b":");
        hasher.update(ctx.prompt.as_bytes());
        Ok(LeafExecutionOutput {
            summary: format!(
                "{} completed {} in {}",
                self.contract.component_id, ctx.task_key, ctx.sandbox.worker_identity
            ),
            spent: self.cost,
            output_digest: hex::encode(hasher.finalize()),
        })
    }
}

fn contract(
    id: &str,
    role: ContainerRole,
    caps: &[&str],
    verified_available: bool,
) -> CapabilityContract {
    CapabilityContract {
        contract_version: RECURSIVE_CONTRACT_SCHEMA.to_string(),
        component_id: id.to_string(),
        role,
        capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
        verified_available,
    }
}

fn grant(
    scopes: &[&str],
    caps: &[&str],
    max_depth: u8,
    micros: u64,
    timeout_ms: u64,
) -> AuthorityGrant {
    AuthorityGrant {
        allowed_scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
        allowed_capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
        allow_network: false,
        allow_git_publish: true,
        max_depth,
        budget_cap: MicroUsd::from_micros(micros),
        timeout_ms,
    }
}

#[test]
fn coordinator_selection_has_zero_dot_dependency_and_fails_over_cleanly() {
    let candidates = vec![
        contract(
            "dot",
            ContainerRole::DriverAndEngine,
            &["wave_plan", "code_edit"],
            false,
        ),
        contract(
            "leaf-only",
            ContainerRole::Engine,
            &["wave_plan", "code_edit"],
            true,
        ),
        contract(
            "carbon",
            ContainerRole::DriverAndEngine,
            &["wave_plan", "code_edit", "distill"],
            true,
        ),
        contract(
            "opus",
            ContainerRole::DriverAndEngine,
            &["wave_plan", "code_edit"],
            true,
        ),
    ];

    let chosen = select_verified_driver(&candidates, &["wave_plan", "distill"]).unwrap();
    assert_eq!(chosen.component_id, "carbon");

    let missing = select_verified_driver(&candidates, &["quantum_asic"]);
    assert!(matches!(
        missing,
        Err(RecursiveError::NoVerifiedDriverAvailable { .. })
    ));
}

#[test]
fn three_level_driver_engine_recursion_preserves_isolation_budgets_and_tamper_evident_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = RecursiveRuntime::new(dir.path(), 10).unwrap();

    let carbon_calls = Arc::new(AtomicUsize::new(0));
    let opus_calls = Arc::new(AtomicUsize::new(0));
    let grok_calls = Arc::new(AtomicUsize::new(0));

    // Carbon is BOTH Driver (at root) and Engine (at leaf depth 2)!
    runtime
        .register(Arc::new(MockContainer {
            contract: contract(
                "carbon",
                ContainerRole::DriverAndEngine,
                &["wave_plan", "code_edit", "distill"],
                true,
            ),
            cost: MicroUsd::from_micros(200),
            calls: Arc::clone(&carbon_calls),
            fail_with: None,
            cancel_on_run: None,
        }))
        .unwrap();
    // Opus acts as an Engine in Wave 0 AND as a nested sub-Driver in Wave 0!
    runtime
        .register(Arc::new(MockContainer {
            contract: contract(
                "opus",
                ContainerRole::DriverAndEngine,
                &["wave_plan", "code_edit"],
                true,
            ),
            cost: MicroUsd::from_micros(300),
            calls: Arc::clone(&opus_calls),
            fail_with: None,
            cancel_on_run: None,
        }))
        .unwrap();
    runtime
        .register(Arc::new(MockContainer {
            contract: contract("grok", ContainerRole::Engine, &["code_edit"], true),
            cost: MicroUsd::from_micros(150),
            calls: Arc::clone(&grok_calls),
            fail_with: None,
            cancel_on_run: None,
        }))
        .unwrap();

    // Depth 0: Carbon (Driver)
    //   Wave 0 (parallel disjoint siblings):
    //     - Depth 1: Opus (acting as sub-Driver over crates/susi-harness)
    //         Wave 0:
    //           - Depth 2: Grok (Engine on crates/susi-harness/src/recursive.rs)
    //           - Depth 2: Carbon (acting as Engine on crates/susi-harness/src/workflow.rs!)
    //     - Depth 1: Opus (acting as Engine on xtask/src/distill_compose.rs)
    let tree = RecursiveTaskNode {
        task_key: "mission_root".to_string(),
        component_id: "carbon".to_string(),
        required_capabilities: vec!["wave_plan".to_string()],
        prompt: "Coordinate autonomous Core delivery".to_string(),
        grant: grant(
            &["crates/susi-harness", "xtask"],
            &["wave_plan", "code_edit", "distill"],
            4,
            1_000,
            60_000,
        ),
        child_waves: vec![vec![
            RecursiveTaskNode {
                task_key: "harness_subdriver".to_string(),
                component_id: "opus".to_string(),
                required_capabilities: vec!["wave_plan".to_string()],
                prompt: "Sub-drive harness modules".to_string(),
                grant: grant(
                    &["crates/susi-harness"],
                    &["wave_plan", "code_edit"],
                    3,
                    600,
                    30_000,
                ),
                child_waves: vec![vec![
                    RecursiveTaskNode {
                        task_key: "leaf_recursive".to_string(),
                        component_id: "grok".to_string(),
                        required_capabilities: vec!["code_edit".to_string()],
                        prompt: "Verify recursive module".to_string(),
                        grant: grant(
                            &["crates/susi-harness/src/recursive.rs"],
                            &["code_edit"],
                            1,
                            200,
                            10_000,
                        ),
                        child_waves: vec![],
                    },
                    RecursiveTaskNode {
                        task_key: "leaf_workflow".to_string(),
                        component_id: "carbon".to_string(),
                        required_capabilities: vec!["code_edit".to_string()],
                        prompt: "Verify workflow fast-main".to_string(),
                        grant: grant(
                            &["crates/susi-harness/src/workflow.rs"],
                            &["code_edit"],
                            1,
                            300,
                            10_000,
                        ),
                        child_waves: vec![],
                    },
                ]],
            },
            RecursiveTaskNode {
                task_key: "xtask_distill_leaf".to_string(),
                component_id: "opus".to_string(),
                required_capabilities: vec!["code_edit".to_string()],
                prompt: "Orchestrate xtask distill".to_string(),
                grant: grant(&["xtask"], &["code_edit"], 2, 400, 20_000),
                child_waves: vec![],
            },
        ]],
    };

    let cancel = CancellationToken::new();
    let receipt = runtime.execute("run_01", &tree, 100, &cancel).unwrap();
    assert!(receipt.verify_integrity());
    assert_eq!(receipt.effective_role, ContainerRole::Driver);
    assert_eq!(receipt.total_spent, MicroUsd::from_micros(650)); // 150 + 200 + 300
    assert_eq!(receipt.children.len(), 2);
    assert_eq!(receipt.children[0].effective_role, ContainerRole::Driver);
    assert_eq!(receipt.children[0].children.len(), 2);
    assert_eq!(
        receipt.children[0].children[1].effective_role,
        ContainerRole::Engine
    );
    assert_eq!(receipt.children[0].children[1].component_id, "carbon");

    // Verify all 4 allocated sandboxes have strictly disjoint ports and worktrees!
    let mut ports = BTreeSet::new();
    let mut worktrees = BTreeSet::new();
    for node_rec in [
        &receipt,
        &receipt.children[0],
        &receipt.children[0].children[0],
        &receipt.children[0].children[1],
        &receipt.children[1],
    ] {
        assert!(ports.insert(node_rec.sandbox.port_offset));
        assert!(worktrees.insert(node_rec.sandbox.worktree_dir.clone()));
    }

    // Tampering with any nested spend or parent_run_id invalidates integrity.
    let mut tampered = receipt.clone();
    tampered.children[0].children[0].summary = "forged".to_string();
    assert!(!tampered.verify_integrity());

    // Re-running an identical leaf task uses idempotent memoization without re-invoking side effects.
    assert_eq!(grok_calls.load(Ordering::SeqCst), 1);
    let replay = runtime.execute("run_02", &tree, 200, &cancel).unwrap();
    assert_eq!(grok_calls.load(Ordering::SeqCst), 1);
    assert!(replay.children[0].children[0].replayed_from_idempotency);
}

#[test]
fn authority_escalation_overlapping_sibling_scopes_and_budget_overruns_are_refused() {
    let parent = grant(&["crates/susi-harness"], &["code_edit"], 3, 500, 10_000);

    // 1. Scope escape
    let escape = grant(&["crates/susi-core"], &["code_edit"], 2, 100, 5_000);
    assert!(matches!(
        parent.delegate(&escape),
        Err(RecursiveError::AuthorityEscalation(_))
    ));

    // 2. Capability escalation
    let extra_cap = grant(
        &["crates/susi-harness/src"],
        &["code_edit", "publish_release"],
        2,
        100,
        5_000,
    );
    assert!(matches!(
        parent.delegate(&extra_cap),
        Err(RecursiveError::AuthorityEscalation(_))
    ));

    // 3. Network escalation
    let mut net_escalation = grant(&["crates/susi-harness/src"], &["code_edit"], 2, 100, 5_000);
    net_escalation.allow_network = true;
    assert!(matches!(
        parent.delegate(&net_escalation),
        Err(RecursiveError::AuthorityEscalation(_))
    ));

    // 4. Non-decreasing depth
    let bad_depth = grant(&["crates/susi-harness/src"], &["code_edit"], 3, 100, 5_000);
    assert!(matches!(
        parent.delegate(&bad_depth),
        Err(RecursiveError::DepthLimitExceeded { .. })
    ));

    // 5. Sibling overlap & cumulative budget overrun in wave
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = RecursiveRuntime::new(dir.path(), 10).unwrap();
    runtime
        .register(Arc::new(MockContainer {
            contract: contract(
                "carbon",
                ContainerRole::DriverAndEngine,
                &["wave_plan", "code_edit"],
                true,
            ),
            cost: MicroUsd::from_micros(350),
            calls: Arc::new(AtomicUsize::new(0)),
            fail_with: None,
            cancel_on_run: None,
        }))
        .unwrap();

    let overlapping_wave = RecursiveTaskNode {
        task_key: "root".to_string(),
        component_id: "carbon".to_string(),
        required_capabilities: vec!["wave_plan".to_string()],
        prompt: "drive".to_string(),
        grant: grant(
            &["crates/susi-harness"],
            &["wave_plan", "code_edit"],
            3,
            500,
            10_000,
        ),
        child_waves: vec![vec![
            RecursiveTaskNode {
                task_key: "left".to_string(),
                component_id: "carbon".to_string(),
                required_capabilities: vec!["code_edit".to_string()],
                prompt: "left".to_string(),
                grant: grant(&["crates/susi-harness/src"], &["code_edit"], 1, 400, 5_000),
                child_waves: vec![],
            },
            RecursiveTaskNode {
                task_key: "right".to_string(),
                component_id: "carbon".to_string(),
                required_capabilities: vec!["code_edit".to_string()],
                prompt: "right".to_string(),
                grant: grant(
                    &["crates/susi-harness/src/lib.rs"],
                    &["code_edit"],
                    1,
                    400,
                    5_000,
                ),
                child_waves: vec![],
            },
        ]],
    };
    let cancel = CancellationToken::new();
    assert!(matches!(
        runtime.execute("overlap_run", &overlapping_wave, 10, &cancel),
        Err(RecursiveError::IsolationConflict(_))
    ));

    // Disjoint scopes, each <= 500, but 350 + 350 = 700 > 500 parent cap!
    let mut over_budget = overlapping_wave;
    over_budget.child_waves[0][0].grant.allowed_scopes =
        vec!["crates/susi-harness/src/lib.rs".to_string()];
    over_budget.child_waves[0][1].grant.allowed_scopes =
        vec!["crates/susi-harness/src/workflow.rs".to_string()];
    assert!(matches!(
        runtime.execute("budget_run", &over_budget, 50, &cancel),
        Err(RecursiveError::BudgetExceeded { .. })
    ));
}

#[test]
fn cycle_detection_cancellation_and_honest_failure_propagation_work() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = RecursiveRuntime::new(dir.path(), 10).unwrap();
    let root_cancel = CancellationToken::new();

    runtime
        .register(Arc::new(MockContainer {
            contract: contract(
                "carbon",
                ContainerRole::DriverAndEngine,
                &["wave_plan", "code_edit"],
                true,
            ),
            cost: MicroUsd::ZERO,
            calls: Arc::new(AtomicUsize::new(0)),
            fail_with: None,
            cancel_on_run: None,
        }))
        .unwrap();
    runtime
        .register(Arc::new(MockContainer {
            contract: contract("failing", ContainerRole::Engine, &["code_edit"], true),
            cost: MicroUsd::ZERO,
            calls: Arc::new(AtomicUsize::new(0)),
            fail_with: Some("compile check failed".to_string()),
            cancel_on_run: None,
        }))
        .unwrap();
    runtime
        .register(Arc::new(MockContainer {
            contract: contract("canceller", ContainerRole::Engine, &["code_edit"], true),
            cost: MicroUsd::ZERO,
            calls: Arc::new(AtomicUsize::new(0)),
            fail_with: None,
            cancel_on_run: Some(root_cancel.clone()),
        }))
        .unwrap();

    // 1. Cycle: (carbon, repeat_key) -> (carbon, repeat_key)
    let cyclic = RecursiveTaskNode {
        task_key: "repeat_key".to_string(),
        component_id: "carbon".to_string(),
        required_capabilities: vec!["wave_plan".to_string()],
        prompt: "outer".to_string(),
        grant: grant(
            &["crates/susi-harness"],
            &["wave_plan", "code_edit"],
            4,
            100,
            10_000,
        ),
        child_waves: vec![vec![RecursiveTaskNode {
            task_key: "repeat_key".to_string(),
            component_id: "carbon".to_string(),
            required_capabilities: vec!["code_edit".to_string()],
            prompt: "inner".to_string(),
            grant: grant(&["crates/susi-harness/src"], &["code_edit"], 2, 50, 5_000),
            child_waves: vec![],
        }]],
    };
    let fresh_cancel = CancellationToken::new();
    assert!(matches!(
        runtime.execute("cycle_run", &cyclic, 10, &fresh_cancel),
        Err(RecursiveError::CycleDetected(chain)) if chain.contains("carbon:repeat_key -> carbon:repeat_key")
    ));

    // 2. Honest failure propagation (never cached in idempotency ledger)
    let failing_node = RecursiveTaskNode {
        task_key: "fail_leaf".to_string(),
        component_id: "failing".to_string(),
        required_capabilities: vec!["code_edit".to_string()],
        prompt: "run failing check".to_string(),
        grant: grant(&["crates/susi-harness"], &["code_edit"], 1, 100, 5_000),
        child_waves: vec![],
    };
    assert!(matches!(
        runtime.execute("fail_1", &failing_node, 30, &fresh_cancel),
        Err(RecursiveError::ExecutionFailed { reason, .. }) if reason.contains("compile check failed")
    ));
    assert!(matches!(
        runtime.execute("fail_2", &failing_node, 40, &fresh_cancel),
        Err(RecursiveError::ExecutionFailed { .. })
    ));

    // 3. Hierarchical cancellation cascades from root token to child frames
    let cancel_node = RecursiveTaskNode {
        task_key: "cancel_root".to_string(),
        component_id: "carbon".to_string(),
        required_capabilities: vec!["wave_plan".to_string()],
        prompt: "drive".to_string(),
        grant: grant(
            &["crates/susi-harness"],
            &["wave_plan", "code_edit"],
            3,
            100,
            5_000,
        ),
        child_waves: vec![vec![RecursiveTaskNode {
            task_key: "trigger_cancel".to_string(),
            component_id: "canceller".to_string(),
            required_capabilities: vec!["code_edit".to_string()],
            prompt: "cancel".to_string(),
            grant: grant(&["crates/susi-harness/src"], &["code_edit"], 1, 50, 5_000),
            child_waves: vec![],
        }]],
    };
    assert!(matches!(
        runtime.execute("cancel_run", &cancel_node, 60, &root_cancel),
        Err(RecursiveError::Cancelled { .. })
    ));
}

struct BridgeSmokeRunner;

impl Runner for BridgeSmokeRunner {
    fn run(&self, config: &ExecutorConfig) -> EaiResult<RunOutcome> {
        Ok(RunOutcome::Succeeded {
            final_text: format!("bridged:{}:{}", config.mission, config.task),
            reported_cost_usd: None,
        })
    }
}

#[test]
fn runner_container_bridge_adapts_harness_runners_into_recursive_containers() {
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = RecursiveRuntime::new(dir.path(), 10).unwrap();
    let bridge = RunnerContainerBridge::new(
        contract(
            "harness-native",
            ContainerRole::Engine,
            &["code_edit"],
            true,
        ),
        BridgeSmokeRunner,
        "http://127.0.0.1:9091",
        "susi-local",
    );
    runtime.register(Arc::new(bridge)).unwrap();

    let leaf = RecursiveTaskNode {
        task_key: "bridged_task".to_string(),
        component_id: "harness-native".to_string(),
        required_capabilities: vec!["code_edit".to_string()],
        prompt: "execute turn".to_string(),
        grant: grant(&["crates/susi-harness"], &["code_edit"], 1, 100, 5_000),
        child_waves: vec![],
    };
    let cancel = CancellationToken::new();
    let receipt = runtime.execute("bridge_root", &leaf, 80, &cancel).unwrap();
    assert!(receipt.verify_integrity());
    assert_eq!(receipt.summary, "bridged:root:bridged_task");
}

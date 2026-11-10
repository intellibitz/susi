use super::*;
use std::process::Command;

fn git_cmd(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed in {}: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn setup_origin_and_two_clones(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let origin = root.join("origin.git");
    let clone_a = root.join("clone-a");
    let clone_b = root.join("clone-b");
    fs::create_dir_all(&origin).unwrap();
    git_cmd(&origin, &["init", "--bare", "--initial-branch=main"]);

    git_cmd(
        root,
        &["clone", origin.to_str().unwrap(), clone_a.to_str().unwrap()],
    );
    for (k, v) in [
        ("user.name", "Test Operator"),
        ("user.email", "operator@localhost"),
        ("commit.gpgsign", "false"),
    ] {
        git_cmd(&clone_a, &["config", k, v]);
    }
    fs::write(clone_a.join("README.md"), "seed\n").unwrap();
    git_cmd(&clone_a, &["add", "README.md"]);
    git_cmd(&clone_a, &["commit", "-m", "chore: seed\n\nTask: T-SEED-1"]);
    git_cmd(&clone_a, &["push", "-u", "origin", "HEAD:refs/heads/main"]);

    git_cmd(
        root,
        &["clone", origin.to_str().unwrap(), clone_b.to_str().unwrap()],
    );
    for (k, v) in [
        ("user.name", "Peer Host"),
        ("user.email", "peer@localhost"),
        ("commit.gpgsign", "false"),
    ] {
        git_cmd(&clone_b, &["config", k, v]);
    }
    (origin, clone_a, clone_b)
}

#[test]
fn parallel_multi_agent_harness_isolates_workers_and_reconciles_remote_on_every_step() {
    let tmp = susi_paths::tempfile::tempdir().unwrap();
    let (origin, clone_a, clone_b) = setup_origin_and_two_clones(tmp.path());
    let iso_root = tmp.path().join("isolated-harness");

    // Push an upstream commit from clone_b BEFORE clone_a starts so `start` must pull+FF!
    fs::create_dir_all(clone_b.join("upstream")).unwrap();
    fs::write(
        clone_b.join("upstream/peer.txt"),
        "from peer before start\n",
    )
    .unwrap();
    git_cmd(&clone_b, &["add", "upstream/peer.txt"]);
    git_cmd(
        &clone_b,
        &["commit", "-m", "feat: peer upstream\n\nTask: T-PEER-1"],
    );
    git_cmd(&clone_b, &["push", "origin", "HEAD:refs/heads/main"]);

    let harness =
        ParallelWorkflowHarness::new(&clone_a, &iso_root, RemoteReconcileConfig::default(), 4)
            .unwrap();

    let specs = vec![
        ParallelAgentSpec {
            agent: "alpha".to_owned(),
            task_id: "T-ALPHA-1".to_owned(),
            title: "Alpha pillar module".to_owned(),
            deps: Vec::new(),
            scopes: vec!["crates/alpha".to_owned()],
        },
        ParallelAgentSpec {
            agent: "beta".to_owned(),
            task_id: "T-BETA-1".to_owned(),
            title: "Beta pillar module".to_owned(),
            deps: Vec::new(),
            scopes: vec!["crates/beta".to_owned()],
        },
    ];

    type ObservedWorkerSlot = (String, PathBuf, PathBuf, PathBuf, u16);
    let observed_isolation: Mutex<Vec<ObservedWorkerSlot>> = Mutex::new(Vec::new());

    let report = harness
        .execute_parallel_plan(&specs, Some("v0-21-61-rc1"), |ws| {
            // Verify private worktree, private target dir, private SUSI_HOME, and recursive contracts.
            assert!(ws.worktree_dir.exists());
            assert!(ws.target_dir.exists());
            assert!(ws.susi_home_dir.exists());
            assert!(ws.worktree_dir.join("upstream/peer.txt").is_file());
            assert_eq!(ws.driver_contract.role, ContainerRole::Driver);
            assert_eq!(ws.engine_contract.role, ContainerRole::Engine);

            observed_isolation.lock().unwrap().push((
                ws.agent.clone(),
                ws.worktree_dir.clone(),
                ws.target_dir.clone(),
                ws.susi_home_dir.clone(),
                ws.port_offset,
            ));

            let mod_dir = ws.worktree_dir.join("crates").join(&ws.agent);
            fs::create_dir_all(&mod_dir).unwrap();
            fs::write(
                mod_dir.join("lib.rs"),
                format!("pub const AGENT: &str = {:?};\n", ws.agent),
            )
            .unwrap();

            // When alpha runs, simulate a concurrent commit landing on origin/main from clone_b
            // so subsequent sync/commit/ship steps exercise 3-way merge reconciliation!
            if ws.agent == "alpha" {
                fs::write(clone_b.join("upstream/concurrent.txt"), "mid-wave peer\n").unwrap();
                git_cmd(&clone_b, &["add", "upstream/concurrent.txt"]);
                git_cmd(
                    &clone_b,
                    &["commit", "-m", "feat: concurrent peer\n\nTask: T-PEER-2"],
                );
                git_cmd(&clone_b, &["push", "origin", "HEAD:refs/heads/main"]);
            }

            Ok(format!("feat({}): implement isolated slice", ws.agent))
        })
        .unwrap();

    assert_eq!(report.waves_executed, 1);
    assert_eq!(report.landed_tasks, vec!["T-ALPHA-1", "T-BETA-1"]);

    // Verify all 2 workers had completely disjoint worktrees, target dirs, SUSI_HOME, and port offsets!
    let iso = observed_isolation.lock().unwrap();
    assert_eq!(iso.len(), 2);
    assert_ne!(iso[0].1, iso[1].1);
    assert_ne!(iso[0].2, iso[1].2);
    assert_ne!(iso[0].3, iso[1].3);
    assert!(iso[0].4.abs_diff(iso[1].4) >= MIN_ISOLATED_PORT_STRIDE);

    // Verify receipts exist for all 6 workflow steps (`start`, `sync`, `commit`, `ship`, `release`, `distill`)
    // and every step reached `origin`!
    let mut seen_steps = BTreeSet::new();
    for rcpt in &report.step_receipts {
        assert!(
            rcpt.remote_sync.reached_remote(),
            "step {:?} did not reach remote: {:?}",
            rcpt.step,
            rcpt.remote_sync
        );
        assert_eq!(rcpt.receipt_sha256.len(), 64);
        seen_steps.insert(rcpt.step);
    }
    assert_eq!(
        seen_steps,
        [
            WorkflowStep::Start,
            WorkflowStep::Sync,
            WorkflowStep::Commit,
            WorkflowStep::Ship,
            WorkflowStep::Release,
            WorkflowStep::Distill,
        ]
        .into_iter()
        .collect()
    );

    // Verify at least one step performed a 3-way merge when clone_b advanced origin/main mid-wave!
    assert!(report
        .step_receipts
        .iter()
        .any(|r| matches!(r.remote_sync, RemoteSyncMode::MergedThreeWay { .. })));

    // Verify origin/main has both workers' files AND both peer commits!
    let origin_main = git_cmd(&origin, &["rev-parse", "refs/heads/main"]);
    assert_eq!(origin_main, report.final_main_commit);

    let distilled = report.distilled_release.unwrap();
    assert_eq!(distilled.branch_ref, "refs/heads/release/v0-21-61-rc1");
    assert!(distilled.included_tasks.contains(&"T-ALPHA-1".to_owned()));
    assert!(distilled.included_tasks.contains(&"T-BETA-1".to_owned()));
    assert!(distilled.included_tasks.contains(&"T-PEER-2".to_owned()));
}

#[test]
fn step_commit_enforces_scope_confinement_and_offline_fallback_works_when_disconnected() {
    let tmp = susi_paths::tempfile::tempdir().unwrap();
    let (_origin, clone_a, _clone_b) = setup_origin_and_two_clones(tmp.path());
    let iso_root = tmp.path().join("iso-offline");

    // Point `origin` at a non-existent path to simulate disconnected laptop / offline mode.
    git_cmd(
        &clone_a,
        &[
            "remote",
            "set-url",
            "origin",
            tmp.path().join("missing-origin.git").to_str().unwrap(),
        ],
    );

    let harness =
        ParallelWorkflowHarness::new(&clone_a, &iso_root, RemoteReconcileConfig::default(), 2)
            .unwrap();
    let spec = ParallelAgentSpec {
        agent: "gamma".to_owned(),
        task_id: "T-GAMMA-1".to_owned(),
        title: "Offline worker".to_owned(),
        deps: Vec::new(),
        scopes: vec!["crates/gamma".to_owned()],
    };

    let (mut ws, start_rcpt) = harness.step_start(&spec, 0).unwrap();
    assert!(matches!(
        start_rcpt.remote_sync,
        RemoteSyncMode::OfflineFallback { .. }
    ));

    // Touching a file outside `crates/gamma` fails closed on commit!
    fs::write(ws.worktree_dir.join("README.md"), "unauthorized edit\n").unwrap();
    let bad_commit = harness.step_commit(&mut ws, "bad scope");
    assert!(bad_commit.is_err());
    git_cmd(&ws.worktree_dir, &["checkout", "--", "README.md"]);

    // Scoped edit succeeds and every step degrades cleanly via OfflineFallback!
    fs::create_dir_all(ws.worktree_dir.join("crates/gamma")).unwrap();
    fs::write(
        ws.worktree_dir.join("crates/gamma/mod.rs"),
        "pub fn ok() {}\n",
    )
    .unwrap();

    let sync_rcpt = harness.step_sync(&mut ws).unwrap();
    assert!(matches!(
        sync_rcpt.remote_sync,
        RemoteSyncMode::OfflineFallback { .. }
    ));

    let commit_rcpt = harness
        .step_commit(&mut ws, "feat(gamma): offline slice")
        .unwrap();
    assert!(matches!(
        commit_rcpt.remote_sync,
        RemoteSyncMode::OfflineFallback { .. }
    ));

    let ship_rcpt = harness.step_ship(&mut ws).unwrap();
    assert!(matches!(
        ship_rcpt.remote_sync,
        RemoteSyncMode::OfflineFallback { .. }
    ));

    let release_rcpt = harness.step_release(&ws).unwrap();
    assert!(matches!(
        release_rcpt.remote_sync,
        RemoteSyncMode::OfflineFallback { .. }
    ));
}

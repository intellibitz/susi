#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! End-to-end workspace integration test verifying that all 5 Apache-2.0 Susi
//! Core pillars cooperate cleanly without vendor bloat or external cloud coupling:
//! 1. **GAWD** (`susi_gawd` — swarm & `MissionDag` coordination)
//! 2. **GEMI** (`susi_gemi` — local/cloud inference model catalog & router)
//! 3. **GMCP** (`susi_gmcp` — MCP protocol & tool resource plane)
//! 4. **ACP** (`susi_gawd::acp` — standard open-socket JSON-RPC/HTTP/SSE & stdio pillar)
//! 5. **Susi Harness** (`susi_harness` — parallel isolated multi-agent workflow with
//!    pure-Git remote pull & merge on every step and release distillation)

use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use susi_core::mac_policy::{MacPolicy, PrivacyMode};
use susi_gawd::acp::{
    AcpAdmissionPolicy, AcpContentBlock, AcpEngine, AcpSocketClient, AcpSocketServer,
    AcpTurnContext, AcpTurnExecutor, ACP_CANONICAL_PORT, ACP_DISCOVERY_SCHEMA,
    ACP_ERR_CAPABILITY_REFUSED, ACP_PROTOCOL_VERSION,
};
use susi_gawd::dag::MissionDag;
use susi_gemi::models::coding_models::CodingModelManager;
use susi_gmcp::protocol::GmcpService;
use susi_harness::recursive::{ContainerRole, MIN_ISOLATED_PORT_STRIDE};
use susi_harness::workflow_runner::{
    ParallelAgentSpec, ParallelWorkflowHarness, RemoteReconcileConfig, RemoteSyncMode, WorkflowStep,
};

struct ScopedSandbox {
    dir: PathBuf,
}

impl ScopedSandbox {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("susi-{label}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).expect("create sandbox directory");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for ScopedSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct PillarBridgeExecutor {
    gmcp_name: String,
    top_model_id: String,
    dag_node_count: usize,
}

impl AcpTurnExecutor for PillarBridgeExecutor {
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit_delta: &mut dyn FnMut(&str),
    ) -> Result<Vec<AcpContentBlock>, String> {
        if cancelled.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        let summary = format!(
            "gawd_nodes={} gemi_top={} gmcp={} prompt_blocks={}",
            self.dag_node_count,
            self.top_model_id,
            self.gmcp_name,
            ctx.prompt.len()
        );
        emit_delta(&summary);
        Ok(vec![AcpContentBlock::text(summary)])
    }
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git binary must execute");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn five_core_pillars_cooperate_end_to_end_with_remote_reconcile_and_distillation() {
    let sandbox = ScopedSandbox::new("five-pillars-workflow");

    // 1. GAWD: build a dependency-ordered swarm MissionDag.
    let mut dag = MissionDag::new("Verify five Apache-2.0 core pillars");
    let n_a = dag.push_node("Implement pillar-a slice", "Scoped worker A", vec![0]);
    let n_b = dag.push_node("Implement pillar-b slice", "Scoped worker B", vec![0]);
    let n_distill = dag.push_node(
        "Distill qualified release",
        "Compose candidate",
        vec![n_a, n_b],
    );
    assert_eq!(dag.nodes.len(), 4);
    assert_eq!(MissionDag::persist_id(n_distill), "n3");

    // 2. GEMI: verify bundled coding-model substrate catalog loads cleanly offline.
    let catalog = CodingModelManager::catalog().expect("load bundled GEMI coding models");
    assert!(!catalog.is_empty(), "GEMI catalog must not be empty");
    let top_model_id = catalog[0].id.clone();
    assert!(!top_model_id.is_empty());

    // 3. GMCP: boot isolated MCP service, install built-in catalog, and inspect discovery.
    let gmcp_ws = sandbox.path().join("gmcp-workspace");
    fs::create_dir_all(&gmcp_ws).unwrap();
    let gmcp = GmcpService::new(gmcp_ws);
    susi_gmcp::catalog::install(&gmcp).expect("install built-in GMCP resources");
    let gmcp_discover = serde_json::to_value(gmcp.discover_result()).unwrap();
    let gmcp_server_name = gmcp_discover["serverInfo"]["name"]
        .as_str()
        .unwrap_or("susi-gmcp")
        .to_owned();
    assert!(gmcp_server_name.contains("susi"));

    // 4. ACP: bind loopback open socket with MacPolicy admission and bridge into GAWD/GEMI/GMCP.
    let mac = Arc::new(MacPolicy::new([0x42; 32], PrivacyMode::Balanced, false));
    let acp_engine = Arc::new(AcpEngine::new(
        AcpAdmissionPolicy::with_mac_policy(Arc::clone(&mac), "external-ide-plug"),
        Arc::new(PillarBridgeExecutor {
            gmcp_name: gmcp_server_name.clone(),
            top_model_id: top_model_id.clone(),
            dag_node_count: dag.nodes.len(),
        }),
    ));
    let mut acp_server =
        AcpSocketServer::bind_loopback(acp_engine, "five-pillars-bearer-token").unwrap();
    let base_url = acp_server.base_url();

    // Public discovery succeeds without auth; unauthenticated JSON-RPC fails closed with 401.
    let anon_client = AcpSocketClient::new(&base_url, None);
    let discovery = anon_client.discover().unwrap();
    assert_eq!(discovery.schema, ACP_DISCOVERY_SCHEMA);
    assert_eq!(discovery.protocol_version, ACP_PROTOCOL_VERSION);
    assert_eq!(discovery.agent.license, "Apache-2.0");
    assert_eq!(discovery.agent.canonical_port, ACP_CANONICAL_PORT);
    let (unauth_code, _) = anon_client
        .raw_rpc(&json!({"jsonrpc":"2.0","id":1,"method":"session/list","params":{}}))
        .unwrap();
    assert_eq!(unauth_code, 401);

    let auth_client = AcpSocketClient::new(&base_url, Some("five-pillars-bearer-token".to_owned()));
    let (init_code, init_body) = auth_client
        .raw_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "initialize",
            "params": {
                "protocol_version": ACP_PROTOCOL_VERSION,
                "client_info": { "name": "commercial-desktop-plug", "version": "1.0.0" }
            }
        }))
        .unwrap();
    assert_eq!(init_code, 200);
    assert_eq!(
        init_body.unwrap()["result"]["protocol_version"],
        ACP_PROTOCOL_VERSION
    );

    // Ungranted `fs.write` capability fails closed (-32003) until MacPolicy grants it.
    let (_, denied_sess) = auth_client
        .raw_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/new",
            "params": { "workspace": "pillars/demo", "capabilities": ["fs.write"] }
        }))
        .unwrap();
    assert_eq!(
        denied_sess.unwrap()["error"]["code"],
        ACP_ERR_CAPABILITY_REFUSED
    );

    mac.grant("external-ide-plug", "fs.write", "pillars/demo", Some(600))
        .unwrap();
    let (_, granted_sess) = auth_client
        .raw_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/new",
            "params": { "workspace": "pillars/demo", "capabilities": ["fs.write"] }
        }))
        .unwrap();
    let session_id = granted_sess.unwrap()["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (sse_status, sse_events) = auth_client
        .prompt_stream_sse(5, &session_id, "Inspect 5 pillars substrate")
        .unwrap();
    assert_eq!(sse_status, 200);
    assert!(sse_events.iter().any(|(ev, _)| ev == "acp.turn.event"));
    let (_, final_frame) = sse_events.last().unwrap();
    assert_eq!(final_frame["result"]["stop_reason"], "end_turn");
    let turn_text = final_frame["result"]["output"][0]["text"].as_str().unwrap();
    assert!(turn_text.contains("gawd_nodes=4"));
    assert!(turn_text.contains(&top_model_id));

    let (_, close_body) = auth_client
        .raw_rpc(&json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "session/close",
            "params": { "session_id": session_id }
        }))
        .unwrap();
    assert_eq!(close_body.unwrap()["result"]["closed"], true);
    acp_server.shutdown();

    // 5. Susi Harness: bare `origin` + primary + peer clones, parallel isolated wave,
    // remote pull/merge on every step, distilled release ref, and offline fallback.
    let origin = sandbox.path().join("origin.git");
    let primary = sandbox.path().join("primary-repo");
    let peer = sandbox.path().join("peer-repo");
    fs::create_dir_all(&origin).unwrap();
    run_git(&origin, &["init", "--bare", "--initial-branch=main"]);
    run_git(
        sandbox.path(),
        &["clone", origin.to_str().unwrap(), primary.to_str().unwrap()],
    );
    for (k, v) in [
        ("user.name", "Susi Harness"),
        ("user.email", "harness@localhost"),
        ("commit.gpgsign", "false"),
    ] {
        run_git(&primary, &["config", k, v]);
    }
    fs::write(primary.join("README.md"), "five-pillars-seed\n").unwrap();
    run_git(&primary, &["add", "README.md"]);
    run_git(
        &primary,
        &["commit", "-m", "chore: seed repo\n\nTask: T-SEED-PILLARS"],
    );
    run_git(&primary, &["push", "-u", "origin", "HEAD:refs/heads/main"]);

    run_git(
        sandbox.path(),
        &["clone", origin.to_str().unwrap(), peer.to_str().unwrap()],
    );
    for (k, v) in [
        ("user.name", "Remote Peer"),
        ("user.email", "peer@localhost"),
        ("commit.gpgsign", "false"),
    ] {
        run_git(&peer, &["config", k, v]);
    }
    // Advance origin/main from peer before `start` so `start` must pull + fast-forward.
    fs::create_dir_all(peer.join("shared")).unwrap();
    fs::write(peer.join("shared/upstream.txt"), "peer-v1\n").unwrap();
    run_git(&peer, &["add", "shared/upstream.txt"]);
    run_git(
        &peer,
        &["commit", "-m", "feat: upstream peer\n\nTask: T-UPSTREAM-1"],
    );
    run_git(&peer, &["push", "origin", "HEAD:refs/heads/main"]);

    let harness = ParallelWorkflowHarness::new(
        &primary,
        sandbox.path().join("harness-isolated"),
        RemoteReconcileConfig::default(),
        4,
    )
    .unwrap();

    let specs = vec![
        ParallelAgentSpec {
            agent: "pillar-a".to_owned(),
            task_id: "T-PILLAR-A".to_owned(),
            title: "Pillar A parallel worker".to_owned(),
            deps: Vec::new(),
            scopes: vec!["pillars/a".to_owned()],
        },
        ParallelAgentSpec {
            agent: "pillar-b".to_owned(),
            task_id: "T-PILLAR-B".to_owned(),
            title: "Pillar B parallel worker".to_owned(),
            deps: Vec::new(),
            scopes: vec!["pillars/b".to_owned()],
        },
    ];

    let slots = Mutex::new(Vec::new());
    let report = harness
        .execute_parallel_plan(&specs, Some("v0-21-61-pillars"), |ws| {
            assert_eq!(ws.driver_contract.role, ContainerRole::Driver);
            assert_eq!(ws.engine_contract.role, ContainerRole::Engine);
            assert!(ws.worktree_dir.join("shared/upstream.txt").is_file());
            slots.lock().unwrap().push((
                ws.worktree_dir.clone(),
                ws.target_dir.clone(),
                ws.susi_home_dir.clone(),
                ws.port_offset,
            ));

            let suffix = ws.agent.strip_prefix("pillar-").unwrap_or("x");
            let dir = ws.worktree_dir.join("pillars").join(suffix);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("mod.rs"),
                format!("pub const ID: &str = {suffix:?};\n"),
            )
            .unwrap();

            if ws.agent == "pillar-a" {
                fs::write(peer.join("shared/midwave.txt"), "peer-v2\n").unwrap();
                run_git(&peer, &["add", "shared/midwave.txt"]);
                run_git(
                    &peer,
                    &["commit", "-m", "feat: midwave peer\n\nTask: T-UPSTREAM-2"],
                );
                run_git(&peer, &["push", "origin", "HEAD:refs/heads/main"]);
            }
            Ok(format!("feat({}): isolated completion", ws.agent))
        })
        .unwrap();

    let recorded = slots.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_ne!(recorded[0].0, recorded[1].0);
    assert_ne!(recorded[0].1, recorded[1].1);
    assert_ne!(recorded[0].2, recorded[1].2);
    assert!(recorded[0].3.abs_diff(recorded[1].3) >= MIN_ISOLATED_PORT_STRIDE);

    let steps: BTreeSet<WorkflowStep> = report.step_receipts.iter().map(|r| r.step).collect();
    assert_eq!(
        steps,
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
    assert!(report
        .step_receipts
        .iter()
        .all(|r| r.remote_sync.reached_remote()));
    assert!(report
        .step_receipts
        .iter()
        .any(|r| matches!(r.remote_sync, RemoteSyncMode::MergedThreeWay { .. })));

    let distilled = report.distilled_release.expect("distill snapshot emitted");
    assert_eq!(distilled.branch_ref, "refs/heads/release/v0-21-61-pillars");
    assert!(distilled.included_tasks.contains(&"T-PILLAR-A".to_owned()));
    assert!(distilled.included_tasks.contains(&"T-PILLAR-B".to_owned()));

    // Verify offline fallback when remote disconnects.
    run_git(
        &primary,
        &[
            "remote",
            "set-url",
            "origin",
            sandbox
                .path()
                .join("unreachable-origin.git")
                .to_str()
                .unwrap(),
        ],
    );
    let (_, offline_start) = harness
        .step_start(
            &ParallelAgentSpec {
                agent: "pillar-offline".to_owned(),
                task_id: "T-PILLAR-OFFLINE".to_owned(),
                title: "Offline fallback check".to_owned(),
                deps: Vec::new(),
                scopes: vec!["pillars/offline".to_owned()],
            },
            2,
        )
        .unwrap();
    assert!(matches!(
        offline_start.remote_sync,
        RemoteSyncMode::OfflineFallback { .. }
    ));
}

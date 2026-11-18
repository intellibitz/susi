#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end integration of the three core pillars:
//! GAWD (A2A and ACP), GEMI (inference routing), and GMCP (MCP protocol/tool
//! plane). The Susi Harness parallel workflow section moved to Susi Commercial
//! Apps with the harness (`crates/susi-harness/tests/parallel_workflow_end_to_end.rs`
//! there; Mandate 84).
//! The legacy filename is retained for existing test selectors.
//! Protocol endpoints, imports and capabilities are unchanged.
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
#[test]
fn three_core_pillars_cooperate_with_gawd_and_acp_end_to_end() {
    let sandbox = ScopedSandbox::new("five-pillars-workflow");
    // 1. GAWD: build a dependency-ordered swarm MissionDag.
    let mut dag = MissionDag::new("Verify three Apache-2.0 core pillars with GAWD capabilities");
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
        .raw_rpc(&json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocol_version":ACP_PROTOCOL_VERSION,"client_info":{"name":"commercial-desktop-plug","version":"1.0.0"}}}))
        .unwrap();
    assert_eq!(init_code, 200);
    assert_eq!(
        init_body.unwrap()["result"]["protocol_version"],
        ACP_PROTOCOL_VERSION
    );
    // Ungranted `fs.write` capability fails closed (-32003) until MacPolicy grants it.
    let (_, denied_sess) = auth_client
        .raw_rpc(&json!({"jsonrpc":"2.0","id":3,"method":"session/new","params":{"workspace":"pillars/demo","capabilities":["fs.write"]}}))
        .unwrap();
    assert_eq!(
        denied_sess.unwrap()["error"]["code"],
        ACP_ERR_CAPABILITY_REFUSED
    );
    mac.grant("external-ide-plug", "fs.write", "pillars/demo", Some(600))
        .unwrap();
    let (_, granted_sess) = auth_client
        .raw_rpc(&json!({"jsonrpc":"2.0","id":4,"method":"session/new","params":{"workspace":"pillars/demo","capabilities":["fs.write"]}}))
        .unwrap();
    let session_id = granted_sess.unwrap()["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (sse_status, sse_events) = auth_client
        .prompt_stream_sse(5, &session_id, "Inspect 3 pillars substrate")
        .unwrap();
    assert_eq!(sse_status, 200);
    assert!(sse_events.iter().any(|(ev, _)| ev == "acp.turn.event"));
    let (_, final_frame) = sse_events.last().unwrap();
    assert_eq!(final_frame["result"]["stop_reason"], "end_turn");
    let turn_text = final_frame["result"]["output"][0]["text"].as_str().unwrap();
    assert!(turn_text.contains("gawd_nodes=4"));
    assert!(turn_text.contains(&top_model_id));
    let (_, close_body) = auth_client
        .raw_rpc(&json!({"jsonrpc":"2.0","id":6,"method":"session/close","params":{"session_id":session_id}}))
        .unwrap();
    assert_eq!(close_body.unwrap()["result"]["closed"], true);
    acp_server.shutdown();
}

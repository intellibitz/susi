#[test]
fn gmcp_req_203_014_01_contract() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::extensions::ExtensionRegistry;
    use susi_gmcp::mcp::model::{
        CallToolResult, ClientCapabilities, ContentBlock, CreateTaskResult, DetailedTask,
        ErrorCode, GetTaskResult, JsonObject, ServerCapabilities, Task, TaskPayload, TaskStatus,
        TASKS_EXTENSION_ID,
    };
    use susi_gmcp::mcp::task_manager::{
        uuid_v4, TaskManager, DEFAULT_POLL_INTERVAL_MS, DEFAULT_TASK_TTL_MS,
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-014.md"))
        .expect("VC-203-014 contract document must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
        "Operations and non-claims",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a ## {section} section"
        );
    }

    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .expect("requirements array")
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-014")
        .collect();
    assert_eq!(
        rows.len(),
        46,
        "the contract must bind all 46 VC-203-014 rows"
    );

    let mut modern = 0usize;
    let mut optional = 0usize;
    let mut adapter = 0usize;
    let mut must = 0usize;
    let mut must_not = 0usize;
    let mut should = 0usize;
    let mut may = 0usize;
    let mut docs = std::collections::BTreeSet::new();
    for row in &rows {
        let id = row["id"].as_str().expect("row id");
        let level = row["level"].as_str().expect("row level");
        let clause = row["clause"]
            .as_str()
            .expect("row clause")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let path = row["trace"]["production_path"]
            .as_str()
            .expect("production_path");
        let doc = row["source"]["document"].as_str().expect("source document");
        docs.insert(doc);
        assert!(
            contract.contains(&format!("| {id} | {level} | {clause} | {path} |")),
            "behavior table must bind {id} verbatim with level {level}, clause, and production_path {path}"
        );
        assert!(
            root.join(path).exists(),
            "declared production path must exist: {path}"
        );
        assert_eq!(
            row["trace"]["positive_test"].as_str(),
            Some("tests/gmcp_requirements.rs::gmcp_req_203_014_03_integration"),
            "unexpected positive_test for {id}"
        );
        assert_eq!(
            row["trace"]["negative_test"].as_str(),
            Some("tests/gmcp_requirements.rs::gmcp_req_203_014_04_adversarial"),
            "unexpected negative_test for {id}"
        );
        assert_eq!(
            row["trace"]["evidence"].as_str(),
            Some(".agents/gmcp/evidence/VC-203-014.jsonl"),
            "unexpected evidence location for {id}"
        );
        match row["disposition"]["status"]
            .as_str()
            .expect("disposition status")
        {
            "modern" => modern += 1,
            "optional" => optional += 1,
            "adapter" => adapter += 1,
            other => panic!("unexpected disposition {other} for {id}"),
        }
        match level {
            "must" => must += 1,
            "must_not" => must_not += 1,
            "should" => should += 1,
            "may" => may += 1,
            other => panic!("unexpected level {other} for {id}"),
        }
    }
    assert_eq!((modern, optional, adapter), (41, 5, 0));
    assert_eq!((must, must_not, should, may), (25, 8, 8, 5));
    assert!(contract.contains("modern=41"));
    assert!(contract.contains("optional=5"));
    assert!(contract.contains("adapter=0"));
    assert!(contract.contains("must=25"));
    assert!(contract.contains("must_not=8"));
    assert!(contract.contains("should=8"));
    assert!(contract.contains("may=5"));

    let sources: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json"))
            .expect("sources.json must exist"),
    )
    .expect("sources.json must parse");
    let ext_tasks_commit = sources["upstream"]["ext-tasks"]["commit"]
        .as_str()
        .expect("ext-tasks commit");
    let mcp_commit = sources["upstream"]["modelcontextprotocol"]["commit"]
        .as_str()
        .expect("modelcontextprotocol commit");
    assert!(
        contract.contains(ext_tasks_commit),
        "contract must pin ext-tasks commit {ext_tasks_commit}"
    );
    assert!(
        contract.contains(mcp_commit),
        "contract must pin modelcontextprotocol commit {mcp_commit}"
    );

    docs.insert("docs/specification/2025-11-25/basic/utilities/tasks.mdx");
    let source_files = sources["files"].as_array().expect("sources.files array");
    for doc in &docs {
        let entry = source_files
            .iter()
            .find(|f| f["document"].as_str() == Some(*doc))
            .unwrap_or_else(|| panic!("missing sources.json entry for {doc}"));
        let expected_sha = entry["sha256"].as_str().expect("sha256 in sources.json");
        let actual_bytes = std::fs::read(root.join(".agents/gmcp/spec").join(doc))
            .unwrap_or_else(|e| panic!("spec doc {doc} readable: {e}"));
        let actual_sha = susi_paths::hex::encode(Sha256::digest(&actual_bytes));
        assert_eq!(actual_sha, expected_sha, "spec hash drift on {doc}");
        assert!(
            contract.contains(&format!("| {doc} | {expected_sha} |")),
            "pinned sources table must pin {doc} with sha256 {expected_sha}"
        );
    }

    for runtime_path in [
        "crates/susi-gmcp/src/mcp/task_manager.rs",
        "crates/susi-gmcp/src/mcp/model.rs",
        "crates/susi-gmcp/src/mcp/service.rs",
        "crates/susi-gmcp/src/mcp/message.rs",
        "crates/susi-gmcp/src/mcp/http.rs",
        "crates/susi-gmcp/src/mcp/transport.rs",
        "crates/susi-gmcp/src/protocol.rs",
        "crates/susi-gmcp/src/jsonrpc_gate.rs",
        "crates/susi-gmcp/src/extensions.rs",
        "crates/susi-gmcp/src/eco_mcp_features.rs",
        "crates/susi-wire-client/src/mcp.rs",
    ] {
        assert!(
            contract.contains(runtime_path),
            "contract must cite canonical module {runtime_path}"
        );
        assert!(
            root.join(runtime_path).exists(),
            "canonical module must exist: {runtime_path}"
        );
    }

    for marker in [
        "io.modelcontextprotocol/tasks",
        "CreateTaskResult",
        "GetTaskResult",
        "UpdateTaskResult",
        "CancelTaskResult",
        "tasks/get",
        "tasks/update",
        "tasks/cancel",
        "tasks/list",
        "tasks/result",
        "notifications/tasks",
        "taskIds",
        "Mcp-Name",
        "-32021",
        "-32602",
        "-32601",
        "isError",
        "used_input_keys",
        "ttlMs",
        "pollIntervalMs",
        "DEFAULT_TASK_TTL_MS",
        "DEFAULT_POLL_INTERVAL_MS",
        "T1:",
        "T2:",
        "T3:",
        "T4:",
        "T5:",
        "T6:",
        "T7:",
        "T8:",
        "F-POS-014-01",
        "F-POS-014-08",
        "F-NEG-014-01",
        "F-NEG-014-08",
        "VC-203-014.jsonl",
        "VC-203-014.json",
        "gmcp_req_203_014_01_contract",
        "gmcp_vc_203_014_mastery",
    ] {
        assert!(
            contract.contains(marker),
            "VC-203-014 contract missing required invariant or fixture marker `{marker}`"
        );
    }

    // Canonical model and baseline surface invariants pinned by contract-203-014.md
    assert_eq!(TASKS_EXTENSION_ID, "io.modelcontextprotocol/tasks");
    assert_eq!(DEFAULT_TASK_TTL_MS, 300_000);
    assert_eq!(DEFAULT_POLL_INTERVAL_MS, 1_000);
    assert!(!TaskStatus::Working.is_terminal());
    assert!(!TaskStatus::InputRequired.is_terminal());
    assert!(TaskStatus::Completed.is_terminal());
    assert!(TaskStatus::Failed.is_terminal());
    assert!(TaskStatus::Cancelled.is_terminal());
    assert!(ClientCapabilities::builder()
        .enable_tasks()
        .build()
        .supports_tasks());
    assert!(ServerCapabilities::builder()
        .enable_tasks()
        .build()
        .supports_tasks());
    assert!(ExtensionRegistry::shipped()
        .capability_map()
        .contains_key(TASKS_EXTENSION_ID));

    let id = uuid_v4();
    assert_eq!(id.len(), 36);
    let task = Task {
        task_id: id.clone(),
        status: TaskStatus::Working,
        status_message: None,
        created_at: "2026-11-15T00:00:00Z".into(),
        last_updated_at: "2026-11-15T00:00:00Z".into(),
        ttl_ms: Some(DEFAULT_TASK_TTL_MS),
        poll_interval_ms: Some(DEFAULT_POLL_INTERVAL_MS),
    };
    let created_json =
        serde_json::to_value(CreateTaskResult::new(task.clone())).expect("serialize");
    assert_eq!(created_json["resultType"], "task");
    assert_eq!(created_json["taskId"], id);
    assert_eq!(created_json["status"], "working");
    assert_eq!(created_json["ttlMs"], DEFAULT_TASK_TTL_MS);
    assert_eq!(created_json["pollIntervalMs"], DEFAULT_POLL_INTERVAL_MS);

    let err_tool = CallToolResult::error(vec![ContentBlock::text("tool domain error")]);
    let result_obj: JsonObject =
        serde_json::from_value(serde_json::to_value(&err_tool).expect("tool res")).expect("map");
    let get_json = serde_json::to_value(GetTaskResult::new(DetailedTask::new(
        task,
        TaskPayload::Completed { result: result_obj },
    )))
    .expect("serialize get");
    assert_eq!(get_json["resultType"], "complete");
    assert_eq!(get_json["status"], "completed");
    assert_eq!(get_json["result"]["isError"], true);
    assert!(get_json.get("error").is_none());

    let err = TaskManager::new()
        .get_task("missing-task-id")
        .expect_err("unknown task fails");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
}

#[test]
fn gmcp_req_203_014_02_production() {
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_core::plane_bus::{topics, with_watched, PlaneBus, PlaneHandler};
    use susi_gmcp::mcp::model::{CallToolResult, ContentBlock, InputRequest, Tool};
    use susi_gmcp::mcp::ServiceExt;
    use susi_gmcp::protocol::GmcpService;
    use susi_gmcp::server::GmcpServer;
    use susi_wire_client::mcp::{
        CallToolOutcome, PersistedTaskHandle, PersistedTaskHandleStore, Session, TaskStatus,
        WireError, PROTOCOL_VERSION_2026_07_28,
    };

    let (_lock, _env, home) = gmcp_http_home_guard("014-prod");
    struct RestoreProdPlane;
    impl Drop for RestoreProdPlane {
        fn drop(&mut self) {
            let bus = PlaneBus::global();
            bus.unregister(topics::TOOLS_LIST);
            bus.unregister(topics::TOOLS_EXISTS);
            bus.unregister(topics::TOOLS_EXECUTE);
            susi_tools::plane_handler::register();
        }
    }
    let _restore_prod_plane = RestoreProdPlane;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let tok_alice = "tok-alice-203-014-02";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_alice}\n")).unwrap();

    // Register deterministic PlaneBus tools exercised through the real GmcpServer HTTP runtime.
    let echo_runs = Arc::new(AtomicUsize::new(0));
    let wait_cancelled = Arc::new(AtomicBool::new(false));
    struct ProdList;
    impl PlaneHandler for ProdList {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let extra = [
                json!({"name": "prod_echo", "description": "Deterministic echo"}),
                json!({"name": "prod_wait", "description": "Cancellable wait"}),
            ];
            let mut merged = if let Ok(Value::Array(tools)) =
                PlaneBus::global().request_local_prefix(topics::TOOLS_LIST, payload)
            {
                tools
            } else {
                Vec::new()
            };
            for tool in extra {
                let name = tool.get("name").and_then(Value::as_str);
                if !merged
                    .iter()
                    .any(|item| item.get("name").and_then(Value::as_str) == name)
                {
                    merged.push(tool);
                }
            }
            Ok(Value::Array(merged))
        }
    }
    struct ProdExists;
    impl PlaneHandler for ProdExists {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            if matches!(name, "prod_echo" | "prod_wait") {
                Ok(json!({ "exists": true }))
            } else {
                PlaneBus::global().request_local_prefix(topics::TOOLS_EXISTS, payload)
            }
        }
    }
    struct ProdExec {
        echo_runs: Arc<AtomicUsize>,
        wait_cancelled: Arc<AtomicBool>,
    }
    impl PlaneHandler for ProdExec {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            if !matches!(name, "prod_echo" | "prod_wait") {
                return PlaneBus::global().request_local_prefix(topics::TOOLS_EXECUTE, payload);
            }
            let args = payload.get("args").cloned().unwrap_or(Value::Null);
            let echo_runs = self.echo_runs.clone();
            let wait_cancelled = self.wait_cancelled.clone();
            with_watched(&payload, || match name {
                "prod_echo" => {
                    echo_runs.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    let msg = args.get("msg").and_then(Value::as_str).unwrap_or("");
                    Ok(json!({ "text": format!("echo:{msg}") }))
                }
                "prod_wait" => {
                    for _ in 0..300 {
                        if susi_core::progress::is_cancelled() {
                            wait_cancelled.store(true, Ordering::Release);
                            return Ok(json!({ "text": "cancelled" }));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(json!({ "text": "finished" }))
                }
                other => Err(format!("unexpected tool {other}")),
            })
        }
    }
    susi_daemon::composition::wire_mcp_serve_substrate(&home.join(".susi"));
    PlaneBus::global().register(topics::TOOLS_LIST, Arc::new(ProdList));
    PlaneBus::global().register(topics::TOOLS_EXISTS, Arc::new(ProdExists));
    PlaneBus::global().register(
        topics::TOOLS_EXECUTE,
        Arc::new(ProdExec {
            echo_runs: echo_runs.clone(),
            wait_cancelled: wait_cancelled.clone(),
        }),
    );

    let spawn_server = || {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let port = listener.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let ws = root.to_path_buf();
        std::thread::spawn(move || {
            GmcpServer::start_http_server(ws, vec![listener], None, false);
        });
        for _ in 0..100 {
            if TcpStream::connect(&addr).is_ok() {
                return (addr, port);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("server did not start on {addr}");
    };

    let (addr1, port1) = spawn_server();
    let mut alice = Session::open_2026_07_28(&addr1, Some(tok_alice), Duration::from_secs(15))
        .expect("stateless 2026-07-28 discover must succeed");
    assert_eq!(alice.protocol_version(), PROTOCOL_VERSION_2026_07_28);
    assert!(alice.supports_server_tasks());

    // 1. Non-opt-in call must run synchronously; opted-in call yields CreateTaskResult; subsequent non-opt-in must stay sync.
    let sync_before = alice
        .call_tool_with_tasks("prod_echo", &json!({"msg": "sync"}), false, &mut |_| {})
        .unwrap();
    match sync_before {
        CallToolOutcome::Complete(val) => assert_eq!(val["content"][0]["text"], "echo:sync"),
        other => panic!("expected sync Complete, got {other:?}"),
    }

    let async_outcome = alice
        .call_tool_with_tasks("prod_echo", &json!({"msg": "async"}), true, &mut |_| {})
        .unwrap();
    let echo_handle = match async_outcome {
        CallToolOutcome::Task(h) => h,
        other => panic!("expected CreateTaskResult Task handle, got {other:?}"),
    };

    let sync_after = alice
        .call_tool_with_tasks("prod_echo", &json!({"msg": "after"}), false, &mut |_| {})
        .unwrap();
    assert!(matches!(sync_after, CallToolOutcome::Complete(_)));

    // 2. Omitting io.modelcontextprotocol/tasks on tasks/get must fail with -32021.
    let missing_cap = alice
        .request(
            "tasks/get",
            &json!({"taskId": echo_handle.task_id}),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(
        matches!(missing_cap, WireError::Rpc(-32021, _)),
        "expected -32021, got {missing_cap:?}"
    );

    // 3. HTTP Mcp-Name mismatch on tasks/get must fail with 400 / -32020 HEADER_MISMATCH.
    let bad_body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 9001,
        "method": "tasks/get",
        "params": {
            "taskId": echo_handle.task_id,
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION_2026_07_28,
                "io.modelcontextprotocol/clientCapabilities": {
                    "extensions": { "io.modelcontextprotocol/tasks": {} }
                }
            }
        }
    }))
    .unwrap();
    let mut raw_stream = TcpStream::connect(("127.0.0.1", port1)).unwrap();
    raw_stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let raw_req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port1}\r\nConnection: close\r\nAuthorization: Bearer {tok_alice}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {PROTOCOL_VERSION_2026_07_28}\r\nMcp-Method: tasks/get\r\nMcp-Name: mismatched-task-id\r\nContent-Length: {}\r\n\r\n{bad_body}",
        bad_body.len()
    );
    raw_stream.write_all(raw_req.as_bytes()).unwrap();
    let mut raw_resp = String::new();
    let _ = raw_stream.read_to_string(&mut raw_resp);
    let raw_status: u16 = raw_resp
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    assert_eq!(raw_status, 400, "{raw_resp}");
    assert!(raw_resp.contains("-32020"), "{raw_resp}");

    // 4. Client handle persistence + polling to completion.
    let handle_store_path = home.join("client-handles.json");
    let mut store = PersistedTaskHandleStore::open(&handle_store_path).unwrap();
    store
        .save(PersistedTaskHandle::from_handle(
            &addr1,
            PROTOCOL_VERSION_2026_07_28,
            &echo_handle,
        ))
        .unwrap();

    let completed = alice
        .poll_task_until_done(&echo_handle, Some(Duration::from_secs(10)), |_| {
            Ok(json!({}))
        })
        .unwrap();
    assert_eq!(completed.handle.status, TaskStatus::Completed);
    assert_eq!(
        completed.result.as_ref().unwrap()["content"][0]["text"],
        "echo:async"
    );
    assert!(store.update_from_snapshot(&completed).unwrap());
    assert_eq!(
        PersistedTaskHandleStore::open(&handle_store_path)
            .unwrap()
            .get(&echo_handle.task_id)
            .unwrap()
            .status,
        TaskStatus::Completed
    );

    // 5. Cancellation + terminal immutability (-32602).
    let wait_handle = match alice
        .call_tool_with_tasks("prod_wait", &json!({}), true, &mut |_| {})
        .unwrap()
    {
        CallToolOutcome::Task(h) => h,
        other => panic!("expected Task handle for prod_wait, got {other:?}"),
    };
    alice.cancel_task(&wait_handle.task_id).unwrap();
    let mut cancelled = None;
    for _ in 0..20 {
        let snap = alice.get_task(&wait_handle.task_id).unwrap();
        if snap.handle.status.is_terminal() {
            cancelled = Some(snap);
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let cancelled = cancelled.expect("cancelled task must settle to terminal state");
    assert_eq!(cancelled.handle.status, TaskStatus::Cancelled);
    assert!(
        wait_cancelled.load(Ordering::Acquire),
        "prod_wait must observe cooperative task cancellation"
    );
    assert!(matches!(
        alice.cancel_task(&wait_handle.task_id),
        Err(WireError::Rpc(-32602, _))
    ));
    assert!(matches!(
        alice.update_task(&wait_handle.task_id, &json!({})),
        Err(WireError::Rpc(-32602, _))
    ));

    // 6. Restart survival & cross-bearer isolation across durable TaskManager HTTP service.
    let runs_before_restart = echo_runs.load(Ordering::SeqCst);
    let (addr2, _port2) = spawn_server();
    let mut alice_restarted =
        Session::open_2026_07_28(&addr2, Some(tok_alice), Duration::from_secs(15)).unwrap();
    let persisted_id = PersistedTaskHandleStore::open(&handle_store_path)
        .unwrap()
        .get(&echo_handle.task_id)
        .unwrap()
        .task_id
        .clone();
    let resumed = alice_restarted.get_task(&persisted_id).unwrap();
    assert_eq!(resumed.handle.status, TaskStatus::Completed);
    assert_eq!(
        resumed.result.as_ref().unwrap()["content"][0]["text"],
        "echo:async"
    );
    assert_eq!(
        echo_runs.load(Ordering::SeqCst),
        runs_before_restart,
        "durable completed task must be replayed without re-execution"
    );

    // Different bearer principal fails owner checks on persisted task handle.
    let tok_bob = "tok-bob-203-014-02";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_bob}\n")).unwrap();
    let mut bob = Session::open_2026_07_28(&addr2, Some(tok_bob), Duration::from_secs(15)).unwrap();
    assert!(matches!(
        bob.get_task(&persisted_id),
        Err(WireError::Rpc(-32602, _))
    ));
    assert!(matches!(
        bob.cancel_task(&persisted_id),
        Err(WireError::Rpc(-32602, _))
    ));
    assert!(matches!(
        bob.get_task("00000000-0000-4000-8000-000000000000"),
        Err(WireError::Rpc(-32602, _))
    ));

    // 7. End-to-end MRTR composition (roots/list, elicitation/create, sampling/createMessage)
    // and subscriptions/listen.taskIds notification push on durable GmcpService.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mrtr_store = home.join("mrtr-durable");
        let svc = GmcpService::open_durable(root.to_path_buf(), mrtr_store).unwrap();
        let mrtr_tool: Tool =
            serde_json::from_value(json!({"name": "mrtr_triplet", "inputSchema": {"type": "object"}}))
                .unwrap();
        svc.register_task_tool(
            mrtr_tool,
            Arc::new(|_, ctx| {
                Box::pin(async move {
                    let roots: InputRequest =
                        serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                    let r_ans = ctx.request_input("k-roots", roots).await?;
                    let elicit: InputRequest = serde_json::from_value(json!({
                        "method": "elicitation/create",
                        "params": {"mode": "form", "message": "Confirm", "requestedSchema": {"type": "object"}}
                    }))
                    .unwrap();
                    let e_ans = ctx.request_input("k-elicit", elicit).await?;
                    let sample: InputRequest = serde_json::from_value(json!({
                        "method": "sampling/createMessage",
                        "params": {"messages": [{"role": "user", "content": {"type": "text", "text": "ping"}}], "maxTokens": 8}
                    }))
                    .unwrap();
                    let s_ans = ctx.request_input("k-sample", sample).await?;
                    Ok(CallToolResult {
                        content: vec![ContentBlock::text("mrtr-complete")],
                        structured_content: Some(json!({
                            "roots": r_ans,
                            "elicit": e_ans,
                            "sample": s_ans,
                        })),
                        is_error: Some(false),
                        meta: None,
                        result_type: None,
                    })
                })
            }),
        )
        .unwrap();

        let sub_svc = svc.session();
        let open_wire = |s: GmcpService| async move {
            let (c, srv) = tokio::io::duplex(65536);
            tokio::spawn(async move {
                let running = s.serve(srv).await.unwrap();
                running.waiting().await.unwrap();
            });
            BufReader::new(c)
        };
        let mut main_wire = open_wire(svc).await;
        let mut sub_wire = open_wire(sub_svc).await;
        async fn send_wire(w: &mut BufReader<tokio::io::DuplexStream>, msg: Value) {
            let bytes = format!("{msg}\n").into_bytes();
            w.get_mut().write_all(&bytes).await.unwrap();
        }
        async fn recv_wire(w: &mut BufReader<tokio::io::DuplexStream>) -> Value {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), w.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        }
        let mod_req = |id: i64, method: &str, mut params: Value| {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION_2026_07_28,
                "io.modelcontextprotocol/clientCapabilities": {
                    "extensions": { "io.modelcontextprotocol/tasks": {} }
                }
            });
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        };

        send_wire(&mut main_wire, mod_req(1, "server/discover", json!({}))).await;
        assert_eq!(recv_wire(&mut main_wire).await["result"]["resultType"], "complete");
        send_wire(&mut sub_wire, mod_req(1, "server/discover", json!({}))).await;
        assert_eq!(recv_wire(&mut sub_wire).await["result"]["resultType"], "complete");

        send_wire(&mut main_wire, mod_req(2, "tools/call", json!({"name": "mrtr_triplet"}))).await;
        let created = recv_wire(&mut main_wire).await;
        assert_eq!(created["result"]["resultType"], "task");
        let mrtr_task_id = created["result"]["taskId"].as_str().unwrap().to_string();

        // Subscribe to taskIds and confirm acknowledged precedes notifications/tasks.
        send_wire(
            &mut sub_wire,
            mod_req(
                2,
                "subscriptions/listen",
                json!({"notifications": {"taskIds": [mrtr_task_id]}}),
            ),
        )
        .await;
        assert_eq!(
            recv_wire(&mut sub_wire).await["method"],
            "notifications/subscriptions/acknowledged"
        );

        let answers = [
            ("k-roots", json!({"roots": []})),
            ("k-elicit", json!({"action": "accept"})),
            ("k-sample", json!({"role": "assistant", "content": {"type": "text", "text": "pong"}})),
        ];
        let mut next_id = 10;
        for (expected_key, answer) in answers {
            let mut updated = false;
            for _ in 0..30 {
                send_wire(&mut main_wire, mod_req(next_id, "tasks/get", json!({"taskId": mrtr_task_id}))).await;
                next_id += 1;
                let snap = recv_wire(&mut main_wire).await;
                if snap["result"]["inputRequests"].get(expected_key).is_some() {
                    let mut map = serde_json::Map::new();
                    map.insert(expected_key.to_string(), answer.clone());
                    send_wire(
                        &mut main_wire,
                        mod_req(next_id, "tasks/update", json!({"taskId": mrtr_task_id, "inputResponses": Value::Object(map)})),
                    )
                    .await;
                    next_id += 1;
                    assert!(recv_wire(&mut main_wire).await.get("error").is_none());
                    updated = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(updated, "never observed inputRequest {expected_key}");
        }

        let mut completed_final = false;
        for _ in 0..40 {
            send_wire(&mut main_wire, mod_req(next_id, "tasks/get", json!({"taskId": mrtr_task_id}))).await;
            next_id += 1;
            let snap = recv_wire(&mut main_wire).await;
            if snap["result"]["status"] == "completed" {
                assert_eq!(snap["result"]["result"]["structuredContent"]["sample"]["content"]["text"], "pong");
                completed_final = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(completed_final, "mrtr_triplet task must complete");

        let mut saw_sub_completed = false;
        for _ in 0..16 {
            let note = recv_wire(&mut sub_wire).await;
            let m = note["method"].as_str().unwrap_or("");
            assert!(m != "notifications/progress" && m != "notifications/message", "forbidden leakage: {note}");
            if m == "notifications/tasks" && note["params"]["status"] == "completed" {
                saw_sub_completed = true;
                break;
            }
        }
        assert!(saw_sub_completed, "subscription must deliver final notifications/tasks");
    });
}

/// GMCP-203-014-03: end-to-end integration of the Official Tasks extension
/// (`io.modelcontextprotocol/tasks`) across wire/HTTP gates, shipped CLI
/// (`susi mcp serve` stdio & `susi mcp call`), daemon/gateway HTTP
/// (`GmcpServer::start_http_server`), `susi_wire_client::mcp::Session`
/// (async handles, `PersistedTaskHandleStore`, and fixed-shape synchronous
/// `call_tool_resolving_tasks` bridge), `susi_core::mcp_client`, embedded and
/// Unix socket adapters, direct durable `GmcpService` MRTR, `McpGatewayProxy`,
/// `SusiConfig`, `NetGuard`, and `ExtensionRegistry` runtime isolation.
#[test]
fn gmcp_req_203_014_03_integration() {
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use susi_core::plane_bus::{topics, with_watched, PlaneBus, PlaneHandler};
    use susi_gmcp::jsonrpc_gate::{
        classify_envelope, classify_line, http_envelope_rejection, EraState, GateVerdict,
    };
    use susi_gmcp::mcp::model::{
        CallToolResult, ContentBlock, InputRequest, Tool, TASKS_EXTENSION_ID,
    };
    use susi_gmcp::mcp::ServiceExt;
    use susi_gmcp::protocol::{GmcpService, McpGatewayProxy};
    use susi_gmcp::server::GmcpServer;
    use susi_wire_client::mcp::{
        CallToolOutcome, PersistedTaskHandle, PersistedTaskHandleStore, Session, TaskStatus,
        WireError, PROTOCOL_VERSION_2026_07_28,
    };

    let (_http_lock, _http_env, home) = gmcp_http_home_guard("014-int");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tasks_meta = |req_id: &str| -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION_2026_07_28,
            "io.modelcontextprotocol/requestId": req_id,
            "io.modelcontextprotocol/clientCapabilities": {
                "extensions": { "io.modelcontextprotocol/tasks": {} }
            }
        })
    };
    let plain_modern_meta = |req_id: &str| -> Value {
        let mut meta = adapter_modern_meta()["_meta"].clone();
        meta["io.modelcontextprotocol/requestId"] = json!(req_id);
        meta
    };

    // ── A. Wire & HTTP gate parity for Tasks RPCs and notifications ──
    let mut mod_era = EraState::default();
    assert!(matches!(
        classify_line(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"t-gate","version":"1"}}}"#,
            &mut mod_era
        ),
        GateVerdict::Forward
    ));
    for (id, method, extra) in [
        ("tg-1", "tasks/get", json!({"taskId": "tid-1"})),
        (
            "tu-1",
            "tasks/update",
            json!({"taskId": "tid-1", "inputResponses": {}}),
        ),
        ("tc-1", "tasks/cancel", json!({"taskId": "tid-1"})),
        (
            "ts-1",
            "subscriptions/listen",
            json!({"notifications": {"taskIds": ["tid-1"]}}),
        ),
    ] {
        let mut params = extra;
        params["_meta"] = tasks_meta(id);
        let frame = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .unwrap();
        assert!(matches!(classify_envelope(&frame), GateVerdict::Forward));
        assert!(matches!(
            classify_line(&frame, &mut mod_era),
            GateVerdict::Forward
        ));
        assert_eq!(http_envelope_rejection(&frame), None);

        // `id: null` task RPC frames are rejected at the envelope gate (-32600);
        // bare no-`id` task RPC frames are NOT dropped as foreign notifications (`Forward` -> codec `-32600`).
        let null_id = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": null,
            "method": method,
            "params": {"taskId": "tid-1"}
        }))
        .unwrap();
        assert!(matches!(
            classify_line(&null_id, &mut mod_era),
            GateVerdict::Reject { code: -32600, .. }
        ));
        assert_eq!(
            http_envelope_rejection(&null_id).and_then(|v| v["error"]["code"].as_i64()),
            Some(-32600)
        );

        let no_id_val = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": {"taskId": "tid-1"}
        });
        let no_id = serde_json::to_vec(&no_id_val).unwrap();
        assert!(
            matches!(
                classify_envelope(&no_id),
                GateVerdict::Reject { code: -32600, .. }
            ),
            "{method} without id must fail closed at classify_envelope (-32600)"
        );
        assert!(
            matches!(
                classify_line(&no_id, &mut mod_era),
                GateVerdict::Reject { code: -32600, .. }
            ),
            "{method} without id must fail closed at classify_line (-32600)"
        );
        assert_eq!(
            http_envelope_rejection(&no_id).and_then(|v| v["error"]["code"].as_i64()),
            Some(-32600),
            "{method} without id must fail closed at http_envelope_rejection (-32600)"
        );
        assert!(
            susi_gmcp::mcp::message::ClientJsonRpcMessage::from_value(no_id_val).is_none(),
            "{method} without id must fail typed codec parse (-> -32600)"
        );
    }
    let foreign_note = br#"{"jsonrpc":"2.0","method":"$/progress","params":{}}"#;
    assert!(matches!(
        classify_line(foreign_note, &mut mod_era),
        GateVerdict::Drop
    ));
    let task_note = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/tasks",
        "params": {"taskId": "tid-1", "status": "working", "createdAt": "t0", "lastUpdatedAt": "t0"}
    }))
    .unwrap();
    assert!(matches!(
        classify_line(&task_note, &mut mod_era),
        GateVerdict::Forward
    ));
    assert_eq!(http_envelope_rejection(&task_note), None);

    let mut leg_era = EraState::default();
    assert!(matches!(
        classify_line(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"leg-gate","version":"1"}}}"#,
            &mut leg_era
        ),
        GateVerdict::Forward
    ));
    let leg_tasks_with_mod_meta = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tasks/get",
        "params": {"taskId": "tid-1", "_meta": tasks_meta("2")}
    }))
    .unwrap();
    assert!(matches!(
        classify_line(&leg_tasks_with_mod_meta, &mut leg_era),
        GateVerdict::Reject { code: -32600, .. }
    ));

    // ── B. Shipped CLI (`susi mcp serve`) stdio, SusiConfig, policy & restart durability ──
    let cli_home = home.join("cli-tasks-home");
    let cli_cfg_dir = cli_home.join(".susi");
    std::fs::create_dir_all(&cli_cfg_dir).unwrap();
    let mut cli_cfg = susi_config::SusiConfig::default();
    cli_cfg
        .settings
        .insert("max_rpc_body_bytes".to_string(), json!(4096));
    cli_cfg.save(&cli_cfg_dir).unwrap();

    let mut cli_mod = StdioSession::spawn_raw(root, &cli_home);
    let cli_disc = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-disc",
                "method": "server/discover",
                "params": adapter_modern_meta()
            }),
            Duration::from_secs(30),
        )
        .expect("susi mcp serve must answer server/discover");
    assert!(
        cli_disc["result"]["capabilities"]["extensions"][TASKS_EXTENSION_ID].is_object(),
        "CLI server/discover must advertise io.modelcontextprotocol/tasks: {cli_disc}"
    );

    // Oversized tasks/get line is rejected (-32600) by SusiConfig max_rpc_body_bytes without killing the session.
    let big_pad = "z".repeat(4300);
    let oversized_task = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":\"over-t\",\"method\":\"tasks/get\",\"params\":{{\"taskId\":\"{big_pad}\"}}}}\n"
    );
    cli_mod.send_raw(oversized_task.as_bytes());
    let over_err = cli_mod
        .response_for(&json!(null), Duration::from_secs(10))
        .expect("oversized tasks/get line must return -32600");
    assert_eq!(over_err["error"]["code"], -32600);

    // Malformed no-`id` tasks/get frame on live `susi mcp serve` stdio fails closed with -32600 (never silently dropped).
    cli_mod.send(&json!({"jsonrpc": "2.0", "method": "tasks/get", "params": {"taskId": "tid-1"}}));
    let no_id_stdio = cli_mod
        .response_for(&json!(null), Duration::from_secs(10))
        .expect("no-id tasks/get on stdio must return -32600");
    assert_eq!(no_id_stdio["error"]["code"], -32600);

    // McpGatewayProxy policy boundary applies identically whether tasks capability is omitted or declared.
    let cli_pol_sync = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-pol-sync",
                "method": "tools/call",
                "params": {
                    "name": "susi_restricted_probe",
                    "arguments": {},
                    "_meta": plain_modern_meta("cli-pol-sync")
                }
            }),
            Duration::from_secs(10),
        )
        .unwrap();
    let cli_pol_task = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-pol-task",
                "method": "tools/call",
                "params": {
                    "name": "susi_restricted_probe",
                    "arguments": {},
                    "_meta": tasks_meta("cli-pol-task")
                }
            }),
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(cli_pol_sync["result"]["isError"], json!(true));
    assert_eq!(
        cli_pol_task["result"]["resultType"],
        json!("complete"),
        "restricted tool must fail at McpGatewayProxy before spawning a task: {cli_pol_task}"
    );
    assert_eq!(
        cli_pol_sync["result"]["content"][0]["text"],
        cli_pol_task["result"]["content"][0]["text"]
    );
    assert!(McpGatewayProxy::evaluate_tool_policy("susi_restricted_probe").is_err());

    // Spawn durable task over `susi mcp serve` on built-in `ledger_verify` with progressToken.
    let mut cli_task_meta = tasks_meta("cli-ledger-task");
    cli_task_meta["progressToken"] = json!("cli-task-prog-1");
    let cli_created = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-ledger-task",
                "method": "tools/call",
                "params": {
                    "name": "ledger_verify",
                    "arguments": {},
                    "_meta": cli_task_meta
                }
            }),
            Duration::from_secs(30),
        )
        .expect("CLI task-enabled tools/call must reply");
    assert_eq!(cli_created["result"]["resultType"], "task", "{cli_created}");
    let cli_task_id = cli_created["result"]["taskId"]
        .as_str()
        .expect("CreateTaskResult must carry taskId")
        .to_string();

    let expected_ledger = ledger_report(root);
    assert_eq!(expected_ledger["verified"], true);
    assert!(expected_ledger["vectors"].as_u64().unwrap_or(0) >= 14);

    let mut cli_settled = None;
    for poll_idx in 0..20 {
        let pid = format!("cli-get-{poll_idx}");
        let snap = cli_mod
            .request_verbatim(
                &json!({
                    "jsonrpc": "2.0",
                    "id": pid,
                    "method": "tasks/get",
                    "params": {
                        "taskId": cli_task_id,
                        "_meta": tasks_meta(&format!("cli-get-{poll_idx}"))
                    }
                }),
                Duration::from_secs(15),
            )
            .expect("CLI tasks/get must answer");
        assert_eq!(snap["result"]["resultType"], "complete", "{snap}");
        if snap["result"]["status"] == "completed" {
            cli_settled = Some(snap);
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    let cli_settled = cli_settled.expect("CLI ledger_verify task must reach completed");
    assert_eq!(
        call_tool_report(&cli_settled["result"]["result"])["rows"],
        expected_ledger["rows"]
    );
    assert!(
        cli_mod.pending.iter().any(|n| {
            n["method"] == "notifications/progress"
                && n["params"]["progressToken"] == "cli-task-prog-1"
        }),
        "CLI task execution must stream correlated progress before settlement: {:?}",
        cli_mod.pending
    );
    cli_mod.pending.clear();

    // Subscribe to taskIds over `susi mcp serve` -> ack precedes terminal `notifications/tasks` replay;
    // `notifications/cancelled` closes only the subscription, never mutating the durable task.
    cli_mod.send(&json!({
        "jsonrpc": "2.0",
        "id": "cli-task-sub",
        "method": "subscriptions/listen",
        "params": {
            "notifications": {"taskIds": [cli_task_id]},
            "_meta": tasks_meta("cli-task-sub")
        }
    }));
    let sub_deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_cli_sub_ack = false;
    let mut saw_cli_task_note = false;
    while Instant::now() < sub_deadline && !(saw_cli_sub_ack && saw_cli_task_note) {
        if let Ok(msg) = cli_mod.rx.recv_timeout(Duration::from_millis(200)) {
            let m = msg["method"].as_str().unwrap_or("");
            assert!(
                m != "notifications/progress" && m != "notifications/message",
                "task subscription stream leaked forbidden notification: {msg}"
            );
            if m == "notifications/subscriptions/acknowledged"
                && msg["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"]
                    == "cli-task-sub"
            {
                assert!(
                    !saw_cli_task_note,
                    "acknowledged must precede notifications/tasks"
                );
                saw_cli_sub_ack = true;
            } else if m == "notifications/tasks" && msg["params"]["taskId"] == cli_task_id {
                assert_eq!(msg["params"]["status"], "completed", "{msg}");
                saw_cli_task_note = true;
            }
        }
    }
    assert!(saw_cli_sub_ack && saw_cli_task_note);
    cli_mod.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": "cli-task-sub"}
    }));
    drop(cli_mod);

    // Restart `susi mcp serve` against the same home and prove durable task replay + terminal immutability.
    let mut cli_restarted = StdioSession::spawn_raw(root, &cli_home);
    let replayed = cli_restarted
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-replay-get",
                "method": "tasks/get",
                "params": {
                    "taskId": cli_task_id,
                    "_meta": tasks_meta("cli-replay-get")
                }
            }),
            Duration::from_secs(30),
        )
        .expect("restarted susi mcp serve must answer tasks/get");
    assert_eq!(replayed["result"]["status"], "completed", "{replayed}");
    assert_eq!(
        call_tool_report(&replayed["result"]["result"])["rows"],
        expected_ledger["rows"]
    );
    let cancel_after_restart = cli_restarted
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-replay-cancel",
                "method": "tasks/cancel",
                "params": {
                    "taskId": cli_task_id,
                    "_meta": tasks_meta("cli-replay-cancel")
                }
            }),
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(cancel_after_restart["error"]["code"], -32602);
    drop(cli_restarted);

    // Legacy 2025-11-25 `susi mcp serve` rejects retired `tasks/list`, `tasks/result` (-32601), and undeclared `tasks/get` (-32021).
    let mut cli_leg = StdioSession::spawn(root, &cli_home);
    for retired in ["tasks/list", "tasks/result"] {
        let resp = cli_leg
            .request_verbatim(
                &json!({"jsonrpc": "2.0", "id": retired, "method": retired, "params": {}}),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(resp["error"]["code"], -32601, "{retired}: {resp}");
    }
    let leg_get = cli_leg
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "leg-get", "method": "tasks/get", "params": {"taskId": cli_task_id}}),
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(leg_get["error"]["code"], -32021, "{leg_get}");
    drop(cli_leg);

    // ── C. Daemon/Gateway HTTP (`GmcpServer::start_http_server`) + SDKs + `susi mcp call` ──
    let echo_runs = Arc::new(AtomicUsize::new(0));
    let wait_cancelled = Arc::new(AtomicBool::new(false));
    struct IntList;
    impl PlaneHandler for IntList {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let extra = [
                json!({"name": "integ_echo", "description": "Integration echo"}),
                json!({"name": "integ_soft_err", "description": "Tool-level soft error"}),
                json!({"name": "integ_wait", "description": "Cancellable wait"}),
            ];
            let mut merged = if let Ok(Value::Array(tools)) =
                PlaneBus::global().request_local_prefix(topics::TOOLS_LIST, payload)
            {
                tools
            } else {
                Vec::new()
            };
            for tool in extra {
                let name = tool.get("name").and_then(Value::as_str);
                if !merged
                    .iter()
                    .any(|item| item.get("name").and_then(Value::as_str) == name)
                {
                    merged.push(tool);
                }
            }
            Ok(Value::Array(merged))
        }
    }
    struct IntExists;
    impl PlaneHandler for IntExists {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            if matches!(name, "integ_echo" | "integ_soft_err" | "integ_wait") {
                Ok(json!({ "exists": true }))
            } else {
                PlaneBus::global().request_local_prefix(topics::TOOLS_EXISTS, payload)
            }
        }
    }
    struct IntExec {
        echo_runs: Arc<AtomicUsize>,
        wait_cancelled: Arc<AtomicBool>,
    }
    impl PlaneHandler for IntExec {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            if !matches!(name, "integ_echo" | "integ_soft_err" | "integ_wait") {
                return PlaneBus::global().request_local_prefix(topics::TOOLS_EXECUTE, payload);
            }
            let args = payload.get("args").cloned().unwrap_or(Value::Null);
            let echo_runs = self.echo_runs.clone();
            let wait_cancelled = self.wait_cancelled.clone();
            with_watched(&payload, || match name {
                "integ_echo" => {
                    echo_runs.fetch_add(1, Ordering::SeqCst);
                    let msg = args.get("msg").and_then(Value::as_str).unwrap_or("");
                    Ok(json!({ "text": format!("echo:{msg}") }))
                }
                "integ_soft_err" => Err("Tool Execution Error: synthetic tool fault".into()),
                "integ_wait" => {
                    for _ in 0..300 {
                        if susi_core::progress::is_cancelled() {
                            wait_cancelled.store(true, Ordering::Release);
                            return Ok(json!({ "text": "cancelled" }));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(json!({ "text": "done" }))
                }
                other => Err(format!("unexpected tool {other}")),
            })
        }
    }
    struct RestoreIntPlane;
    impl Drop for RestoreIntPlane {
        fn drop(&mut self) {
            let bus = PlaneBus::global();
            bus.unregister(topics::TOOLS_LIST);
            bus.unregister(topics::TOOLS_EXISTS);
            bus.unregister(topics::TOOLS_EXECUTE);
            susi_tools::plane_handler::register();
        }
    }
    let _restore_int_plane = RestoreIntPlane;
    let runtime = adapter_runtime(&home);
    PlaneBus::global().register(topics::TOOLS_LIST, Arc::new(IntList));
    PlaneBus::global().register(topics::TOOLS_EXISTS, Arc::new(IntExists));
    PlaneBus::global().register(
        topics::TOOLS_EXECUTE,
        Arc::new(IntExec {
            echo_runs: echo_runs.clone(),
            wait_cancelled: wait_cancelled.clone(),
        }),
    );

    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let tok_alice = "tok-alice-203-014-03";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_alice}\n")).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let root_http = root.to_path_buf();
    std::thread::spawn(move || {
        GmcpServer::start_http_server(root_http, vec![listener], None, false);
    });
    for _ in 0..80 {
        if TcpStream::connect(&addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let http_send = |method: &str, headers: &[(&str, &str)], body: Option<&str>| -> String {
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        let mut req = format!("{method} /mcp HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(b) = body {
            req.push_str(&format!("Content-Length: {}\r\n\r\n{b}", b.len()));
        } else {
            req.push_str("\r\n");
        }
        s.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        let _ = s.read_to_string(&mut resp);
        resp
    };
    let auth_alice = format!("Bearer {tok_alice}");

    // NetGuard 401 unauthenticated + 403 bad Origin on HTTP `/mcp` tasks/get.
    let task_get_probe = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tasks/get",
        "params": {"taskId": "probe-id", "_meta": tasks_meta("1")}
    })
    .to_string();
    assert!(http_send(
        "POST",
        &[
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
            ("Mcp-Method", "tasks/get"),
            ("Mcp-Name", "probe-id"),
        ],
        Some(&task_get_probe),
    )
    .starts_with("HTTP/1.1 401"));
    assert!(http_send(
        "POST",
        &[
            ("Authorization", &auth_alice),
            ("Origin", "https://evil.example"),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
            ("Mcp-Method", "tasks/get"),
            ("Mcp-Name", "probe-id"),
        ],
        Some(&task_get_probe),
    )
    .starts_with("HTTP/1.1 403"));

    // Missing or mismatched Mcp-Name fails 400 / -32020 across tasks/get, tasks/update, tasks/cancel.
    for (m, extra) in [
        ("tasks/get", json!({"taskId": "probe-id"})),
        (
            "tasks/update",
            json!({"taskId": "probe-id", "inputResponses": {}}),
        ),
        ("tasks/cancel", json!({"taskId": "probe-id"})),
    ] {
        let mut p = extra;
        p["_meta"] = tasks_meta("hdr-chk");
        let body = json!({"jsonrpc": "2.0", "id": "hdr-chk", "method": m, "params": p}).to_string();
        let missing_hdr = http_send(
            "POST",
            &[
                ("Authorization", &auth_alice),
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
                ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
                ("Mcp-Method", m),
            ],
            Some(&body),
        );
        assert!(
            missing_hdr.starts_with("HTTP/1.1 400"),
            "{m}: {missing_hdr}"
        );
        assert!(missing_hdr.contains("-32020"), "{m}: {missing_hdr}");
    }
    let no_id_http = http_send(
        "POST",
        &[
            ("Authorization", &auth_alice),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
            ("Mcp-Method", "tasks/get"),
            ("Mcp-Name", "tid-1"),
        ],
        Some(r#"{"jsonrpc":"2.0","method":"tasks/get","params":{"taskId":"tid-1"}}"#),
    );
    assert!(no_id_http.starts_with("HTTP/1.1 400"), "{no_id_http}");
    assert!(no_id_http.contains("-32600"), "{no_id_http}");

    // Modern SDK (`Session::open_2026_07_28`): policy parity, fixed-shape task-resolving bridge,
    // persisted handle store, cooperative cancel, SSE task subscription, and cross-principal isolation.
    let mut alice =
        Session::open_2026_07_28(&addr, Some(tok_alice), Duration::from_secs(15)).unwrap();
    assert!(alice.supports_server_tasks());

    let http_pol = alice
        .call_tool_with_tasks("susi_restricted_probe", &json!({}), true, &mut |_| {})
        .unwrap();
    match http_pol {
        CallToolOutcome::Complete(v) => {
            assert_eq!(v["isError"], json!(true));
            assert_eq!(
                v["content"][0]["text"], cli_pol_task["result"]["content"][0]["text"],
                "HTTP and CLI must share identical McpGatewayProxy policy denial"
            );
        }
        other => panic!("restricted tool must not spawn task on HTTP: {other:?}"),
    }

    // Fixed-shape synchronous bridge (`call_tool_resolving_tasks`) resolves `CreateTaskResult` transparently
    // and preserves `isError: true` tool errors as `Ok(CallToolResult)`.
    let resolved_ok = alice
        .call_tool_resolving_tasks(
            "integ_echo",
            &json!({"msg": "bridge"}),
            Some(Duration::from_secs(10)),
            |_| Ok(json!({})),
        )
        .unwrap();
    assert_eq!(resolved_ok["content"][0]["text"], "echo:bridge");

    let resolved_soft_err = alice
        .call_tool_resolving_tasks(
            "integ_soft_err",
            &json!({}),
            Some(Duration::from_secs(10)),
            |_| Ok(json!({})),
        )
        .unwrap();
    assert_eq!(resolved_soft_err["isError"], json!(true));

    // Async handle + PersistedTaskHandleStore + HTTP SSE `subscriptions/listen` (`taskIds`).
    let handle_path = home.join("integ-handles.json");
    let mut store = PersistedTaskHandleStore::open(&handle_path).unwrap();
    let echo_handle = match alice
        .call_tool_with_tasks(
            "integ_echo",
            &json!({"msg": "persisted"}),
            true,
            &mut |_| {},
        )
        .unwrap()
    {
        CallToolOutcome::Task(h) => h,
        other => panic!("expected Task handle, got {other:?}"),
    };
    store
        .save(PersistedTaskHandle::from_handle(
            &addr,
            PROTOCOL_VERSION_2026_07_28,
            &echo_handle,
        ))
        .unwrap();
    let done_snap = alice
        .poll_task_until_done(&echo_handle, Some(Duration::from_secs(10)), |_| {
            Ok(json!({}))
        })
        .unwrap();
    assert_eq!(done_snap.handle.status, TaskStatus::Completed);
    assert!(store.update_from_snapshot(&done_snap).unwrap());

    {
        let sse_body = json!({
            "jsonrpc": "2.0",
            "id": "http-task-sub",
            "method": "subscriptions/listen",
            "params": {
                "notifications": {"taskIds": [echo_handle.task_id]},
                "_meta": tasks_meta("http-task-sub")
            }
        })
        .to_string();
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {auth_alice}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: {PROTOCOL_VERSION_2026_07_28}\r\nMcp-Method: subscriptions/listen\r\nContent-Length: {}\r\n\r\n{sse_body}",
            sse_body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut rdr = BufReader::new(s);
        let mut status_line = String::new();
        rdr.read_line(&mut status_line).unwrap();
        assert!(status_line.starts_with("HTTP/1.1 200"), "{status_line}");
        let (mut saw_ack, mut saw_task) = (false, false);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut line = String::new();
        while Instant::now() < deadline && !(saw_ack && saw_task) {
            line.clear();
            if rdr.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if let Some(data) = line.strip_prefix("data:").map(str::trim) {
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if v["method"] == "notifications/subscriptions/acknowledged" {
                        saw_ack = true;
                    } else if v["method"] == "notifications/tasks"
                        && v["params"]["taskId"] == echo_handle.task_id
                    {
                        assert!(saw_ack, "SSE ack must precede notifications/tasks");
                        assert_eq!(v["params"]["status"], "completed");
                        saw_task = true;
                    }
                }
            }
        }
        assert!(
            saw_ack && saw_task,
            "HTTP SSE must stream ack and notifications/tasks"
        );
    }

    // Cooperative `tasks/cancel` on `integ_wait`.
    let wait_h = match alice
        .call_tool_with_tasks("integ_wait", &json!({}), true, &mut |_| {})
        .unwrap()
    {
        CallToolOutcome::Task(h) => h,
        other => panic!("expected Task handle for integ_wait, got {other:?}"),
    };
    alice.cancel_task(&wait_h.task_id).unwrap();
    let mut cancelled_ok = false;
    for _ in 0..20 {
        let s = alice.get_task(&wait_h.task_id).unwrap();
        if s.handle.status == TaskStatus::Cancelled {
            cancelled_ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(cancelled_ok && wait_cancelled.load(Ordering::Acquire));

    // Legacy `2025-11-25` SDKs (`Session::open`, `susi_core::mcp_client`) + CLI `susi mcp call` (network & local)
    // share the same server/catalog while executing synchronously without task regressions.
    let mut leg_wire =
        Session::open(&addr, Some(tok_alice), Duration::from_secs(15)).expect("2025-11-25 open");
    assert!(leg_wire.supports_server_tasks());
    assert!(leg_wire
        .request("tasks/list", &json!({}), &mut |_| {})
        .unwrap_err()
        .to_string()
        .contains("-32601"));
    let leg_echo = leg_wire
        .call_tool(
            "integ_echo",
            &json!({"msg": "legacy-sdk"}),
            None,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(leg_echo["content"][0]["text"], "echo:legacy-sdk");
    leg_wire.close().unwrap();

    let core_echo = susi_core::mcp_client::call_tool(
        &addr,
        "integ_echo",
        &json!({"msg": "core-client"}),
        Some(tok_alice),
    )
    .unwrap();
    assert_eq!(core_echo["content"][0]["text"], "echo:core-client");
    let core_sess = susi_core::mcp_client::session_call(
        &addr,
        "tools/call",
        &json!({"name": "integ_echo", "arguments": {"msg": "session-call"}}),
        Some(tok_alice),
    )
    .unwrap();
    assert_eq!(core_sess["content"][0]["text"], "echo:session-call");

    let cli_net = susi_json(
        root,
        &home,
        &[
            "mcp",
            "call",
            &addr,
            "integ_echo",
            r#"{"msg":"cli-net"}"#,
            "--token",
            tok_alice,
        ],
    );
    assert!(
        cli_net.to_string().contains("echo:cli-net"),
        "susi mcp call <addr> must invoke shared HTTP GmcpServer: {cli_net}"
    );

    let cli_loc = susi_json(root, &home, &["mcp", "call", "susi", "ledger_verify", "{}"]);
    let cli_loc_rep = if cli_loc.get("content").is_some() {
        call_tool_report(&cli_loc)
    } else {
        cli_loc
    };
    assert_eq!(cli_loc_rep["verified"], true);
    assert_eq!(cli_loc_rep["rows"], expected_ledger["rows"]);
    assert_eq!(cli_loc_rep["vectors"], expected_ledger["vectors"]);

    // Cross-principal bearer isolation across `tasks/get`, `tasks/update`, `tasks/cancel`, and `subscriptions/listen`.
    let tok_bob = "tok-bob-203-014-03";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_bob}\n")).unwrap();
    let mut bob = Session::open_2026_07_28(&addr, Some(tok_bob), Duration::from_secs(15)).unwrap();
    assert!(matches!(
        bob.get_task(&echo_handle.task_id),
        Err(WireError::Rpc(-32602, _))
    ));
    assert!(matches!(
        bob.update_task(&echo_handle.task_id, &json!({})),
        Err(WireError::Rpc(-32602, _))
    ));
    assert!(matches!(
        bob.cancel_task(&echo_handle.task_id),
        Err(WireError::Rpc(-32602, _))
    ));
    let bob_sub_err = bob
        .request_with_capabilities(
            "subscriptions/listen",
            &json!({"notifications": {"taskIds": [echo_handle.task_id]}}),
            &json!({"extensions": {TASKS_EXTENSION_ID: {}}}),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(matches!(bob_sub_err, WireError::Rpc(-32602, _)));

    // ── D. Embedded & Unix Socket Adapters + Direct `GmcpService` MRTR & Extension Toggle Isolation ──
    let run_adapter_task_probe = |client: &mut AdapterClient, tag: &str| {
        let disc = client.call(
            &format!("{tag}-disc"),
            "server/discover",
            json!({"_meta": tasks_meta(&format!("{tag}-disc"))}),
        );
        assert!(disc["result"]["capabilities"]["extensions"][TASKS_EXTENSION_ID].is_object());
        let spawned = client.call(
            &format!("{tag}-call"),
            "tools/call",
            json!({
                "name": "integ_echo",
                "arguments": {"msg": tag},
                "_meta": tasks_meta(&format!("{tag}-call"))
            }),
        );
        assert_eq!(spawned["result"]["resultType"], "task", "{spawned}");
        let tid = spawned["result"]["taskId"].as_str().unwrap().to_string();
        let mut done = false;
        for i in 0..20 {
            let gid = format!("{tag}-get-{i}");
            let g = client.call(
                &gid,
                "tasks/get",
                json!({"taskId": tid, "_meta": tasks_meta(&gid)}),
            );
            if g["result"]["status"] == "completed" {
                assert_eq!(
                    g["result"]["result"]["content"][0]["text"],
                    format!("echo:{tag}")
                );
                done = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(done, "{tag}: adapter task must complete");
    };

    let (mut embedded, emb_sess) = embedded_adapter(&runtime, root);
    run_adapter_task_probe(&mut embedded, "emb-014");
    drop(embedded);
    adapter_session_ended(&runtime, emb_sess, "014-int embedded");

    #[cfg(unix)]
    {
        let (mut socket, sock_sess) = socket_adapter(&runtime, root);
        run_adapter_task_probe(&mut socket, "sock-014");
        drop(socket);
        adapter_session_ended(&runtime, sock_sess, "014-int unix socket");
    }

    // Direct durable `GmcpService`: runtime ExtensionRegistry toggle isolation + partial MRTR `tasks/update`.
    runtime.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let direct_store = home.join("direct-durable-tasks");
        let svc = GmcpService::open_durable(root.to_path_buf(), direct_store).unwrap();
        let sibling = GmcpService::new(root.to_path_buf());
        let mrtr_tool: Tool =
            serde_json::from_value(json!({"name": "integ_partial_mrtr", "inputSchema": {"type": "object"}}))
                .unwrap();
        svc.register_task_tool(
            mrtr_tool,
            Arc::new(|_, ctx| {
                Box::pin(async move {
                    let r1: InputRequest =
                        serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                    let r2: InputRequest = serde_json::from_value(json!({
                        "method": "elicitation/create",
                        "params": {"mode": "form", "message": "Approve", "requestedSchema": {"type": "object"}}
                    }))
                    .unwrap();
                    let (a1, a2) = tokio::try_join!(
                        ctx.request_input("k1", r1),
                        ctx.request_input("k2", r2),
                    )?;
                    Ok(CallToolResult {
                        content: vec![ContentBlock::text("partial-mrtr-ok")],
                        structured_content: Some(json!({"k1": a1, "k2": a2})),
                        is_error: Some(false),
                        meta: None,
                        result_type: None,
                    })
                })
            }),
        )
        .unwrap();

        let (c_half, s_half) = tokio::io::duplex(65536);
        let svc_run = svc.clone();
        tokio::spawn(async move {
            let running = svc_run.serve(s_half).await.unwrap();
            let _ = running.waiting().await;
        });
        let mut wire = BufReader::new(c_half);
        async fn send_w(w: &mut BufReader<tokio::io::DuplexStream>, msg: Value) {
            w.get_mut()
                .write_all(format!("{msg}\n").as_bytes())
                .await
                .unwrap();
        }
        async fn recv_w(w: &mut BufReader<tokio::io::DuplexStream>) -> Value {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(5), w.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        }

        // Disable io.modelcontextprotocol/tasks on `svc`; sibling stays enabled.
        assert!(svc.extensions().write().set_enabled(TASKS_EXTENSION_ID, false));
        assert!(
            serde_json::to_value(sibling.discover_result()).unwrap()["capabilities"]["extensions"]
                [TASKS_EXTENSION_ID]
                .is_object()
        );
        send_w(
            &mut wire,
            json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": tasks_meta("1")}}),
        )
        .await;
        let d_off = recv_w(&mut wire).await;
        assert!(d_off["result"]["capabilities"]["extensions"]
            .get(TASKS_EXTENSION_ID)
            .is_none());

        // Built-in tool executes synchronously (`resultType: "complete"`) while tasks extension is disabled;
        // task-only tool and `tasks/get` fail closed with `-32601`.
        send_w(
            &mut wire,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "integ_echo", "arguments": {"msg": "disabled-sync"}, "_meta": tasks_meta("2")}
            }),
        )
        .await;
        let sync_when_off = recv_w(&mut wire).await;
        assert_eq!(sync_when_off["result"]["resultType"], "complete");
        assert_eq!(
            sync_when_off["result"]["content"][0]["text"],
            "echo:disabled-sync"
        );

        for (id, m, p) in [
            (
                3,
                "tools/call",
                json!({"name": "integ_partial_mrtr", "arguments": {}, "_meta": tasks_meta("3")}),
            ),
            (
                4,
                "tasks/get",
                json!({"taskId": "none", "_meta": tasks_meta("4")}),
            ),
            (
                5,
                "subscriptions/listen",
                json!({"notifications": {"taskIds": ["none"]}, "_meta": tasks_meta("5")}),
            ),
        ] {
            send_w(&mut wire, json!({"jsonrpc": "2.0", "id": id, "method": m, "params": p})).await;
            assert_eq!(recv_w(&mut wire).await["error"]["code"], -32601);
        }

        // Re-enable io.modelcontextprotocol/tasks and verify concurrent 2-key partial MRTR (`k1` then `k2` + stale/unknown ignored).
        assert!(svc.extensions().write().set_enabled(TASKS_EXTENSION_ID, true));
        send_w(
            &mut wire,
            json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": {"name": "integ_partial_mrtr", "arguments": {}, "_meta": tasks_meta("6")}
            }),
        )
        .await;
        let spawned = recv_w(&mut wire).await;
        assert_eq!(spawned["result"]["resultType"], "task");
        let tid = spawned["result"]["taskId"].as_str().unwrap().to_string();

        let mut both_pending = false;
        for i in 0..10 {
            tokio::time::sleep(Duration::from_millis(15)).await;
            let id = 20 + i;
            send_w(
                &mut wire,
                json!({"jsonrpc": "2.0", "id": id, "method": "tasks/get", "params": {"taskId": tid, "_meta": tasks_meta(&id.to_string())}}),
            )
            .await;
            let snap = recv_w(&mut wire).await;
            if snap["result"]["inputRequests"].get("k1").is_some()
                && snap["result"]["inputRequests"].get("k2").is_some()
            {
                both_pending = true;
                break;
            }
        }
        assert!(both_pending, "both k1 and k2 must appear in inputRequests");

        // Partial update (`k1` + unknown `"stale"`): task stays `input_required` with only `k2` remaining.
        send_w(
            &mut wire,
            json!({
                "jsonrpc": "2.0",
                "id": 60,
                "method": "tasks/update",
                "params": {
                    "taskId": tid,
                    "inputResponses": {"k1": {"roots": []}, "stale": {"ignored": true}},
                    "_meta": tasks_meta("60")
                }
            }),
        )
        .await;
        let ack1 = recv_w(&mut wire).await;
        assert_eq!(ack1["result"]["resultType"], "complete");

        send_w(
            &mut wire,
            json!({"jsonrpc": "2.0", "id": 61, "method": "tasks/get", "params": {"taskId": tid, "_meta": tasks_meta("61")}}),
        )
        .await;
        let mid = recv_w(&mut wire).await;
        assert_eq!(mid["result"]["status"], "input_required", "{mid}");
        assert!(mid["result"]["inputRequests"].get("k1").is_none());
        assert!(mid["result"]["inputRequests"].get("k2").is_some());

        // Complete `k2` (+ replay already-answered `k1` which must be ignored cleanly).
        send_w(
            &mut wire,
            json!({
                "jsonrpc": "2.0",
                "id": 62,
                "method": "tasks/update",
                "params": {
                    "taskId": tid,
                    "inputResponses": {"k1": {"roots": ["replayed"]}, "k2": {"action": "accept"}},
                    "_meta": tasks_meta("62")
                }
            }),
        )
        .await;
        assert_eq!(recv_w(&mut wire).await["result"]["resultType"], "complete");

        let mut settled = false;
        for i in 0..10 {
            tokio::time::sleep(Duration::from_millis(15)).await;
            let id = 70 + i;
            send_w(
                &mut wire,
                json!({"jsonrpc": "2.0", "id": id, "method": "tasks/get", "params": {"taskId": tid, "_meta": tasks_meta(&id.to_string())}}),
            )
            .await;
            let fin = recv_w(&mut wire).await;
            if fin["result"]["status"] == "completed" {
                assert_eq!(
                    fin["result"]["result"]["structuredContent"],
                    json!({"k1": {"roots": []}, "k2": {"action": "accept"}}),
                    "replayed k1 must be ignored and original k1 answer preserved"
                );
                settled = true;
                break;
            }
        }
        assert!(settled, "integ_partial_mrtr must settle to completed");
    });
}

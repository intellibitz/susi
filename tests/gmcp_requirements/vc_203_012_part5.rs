/// GMCP-203-012-06: operations and evidence for subscriptions, notifications,
/// progress, and operational messages.
/// Verifies inspectable CLI/tool ledger & status state, live multi-transport API
/// discovery and subscription acknowledgement metadata (`embedded_adapter`,
/// `#[cfg(unix)]` socket, `susi mcp serve` stdio, and Streamable HTTP `/mcp`
/// with `traceparent`, `logLevel` redaction & custom header error codes),
/// configuration validation and zero-limit fail-closed diagnostics, bounded
/// protocol ceiling invariants, deterministic drift detection across all 6
/// pinned `VC-203-012` specifications (`hash_drift` + `unfetchable`), operator
/// runbook/migration/rollback/non-claims completeness, and retained evidence
/// currency. Note: `.agents/gmcp/evidence/VC-203-012.jsonl` is intentionally
/// NOT created in `T-CODEXGMCPROADMAP-98` (it is created alongside
/// `gmcp_vc_203_012_mastery` in `T-CODEXGMCPROADMAP-99`).
#[test]
fn gmcp_req_203_012_06_operations() {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};
    use susi_gmcp::protocol::{
        MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION, MAX_LOG_NOTIFICATIONS_PER_REQUEST,
        MAX_NOTIFICATION_PAYLOAD_BYTES, MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER,
    };
    use susi_gmcp::susi_config::validate::{validate_dir, ConfigIssueKind};

    let (_lock, _env, home) = gmcp_http_home_guard("012-ops");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    let op_home = scratch.join("home");
    std::fs::create_dir_all(&op_home).unwrap();
    let modern_meta = |req_id: &str| -> Value {
        let mut meta = adapter_modern_meta()["_meta"].clone();
        meta["io.modelcontextprotocol/requestId"] = json!(req_id);
        meta
    };

    // ── 1. Inspectable CLI & tool ledger state + `susi mcp status` ────────
    let report = ledger_report(root);
    assert_eq!(report["verified"], true);
    for key in [
        "rows",
        "documents",
        "revisions",
        "dispositions",
        "categories",
        "vectors",
    ] {
        assert!(!report[key].is_null(), "ledger report must expose {key}");
    }
    assert!(
        report["vectors"].as_u64().unwrap_or(0) >= 1 && report["rows"].as_u64().unwrap_or(0) >= 80,
        "ledger must report verified vector/row counts: {report}"
    );
    assert!(report["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0);
    assert!(report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0);
    assert_eq!(
        ledger_tool_report(root),
        report,
        "direct ledger_verify tool and ledger_report must match"
    );

    let cli_ledger = susi_json(root, &op_home, &["mcp", "ledger"]);
    assert_eq!(
        cli_ledger, report,
        "CLI `susi mcp ledger` must match ledger_report"
    );

    let status_out = susi_command(root, &op_home, &["mcp", "status"])
        .output()
        .expect("susi mcp status must execute");
    assert!(
        status_out.status.success(),
        "susi mcp status failed: {}",
        String::from_utf8_lossy(&status_out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&status_out.stdout)
            .trim()
            .is_empty(),
        "susi mcp status must emit inspectable state"
    );

    // ── 2. Inspectable live API state across served transports ────────────
    let assert_discover_shape = |discover: &Value, label: &str| {
        assert_eq!(
            discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "susi-gmcp",
            "{label}: serverInfo.name must be susi-gmcp: {discover}"
        );
        assert!(
            discover["ttlMs"].is_number(),
            "{label}: server/discover must expose ttlMs: {discover}"
        );
        assert_eq!(
            discover["capabilities"]["logging"],
            json!({}),
            "{label}: capabilities.logging must be advertised"
        );
        assert_eq!(
            discover["capabilities"]["tools"]["listChanged"],
            json!(true),
            "{label}: tools.listChanged must be true"
        );
        assert_eq!(
            discover["capabilities"]["prompts"]["listChanged"],
            json!(true),
            "{label}: prompts.listChanged must be true"
        );
        assert_eq!(
            discover["capabilities"]["resources"],
            json!({"subscribe": true, "listChanged": true}),
            "{label}: resources subscribe & listChanged must be true"
        );
    };

    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let token = "susi-012-ops-bearer-secret";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();

    let runtime = adapter_runtime(&home);
    let (mut embedded, embedded_session) = embedded_adapter(&runtime, root);
    let emb_disc = embedded.call("ops-emb-disc", "server/discover", adapter_modern_meta());
    assert!(
        emb_disc.get("error").is_none_or(Value::is_null),
        "{emb_disc}"
    );
    let embedded_discover = emb_disc["result"].clone();
    assert_discover_shape(&embedded_discover, "embedded");

    embedded.send(&json!({ "jsonrpc": "2.0", "id": "ops-emb-sub", "method": "subscriptions/listen", "params": { "notifications": { "toolsListChanged": true, "promptsListChanged": true, "resourcesListChanged": true, "resourceSubscriptions": ["susi://tools"]}, "_meta": modern_meta("ops-emb-sub")}}));
    let emb_ack = embedded.take("embedded subscription ack", |msg| {
        msg["method"] == "notifications/subscriptions/acknowledged"
            && msg["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "ops-emb-sub"
    });
    assert_eq!(emb_ack["params"]["notifications"]["toolsListChanged"], true);
    embedded.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "ops-emb-sub", "reason": "ops cleanup"}}));
    let emb_tool = embedded.call(
        "ops-emb-verify",
        "tools/call",
        json!({ "name": "ledger_verify", "arguments": {}, "_meta": modern_meta("ops-emb-verify")}),
    );
    assert_eq!(call_tool_report(&emb_tool["result"]), report);
    drop(embedded);
    adapter_session_ended(&runtime, embedded_session, "in-process");

    #[cfg(unix)]
    {
        let (mut socket, socket_session) = socket_adapter(&runtime, root);
        let sock_disc = socket.call("ops-sock-disc", "server/discover", adapter_modern_meta());
        assert_eq!(
            sock_disc["result"], embedded_discover,
            "Unix socket and embedded server/discover must match"
        );
        // A served session ends as `Closed` only once it was negotiated, as
        // on the in-process adapter above; a client that closes before
        // `initialize` ends it as a failed negotiation.
        adapter_handshake(&mut socket, "ops-socket");
        drop(socket);
        adapter_session_ended(&runtime, socket_session, "unix socket");
    }

    let mut served = StdioSession::spawn_raw(root, &op_home);
    let served_disc = served
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "ops-stdio-disc", "method": "server/discover", "params": adapter_modern_meta()}),
            Duration::from_secs(120),
        )
        .expect("served stdio server/discover must answer");
    assert_eq!(
        served_disc["result"], embedded_discover,
        "served stdio and embedded server/discover must match"
    );

    // Modern stdio: omitted logLevel suppresses notifications/message, recognized
    // invalid logLevel (-32602), and legacy logging/setLevel fails (-32601).
    let quiet_call = served
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "ops-stdio-quiet", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": modern_meta("ops-stdio-quiet")}}),
            Duration::from_secs(30),
        )
        .expect("quiet ledger_verify must answer");
    assert_eq!(call_tool_report(&quiet_call["result"]), report);
    assert!(
        served
            .pending
            .iter()
            .all(|n| n["method"] != "notifications/message"),
        "omitted logLevel must suppress notifications/message: {:?}",
        served.pending
    );

    let mut bad_log_meta = modern_meta("ops-stdio-badlog");
    bad_log_meta["io.modelcontextprotocol/logLevel"] = json!("verbose");
    let bad_log_resp = served
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "ops-stdio-badlog", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": bad_log_meta}}),
            Duration::from_secs(15),
        )
        .expect("bad logLevel must reply");
    assert_eq!(bad_log_resp["error"]["code"], -32602, "{bad_log_resp}");

    let leg_set_level = served
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "ops-stdio-setlevel", "method": "logging/setLevel", "params": {"level": "info", "_meta": modern_meta("ops-stdio-setlevel")}}),
            Duration::from_secs(15),
        )
        .expect("legacy logging/setLevel on modern session must reply");
    assert_eq!(leg_set_level["error"]["code"], -32601, "{leg_set_level}");

    served.send(&json!({ "jsonrpc": "2.0", "id": "ops-stdio-sub", "method": "subscriptions/listen", "params": { "notifications": {"resourceSubscriptions": ["susi://tools"]}, "_meta": modern_meta("ops-stdio-sub")}}));
    let sub_deadline = Instant::now() + Duration::from_secs(15);
    let mut stdio_ack_seen = false;
    while Instant::now() < sub_deadline && !stdio_ack_seen {
        if let Ok(msg) = served.rx.recv_timeout(Duration::from_millis(200)) {
            if msg["method"] == "notifications/subscriptions/acknowledged"
                && msg["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"]
                    == "ops-stdio-sub"
            {
                stdio_ack_seen = true;
            }
        }
    }
    assert!(stdio_ack_seen, "stdio subscriptions/listen must emit ack");
    served.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "ops-stdio-sub", "reason": "ops done"}}));
    drop(served);

    // Streamable HTTP `/mcp`: W3C traceparent, Bearer auth, server/discover parity,
    // `io.modelcontextprotocol/logLevel: "info"` SSE notifications/message + secret redaction,
    // `-32020` HEADER_MISMATCH, and `-32022` UNSUPPORTED_PROTOCOL_VERSION.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback HTTP port");
    let port = listener.local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let http_root = root.to_path_buf();
    let _http_srv = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(http_root, vec![listener], None, false);
    });
    for _ in 0..40 {
        if TcpStream::connect(&addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let auth_header = format!("Bearer {token}");
    let http_post = |extra_headers: &[(&str, &str)],
                     body_val: &Value|
     -> (String, String, Vec<Value>) {
        let body = body_val.to_string();
        let mut req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAuthorization: {auth_header}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n"
        );
        for (k, v) in extra_headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        let mut stream = TcpStream::connect(&addr).expect("connect HTTP /mcp");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        let _ = stream.read_to_string(&mut raw);
        let (head, resp_body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status_line = head.lines().next().unwrap_or_default().to_string();
        let mut frames = Vec::new();
        let trimmed = resp_body.trim();
        if trimmed.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                frames.push(v);
            }
        } else {
            for line in resp_body.lines() {
                if let Some(data) = line.strip_prefix("data:").map(str::trim) {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        frames.push(v);
                    }
                }
            }
        }
        (status_line, raw, frames)
    };

    let (http_disc_st, http_disc_raw, http_disc_frames) = http_post(
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "server/discover"),
            (
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            ),
        ],
        &json!({ "jsonrpc": "2.0", "id": "ops-http-disc", "method": "server/discover", "params": {"_meta": modern_meta("ops-http-disc")}}),
    );
    assert!(
        http_disc_st.starts_with("HTTP/1.1 200"),
        "HTTP server/discover failed: {http_disc_st} {http_disc_raw}"
    );
    assert_eq!(
        http_disc_frames
            .iter()
            .find(|f| f["id"] == "ops-http-disc")
            .expect("HTTP server/discover result frame")["result"],
        embedded_discover
    );

    let mut http_info_meta = modern_meta("ops-http-verify");
    http_info_meta["io.modelcontextprotocol/logLevel"] = json!("info");
    http_info_meta["progressToken"] = json!("ops-http-prog");
    let (http_call_st, http_call_raw, http_call_frames) = http_post(
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
            (
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            ),
        ],
        &json!({ "jsonrpc": "2.0", "id": "ops-http-verify", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": http_info_meta}}),
    );
    assert!(
        http_call_st.starts_with("HTTP/1.1 200"),
        "HTTP tools/call failed: {http_call_st} {http_call_raw}"
    );
    let http_result_frame = http_call_frames
        .iter()
        .find(|f| f["id"] == "ops-http-verify")
        .expect("HTTP tools/call result frame");
    assert_eq!(call_tool_report(&http_result_frame["result"]), report);
    assert!(
        http_call_frames
            .iter()
            .any(|f| f["method"] == "notifications/message"),
        "HTTP SSE must carry notifications/message at logLevel=info: {http_call_raw}"
    );
    assert!(
        !http_call_raw.contains(token),
        "HTTP SSE operational notifications must never expose bearer token"
    );

    let (hdr_mismatch_st, hdr_mismatch_raw, hdr_mismatch_frames) = http_post(
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/list"),
        ],
        &json!({ "jsonrpc": "2.0", "id": "ops-http-mismatch", "method": "server/discover", "params": {"_meta": modern_meta("ops-http-mismatch")}}),
    );
    assert!(
        hdr_mismatch_st.starts_with("HTTP/1.1 400"),
        "Mcp-Method mismatch must return HTTP 400: {hdr_mismatch_st} {hdr_mismatch_raw}"
    );
    assert_eq!(hdr_mismatch_frames[0]["error"]["code"], -32020);

    let mut bad_ver_meta = adapter_modern_meta()["_meta"].clone();
    bad_ver_meta["io.modelcontextprotocol/protocolVersion"] = json!("2024-01-01");
    let (bad_ver_st, bad_ver_raw, bad_ver_frames) = http_post(
        &[
            ("MCP-Protocol-Version", "2024-01-01"),
            ("Mcp-Method", "server/discover"),
        ],
        &json!({ "jsonrpc": "2.0", "id": "ops-http-badver", "method": "server/discover", "params": {"_meta": bad_ver_meta}}),
    );
    assert!(
        bad_ver_st.starts_with("HTTP/1.1 400"),
        "Unsupported protocol version must return HTTP 400: {bad_ver_st} {bad_ver_raw}"
    );
    assert_eq!(bad_ver_frames[0]["error"]["code"], -32022);

    // ── 3. Configuration validation & bounded metrics/ceiling constants ───
    assert_eq!(MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION, 16);
    assert_eq!(MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER, 256);
    assert_eq!(MAX_LOG_NOTIFICATIONS_PER_REQUEST, 64);
    assert_eq!(MAX_NOTIFICATION_PAYLOAD_BYTES, 16 * 1024);

    let invalid_cli = susi_command(
        root,
        &op_home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must execute");
    assert!(!invalid_cli.status.success());

    let cfg_home = scratch.join("cfg-valid");
    let cfg_susi = cfg_home.join(".susi");
    std::fs::create_dir_all(&cfg_susi).unwrap();
    std::fs::write(
        cfg_susi.join("config.json"),
        r#"{"max_rpc_body_bytes": 4096}"#,
    )
    .unwrap();
    let valid_cfg = validate_dir(&cfg_susi).expect("valid config must validate");
    assert!(
        !valid_cfg.has_blocking(),
        "valid config must have no blocking schema issues: {:?}",
        valid_cfg.issues
    );

    let bad_home = scratch.join("cfg-bad");
    let bad_susi = bad_home.join(".susi");
    std::fs::create_dir_all(&bad_susi).unwrap();
    std::fs::write(
        bad_susi.join("config.json"),
        r#"{"max_rpc_body_bytes": "bad"}"#,
    )
    .unwrap();
    let bad_cfg = validate_dir(&bad_susi).expect("type mismatch must validate");
    assert!(bad_cfg.has_blocking());
    assert!(bad_cfg.issues.iter().any(|issue| {
        issue.path == "max_rpc_body_bytes" && issue.kind == ConfigIssueKind::TypeMismatch
    }));

    let zero_home = scratch.join("cfg-zero");
    let zero_susi = zero_home.join(".susi");
    std::fs::create_dir_all(&zero_susi).unwrap();
    std::fs::write(
        zero_susi.join("config.json"),
        r#"{"max_rpc_body_bytes": 0}"#,
    )
    .unwrap();
    let mut zero_child = susi_command(root, &zero_home, &["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("susi mcp serve must spawn");
    drop(zero_child.stdin.take());
    let zero_out = zero_child.wait_with_output().unwrap();
    let zero_stderr = String::from_utf8_lossy(&zero_out.stderr);
    assert!(zero_out.stdout.is_empty());
    assert!(
        zero_stderr.contains("max_rpc_body_bytes") && zero_stderr.contains("must be > 0"),
        "expected bounded zero-limit diagnostic on stderr, got: {zero_stderr}"
    );

    // ── 4. Pinned VC-203-012 specs & deterministic drift detection ────────
    let sources: Value = serde_json::from_slice(
        &std::fs::read(root.join(".agents/gmcp/sources.json")).expect("sources.json readable"),
    )
    .expect("sources.json valid JSON");
    let pinned_specs = [
        "docs/specification/2025-11-25/basic/utilities/progress.mdx",
        "docs/specification/2025-11-25/server/utilities/logging.mdx",
        "docs/specification/2026-07-28/basic/patterns/index.mdx",
        "docs/specification/2026-07-28/basic/patterns/progress.mdx",
        "docs/specification/2026-07-28/basic/patterns/subscriptions.mdx",
        "docs/specification/2026-07-28/server/utilities/logging.mdx",
    ];
    for doc in &pinned_specs {
        let entry = sources["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["document"] == *doc)
            .unwrap_or_else(|| panic!("missing pinned source entry for {doc}"));
        let disk_sha = susi_paths::hex::encode(Sha256::digest(
            std::fs::read(root.join(".agents/gmcp/spec").join(doc))
                .unwrap_or_else(|e| panic!("missing vendored spec {doc}: {e}")),
        ));
        assert_eq!(
            entry["sha256"].as_str(),
            Some(disk_sha.as_str()),
            "vendored spec hash mismatch for {doc}"
        );
    }

    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let refetched_str = refetched.to_str().unwrap();

    let clean_a = susi_json(
        root,
        &op_home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_str],
    );
    let clean_b = susi_json(
        root,
        &op_home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_str],
    );
    assert_eq!(clean_a, clean_b, "drift checks must be deterministic");
    assert_eq!(clean_a["verified"], true);
    assert!(clean_a["findings"].as_array().unwrap().is_empty());

    for tampered_doc in &pinned_specs {
        let copy = scratch.join("refetched-tamper");
        let _ = std::fs::remove_dir_all(&copy);
        copy_tree(&root.join(".agents/gmcp/spec"), &copy);
        let victim = copy.join(tampered_doc);
        let mut bytes = std::fs::read(&victim).unwrap();
        bytes.push(b'\n');
        std::fs::write(&victim, bytes).unwrap();
        let out = susi_command(
            root,
            &op_home,
            &[
                "mcp",
                "ledger",
                "--drift",
                "--refetched",
                copy.to_str().unwrap(),
            ],
        )
        .output()
        .unwrap();
        assert!(!out.status.success(), "{tampered_doc}: drift must fail");
        let drift_report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(drift_report["verified"], false);
        assert!(
            drift_report["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["document"] == *tampered_doc && f["kind"] == "hash_drift"),
            "expected hash_drift for {tampered_doc}: {drift_report}"
        );
        let _ = std::fs::remove_dir_all(&copy);
    }

    let missing_doc = "docs/specification/2026-07-28/basic/patterns/subscriptions.mdx";
    std::fs::remove_file(refetched.join(missing_doc)).unwrap();
    let missing_out = susi_command(
        root,
        &op_home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_str],
    )
    .output()
    .unwrap();
    assert!(!missing_out.status.success());
    let missing_report: Value = serde_json::from_slice(&missing_out.stdout).unwrap();
    assert_eq!(missing_report["verified"], false);
    assert!(
        missing_report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["document"] == missing_doc && f["kind"] == "unfetchable"),
        "expected unfetchable for {missing_doc}: {missing_report}"
    );

    // ── 5. Operator runbook, migration, rollback & ledger/evidence checks ─
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-012.md"))
        .expect("docs/gmcp/contract-203-012.md readable");
    for required in [
        "## Operations and non-claims",
        "Operations and incident runbook",
        "Inspectable state",
        "Configuration validation",
        "Bounded metrics, logs and traces",
        "Diagnostics and incident triage",
        "Examples",
        "Incident triage",
        "Migration (SEP-2577",
        "Rollback",
        "Production guarantees and non-claims",
        "-32602",
        "-32601",
        "-32600",
        "-32603",
        "-32020",
        "-32021",
        "-32022",
        "hash_drift",
        "unfetchable",
        "MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION = 16",
        "MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER = 256",
        "MAX_LOG_NOTIFICATIONS_PER_REQUEST = 64",
        "MAX_NOTIFICATION_PAYLOAD_BYTES = 16384",
        "2025-11-25/basic/utilities/progress.mdx",
        "2025-11-25/server/utilities/logging.mdx",
        "2026-07-28/basic/patterns/index.mdx",
        "2026-07-28/basic/patterns/progress.mdx",
        "2026-07-28/basic/patterns/subscriptions.mdx",
        "2026-07-28/server/utilities/logging.mdx",
        "35d27ca9ad8ee9e029ab84afc751de208f7732c97c7a5d792abbcc237ab72528",
        "37cfde22e75d2444c9d796c2df636b96c1c9d486e64b109e38169f2d7f2cf82a",
        "9e08dd8b295cacc59bd4e233e9e1d6bfcc149695c265d95a18e2fddc9ad3968d",
        "9f55aca08920c633f3a2a35e0c77533db08d3fd19ced43918ea7c57aa5f85409",
        "8333cbc3280cad293e96b4a20c3200face8e185ebf58f1c4a51c35bb96654abd",
        "4e77ec3257e07ad69bfd6ed902771f14d6498b60fe6c9c325b8ff4bfc35647a6",
    ] {
        assert!(
            contract.contains(required),
            "contract-203-012.md missing runbook section/token `{required}`"
        );
    }

    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap();
    let ledger_sha = susi_paths::hex::encode(Sha256::digest(&ledger_bytes));
    let ledger_json: Value = serde_json::from_slice(&ledger_bytes).unwrap();
    let vc_rows: Vec<&Value> = ledger_json["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-012")
        .collect();
    assert_eq!(vc_rows.len(), 57);
    for row in vc_rows {
        assert!(contract.contains(row["id"].as_str().unwrap()));
    }

    let evidence_dir = root.join(".agents/gmcp/evidence");
    assert!(evidence_dir.is_dir(), ".agents/gmcp/evidence must exist");
    let evidence_files: Vec<PathBuf> = std::fs::read_dir(&evidence_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    assert!(!evidence_files.is_empty());
    for file in evidence_files {
        let records = newest_records(&std::fs::read_to_string(&file).unwrap());
        assert!(
            records
                .iter()
                .all(|rec| rec["exit"] == 0 && rec["artifact_sha256"] == ledger_sha),
            "{}: retained records must pass and match current ledger sha {ledger_sha}",
            file.display()
        );
    }

    let _ = _http_srv;
    let _ = std::fs::remove_dir_all(&home);
}

/// VC-203-012 mastery: subscriptions, notifications, progress, and operational messages.
/// Verifies production-path CLI/tool ledger parity for all 57 VC-203-012 rows,
/// multi-transport runtime parity (`embedded_adapter`, `#[cfg(unix)]` socket,
/// shipped `susi mcp serve` stdio, and Streamable HTTP `/mcp`), modern
/// `subscriptions/listen` filter intersection and acknowledgement metadata,
/// dynamic `notifications/tools/list_changed` routing and cooperative cancellation,
/// correlated progress and RFC 5424 `io.modelcontextprotocol/logLevel` redaction,
/// legacy `2025-11-25` vs modern `2026-07-28` era enforcement, bounded protocol
/// invariants, all 6 pinned spec SHA-256 bindings, explicit contract non-claims,
/// signed committed evidence across `GMCP-203-012-01..06`, roadmap/verdict
/// delivery state, and clean committed artifacts.
#[test]
fn gmcp_vc_203_012_mastery() {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeSet;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};
    use susi_gmcp::mcp::model::{CallToolResult, ContentBlock, Tool};
    use susi_gmcp::mcp::ServiceExt;
    use susi_gmcp::protocol::{
        GmcpService, MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION, MAX_LOG_NOTIFICATIONS_PER_REQUEST,
        MAX_NOTIFICATION_PAYLOAD_BYTES, MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER,
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // ── 1. Production path CLI/tool parity and 57 VC-203-012 bindings ───
    let cli = ledger_report(root);
    assert_eq!(cli["verified"], true, "production path must verify");
    assert!(
        cli["rows"].as_u64().unwrap_or(0) >= 57,
        "shipped ledger must hold VC-203-012 rows"
    );
    assert_eq!(
        ledger_tool_report(root),
        cli,
        "ledger_verify tool and CLI must emit identical canonical reports"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-012.md"))
        .expect("contract-203-012.md must exist");
    let ledger_bytes =
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).expect("ledger readable");
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(&ledger_bytes));
    let ledger: Value = serde_json::from_slice(&ledger_bytes).expect("canonical ledger must parse");
    let vc_rows: Vec<_> = ledger["requirements"]
        .as_array()
        .expect("ledger requirements must be an array")
        .iter()
        .filter(|row| row["trace"]["vector"] == "VC-203-012")
        .collect();
    assert_eq!(
        vc_rows.len(),
        57,
        "VC-203-012 must bind exactly 57 normative requirements"
    );
    for row in vc_rows {
        let id = row["id"].as_str().unwrap_or("?");
        for field in [
            "vector",
            "owner",
            "production_path",
            "positive_test",
            "negative_test",
            "evidence",
        ] {
            assert!(
                !row["trace"][field].as_str().unwrap_or_default().is_empty(),
                "{id}: trace.{field} must be non-empty"
            );
        }
        let production = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        assert_eq!(
            row["trace"]["positive_test"],
            "tests/gmcp_requirements.rs::gmcp_req_203_012_03_integration"
        );
        assert_eq!(
            row["trace"]["negative_test"],
            "tests/gmcp_requirements.rs::gmcp_req_203_012_04_adversarial"
        );
        assert_eq!(
            row["trace"]["evidence"],
            ".agents/gmcp/evidence/VC-203-012.jsonl"
        );
        assert!(
            matches!(
                row["disposition"]["status"].as_str(),
                Some("adapter" | "modern" | "optional")
            ),
            "{id}: unexpected disposition {:?}",
            row["disposition"]
        );
        assert!(
            contract.contains(id),
            "contract-203-012.md missing normative binding {id}"
        );
    }

    // ── 2. Multi-transport subscriptions, progress, logging & limits ─────
    assert_eq!(MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION, 16);
    assert_eq!(MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER, 256);
    assert_eq!(MAX_LOG_NOTIFICATIONS_PER_REQUEST, 64);
    assert_eq!(MAX_NOTIFICATION_PAYLOAD_BYTES, 16 * 1024);

    let (_lock, _env, home) = gmcp_http_home_guard("vc-203-012-mastery");
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let token = "susi-012-mastery-bearer-secret";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();

    let modern_meta = |req_id: &str| -> Value {
        let mut meta = adapter_modern_meta()["_meta"].clone();
        meta["io.modelcontextprotocol/requestId"] = json!(req_id);
        meta
    };

    let runtime = adapter_runtime(&home);
    let (mut embedded, embedded_session) = embedded_adapter(&runtime, root);
    let emb_disc = embedded.call("m-emb-disc", "server/discover", adapter_modern_meta());
    assert_eq!(
        emb_disc["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "susi-gmcp"
    );
    assert_eq!(emb_disc["result"]["capabilities"]["logging"], json!({}));
    assert_eq!(
        emb_disc["result"]["capabilities"]["tools"]["listChanged"],
        json!(true)
    );
    assert_eq!(
        emb_disc["result"]["capabilities"]["prompts"]["listChanged"],
        json!(true)
    );
    assert_eq!(
        emb_disc["result"]["capabilities"]["resources"],
        json!({"subscribe": true, "listChanged": true})
    );
    let emb_tool = embedded.call(
        "m-emb-verify",
        "tools/call",
        json!({ "name": "ledger_verify", "arguments": {}, "_meta": modern_meta("m-emb-verify")}),
    );
    assert_eq!(call_tool_report(&emb_tool["result"]), cli);
    drop(embedded);
    adapter_session_ended(&runtime, embedded_session, "mastery embedded");

    // Legacy 2025-11-25 adapter session parity: supports initialize + logging/setLevel +
    // resources/subscribe while rejecting modern subscriptions/listen (-32601) and modern _meta (-32600).
    let (mut legacy_emb, legacy_session) = embedded_adapter(&runtime, root);
    adapter_handshake(&mut legacy_emb, "mastery-012-legacy");
    let set_lvl = legacy_emb.call("m-leg-log", "logging/setLevel", json!({"level": "notice"}));
    assert_eq!(set_lvl["result"], json!({}));
    let leg_listen = legacy_emb.call(
        "m-leg-sub",
        "subscriptions/listen",
        json!({"notifications": {"toolsListChanged": true}}),
    );
    assert_eq!(leg_listen["error"]["code"], -32601);
    let leg_inj = legacy_emb.call(
        "m-leg-inj",
        "ping",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
    );
    assert_eq!(leg_inj["error"]["code"], -32600);
    drop(legacy_emb);
    adapter_session_ended(&runtime, legacy_session, "mastery legacy");

    #[cfg(unix)]
    {
        let (mut socket, socket_session) = socket_adapter(&runtime, root);
        let sock_disc = socket.call("m-sock-disc", "server/discover", adapter_modern_meta());
        assert_eq!(sock_disc["result"], emb_disc["result"]);
        let sock_tool = socket.call(
            "m-sock-verify",
            "tools/call",
            json!({ "name": "ledger_verify", "arguments": {}, "_meta": modern_meta("m-sock-verify")}),
        );
        assert_eq!(call_tool_report(&sock_tool["result"]), cli);
        drop(socket);
        adapter_session_ended(&runtime, socket_session, "mastery unix socket");
    }

    // Shipped `susi mcp serve` stdio modern session: discovery parity, >256 URI rejection (-32602),
    // legacy logging/setLevel refusal (-32601), progress ordering & logLevel gating.
    let mut modern_stdio = StdioSession::spawn_raw(root, &home);
    let stdio_disc = modern_stdio
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "m-stdio-disc", "method": "server/discover", "params": adapter_modern_meta()}),
            Duration::from_secs(120),
        )
        .expect("stdio server/discover must reply");
    assert_eq!(stdio_disc["result"], emb_disc["result"]);

    let too_many_uris: Vec<String> = (0..257).map(|i| format!("susi://m/{i}")).collect();
    let over_uri = modern_stdio
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "m-stdio-overuri", "method": "subscriptions/listen", "params": { "notifications": {"resourceSubscriptions": too_many_uris}, "_meta": modern_meta("m-stdio-overuri")}}),
            Duration::from_secs(15),
        )
        .expect(">256 URI subscription must reject");
    assert_eq!(over_uri["error"]["code"], -32602);

    let mod_set_lvl = modern_stdio
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "m-stdio-setlvl", "method": "logging/setLevel", "params": {"level": "info", "_meta": modern_meta("m-stdio-setlvl")}}),
            Duration::from_secs(15),
        )
        .expect("modern logging/setLevel must reject");
    assert_eq!(mod_set_lvl["error"]["code"], -32601);

    let mut prog_meta = modern_meta("m-stdio-prog");
    prog_meta["io.modelcontextprotocol/logLevel"] = json!("info");
    prog_meta["progressToken"] = json!("m-tok-1");
    let stdio_call = modern_stdio
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "m-stdio-prog", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": prog_meta}}),
            Duration::from_secs(30),
        )
        .expect("stdio tools/call must reply");
    assert_eq!(call_tool_report(&stdio_call["result"]), cli);
    let stdio_prog: Vec<f64> = modern_stdio
        .pending
        .iter()
        .filter(|n| {
            n["method"] == "notifications/progress" && n["params"]["progressToken"] == "m-tok-1"
        })
        .filter_map(|n| n["params"]["progress"].as_f64())
        .collect();
    assert!(!stdio_prog.is_empty());
    assert!(stdio_prog.windows(2).all(|w| w[0] < w[1]));
    assert!(modern_stdio
        .pending
        .iter()
        .any(|n| n["method"] == "notifications/message"));
    drop(modern_stdio);

    // Direct vs HTTP envelope gate parity on malformed notification.
    let bad_line = br#"{"jsonrpc":"2.0","id":null,"method":"subscriptions/listen"}"#;
    assert!(matches!(
        classify_line(bad_line, &mut EraState::default()),
        GateVerdict::Reject { code: -32600, .. }
    ));
    assert_eq!(
        http_envelope_rejection(bad_line).expect("http gate rejects null id")["error"]["code"]
            .as_i64(),
        Some(-32600)
    );

    // Streamable HTTP `/mcp`: discovery, SSE logs/progress without secret leak, header mismatch (-32020),
    // unsupported protocol version (-32022).
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP port");
    let port = listener.local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let http_root = root.to_path_buf();
    let _http_srv = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(http_root, vec![listener], None, false);
    });
    for _ in 0..40 {
        if TcpStream::connect(&addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let auth_header = format!("Bearer {token}");
    let http_post = |headers: &[(&str, &str)], body_val: Value| -> (String, String, Vec<Value>) {
        let body = body_val.to_string();
        let mut req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAuthorization: {auth_header}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n"
        );
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        let mut stream = TcpStream::connect(&addr).expect("connect HTTP /mcp");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        let _ = stream.read_to_string(&mut raw);
        let (_, resp_body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status_line = raw.lines().next().unwrap_or_default().to_string();
        let mut frames = Vec::new();
        let trimmed = resp_body.trim();
        if trimmed.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                frames.push(v);
            }
        } else {
            for line in resp_body.lines() {
                if let Some(data) = line.strip_prefix("data:").map(str::trim) {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        frames.push(v);
                    }
                }
            }
        }
        (status_line, raw, frames)
    };

    let (st_call, raw_call, frames_call) = http_post(
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
        ],
        json!({ "jsonrpc": "2.0", "id": "m-http-call", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/requestId": "m-http-call", "io.modelcontextprotocol/logLevel": "info", "progressToken": "m-http-prog"}}}),
    );
    assert!(st_call.starts_with("HTTP/1.1 200"), "{st_call} {raw_call}");
    let call_res = frames_call
        .iter()
        .find(|f| f["id"] == "m-http-call")
        .expect("HTTP tools/call reply");
    assert_eq!(call_tool_report(&call_res["result"]), cli);
    assert!(frames_call
        .iter()
        .any(|f| f["method"] == "notifications/message"));
    assert!(frames_call
        .iter()
        .any(|f| f["method"] == "notifications/progress"));
    assert!(!raw_call.contains(token));

    let (st_mm, raw_mm, frames_mm) = http_post(
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/list"),
        ],
        json!({ "jsonrpc": "2.0", "id": "m-http-mm", "method": "server/discover", "params": {"_meta": modern_meta("m-http-mm")}}),
    );
    assert!(st_mm.starts_with("HTTP/1.1 400"), "{st_mm} {raw_mm}");
    assert_eq!(frames_mm[0]["error"]["code"], -32020);

    let mut bad_ver_meta = modern_meta("m-http-badver");
    bad_ver_meta["io.modelcontextprotocol/protocolVersion"] = json!("2024-01-01");
    let (st_bv, raw_bv, frames_bv) = http_post(
        &[
            ("MCP-Protocol-Version", "2024-01-01"),
            ("Mcp-Method", "server/discover"),
        ],
        json!({ "jsonrpc": "2.0", "id": "m-http-badver", "method": "server/discover", "params": {"_meta": bad_ver_meta}}),
    );
    assert!(st_bv.starts_with("HTTP/1.1 400"), "{st_bv} {raw_bv}");
    assert_eq!(frames_bv[0]["error"]["code"], -32022);

    // Dynamic subscription filter & correlated `notifications/tools/list_changed` + cancel.
    let custom_service = GmcpService::new(scratch.clone());
    susi_gmcp::catalog::install(&custom_service).expect("install custom catalog");
    let (mut custom_client, custom_sess) = {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let service_for_task = custom_service.clone();
        let (client_half, server_half) = tokio::io::duplex(ADAPTER_LIMIT);
        let (server_read, server_write) = tokio::io::split(server_half);
        let session = runtime.spawn(async move {
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                susi_gmcp::stdio::BoundedLines::new(server_read, ADAPTER_LIMIT),
                server_write,
            );
            let running = service_for_task
                .serve(transport)
                .await
                .map_err(|e| format!("negotiation failed: {e:?}"))?;
            running.waiting().await.map_err(|e| e.to_string())
        });
        let (client_read, mut client_write) = tokio::io::split(client_half);
        let (line_tx, line_rx) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let mut lines = tokio::io::BufReader::new(client_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(value) = serde_json::from_str(&line) {
                    if line_tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        let (byte_tx, mut byte_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        runtime.spawn(async move {
            while let Some(bytes) = byte_rx.recv().await {
                if client_write.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = client_write.shutdown().await;
        });
        (
            AdapterClient {
                write: Box::new(move |bytes| {
                    let _ = byte_tx.send(bytes.to_vec());
                }),
                rx: line_rx,
                pending: Vec::new(),
            },
            session,
        )
    };
    let custom_disc = custom_client.call("m-cust-disc", "server/discover", adapter_modern_meta());
    assert!(custom_disc["result"]["capabilities"].is_object());
    custom_client.send(&json!({ "jsonrpc": "2.0", "id": "m-dyn-sub", "method": "subscriptions/listen", "params": { "notifications": {"toolsListChanged": true}, "_meta": modern_meta("m-dyn-sub")}}));
    let ack = custom_client.take("m-dyn-sub ack", |v| {
        v["method"] == "notifications/subscriptions/acknowledged"
            && v["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "m-dyn-sub"
    });
    assert_eq!(ack["params"]["notifications"]["toolsListChanged"], true);

    custom_service
        .register_tool(
            Tool::new_with_raw(
                "mastery_dynamic_probe",
                Some("mastery subscription notification probe".into()),
                Arc::new(Default::default()),
            ),
            Arc::new(|_, _| {
                Box::pin(async move {
                    Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
                })
            }),
        )
        .unwrap();
    let changed = custom_client.take("tools/list_changed", |v| {
        v["method"] == "notifications/tools/list_changed"
            && v["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "m-dyn-sub"
    });
    assert_eq!(changed["method"], "notifications/tools/list_changed");

    custom_client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "m-dyn-sub", "reason": "mastery complete"}}));
    let post_cancel = custom_client.call("m-post-cancel", "server/discover", adapter_modern_meta());
    assert!(post_cancel["result"]["capabilities"].is_object());
    let custom_probe = custom_client.call(
        "m-cust-probe",
        "tools/call",
        json!({ "name": "mastery_dynamic_probe", "arguments": {}, "_meta": modern_meta("m-cust-probe")}),
    );
    assert_eq!(custom_probe["result"]["content"][0]["text"], "ok");
    drop(custom_client);
    adapter_session_ended(&runtime, custom_sess, "mastery custom service");

    // ── 3. Contract sections, explicit non-claims, and 6 pinned specs ───
    for token in [
        "## Operations and non-claims",
        "Operations and incident runbook",
        "Configuration validation",
        "### Bounded metrics, logs and traces",
        "### Production guarantees and non-claims",
        "Non-claim:",
    ] {
        assert!(
            contract.contains(token),
            "contract-203-012.md missing required section/token: {token}"
        );
    }
    let sources: Value = serde_json::from_slice(
        &std::fs::read(root.join(".agents/gmcp/sources.json")).expect("sources.json readable"),
    )
    .expect("sources.json valid JSON");
    for doc in [
        "docs/specification/2025-11-25/basic/utilities/progress.mdx",
        "docs/specification/2025-11-25/server/utilities/logging.mdx",
        "docs/specification/2026-07-28/basic/patterns/index.mdx",
        "docs/specification/2026-07-28/basic/patterns/progress.mdx",
        "docs/specification/2026-07-28/basic/patterns/subscriptions.mdx",
        "docs/specification/2026-07-28/server/utilities/logging.mdx",
    ] {
        let bytes = std::fs::read(root.join(".agents/gmcp/spec").join(doc))
            .unwrap_or_else(|e| panic!("pinned spec {doc} missing: {e}"));
        let hex = susi_paths::hex::encode(Sha256::digest(&bytes));
        let source_hex = sources["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["document"] == doc)
            .and_then(|f| f["sha256"].as_str());
        assert_eq!(
            source_hex,
            Some(hex.as_str()),
            "sources.json sha mismatch for {doc}"
        );
        assert!(
            contract.contains(&hex),
            "contract-203-012.md must bind live sha256 {hex} of {doc}"
        );
    }

    // ── 4. Evidence verification: complete, bound to HEAD commit,
    //    signed, exit 0, matching current ledger hash ────────────────────
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-012.jsonl"))
        .expect("VC-203-012 must retain an evidence artifact");
    let artifacts = evidence_artifacts(root, "VC-203-012");
    let head_digest =
        artifacts_digest(root, "HEAD", &artifacts).expect("HEAD must list VC-203-012 artifacts");
    let mut covered = BTreeSet::new();
    for record in newest_records(&evidence) {
        if record["requirement"]
            .as_str()
            .is_some_and(|requirement| requirement.ends_with("-M"))
        {
            continue;
        }
        assert_bound_to_tested_commit(root, &record, &artifacts, &head_digest);
        for field in [
            "requirement",
            "check",
            "exit",
            "detail",
            "recorded_unix",
            "artifact_sha256",
            "identity",
        ] {
            assert!(
                !record[field].is_null(),
                "incomplete evidence record lacks {field}: {record}"
            );
        }
        assert!(
            record.get("waived").is_none() && record.get("mock").is_none(),
            "waived or mock evidence rejected: {record}"
        );
        assert_eq!(record["exit"], 0, "evidence must record exit 0");
        assert!(
            !record["identity"].as_str().unwrap().is_empty()
                && !record["detail"].as_str().unwrap().is_empty(),
            "evidence must include non-empty identity and detail"
        );
        assert_eq!(
            record["artifact_sha256"].as_str().unwrap(),
            ledger_hash,
            "evidence artifact_sha256 must match current ledger hash"
        );
        let recorded = record["recorded_unix"].as_u64().unwrap();
        assert!(
            recorded > 1_792_256_000 && recorded <= now + 60,
            "recorded_unix {recorded} outside valid range"
        );
        let check = record["check"].as_str().unwrap();
        if let Some(test_name) = check.strip_prefix("cargo test --locked --test gmcp_requirements ")
        {
            assert!(
                test_name.starts_with("gmcp_")
                    && !test_name.contains(char::is_whitespace)
                    && is_test_fn(&test_src, test_name),
                "evidence names nonexistent test {test_name}"
            );
        } else {
            assert!(
                check
                    .split_whitespace()
                    .filter(|token| !token.starts_with('-'))
                    .any(|token| root.join(token).exists()),
                "evidence check must name a committed artifact: {check}"
            );
        }
        covered.insert(record["requirement"].as_str().unwrap().to_string());
    }
    for req in [
        "GMCP-203-012-01",
        "GMCP-203-012-02",
        "GMCP-203-012-03",
        "GMCP-203-012-04",
        "GMCP-203-012-05",
        "GMCP-203-012-06",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── 5. Roadmap and verdict delivery verification ────────────────────
    let roadmap_json: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/roadmap.json"))
            .expect("roadmap.json must be readable"),
    )
    .expect("roadmap.json must parse");
    let vc_entry = roadmap_json["vectors"]
        .as_array()
        .expect("roadmap.json vectors must be an array")
        .iter()
        .find(|entry| entry["id"] == "VC-203-012")
        .expect("VC-203-012 must exist in roadmap.json");
    assert!(
        vc_entry["progress"]
            .as_str()
            .unwrap_or_default()
            .starts_with("DELIVERED"),
        "VC-203-012 must be marked DELIVERED in roadmap.json"
    );

    let verdict: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/roadmap-verdicts/VC-203-012.json"))
            .expect("VC-203-012.json verdict must exist"),
    )
    .expect("verdict must parse");
    assert_eq!(verdict["vector"], "VC-203-012");
    assert_eq!(verdict["verdict"], "delivered");
    assert!(
        !verdict["evidence"].as_str().unwrap_or_default().is_empty()
            && !verdict["recorded_by"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
        "verdict must include evidence summary and recorded_by"
    );
    let tracked: Vec<&str> = verdict["tracked_by"]
        .as_array()
        .expect("tracked_by array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(tracked.len(), 7, "VC-203-012 must track all 7 tasks");
    for num in 93..=99 {
        let tid = format!("T-CODEXGMCPROADMAP-{num}");
        assert!(
            tracked.contains(&tid.as_str()),
            "verdict missing tracked task {tid}"
        );
    }

    // ── 6. Committed proof ──────────────────────────────────────────────
    let status = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--",
            ".agents/gmcp",
            "docs/gmcp",
            "tests/gmcp_requirements.rs",
            "tests/gmcp_requirements",
            "crates/susi-gmcp",
            ".agents/roadmap.json",
            ".agents/roadmap-verdicts",
        ])
        .current_dir(root)
        .output()
        .expect("git status must run — committed proof is required");
    assert!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "locally modified artifacts are not proof:\n{}",
        String::from_utf8_lossy(&status.stdout)
    );

    let _ = _http_srv;
    let _ = std::fs::remove_dir_all(&home);
}

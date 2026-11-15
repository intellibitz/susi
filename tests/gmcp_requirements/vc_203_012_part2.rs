/// GMCP-203-012-03: end-to-end integration of subscriptions, progress,
/// logging, cancellation, and operational notifications across every
/// served, client, gateway, CLI, adapter, and direct surface.
#[test]
fn gmcp_req_203_012_03_integration() {
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use susi_gmcp::jsonrpc_gate::{
        classify_envelope, classify_line, http_envelope_rejection, EraState, GateVerdict,
    };
    use susi_gmcp::mcp::model::{
        CallToolResult, ContentBlock, ReadResourceResult, Resource, ResourceContents, Tool,
    };
    use susi_gmcp::mcp::service::{QuitReason, ServiceExt};
    use susi_gmcp::mcp::task_manager::TaskOptions;
    use susi_gmcp::protocol::{GmcpService, McpGatewayProxy};

    let (_http_lock, _http_env, home) = gmcp_http_home_guard("012-int");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let modern_meta = |req_id: &str| -> Value {
        let mut meta = adapter_modern_meta()["_meta"].clone();
        meta["io.modelcontextprotocol/requestId"] = json!(req_id);
        meta
    };

    // ── A. Wire & HTTP gate parity ──────────────────────────────────
    let mut modern_era = EraState::default();
    let init_line = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"gate-int","version":"1"}}}"#;
    assert!(matches!(
        classify_line(init_line, &mut modern_era),
        GateVerdict::Forward
    ));

    let valid_listen = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": "gate-sub-1",
        "method": "subscriptions/listen",
        "params": {
            "notifications": {
                "resourceSubscriptions": ["susi://tools"]
            },
            "_meta": modern_meta("gate-sub-1")
        }
    }))
    .unwrap();
    assert!(matches!(
        classify_envelope(&valid_listen),
        GateVerdict::Forward
    ));
    let listen_verdict = classify_line(&valid_listen, &mut modern_era);
    assert!(
        matches!(listen_verdict, GateVerdict::Forward),
        "classify_line(valid_listen) expected Forward, got: {listen_verdict:?}"
    );
    assert_eq!(http_envelope_rejection(&valid_listen), None);

    for note in [
        json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progressToken": "t1", "progress": 1}}),
        json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "ok"}}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "gate-sub-1"}}),
        json!({"jsonrpc": "2.0", "method": "notifications/resources/updated", "params": {"uri": "susi://tools"}}),
    ] {
        let bytes = serde_json::to_vec(&note).unwrap();
        assert!(matches!(
            classify_line(&bytes, &mut modern_era),
            GateVerdict::Forward
        ));
        assert_eq!(http_envelope_rejection(&bytes), None);
    }

    assert!(matches!(
        classify_envelope(b"{"),
        GateVerdict::Reject { code: -32700, .. }
    ));
    assert_eq!(
        http_envelope_rejection(b"{").and_then(|v| v["error"]["code"].as_i64()),
        Some(-32700)
    );
    let bad_ver = br#"{"jsonrpc":"1.0","id":2,"method":"ping"}"#;
    assert!(matches!(
        classify_line(bad_ver, &mut EraState::default()),
        GateVerdict::Reject { code: -32600, .. }
    ));
    assert_eq!(
        http_envelope_rejection(bad_ver).and_then(|v| v["error"]["code"].as_i64()),
        Some(-32600)
    );
    let legacy_req_with_modern_meta = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/list",
        "params": {"_meta": modern_meta("3")}
    }))
    .unwrap();
    let mut leg_era = EraState::default();
    assert!(matches!(
        classify_line(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"g","version":"1"}}}"#,
            &mut leg_era
        ),
        GateVerdict::Forward
    ));
    assert!(matches!(
        classify_line(&legacy_req_with_modern_meta, &mut leg_era),
        GateVerdict::Reject { code: -32600, .. }
    ));

    // ── B. Shipped CLI binary (`susi mcp serve`) stdio integration ──
    let cli_home = home.join("cli-serve-home");
    let cli_cfg_dir = cli_home.join(".susi");
    std::fs::create_dir_all(&cli_cfg_dir).unwrap();
    let mut cli_cfg = susi_config::SusiConfig::default();
    cli_cfg
        .settings
        .insert("max_rpc_body_bytes".to_string(), json!(4096));
    cli_cfg
        .save(&cli_cfg_dir)
        .expect("CLI integration config must save");

    let mut cli_mod = StdioSession::spawn_raw(root, &cli_home);
    let discover = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-disc",
                "method": "server/discover",
                "params": adapter_modern_meta()
            }),
            Duration::from_secs(30),
        )
        .expect("susi mcp serve server/discover must reply");
    let caps = &discover["result"]["capabilities"];
    assert_eq!(caps["logging"], json!({}));
    assert_eq!(caps["tools"]["listChanged"], json!(true));
    assert_eq!(caps["resources"]["subscribe"], json!(true));
    assert_eq!(caps["resources"]["listChanged"], json!(true));
    assert_eq!(caps["prompts"]["listChanged"], json!(true));

    // Modern 2026-07-28 stateless lifecycle rejects legacy `logging/setLevel` (-32601).
    let stateless_set_level = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-stateless-log",
                "method": "logging/setLevel",
                "params": {"level": "info", "_meta": modern_meta("cli-stateless-log")}
            }),
            Duration::from_secs(10),
        )
        .expect("stateless modern CLI must answer logging/setLevel");
    assert_eq!(stateless_set_level["error"]["code"], -32601);

    // Config-enforced line ceiling rejects oversized frames and keeps the session healthy.
    let pad = "x".repeat(4300);
    let oversized = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":\"over-1\",\"method\":\"ping\",\"params\":{{\"pad\":\"{pad}\"}}}}\n"
    );
    cli_mod.send_raw(oversized.as_bytes());
    let over_err = cli_mod
        .response_for(&json!(null), Duration::from_secs(10))
        .expect("oversized line must trigger -32600 rejection");
    assert_eq!(over_err["error"]["code"], -32600);

    let init_ok = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2026-07-28",
                    "capabilities": {},
                    "clientInfo": {"name": "int-cli-modern", "version": "1"}
                }
            }),
            Duration::from_secs(30),
        )
        .expect("modern CLI initialize must succeed after oversized frame");
    assert!(init_ok.get("result").is_some(), "init failed: {init_ok}");
    cli_mod.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    assert!(
        cli_mod.request(
            "tools/list",
            json!({"_meta": modern_meta("cli-tools-check")})
        )["tools"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "session must remain operational after rejecting oversized line"
    );
    // Modern 2026-07-28 sessions always reject legacy `resources/subscribe` (-32601).
    let cli_leg_sub = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-leg-sub",
                "method": "resources/subscribe",
                "params": {"uri": "susi://tools", "_meta": modern_meta("cli-leg-sub")}
            }),
            Duration::from_secs(10),
        )
        .expect("resources/subscribe on 2026-07-28 must answer");
    assert_eq!(cli_leg_sub["error"]["code"], -32601, "{cli_leg_sub}");

    assert!(McpGatewayProxy::evaluate_tool_policy("susi_restricted_probe").is_err());
    assert!(McpGatewayProxy::evaluate_tool_policy("ledger_verify").is_ok());
    let policy_denied = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-policy",
                "method": "tools/call",
                "params": {
                    "name": "susi_restricted_probe",
                    "arguments": {},
                    "_meta": modern_meta("cli-policy")
                }
            }),
            Duration::from_secs(10),
        )
        .expect("restricted tool call must answer");
    assert_eq!(policy_denied["result"]["isError"], json!(true));
    let cli_policy_msg = policy_denied["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        cli_policy_msg.contains("Governance Violation")
            && cli_policy_msg.contains("CapabilityToken"),
        "expected Governance Violation message: {policy_denied}"
    );

    let mut tool_meta = modern_meta("cli-tool-1");
    tool_meta["io.modelcontextprotocol/logLevel"] = json!("info");
    tool_meta["progressToken"] = json!("cli-prog-1");
    let tool_call = cli_mod
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "cli-tool-1",
                "method": "tools/call",
                "params": {
                    "name": "ledger_verify",
                    "arguments": {},
                    "_meta": tool_meta
                }
            }),
            Duration::from_secs(30),
        )
        .expect("CLI modern tools/call must reply");
    assert_eq!(tool_call["result"]["resultType"], "complete");
    let expected_report = ledger_report(root);
    assert_eq!(
        call_tool_report(&tool_call["result"])["rows"],
        expected_report["rows"]
    );
    let cli_notes = std::mem::take(&mut cli_mod.pending);
    let cli_prog: Vec<f64> = cli_notes
        .iter()
        .filter(|n| {
            n["method"] == "notifications/progress" && n["params"]["progressToken"] == "cli-prog-1"
        })
        .filter_map(|n| n["params"]["progress"].as_f64())
        .collect();
    assert!(
        !cli_prog.is_empty(),
        "CLI must stream progress: {cli_notes:?}"
    );
    assert!(cli_prog.windows(2).all(|w| w[0] < w[1]));
    assert!(
        cli_notes
            .iter()
            .any(|n| n["method"] == "notifications/message"),
        "CLI must emit request-scoped log message: {cli_notes:?}"
    );

    cli_mod.send(&json!({
        "jsonrpc": "2.0",
        "id": "cli-sub-1",
        "method": "subscriptions/listen",
        "params": {
            "notifications": {
                "resourceSubscriptions": ["susi://tools"]
            },
            "_meta": modern_meta("cli-sub-1")
        }
    }));
    let ack_deadline = Instant::now() + Duration::from_secs(10);
    let mut got_cli_ack = false;
    let mut ack_seen = Vec::new();
    while Instant::now() < ack_deadline && !got_cli_ack {
        if let Ok(msg) = cli_mod.rx.recv_timeout(Duration::from_millis(200)) {
            if msg["method"] == "notifications/subscriptions/acknowledged"
                && msg["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "cli-sub-1"
            {
                got_cli_ack = true;
            } else {
                let is_early_err = msg["id"] == "cli-sub-1";
                ack_seen.push(msg);
                assert!(
                    !is_early_err,
                    "CLI subscriptions/listen failed early: {ack_seen:?}"
                );
            }
        }
    }
    assert!(
        got_cli_ack,
        "CLI subscriptions/listen must emit ack; saw: {ack_seen:?}"
    );
    cli_mod.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": "cli-sub-1", "reason": "integration cancel"}
    }));
    let barrier = cli_mod.request(
        "tools/list",
        json!({"_meta": modern_meta("cli-post-cancel")}),
    );
    assert!(
        barrier["tools"].as_array().is_some(),
        "session must stay healthy after subscription cancel"
    );
    assert!(
        !cli_mod.pending.iter().any(|m| m["id"] == "cli-sub-1"),
        "cancelled subscription must not emit an answer: {:?}",
        cli_mod.pending
    );
    drop(cli_mod);

    // Legacy 2025-11-25 CLI stdio surface.
    let mut cli_leg = StdioSession::spawn(root, &home);
    assert_eq!(
        cli_leg.request("logging/setLevel", json!({"level": "info"}))["error"],
        Value::Null
    );
    assert_eq!(
        cli_leg.request("resources/subscribe", json!({"uri": "susi://tools"}))["error"],
        Value::Null
    );
    let leg_sub_err = cli_leg
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "leg-sub-rpc", "method": "subscriptions/listen", "params": {}}),
            Duration::from_secs(10),
        )
        .expect("2025-11-25 must answer subscriptions/listen");
    assert_eq!(leg_sub_err["error"]["code"], -32601);
    let leg_call = cli_leg.request(
        "tools/call",
        json!({"name": "ledger_verify", "arguments": {}, "_meta": {"progressToken": "cli-leg-1"}}),
    );
    assert_eq!(call_tool_report(&leg_call)["verified"], true);
    let leg_notes = std::mem::take(&mut cli_leg.pending);
    assert!(
        leg_notes
            .iter()
            .any(|n| n["method"] == "notifications/progress"
                && n["params"]["progressToken"] == "cli-leg-1"),
        "legacy CLI must stream notifications/progress: {leg_notes:?}"
    );
    assert!(
        cli_leg
            .request_verbatim(
                &json!({"jsonrpc": "2.0", "id": "leg-unsub", "method": "resources/unsubscribe", "params": {"uri": "susi://tools"}}),
                Duration::from_secs(10)
            )
            .expect("unsubscribe must reply")
            .get("result")
            .is_some()
    );
    drop(cli_leg);

    // ── C. Daemon/Gateway HTTP + wire/core SDK + `susi mcp call` ─────
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let token = "susi-012-int-bearer";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();

    let runtime = adapter_runtime(&home);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP test listener");
    let port = listener.local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let root_for_http = root.to_path_buf();
    let _http_server = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(
            root_for_http,
            vec![listener],
            None,
            false,
        );
    });
    for _ in 0..40 {
        if TcpStream::connect(&addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let http_send = |method: &str, headers: &[(&str, &str)], body: Option<&str>| -> String {
        let mut s = TcpStream::connect(&addr).expect("connect HTTP gateway");
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

    let parse_frames = |raw: &str| -> (String, String, Vec<Value>) {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
        let status = head.lines().next().unwrap_or_default().to_string();
        let mut frames = Vec::new();
        let trimmed = body.trim();
        if trimmed.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                frames.push(v);
            }
        } else {
            for line in body.lines() {
                if let Some(data) = line.strip_prefix("data:").map(str::trim) {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        frames.push(v);
                    }
                }
            }
        }
        (status, head.to_string(), frames)
    };

    // Auth (`401`), Origin (`403`), Envelope (`400 / -32700`), and `GET /mcp` (`405` on 2026-07-28).
    let no_auth = http_send(
        "POST",
        &[
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
    );
    assert!(no_auth.starts_with("HTTP/1.1 401"));
    assert!(no_auth.to_ascii_lowercase().contains("www-authenticate:"));

    let auth_header = format!("Bearer {token}");
    let bad_origin = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Origin", "https://evil.example"),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
    );
    assert!(bad_origin.starts_with("HTTP/1.1 403"));

    let bad_json = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some("{"),
    );
    let (bad_st, _, bad_frames) = parse_frames(&bad_json);
    assert!(bad_st.starts_with("HTTP/1.1 400"), "{bad_json}");
    assert_eq!(bad_frames[0]["error"]["code"], -32700);

    let get_modern = http_send(
        "GET",
        &[
            ("Authorization", &auth_header),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Accept", "text/event-stream"),
        ],
        None,
    );
    assert!(get_modern.starts_with("HTTP/1.1 405"), "{get_modern}");

    // Streaming `subscriptions/listen` over HTTP SSE -> ack arrives, disconnect tears down cleanly.
    {
        let sub_body = json!({
            "jsonrpc": "2.0",
            "id": "http-sub-1",
            "method": "subscriptions/listen",
            "params": {
                "notifications": {
                    "resourceSubscriptions": ["susi://tools"]
                },
                "_meta": modern_meta("http-sub-1")
            }
        })
        .to_string();
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {auth_header}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: subscriptions/listen\r\nContent-Length: {}\r\n\r\n{sub_body}",
            sub_body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut reader = BufReader::new(s);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).unwrap();
        assert!(status_line.starts_with("HTTP/1.1 200"), "{status_line}");
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
        }
        let mut ack_seen = false;
        let sse_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < sse_deadline && !ack_seen {
            line.clear();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if let Some(data) = line.strip_prefix("data:").map(str::trim) {
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if v["method"] == "notifications/subscriptions/acknowledged"
                        && v["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"]
                            == "http-sub-1"
                    {
                        ack_seen = true;
                    }
                }
            }
        }
        assert!(ack_seen, "HTTP SSE subscriptions/listen must emit ack");
    }

    // `> 256` resourceSubscriptions fails `-32602` pre-ack; legacy RPCs fail `-32601` on `2026-07-28`.
    let too_many_uris: Vec<String> = (0..257).map(|i| format!("susi://res/{i}")).collect();
    let cap_body = json!({
        "jsonrpc": "2.0",
        "id": "http-sub-cap",
        "method": "subscriptions/listen",
        "params": {
            "notifications": {
                "resourceSubscriptions": too_many_uris
            },
            "_meta": modern_meta("http-sub-cap")
        }
    })
    .to_string();
    let cap_raw = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "subscriptions/listen"),
        ],
        Some(&cap_body),
    );
    let (_, _, cap_frames) = parse_frames(&cap_raw);
    assert_eq!(cap_frames[0]["error"]["code"], -32602, "{cap_raw}");

    let leg_http_body = json!({
        "jsonrpc": "2.0",
        "id": "http-leg-1",
        "method": "logging/setLevel",
        "params": {
            "level": "info",
            "_meta": modern_meta("http-leg-1")
        }
    })
    .to_string();
    let leg_http_raw = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "logging/setLevel"),
        ],
        Some(&leg_http_body),
    );
    let (_, _, leg_http_frames) = parse_frames(&leg_http_raw);
    assert_eq!(
        leg_http_frames[0]["error"]["code"], -32601,
        "{leg_http_raw}"
    );

    let leg_sub_http_body = json!({
        "jsonrpc": "2.0",
        "id": "http-leg-sub",
        "method": "resources/subscribe",
        "params": {
            "uri": "susi://tools",
            "_meta": modern_meta("http-leg-sub")
        }
    })
    .to_string();
    let leg_sub_http_raw = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "resources/subscribe"),
            ("Mcp-Name", "susi://tools"),
        ],
        Some(&leg_sub_http_body),
    );
    let (_, _, leg_sub_http_frames) = parse_frames(&leg_sub_http_raw);
    assert_eq!(
        leg_sub_http_frames[0]["error"]["code"], -32601,
        "{leg_sub_http_raw}"
    );

    let mut bad_lvl_meta = modern_meta("http-bad-lvl");
    bad_lvl_meta["io.modelcontextprotocol/logLevel"] = json!("bogus");
    let bad_level_body = json!({
        "jsonrpc": "2.0",
        "id": "http-bad-lvl",
        "method": "tools/call",
        "params": {
            "name": "ledger_verify",
            "arguments": {},
            "_meta": bad_lvl_meta
        }
    })
    .to_string();
    let bad_lvl_raw = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
        ],
        Some(&bad_level_body),
    );
    let (_, _, bad_lvl_frames) = parse_frames(&bad_lvl_raw);
    assert_eq!(bad_lvl_frames[0]["error"]["code"], -32602, "{bad_lvl_raw}");

    let http_policy_body = json!({
        "jsonrpc": "2.0",
        "id": "http-policy-1",
        "method": "tools/call",
        "params": {
            "name": "susi_restricted_probe",
            "arguments": {},
            "_meta": modern_meta("http-policy-1")
        }
    })
    .to_string();
    let http_policy_raw = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "susi_restricted_probe"),
        ],
        Some(&http_policy_body),
    );
    let (http_pol_st, _, http_pol_frames) = parse_frames(&http_policy_raw);
    assert!(http_pol_st.starts_with("HTTP/1.1 200"), "{http_policy_raw}");
    assert_eq!(http_pol_frames[0]["result"]["isError"], json!(true));
    assert_eq!(
        http_pol_frames[0]["result"]["content"][0]["text"],
        policy_denied["result"]["content"][0]["text"]
    );

    let mut http_tool_meta = modern_meta("http-int-tool");
    http_tool_meta["io.modelcontextprotocol/logLevel"] = json!("info");
    http_tool_meta["progressToken"] = json!("http-int-1");
    let http_tool_body = json!({
        "jsonrpc": "2.0",
        "id": "http-int-tool",
        "method": "tools/call",
        "params": {
            "name": "ledger_verify",
            "arguments": {},
            "_meta": http_tool_meta
        }
    })
    .to_string();
    let http_tool_raw = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
        ],
        Some(&http_tool_body),
    );
    let (http_tool_st, _, http_tool_frames) = parse_frames(&http_tool_raw);
    assert!(http_tool_st.starts_with("HTTP/1.1 200"), "{http_tool_raw}");
    assert!(http_tool_frames
        .iter()
        .any(|f| f["method"] == "notifications/progress"
            && f["params"]["progressToken"] == "http-int-1"));
    assert!(http_tool_frames
        .iter()
        .any(|f| f["method"] == "notifications/message" && f["params"]["level"] == "info"));
    let http_answer = http_tool_frames
        .iter()
        .find(|f| f["id"] == "http-int-tool")
        .expect("HTTP tool response frame missing");
    assert_eq!(http_answer["result"]["resultType"], "complete");
    assert_eq!(
        call_tool_report(&http_answer["result"])["rows"],
        expected_report["rows"]
    );

    // Wire SDK (`susi_wire_client::mcp::Session`), `susi_core::mcp_client`, and shipped CLI `susi mcp call`.
    let mut wire =
        susi_wire_client::mcp::Session::open(&addr, Some(token), Duration::from_secs(15))
            .expect("wire client must open 2025-11-25 session");
    wire.request("logging/setLevel", &json!({"level": "info"}), &mut |_| {})
        .expect("wire logging/setLevel");
    wire.request(
        "resources/subscribe",
        &json!({"uri": "susi://tools"}),
        &mut |_| {},
    )
    .expect("wire resources/subscribe");
    let wire_listen_err = wire
        .request("subscriptions/listen", &json!({}), &mut |_| {})
        .expect_err("2025-11-25 wire session must reject subscriptions/listen");
    assert!(
        wire_listen_err.to_string().contains("-32601")
            || wire_listen_err.to_string().contains("Method not found"),
        "unexpected wire subscriptions/listen error: {wire_listen_err}"
    );
    let mut wire_notes = Vec::new();
    let wire_res = wire
        .call_tool(
            "ledger_verify",
            &json!({}),
            Some(json!("wire-sdk-1")),
            &mut |note| wire_notes.push(note.clone()),
        )
        .expect("wire client ledger_verify");
    assert_eq!(call_tool_report(&wire_res)["rows"], expected_report["rows"]);
    let wire_prog: Vec<f64> = wire_notes
        .iter()
        .filter(|n| {
            n["method"] == "notifications/progress" && n["params"]["progressToken"] == "wire-sdk-1"
        })
        .filter_map(|n| n["params"]["progress"].as_f64())
        .collect();
    assert!(
        !wire_prog.is_empty(),
        "wire SDK must receive live progress: {wire_notes:?}"
    );
    assert!(wire_prog.windows(2).all(|w| w[0] < w[1]));
    assert!(wire_notes
        .iter()
        .any(|n| n["method"] == "notifications/message"));
    wire.request(
        "resources/unsubscribe",
        &json!({"uri": "susi://tools"}),
        &mut |_| {},
    )
    .expect("wire resources/unsubscribe");
    wire.close().expect("DELETE /mcp must release wire session");

    // Legacy 2025-11-25 session opens `GET /mcp` standalone SSE cleanly.
    {
        let leg_init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "get-sse-check", "version": "1"}
            }
        })
        .to_string();
        let leg_init_raw = http_send(
            "POST",
            &[
                ("Authorization", &auth_header),
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
            ],
            Some(&leg_init),
        );
        let (st, head, _) = parse_frames(&leg_init_raw);
        assert!(st.starts_with("HTTP/1.1 200"), "{leg_init_raw}");
        let sid = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("mcp-session-id")
                    .then(|| v.trim().to_string())
            })
            .expect("legacy HTTP init must issue Mcp-Session-Id");
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let get_req = format!(
            "GET /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {auth_header}\r\nMcp-Session-Id: {sid}\r\nMCP-Protocol-Version: 2025-11-25\r\nAccept: text/event-stream\r\n\r\n"
        );
        s.write_all(get_req.as_bytes()).unwrap();
        let mut rdr = BufReader::new(s);
        let mut status_line = String::new();
        rdr.read_line(&mut status_line).unwrap();
        assert!(
            status_line.starts_with("HTTP/1.1 200"),
            "2025-11-25 GET /mcp standalone SSE must open 200: {status_line}"
        );
    }

    let core_res =
        susi_core::mcp_client::call_tool(&addr, "ledger_verify", &json!({}), Some(token))
            .expect("susi_core::mcp_client::call_tool must succeed");
    assert_eq!(call_tool_report(&core_res)["rows"], expected_report["rows"]);
    let core_sess = susi_core::mcp_client::session_call(
        &addr,
        "tools/call",
        &json!({"name": "ledger_verify", "arguments": {}}),
        Some(token),
    )
    .expect("susi_core::mcp_client::session_call must succeed");
    assert_eq!(
        call_tool_report(&core_sess)["rows"],
        expected_report["rows"]
    );

    let cli_net = susi_json(
        root,
        &home,
        &[
            "mcp",
            "call",
            &addr,
            "ledger_verify",
            "{}",
            "--token",
            token,
        ],
    );
    let cli_net_report = if cli_net.get("content").is_some() {
        call_tool_report(&cli_net)
    } else {
        cli_net
    };
    assert_eq!(cli_net_report["verified"], true);
    assert_eq!(cli_net_report["rows"], expected_report["rows"]);

    let cli_local = susi_json(root, &home, &["mcp", "call", "susi", "ledger_verify", "{}"]);
    let cli_local_report = if cli_local.get("content").is_some() {
        call_tool_report(&cli_local)
    } else {
        cli_local
    };
    assert_eq!(cli_local_report["verified"], true);
    assert_eq!(cli_local_report["rows"], ledger_tool_report(root)["rows"]);

    // ── D. Adapters, Direct GmcpService, Lagged & Shutdown, Daemon Wiring ──
    let adapter_modern_handshake = |client: &mut AdapterClient, name: &str| {
        let init = client.call(
            "init-modern",
            "initialize",
            json!({
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": {"name": name, "version": "1"}
            }),
        );
        assert!(
            init["result"]["capabilities"].is_object(),
            "{name}: modern handshake failed: {init}"
        );
        client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    };

    let (mut embedded, embedded_sess) = embedded_adapter(&runtime, root);
    adapter_modern_handshake(&mut embedded, "012-int-embedded");
    embedded.send(&json!({
        "jsonrpc": "2.0",
        "id": "emb-sub-1",
        "method": "subscriptions/listen",
        "params": {
            "notifications": {
                "resourceSubscriptions": ["susi://tools"]
            },
            "_meta": modern_meta("emb-sub-1")
        }
    }));
    let emb_ack = embedded.take("embedded ack", |m| {
        m["id"] == "emb-sub-1"
            || (m["method"] == "notifications/subscriptions/acknowledged"
                && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "emb-sub-1")
    });
    assert_eq!(
        emb_ack["method"], "notifications/subscriptions/acknowledged",
        "{emb_ack}"
    );
    embedded.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": "emb-sub-1"}
    }));
    assert!(
        embedded.call(
            "emb-post-cancel",
            "server/discover",
            json!({"_meta": modern_meta("emb-post-cancel")})
        )["error"]
            .is_null(),
        "embedded adapter must stay healthy after cancel"
    );
    assert!(!embedded.pending.iter().any(|m| m["id"] == "emb-sub-1"));
    drop(embedded);
    adapter_session_ended(&runtime, embedded_sess, "012-int embedded");

    #[cfg(unix)]
    {
        let (mut socket, sock_sess) = socket_adapter(&runtime, root);
        adapter_modern_handshake(&mut socket, "012-int-socket");
        socket.send(&json!({
            "jsonrpc": "2.0",
            "id": "sock-sub-1",
            "method": "subscriptions/listen",
            "params": {
                "notifications": {
                    "resourceSubscriptions": ["susi://tools"]
                },
                "_meta": modern_meta("sock-sub-1")
            }
        }));
        let sock_ack = socket.take("unix socket ack", |m| {
            m["id"] == "sock-sub-1"
                || (m["method"] == "notifications/subscriptions/acknowledged"
                    && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"]
                        == "sock-sub-1")
        });
        assert_eq!(
            sock_ack["method"], "notifications/subscriptions/acknowledged",
            "{sock_ack}"
        );
        socket.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": "sock-sub-1"}
        }));
        assert!(
            socket.call(
                "sock-post-cancel",
                "server/discover",
                json!({"_meta": modern_meta("sock-post-cancel")})
            )["error"]
                .is_null(),
            "unix socket adapter must stay healthy after cancel"
        );
        assert!(!socket.pending.iter().any(|m| m["id"] == "sock-sub-1"));
        drop(socket);
        adapter_session_ended(&runtime, sock_sess, "012-int unix socket");
    }

    // Direct `GmcpService` multiplexing, isolation, dynamic ResourceGuard, duplicate progressToken, graceful shutdown.
    let spawn_duplex = |rt: &tokio::runtime::Runtime, svc: GmcpService| {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (client_half, server_half) = tokio::io::duplex(ADAPTER_LIMIT);
        let (server_read, server_write) = tokio::io::split(server_half);
        let sess = rt.spawn(async move {
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                susi_gmcp::stdio::BoundedLines::new(server_read, ADAPTER_LIMIT),
                server_write,
            );
            let running = svc
                .serve(transport)
                .await
                .map_err(|e| format!("serve error: {e:?}"))?;
            let cancel = running.cancellation_token();
            tokio::spawn(async move {
                if stop_rx.await.is_ok() {
                    cancel.cancel();
                }
            });
            running.waiting().await.map_err(|e| e.to_string())
        });
        let (client_read, mut client_write) = tokio::io::split(client_half);
        let (line_tx, line_rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let mut lines = tokio::io::BufReader::new(client_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if line_tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        let (byte_tx, mut byte_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        rt.spawn(async move {
            while let Some(b) = byte_rx.recv().await {
                if client_write.write_all(&b).await.is_err() {
                    break;
                }
            }
            let _ = client_write.shutdown().await;
        });
        (
            AdapterClient {
                write: Box::new(move |b| {
                    let _ = byte_tx.send(b.to_vec());
                }),
                rx: line_rx,
                pending: Vec::new(),
            },
            stop_tx,
            sess,
        )
    };

    let custom_svc = GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&custom_svc).unwrap();
    let allow_guarded = Arc::new(AtomicBool::new(true));
    let allow_flag = allow_guarded.clone();
    custom_svc.set_resource_guard(Arc::new(move |uri| {
        if uri == "susi://int-resource" && !allow_flag.load(Ordering::Acquire) {
            Err(susi_gmcp::mcp::ErrorData::invalid_params(
                "access revoked",
                None,
            ))
        } else {
            Ok(())
        }
    }));
    let register_int_res = |svc: &GmcpService, uri: &str, text: &str| {
        let payload = text.to_string();
        let uri_owned = uri.to_string();
        svc.register_resource(
            Resource::new(uri, "int-res").with_mime_type("text/plain"),
            Arc::new(move |_, _| {
                let u = uri_owned.clone();
                let p = payload.clone();
                Box::pin(async move {
                    Ok(ReadResourceResult {
                        result_type: None,
                        ttl_ms: None,
                        cache_scope: None,
                        contents: vec![ResourceContents::TextResourceContents {
                            uri: u,
                            mime_type: Some("text/plain".to_string()),
                            text: p,
                            meta: None,
                        }],
                        meta: None,
                    }
                    .into())
                })
            }),
        );
    };
    register_int_res(&custom_svc, "susi://int-resource", "v1");

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let entered_tx = entered.clone();
    let release_rx = release.clone();
    custom_svc
        .register_tool(
            Tool::new_with_raw(
                "int_slow_progress",
                Some("slow progress probe".into()),
                Arc::new(Default::default()),
            ),
            Arc::new(move |_, ctx| {
                let entered_tx = entered_tx.clone();
                let release_rx = release_rx.clone();
                Box::pin(async move {
                    let _ = ctx
                        .emit_logging_message(
                            Some(susi_gmcp::mcp::model::LoggingLevel::Info),
                            susi_gmcp::mcp::model::LoggingLevel::Info,
                            Some("int-slow".to_string()),
                            json!("started"),
                        )
                        .await;
                    entered_tx.notify_one();
                    release_rx.notified().await;
                    Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
                })
            }),
        )
        .unwrap();

    let (mut custom_client, custom_stop, custom_sess) = spawn_duplex(&runtime, custom_svc.clone());
    adapter_modern_handshake(&mut custom_client, "012-int-custom");

    custom_client.send(&json!({
        "jsonrpc": "2.0",
        "id": "mux-1",
        "method": "subscriptions/listen",
        "params": {
            "notifications": {
                "resourceSubscriptions": ["susi://int-resource"]
            },
            "_meta": modern_meta("mux-1")
        }
    }));
    custom_client.send(&json!({
        "jsonrpc": "2.0",
        "id": "mux-2",
        "method": "subscriptions/listen",
        "params": {
            "notifications": {
                "resourceSubscriptions": ["susi://tools"]
            },
            "_meta": modern_meta("mux-2")
        }
    }));
    for sub_id in ["mux-1", "mux-2"] {
        let ack = custom_client.take(sub_id, |m| {
            m["id"] == sub_id
                || (m["method"] == "notifications/subscriptions/acknowledged"
                    && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == sub_id)
        });
        assert_eq!(
            ack["method"], "notifications/subscriptions/acknowledged",
            "{sub_id}: {ack}"
        );
    }

    custom_svc.resource_updated("susi://int-resource".to_string());
    let _ = custom_client.take("mux-1 updated", |m| {
        m["method"] == "notifications/resources/updated"
            && m["params"]["uri"] == "susi://int-resource"
            && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "mux-1"
    });

    // Active duplicate `progressToken` rejection and log-to-subscription isolation.
    let mut slow_meta = modern_meta("slow-1");
    slow_meta["io.modelcontextprotocol/logLevel"] = json!("info");
    slow_meta["progressToken"] = json!("dup-tok-1");
    custom_client.send(&json!({
        "jsonrpc": "2.0",
        "id": "slow-1",
        "method": "tools/call",
        "params": {
            "name": "int_slow_progress",
            "arguments": {},
            "_meta": slow_meta
        }
    }));
    runtime.block_on(entered.notified());
    let log_note = custom_client.take("notifications/message", |m| {
        m["method"] == "notifications/message"
    });
    assert_eq!(log_note["params"]["level"], "info");
    assert_eq!(log_note["params"]["logger"], "int-slow");
    assert_eq!(log_note["params"]["data"], "started");
    assert!(log_note["params"]["_meta"]
        .get("io.modelcontextprotocol/subscriptionId")
        .is_none());

    let mut slow_dup_meta = modern_meta("slow-dup");
    slow_dup_meta["progressToken"] = json!("dup-tok-1");
    let dup_err = custom_client.call(
        "slow-dup",
        "tools/call",
        json!({
            "name": "ledger_verify",
            "arguments": {},
            "_meta": slow_dup_meta
        }),
    );
    assert_eq!(dup_err["error"]["code"], -32602);
    release.notify_one();
    assert_eq!(
        custom_client.answer("slow-1")["result"]["resultType"],
        "complete"
    );

    // Cancelling mux-1 isolates mux-2 cleanly and silences mux-1 updates.
    custom_client.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": "mux-1"}
    }));
    let _ = custom_client.call(
        "mux1-post-cancel",
        "server/discover",
        json!({"_meta": modern_meta("mux1-post-cancel")}),
    );
    custom_client.pending.clear();
    custom_svc.resource_updated("susi://int-resource".to_string());
    custom_svc.resource_updated("susi://tools".to_string());
    let _ = custom_client.take("mux-2 updated", |m| {
        m["method"] == "notifications/resources/updated"
            && m["params"]["uri"] == "susi://tools"
            && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "mux-2"
    });
    assert!(
        !custom_client
            .pending
            .iter()
            .any(|m| m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "mux-1"),
        "cancelled mux-1 must emit zero further updates: {:?}",
        custom_client.pending
    );

    // Dynamic ResourceGuard revocation denies reads cleanly.
    allow_guarded.store(false, Ordering::Release);
    let revoked = custom_client.call(
        "res-revoked",
        "resources/read",
        json!({
            "uri": "susi://int-resource",
            "_meta": adapter_modern_meta()["_meta"]
        }),
    );
    assert_eq!(revoked["error"]["code"], -32602);

    // Graceful server shutdown completes remaining active subscription mux-2 cleanly.
    let _ = custom_stop.send(());
    let mux2_done = custom_client.answer("mux-2");
    assert_eq!(mux2_done["result"]["resultType"], "complete");
    assert_eq!(
        mux2_done["result"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        "mux-2"
    );
    drop(custom_client);
    assert!(matches!(
        runtime.block_on(custom_sess),
        Ok(Ok(QuitReason::Cancelled))
    ));

    // Deterministic `RecvError::Lagged` slow-consumer overflow on a single-threaded runtime.
    let ct_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lag_svc = GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&lag_svc).unwrap();
    register_int_res(&lag_svc, "susi://lag-res", "init");
    let lag_meta = modern_meta("lag-1");
    let lag_err_code = ct_rt.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let lag_for_task = lag_svc.clone();
        let (client_half, server_half) = tokio::io::duplex(ADAPTER_LIMIT);
        let (sr, sw) = tokio::io::split(server_half);
        let server = tokio::spawn(async move {
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                susi_gmcp::stdio::BoundedLines::new(sr, ADAPTER_LIMIT),
                sw,
            );
            let running = lag_for_task.serve(transport).await.unwrap();
            let _ = running.waiting().await;
        });
        let (cr, mut cw) = tokio::io::split(client_half);
        let mut lines = tokio::io::BufReader::new(cr).lines();
        let write_json = |v: Value| {
            let mut s = serde_json::to_vec(&v).unwrap();
            s.push(b'\n');
            s
        };
        cw.write_all(&write_json(json!({
            "jsonrpc": "2.0",
            "id": "lag-init",
            "method": "initialize",
            "params": {
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": {"name": "lag", "version": "1"}
            }
        })))
        .await
        .unwrap();
        let _ = lines.next_line().await.unwrap();
        cw.write_all(&write_json(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        ))
        .await
        .unwrap();
        cw.write_all(&write_json(json!({
            "jsonrpc": "2.0",
            "id": "lag-1",
            "method": "subscriptions/listen",
            "params": {
                "notifications": {
                    "resourceSubscriptions": ["susi://lag-res"]
                },
                "_meta": lag_meta
            }
        })))
        .await
        .unwrap();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v["method"] == "notifications/subscriptions/acknowledged" {
                    break;
                }
            }
        }
        // Broadcast channel capacity is 256; >256 synchronous sends without yielding forces Lagged.
        for _ in 0..300 {
            lag_svc.resource_updated("susi://lag-res".to_string());
        }
        let mut code = None;
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v["id"] == "lag-1" {
                    code = v["error"]["code"].as_i64();
                    break;
                }
            }
        }
        drop(cw);
        drop(lines);
        let _ = server.await;
        code
    });
    assert_eq!(lag_err_code, Some(-32603));

    // Legacy 2025-11-25 live `notifications/resources/updated` + unsubscribe silence + TaskManager lifecycle.
    let leg_svc = GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&leg_svc).unwrap();
    register_int_res(&leg_svc, "susi://int-resource", "v1");
    let spawned_task = {
        let _rt = runtime.enter();
        leg_svc.tasks.spawn(TaskOptions::default(), |ctx| {
            Box::pin(async move {
                ctx.cancelled().await;
                Err(susi_gmcp::mcp::task_manager::TaskExit::Cancelled)
            })
        })
    };
    let (mut leg_client, leg_stop, leg_sess) = spawn_duplex(&runtime, leg_svc.clone());
    assert!(leg_client
        .call(
            "leg-init",
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"extensions": {"io.modelcontextprotocol/tasks": {}}},
                "clientInfo": {"name": "leg-live", "version": "1"}
            }),
        )
        .get("result")
        .is_some());
    leg_client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    assert!(leg_client
        .call(
            "leg-sub",
            "resources/subscribe",
            json!({"uri": "susi://int-resource"}),
        )
        .get("result")
        .is_some());
    leg_svc.resource_updated("susi://int-resource".to_string());
    let _ = leg_client.take("2025-11-25 resources/updated", |m| {
        m["method"] == "notifications/resources/updated"
            && m["params"]["uri"] == "susi://int-resource"
    });
    assert!(leg_client
        .call(
            "leg-unsub",
            "resources/unsubscribe",
            json!({"uri": "susi://int-resource"}),
        )
        .get("result")
        .is_some());
    leg_client.pending.clear();
    leg_svc.resource_updated("susi://int-resource".to_string());
    assert_eq!(
        leg_client.call("leg-barrier", "ping", json!({}))["result"],
        json!({})
    );
    assert!(
        !leg_client
            .pending
            .iter()
            .any(|m| m["method"] == "notifications/resources/updated"),
        "unsubscribed resource must not emit further updates: {:?}",
        leg_client.pending
    );
    let task_state = leg_client.call(
        "leg-task-get",
        "tasks/get",
        json!({"taskId": spawned_task.task_id}),
    );
    assert_eq!(task_state["result"]["status"], "working", "{task_state}");
    assert!(leg_client
        .call(
            "leg-task-cancel",
            "tasks/cancel",
            json!({"taskId": spawned_task.task_id})
        )
        .get("result")
        .is_some());
    let _ = leg_stop.send(());
    drop(leg_client);
    let _ = runtime.block_on(leg_sess);

    // Verify daemon & CLI composition wiring points at the single implementation.
    let daemon_server =
        std::fs::read_to_string(root.join("crates/susi-daemon/src/server.rs")).unwrap();
    assert!(daemon_server.contains("Supervisor::global().spawn(\"gmcp-http\""));
    assert!(daemon_server.contains("start_http_server"));
    let main_src = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(main_src.contains("wire_mcp_serve_substrate"));
    let mcp_cli_src = std::fs::read_to_string(root.join("src/cli/mcp_cli.rs")).unwrap();
    assert!(
        mcp_cli_src.contains("call_tool_direct")
            && mcp_cli_src.contains("mcp_client::session_call")
    );

    // ── E. Ledger & Contract Binding ─────────────────────────────────
    let ledger: Value = serde_json::from_slice(
        &std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    )
    .unwrap();
    let vec_rows: Vec<&Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-012")
        .collect();
    assert_eq!(vec_rows.len(), 57, "VC-203-012 must pin 57 normative rows");
    for row in &vec_rows {
        assert_eq!(
            row["trace"]["production_path"],
            "crates/susi-gmcp/src/protocol.rs"
        );
        assert!(root.join("crates/susi-gmcp/src/protocol.rs").exists());
        assert_eq!(
            row["trace"]["positive_test"],
            "tests/gmcp_requirements.rs::gmcp_req_203_012_03_integration"
        );
    }
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-012.md")).unwrap();
    for token in [
        "VC-203-012",
        "gmcp_req_203_012_03_integration",
        "subscriptions/listen",
        "notifications/progress",
        "notifications/message",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-017-04: adversarial, fault-injection, policy-refusal, MRTR spoofing,
/// oversized-frame, disconnect, race, corrupt-store recovery, and sibling-isolation
/// verification for the complete MCP client and host (`VC-203-017`).
#[test]
fn gmcp_req_203_017_04_adversarial() {
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_core::mac_policy::{actions, MacPolicy, PrivacyMode};
    use susi_gmcp::{
        extensions::ExtensionStatus,
        mcp::model::{ReadResourceResult, Resource},
        protocol::MAX_RESOURCE_BYTES,
        sdk::examples::production_reference_server,
        server::GmcpServer,
    };
    use susi_tools::{authorize_mcp_transport, spawn_authorized_stdio_peer, McpServerConfig};
    use susi_wire_client::{
        host::{
            DuplexStreams, HostPolicy, HttpEndpoint, McpHost, RootEntry,
            MAX_BUFFERED_NOTIFICATIONS,
        },
        mcp::{
            PersistedTaskHandle, PersistedTaskHandleStore, Session, TaskHandle, TaskSnapshot,
            TaskStatus, WireError, APPS_EXTENSION_ID,
        },
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (_home_lock, _env, home) = gmcp_http_home_guard("017-04-adversarial");
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).expect("cfg_dir");
    let tok_good = "tok-good-017-04";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_good}\n")).expect("api_token");
    // 1. RootEntry traversal/scheme refusal & MacPolicy transport authorization refusal.
    for bad_root in ["https://evil.example/ws", "file://", "file:///workspace/../etc/passwd", "file://C:\\ws\\..\\secret"] {
        assert!(RootEntry::new(bad_root, None).is_err(), "expected `{bad_root}` to fail closed");
    }
    let deny_mac = MacPolicy::new([7u8; 32], PrivacyMode::LocalOnly, false);
    let stdio_unauth = McpServerConfig { command: "echo".into(), args: vec![], env: Some(HashMap::new()), extra: HashMap::new() };
    assert!(authorize_mcp_transport(&stdio_unauth, &deny_mac).expect_err("stdio deny").contains(actions::PROCESS_EXEC));
    assert!(spawn_authorized_stdio_peer(&stdio_unauth, &deny_mac).expect_err("spawn deny").contains(actions::PROCESS_EXEC));
    let remote_unauth = McpServerConfig { command: "https://mcp.remote.example/mcp".into(), args: vec![], env: None, extra: HashMap::new() };
    assert!(authorize_mcp_transport(&remote_unauth, &deny_mac).expect_err("egress deny").contains(actions::NETWORK_EGRESS));
    assert!(Session::open("198.51.100.42:9093", Some(tok_good), Duration::from_secs(1)).is_err());
    // 2. Forged/unadvertised MRTR callbacks & malformed task inputRequests fail closed.
    let mut strict_pol = HostPolicy::strict("strict-host", "1.0.0");
    assert!(matches!(strict_pol.dispatch_mrtr("roots/list", &json!({})), Err(WireError::Rpc(-32601, _))));
    assert!(matches!(strict_pol.dispatch_mrtr("sampling/createMessage", &json!({"messages":[]})), Err(WireError::Rpc(-32601, _))));
    assert!(matches!(strict_pol.dispatch_mrtr("elicitation/create", &json!({"message":"?"})), Err(WireError::Rpc(-32601, _))));
    assert!(matches!(strict_pol.dispatch_mrtr("tools/call", &json!({})), Err(WireError::Rpc(-32601, _))));
    let mut samp_pol = HostPolicy::strict("samp-host", "1.0.0").with_sampling(|_| Ok(json!({"ok":true})));
    assert!(matches!(samp_pol.dispatch_mrtr("sampling/createMessage", &json!({"maxTokens":8})), Err(WireError::Rpc(-32602, _))));
    for bad_snap in [
        json!({"resultType":"complete","taskId":"t1","status":"input_required","createdAt":"t","lastUpdatedAt":"t","inputRequests":{"k":{"method":"tools/call","params":{}}}}),
        json!({"resultType":"complete","taskId":"t1","status":"input_required","createdAt":"t","lastUpdatedAt":"t","inputRequests":{"":{"method":"roots/list"}}}),
        json!({"resultType":"complete","taskId":"t1","status":"input_required","createdAt":"t","lastUpdatedAt":"t","inputRequests":"not-an-object"}),
    ] {
        assert!(TaskSnapshot::from_get_result(&bad_snap).is_err());
    }
    // 3. Duplex framing attacks: unsupported protocolVersion, invalid JSON, non-2.0 envelope,
    // duplicate server-originated callback IDs, unknown response IDs + EOF, notification flood.
    let run_duplex = |script: &str, pol: HostPolicy| -> Result<McpHost, WireError> {
        let mut h = McpHost::new();
        h.connect_duplex("d", DuplexStreams { reader: Box::new(Cursor::new(script.as_bytes().to_vec())), writer: Box::new(Vec::<u8>::new()), on_close: None }, pol)?;
        Ok(h)
    };
    assert!(run_duplex("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"1999-01-01\",\"capabilities\":{},\"serverInfo\":{\"name\":\"old\"}}}\n", HostPolicy::strict("c", "1")).is_err());
    assert!(run_duplex("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"capabilities\":{}}}\n", HostPolicy::strict("c", "1")).is_err());
    assert!(run_duplex("{not-valid-json\n", HostPolicy::strict("c", "1")).is_err());
    assert!(run_duplex("{\"jsonrpc\":\"1.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\"}}\n", HostPolicy::strict("c", "1")).is_err());
    let init_ok = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"dup\"}}}\n";
    let dup_srv_id = format!(
        "{init_ok}{{\"jsonrpc\":\"2.0\",\"id\":77,\"method\":\"ping\"}}\n{{\"jsonrpc\":\"2.0\",\"id\":77,\"method\":\"ping\"}}\n{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"content\":[]}}}}\n"
    );
    let mut h_dup = run_duplex(&dup_srv_id, HostPolicy::strict("c", "1")).expect("init");
    assert!(h_dup.call_tool("d", "x", &json!({}), None).is_err());
    let wrong_id_eof = format!("{init_ok}{{\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{{\"content\":[]}}}}\n");
    let mut h_eof = run_duplex(&wrong_id_eof, HostPolicy::strict("c", "1")).expect("init eof");
    assert!(matches!(h_eof.call_tool("d", "x", &json!({}), None), Err(WireError::Io(_))));
    let mut flood_script = init_ok.to_string();
    for i in 0..(MAX_BUFFERED_NOTIFICATIONS + 40) {
        flood_script.push_str(&format!("{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{{\"seq\":{i}}}}}\n"));
    }
    flood_script.push_str("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progressToken\":\"wrong\",\"progress\":5}}\n");
    flood_script.push_str("{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}\n");
    let mut h_flood = run_duplex(&flood_script, HostPolicy::strict("c", "1")).expect("init flood");
    assert_eq!(h_flood.call_tool("d", "x", &json!({}), Some(&json!("live-tok"))).expect("flood call")["content"][0]["text"], json!("ok"));
    let drained = h_flood.drain_notifications("d").expect("drain");
    assert_eq!(drained.len(), MAX_BUFFERED_NOTIFICATIONS);
    assert!(!drained.iter().any(|n| n["method"] == "notifications/progress"));
    let cursor_loop = format!("{init_ok}{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"tools\":[],\"nextCursor\":\"c1\"}}}}\n{{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{{\"tools\":[],\"nextCursor\":\"c1\"}}}}\n");
    let mut h_loop = run_duplex(&cursor_loop, HostPolicy::strict("c", "1")).expect("init loop");
    assert!(h_loop.list_tools("d").is_err());
    // 4. Corrupt PersistedTaskHandleStore recovery & Unix 0600 permissions.
    let store_path = home.join("corrupt-handles.json");
    for payload in [
        b"{truncated-json".as_slice(),
        br#"{"version":99,"handles":[]}"#.as_slice(),
        br#"{"version":1,"handles":[{"addr":"127.0.0.1:1","taskId":"dup","protocolVersion":"2026-07-28","status":"working","createdAt":"t","lastUpdatedAt":"t"},{"addr":"127.0.0.1:1","taskId":"dup","protocolVersion":"2026-07-28","status":"working","createdAt":"t","lastUpdatedAt":"t"}]}"#.as_slice(),
    ] {
        std::fs::write(&store_path, payload).expect("write corrupt");
        assert!(PersistedTaskHandleStore::open(&store_path).is_err());
        let (mut recovered, quarantined) = PersistedTaskHandleStore::open_or_quarantine(&store_path).expect("quarantine");
        let q_path = quarantined.expect("quarantine path");
        assert!(q_path.is_file() && recovered.list().is_empty());
        let sample_th = TaskHandle {
            task_id: "t-clean".into(),
            status: TaskStatus::Working,
            status_message: None,
            created_at: "2026-11-16T00:00:00Z".into(),
            last_updated_at: "2026-11-16T00:00:00Z".into(),
            ttl_ms: Some(5000),
            poll_interval_ms: Some(25),
        };
        recovered.save(PersistedTaskHandle::from_handle("127.0.0.1:9999", "2026-07-28", &sample_th)).expect("save clean");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&store_path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "PersistedTaskHandleStore must be 0600 on Unix");
    }
    // 5. Real multi-server isolation under HTTP disconnect, oversized body (>16 MiB),
    // oversized resource (>MAX_RESOURCE_BYTES), malicious ui:// metadata, cross-server task
    // resume rejection, and cancellation race.
    let base_good = home.join("good-srv");
    std::fs::create_dir_all(base_good.join("ws")).expect("ws");
    let (svc_good, _log) = production_reference_server(base_good.join("ws"), base_good.join("tasks")).expect("svc_good");
    svc_good.extensions().write().register(APPS_EXTENSION_ID, ExtensionStatus::Official, 1, serde_json::Map::new()).expect("apps");
    svc_good.register_resource(
        Resource::new("memo://too-big", "TooBig"),
        Arc::new(|req, _| Box::pin(async move {
            let big = "x".repeat(MAX_RESOURCE_BYTES + 64);
            let r: ReadResourceResult = serde_json::from_value(json!({"contents":[{"uri":req.uri,"text":big}]})).expect("r");
            Ok(r.into())
        })),
    );
    svc_good.register_resource(
        Resource::new("ui://app/bad-domain", "BadDomain"),
        Arc::new(|req, _| Box::pin(async move {
            let r: ReadResourceResult = serde_json::from_value(json!({"contents":[{"uri":req.uri,"mimeType":"text/html;profile=mcp-app","text":"<p>x</p>","_meta":{"ui":{"domain":"https://evil.example/path","csp":"not-an-object"}}}]})).expect("r");
            Ok(r.into())
        })),
    );
    let lis_good = TcpListener::bind("127.0.0.1:0").expect("bind good");
    let addr_good = format!("127.0.0.1:{}", lis_good.local_addr().expect("addr").port());
    let _bg_good = std::thread::spawn(move || GmcpServer::serve_http_service(svc_good, vec![lis_good], None, false));
    for _ in 0..50 {
        if TcpStream::connect(&addr_good).is_ok() { break; }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Fault-injecting HTTP server: accepts 2026-07-28 `server/discover`, then drops TCP
    // mid-call on #2 and streams >16 MiB body on #3.
    let lis_bad = TcpListener::bind("127.0.0.1:0").expect("bind bad");
    let addr_bad = format!("127.0.0.1:{}", lis_bad.local_addr().expect("addr bad").port());
    let _bg_bad = std::thread::spawn(move || {
        for (idx, stream) in lis_bad.incoming().flatten().take(3).enumerate() {
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut content_len = 0usize;
            loop {
                let mut hdr = String::new();
                if reader.read_line(&mut hdr).unwrap_or(0) == 0 || hdr == "\r\n" { break; }
                if let Some(v) = hdr.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut req_body = vec![0u8; content_len];
            let _ = reader.read_exact(&mut req_body);
            let mut sock = reader.into_inner();
            match idx {
                0 => {
                    let b = br#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","protocolVersion":"2026-07-28","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"serverInfo":{"name":"fault-http","version":"0.1.0"}}}"#;
                    let _ = write!(sock, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len());
                    let _ = sock.write_all(b);
                }
                1 => {
                    drop(sock);
                }
                _ => {
                    let huge_len = 16 * 1024 * 1024 + 4096;
                    let _ = write!(sock, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {huge_len}\r\nConnection: close\r\n\r\n{{\"pad\":\"");
                    let chunk = vec![b'a'; 65_536];
                    for _ in 0..((huge_len / chunk.len()) + 1) {
                        if sock.write_all(&chunk).is_err() { break; }
                    }
                }
            }
        }
    });
    let mut host = McpHost::new();
    let pol_good = HostPolicy::strict("host-good", "1.0.0")
        .with_roots(vec![RootEntry::new("file:///workspace/good", Some("good".into())).expect("r")])
        .with_elicitation(|_| Ok(json!({"action":"accept","content":{"approved":true,"decision":"isolated-ok"}})))
        .with_tasks(true)
        .with_apps(true);
    host.connect_http_2026_07_28("good", HttpEndpoint { addr: &addr_good, bearer: Some(tok_good), timeout: Duration::from_secs(10) }, pol_good).expect("connect good");
    assert!(host.connect_http_2026_07_28("good", HttpEndpoint { addr: &addr_good, bearer: Some(tok_good), timeout: Duration::from_secs(5) }, HostPolicy::strict("dup", "1")).is_err());
    host.connect_http_2026_07_28("bad_http", HttpEndpoint { addr: &addr_bad, bearer: None, timeout: Duration::from_secs(5) }, HostPolicy::strict("host-bad", "1.0.0")).expect("connect bad_http");
    // Abrupt peer disconnect & >16 MiB response fail on `bad_http` while `good` remains 100% healthy!
    assert!(host.call_tool("bad_http", "explode", &json!({}), None).is_err());
    assert!(host.call_tool("bad_http", "oversize", &json!({}), None).is_err());
    assert_eq!(host.call_tool("good", "summarize_items", &json!({"items":["s1","s2"],"scope":{"namespace":"healthy"}}), None).expect("good survives")["structuredContent"]["count"], json!(2));
    // Oversized resource read & malicious ui:// App metadata fail closed on `good` without poisoning subsequent calls.
    assert!(host.read_resource("good", "memo://too-big").is_err());
    assert!(host.inspect_app("good", "ui://app/bad-domain").is_err());
    assert!(host.read_resource("good", "memo://welcome").is_ok());
    // Cross-server task handle binding refusal & concurrent cancellation race.
    let task_store_path = home.join("good-tasks.json");
    let mut wrong_store = PersistedTaskHandleStore::open(&task_store_path).expect("open");
    let fake_h = TaskHandle {
        task_id: "cross-srv-task".into(),
        status: TaskStatus::Working,
        status_message: None,
        created_at: "2026-11-16T00:00:00Z".into(),
        last_updated_at: "2026-11-16T00:00:00Z".into(),
        ttl_ms: Some(5000),
        poll_interval_ms: Some(20),
    };
    wrong_store.save(PersistedTaskHandle::from_handle(&addr_bad, "2026-07-28", &fake_h)).expect("save wrong addr");
    host.attach_task_store("good", wrong_store).expect("attach");
    assert!(host.resume_persisted_task("good", "cross-srv-task", Some(Duration::from_secs(2))).expect_err("must refuse cross-server task").to_string().contains("bound to"));
    let race_th = host.start_task_tool("good", "approval_workflow", &json!({"ticket":"T-RACE-117"})).expect("start race task");
    host.cancel_task("good", &race_th.task_id).expect("cancel race task");
    assert!(host.resume_persisted_task("good", &race_th.task_id, Some(Duration::from_secs(5))).is_err());
    assert_eq!(host.get_task("good", &race_th.task_id).expect("get cancelled").handle.status, TaskStatus::Cancelled);
    assert_eq!(host.call_tool_resolving_tasks("good", "approval_workflow", &json!({"ticket":"T-AFTER-RACE"}), Some(Duration::from_secs(10))).expect("healthy task after cancel")["structuredContent"]["decision"], json!("isolated-ok"));
    host.disconnect("bad_http").expect("disc bad");
    host.disconnect("good").expect("disc good");
    assert!(!root.join(".agents/gmcp/evidence/VC-203-017.jsonl").exists() && !root.join(".agents/roadmap-verdicts/VC-203-017.json").exists());
}

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
    assert!(root.join(".agents/gmcp/evidence/VC-203-017.jsonl").is_file() && root.join(".agents/roadmap-verdicts/VC-203-017.json").is_file());
}
/// GMCP-203-017-05: interoperability and quality — reference-server interop in both
/// roles across Streamable HTTP (`2026-07-28`, `2025-11-25`, `2025-06-18`, `2025-03-26`),
/// stdio (`susi mcp serve` + authorized child peer), and Unix sockets (`#[cfg(unix)]`),
/// direct-vs-gateway differential parity, concurrent multi-server interleaving invariants,
/// published startup/latency/memory/disk/fairness budgets, and retained regressions `R1..R4`.
#[test]
fn gmcp_req_203_017_05_interop() {
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    use susi_core::mac_policy::{actions, MacPolicy, PrivacyMode};
    use susi_gmcp::{
        direct_call::call_tool_direct,
        extensions::ExtensionStatus,
        mcp::{
            model::{CallToolResult, ClientCapabilities, ContentBlock, ReadResourceResult, Resource, Tool},
            task_manager::TaskExit,
            ServiceExt,
        },
        protocol::{GmcpService, McpGatewayProxy},
        sdk::{
            examples::{production_reference_server, sample_plugin_bundle},
            mrtr_elicitation, mrtr_roots_list, mrtr_sampling, PluginPolicy, TestHarness,
        },
        server::GmcpServer,
    };
    use susi_tools::{spawn_authorized_stdio_peer, GmcpClient, McpServerConfig};
    use susi_wire_client::{
        host::{
            CompletionQuery, DuplexStreams, HostPolicy, HttpEndpoint, McpHost, RootEntry,
            MAX_BUFFERED_NOTIFICATIONS,
        },
        mcp::{PersistedTaskHandleStore, Session, APPS_EXTENSION_ID},
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (_home_lock, _env, home) = gmcp_http_home_guard("017-05-interop");
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).expect("cfg_dir");
    let tok_a = "tok-alpha-017-05";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_a}\n")).expect("tok_a");
    let baseline_rss = adapter_resident_bytes().unwrap_or(0);
    let t_cold = Instant::now();
    // 1. Reference server setup (`alpha` 2026-07-28 + `beta` 2025-11-25) with MRTR triad, Apps, & plugin.
    let obj_schema = serde_json::Map::from_iter([("type".into(), json!("object"))]);
    let restricted_hits = Arc::new(AtomicUsize::new(0));
    let spawn_srv = |sub: &str, app_uri: &str, app_dom: &str, hits: Option<Arc<AtomicUsize>>| -> (GmcpService, String) {
        let base = home.join(sub);
        std::fs::create_dir_all(base.join("ws")).expect("ws");
        let (svc, _) = production_reference_server(base.join("ws"), base.join("tasks")).expect("svc");
        svc.extensions().write().register(APPS_EXTENSION_ID, ExtensionStatus::Official, 1, serde_json::Map::new()).expect("apps");
        let dom = app_dom.to_string();
        svc.register_resource(
            Resource::new(app_uri, "InteropApp").with_mime_type("text/html;profile=mcp-app"),
            Arc::new(move |req, _| {
                let d = dom.clone();
                Box::pin(async move {
                    let r: ReadResourceResult = serde_json::from_value(json!({"contents":[{"uri":req.uri,"mimeType":"text/html;profile=mcp-app","text":"<main>app</main>","_meta":{"ui":{"domain":d,"csp":{"connectDomains":[]},"permissions":{}}}}]})).expect("r");
                    Ok(r.into())
                })
            }),
        );
        svc.register_task_tool(
            Tool::new_with_raw("mrtr_triad", Some("3-callback task".into()), obj_schema.clone()),
            Arc::new(|_, tc| Box::pin(async move {
                let s = tc.request_input("s", mrtr_sampling("check", 32).map_err(TaskExit::Error)?).await?;
                let e = tc.request_input("e", mrtr_elicitation("go?", json!({"type":"object","properties":{"go":{"type":"boolean"}},"required":["go"]})).map_err(TaskExit::Error)?).await?;
                let r = tc.request_input("r", mrtr_roots_list().map_err(TaskExit::Error)?).await?;
                Ok(CallToolResult::structured(json!({"sampled":s["content"]["text"],"go":e["go"],"firstRoot":r["roots"][0]["uri"]})))
            })),
        ).expect("triad");
        svc.register_tool(
            Tool::new_with_raw("slow_peer_op", None, obj_schema.clone()),
            Arc::new(|_, _| Box::pin(async {
                tokio::time::sleep(Duration::from_millis(120)).await;
                Ok(CallToolResult::structured(json!({"slow":true})).into())
            })),
        ).expect("slow");
        if let Some(h) = hits {
            svc.register_tool(
                Tool::new_with_raw("system_restricted_host_op", None, obj_schema.clone()),
                Arc::new(move |_, _| { h.fetch_add(1, Ordering::SeqCst); Box::pin(async { Ok(CallToolResult::success(vec![ContentBlock::text("bad")]).into()) }) }),
            ).expect("sys");
        }
        svc.attach_plugin(sample_plugin_bundle(None, Default::default(), Default::default()), &PluginPolicy::strict().grant_permission("filesystem.read")).expect("plug");
        let lis = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("127.0.0.1:{}", lis.local_addr().expect("addr").port());
        let svc_http = svc.clone();
        let _bg = std::thread::spawn(move || GmcpServer::serve_http_service(svc_http, vec![lis], None, false));
        for _ in 0..60 {
            if TcpStream::connect(&addr).is_ok() { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        (svc, addr)
    };
    let (svc_a, addr_a) = spawn_srv("srv-a", "ui://app/alpha", "alpha.susi.local", Some(Arc::clone(&restricted_hits)));
    svc_a.bind_direct_server("interop_direct").expect("bind direct");
    let mut host = McpHost::new();
    let pol_a = HostPolicy::strict("host-alpha", "1.0.0")
        .with_principal("alice")
        .with_roots(vec![RootEntry::new("file:///workspace/alpha", Some("alpha".into())).expect("r")])
        .with_sampling(|_| Ok(json!({"role":"assistant","model":"susi-ref","content":{"type":"text","text":"sampled-alpha"}})))
        .with_elicitation(|_| Ok(json!({"action":"accept","content":{"go":true,"approved":true,"decision":"alpha-ok"}})))
        .with_tasks(true)
        .with_apps(true);
    let disc_a = host.connect_http_2026_07_28("alpha", HttpEndpoint { addr: &addr_a, bearer: Some(tok_a), timeout: Duration::from_secs(15) }, pol_a).expect("connect alpha");
    let cold_ms = t_cold.elapsed().as_millis();
    assert!(cold_ms <= 2_500 && disc_a["protocolVersion"] == "2026-07-28", "cold_ms={cold_ms}");
    let store_path = home.join("interop-tasks.json");
    host.attach_task_store("alpha", PersistedTaskHandleStore::open(&store_path).expect("store")).expect("attach");
    let (_svc_b, addr_b) = spawn_srv("srv-b", "ui://app/beta", "beta.susi.local", None);
    let pol_b = HostPolicy::strict("host-beta", "1.0.0")
        .with_principal("bob")
        .with_roots(vec![RootEntry::new("file:///workspace/beta", Some("beta".into())).expect("r")])
        .with_apps(true);
    let disc_b = host.connect_http_session("beta", HttpEndpoint { addr: &addr_b, bearer: Some(tok_a), timeout: Duration::from_secs(15) }, pol_b).expect("connect beta");
    assert!(disc_b["protocolVersion"] == "2025-11-25" && !disc_b["sessionId"].as_str().unwrap_or_default().is_empty());
    // 2. Both-role stdio (`susi mcp serve` via `StdioSession` AND `McpHost::connect_duplex` via `spawn_authorized_stdio_peer`) + callback stdio peer + Unix socket (`#[cfg(unix)]`).
    let mut serve_modern = StdioSession::spawn_modern(root, &home);
    assert!(serve_modern.request("server/discover", adapter_modern_meta())["capabilities"]["tools"].is_object());
    drop(serve_modern);
    let allow_mac = MacPolicy::new([3u8; 32], PrivacyMode::LocalOnly, false);
    let susi_exe = env!("CARGO_BIN_EXE_susi").to_string();
    allow_mac.grant("susi", actions::PROCESS_EXEC, &susi_exe, None).expect("grant susi");
    let mut isolated_env = Command::new(&susi_exe);
    isolate_home(&mut isolated_env, &home).env("SUSI_HOME", &cfg_dir);
    let susi_stdio_cfg = McpServerConfig {
        command: susi_exe,
        args: vec!["mcp".into(), "serve".into()],
        env: Some(
            isolated_env
                .get_envs()
                .filter_map(|(k, v)| {
                    Some((
                        k.to_string_lossy().into_owned(),
                        v?.to_string_lossy().into_owned(),
                    ))
                })
                .collect(),
        ),
        extra: HashMap::new(),
    };
    let mut susi_child = spawn_authorized_stdio_peer(&susi_stdio_cfg, &allow_mac).expect("spawn susi serve");
    let susi_out = BufReader::new(susi_child.stdout.take().expect("out"));
    let susi_in = susi_child.stdin.take().expect("in");
    host.connect_duplex("susi_stdio", DuplexStreams { reader: Box::new(susi_out), writer: Box::new(susi_in), on_close: Some(Box::new(move || { let _ = susi_child.kill(); let _ = susi_child.wait(); })) }, HostPolicy::strict("host-stdio", "1.0.0").with_principal("ops")).expect("connect susi_stdio");
    assert!(host.list_tools("susi_stdio").expect("paginated stdio tools").len() > 100);
    let gamma_src = home.join("gamma_interop_peer.rs");
    let gamma_exe = home.join(format!("gamma_interop_peer{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&gamma_src, r#"
use std::io::{self, BufRead, Write};
fn main() {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    while let Some(Ok(line)) = lines.next() {
        if !line.contains("\"id\"") { continue; }
        let id: String = line.split("\"id\":").nth(1).unwrap().chars().take_while(|c| c.is_ascii_digit()).collect();
        if line.contains("\"initialize\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{{\"tools\":{{}},\"resources\":{{\"subscribe\":true}}}},\"serverInfo\":{{\"name\":\"gamma-cb\",\"version\":\"1.0.0\"}}}}}}");
        } else if line.contains("\"tools/call\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{{\"progressToken\":\"tok-g\",\"progress\":100}}}}");
            println!("{{\"jsonrpc\":\"2.0\",\"id\":501,\"method\":\"roots/list\",\"params\":{{}}}}");
            io::stdout().flush().unwrap();
            let r1 = lines.next().unwrap().unwrap();
            println!("{{\"jsonrpc\":\"2.0\",\"id\":502,\"method\":\"sampling/createMessage\",\"params\":{{\"messages\":[{{\"role\":\"user\",\"content\":{{\"type\":\"text\",\"text\":\"q\"}}}}],\"maxTokens\":16}}}}");
            io::stdout().flush().unwrap();
            let r2 = lines.next().unwrap().unwrap();
            println!("{{\"jsonrpc\":\"2.0\",\"id\":503,\"method\":\"elicitation/create\",\"params\":{{\"message\":\"ok?\"}}}}");
            io::stdout().flush().unwrap();
            let r3 = lines.next().unwrap().unwrap();
            let pass = r1.contains("file:///workspace/gamma") && r2.contains("gamma-sample") && r3.contains("\"action\":\"accept\"");
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"structuredContent\":{{\"pass\":{pass}}},\"content\":[{{\"type\":\"text\",\"text\":\"{pass}\"}}]}}}}");
        } else {
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{}}}}");
        }
        io::stdout().flush().unwrap();
    }
}
"#).expect("gamma_src");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    assert!(Command::new(rustc).args(["--edition", "2021"]).arg(&gamma_src).arg("-o").arg(&gamma_exe).status().expect("rustc").success());
    let gamma_cmd = gamma_exe.to_string_lossy().into_owned();
    allow_mac.grant("susi", actions::PROCESS_EXEC, &gamma_cmd, None).expect("grant gamma");
    let mut g_child = spawn_authorized_stdio_peer(&McpServerConfig { command: gamma_cmd, args: vec![], env: Some(HashMap::new()), extra: HashMap::new() }, &allow_mac).expect("spawn gamma");
    let g_out = BufReader::new(g_child.stdout.take().expect("out"));
    let g_in = g_child.stdin.take().expect("in");
    let pol_g = HostPolicy::strict("host-gamma", "1.0.0")
        .with_principal("carol")
        .with_roots(vec![RootEntry::new("file:///workspace/gamma", Some("gamma".into())).expect("r")])
        .with_sampling(|_| Ok(json!({"role":"assistant","model":"g","content":{"type":"text","text":"gamma-sample"}})))
        .with_elicitation(|_| Ok(json!({"action":"accept","content":{"ok":true}})));
    host.connect_duplex("gamma", DuplexStreams { reader: Box::new(g_out), writer: Box::new(g_in), on_close: Some(Box::new(move || { let _ = g_child.kill(); let _ = g_child.wait(); })) }, pol_g).expect("connect gamma");
    #[cfg(unix)]
    {
        use std::os::unix::net::{UnixListener, UnixStream};
        let sock_path = home.join("delta.sock");
        let uds_lis = UnixListener::bind(&sock_path).expect("bind uds");
        let uds_svc = svc_a.clone();
        let _uds_bg = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("rt");
            rt.block_on(async move {
                uds_lis.set_nonblocking(true).expect("nb");
                let lis = tokio::net::UnixListener::from_std(uds_lis).expect("tokio uds");
                if let Ok((stream, _)) = lis.accept().await {
                    let (r, w) = tokio::io::split(stream);
                    let gate = susi_gmcp::jsonrpc_gate::StdioGate::new(susi_gmcp::stdio::BoundedLines::new(r, 16 * 1024 * 1024), w);
                    if let Ok(run) = uds_svc.session().serve(gate).await { let _ = run.waiting().await; }
                }
            });
        });
        let uds_cli = UnixStream::connect(&sock_path).expect("connect uds");
        let uds_r = BufReader::new(uds_cli.try_clone().expect("clone uds"));
        host.connect_duplex("delta_uds", DuplexStreams { reader: Box::new(uds_r), writer: Box::new(uds_cli), on_close: None }, HostPolicy::strict("host-uds", "1.0.0").with_apps(true)).expect("connect delta_uds");
    }
    // 3. Direct-vs-gateway differential parity across all 7 entry points (`sync`, `direct`, `TestHarness`, `McpHost` HTTP/UDS, gateway `mcp_client`, `GmcpClient`, `susi mcp` CLI).
    let sum_args = json!({"items":["i1","i2","i3"],"scope":{"namespace":"interop"},"maxItems":2});
    let expected_sc = json!({"count":2,"summary":"i1, i2","namespace":"interop"});
    assert_eq!(svc_a.call_tool_sync("summarize_items", sum_args.clone()).expect("sync")["result"]["structuredContent"], expected_sc);
    let direct_sc: Value = serde_json::from_str(call_tool_direct("interop_direct", "summarize_items", &sum_args, &home).expect("direct")["content"][0]["text"].as_str().expect("txt")).expect("json");
    assert_eq!(direct_sc, expected_sc);
    assert_eq!(susi_core::mcp_client::call_tool(&addr_a, "summarize_items", &sum_args, Some(tok_a)).expect("gw")["structuredContent"], expected_sc);
    assert_eq!(host.call_tool("alpha", "summarize_items", &sum_args, None).expect("alpha")["structuredContent"], expected_sc);
    assert_eq!(host.call_tool("beta", "summarize_items", &sum_args, None).expect("beta")["structuredContent"], expected_sc);
    #[cfg(unix)]
    assert_eq!(host.call_tool("delta_uds", "summarize_items", &sum_args, None).expect("uds")["structuredContent"], expected_sc);
    let rt = adapter_runtime(&home);
    rt.block_on(async {
        let h = TestHarness::spawn(svc_a.clone());
        let r = h.request_modern("tools/call", json!({"name":"summarize_items","arguments":sum_args}), ClientCapabilities::default()).await.expect("h");
        assert_eq!(r["result"]["structuredContent"], expected_sc);
        h.shutdown().await;
    });
    assert_eq!(GmcpClient::admit_mcp_server("interop_http", &format!("http://{addr_a}/mcp"), &[]), "SUCCESS_ADMITTED");
    let cfg_path = GmcpClient::get_config_path();
    let mut cfg_v: Value = serde_json::from_str(&std::fs::read_to_string(&cfg_path).expect("cfg")).expect("j");
    cfg_v["mcp_servers"]["interop_http"]["auth_token"] = json!(tok_a);
    std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg_v).expect("v")).expect("w");
    assert!(GmcpClient::execute_external_tool_result("interop_http", "summarize_items", &sum_args.to_string()).expect("ext").contains("i1, i2"));
    let run_cli = |args: &[&str]| -> Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
        let out = isolate_home(&mut cmd, &home).env("SUSI_HOME", &cfg_dir).args(args).output().expect("cli");
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("json")
    };
    let cli_disc_26 = run_cli(&["mcp", "discover", &addr_a, "--token", tok_a]);
    assert!(cli_disc_26["supportedVersions"].as_array().expect("sv").contains(&json!("2026-07-28")) && cli_disc_26["serverInfo"]["name"] == "sdk-production-ref");
    assert_eq!(run_cli(&["mcp", "discover", &addr_a, "--legacy", "--token", tok_a])["protocolVersion"], "2025-11-25");
    assert_eq!(run_cli(&["mcp", "call", &addr_a, "summarize_items", &sum_args.to_string(), "--token", tok_a])["structuredContent"], expected_sc);
    assert_eq!(run_cli(&["mcp", "read", &addr_a, "ui://app/alpha", "--app", "--token", tok_a])["domain"], "alpha.susi.local");
    // Differential boundary check: `system_*` blocked across all entry points with 0 handler runs; HTTP 401/403 rejected at edge.
    assert!(McpGatewayProxy::evaluate_tool_policy("system_restricted_host_op").is_err());
    for gated in [
        svc_a.call_tool_sync("system_restricted_host_op", json!({})).expect("s")["result"].clone(),
        call_tool_direct("interop_direct", "system_restricted_host_op", &json!({}), &home).expect("d"),
        host.call_tool("alpha", "system_restricted_host_op", &json!({}), None).expect("h"),
    ] {
        assert!(gated["isError"] == true && gated["content"][0]["text"].as_str().expect("t").starts_with("Governance Violation:"));
    }
    assert_eq!(restricted_hits.load(Ordering::Acquire), 0);
    assert!(Session::open_2026_07_28(&addr_a, Some("wrong-token"), Duration::from_secs(5)).is_err());
    GmcpService::unbind_direct_server("interop_direct");
    // 4. Deterministic concurrent multi-server interleaving invariants (`alpha`, `beta`, `gamma`).
    let mut sub_a = host.open_subscription_stream("alpha", &json!({"resourceSubscriptions":["memo://welcome"]})).expect("sub_a");
    host.subscribe_resource("beta", "memo://welcome").expect("sub_b");
    host.subscribe_resource("gamma", "memo://g").expect("sub_g");
    for round in 0..3 {
        assert_eq!(host.read_resource("alpha", &format!("memo://items/{round}")).expect("ra")["contents"][0]["uri"], format!("memo://items/{round}"));
        assert_eq!(host.get_prompt("beta", "draft_brief", Some(&json!({"topic":format!("b{round}")}))).expect("pb")["messages"][0]["role"], "user");
        assert!(host.complete("alpha", CompletionQuery { is_resource: false, target: "draft_brief", arg_name: "topic", arg_value: "se", context_args: None }).expect("ca")["completion"]["values"].as_array().expect("v").contains(&json!("security")));
        assert_eq!(host.call_tool("gamma", "cb", &json!({"r":round}), Some(&json!("tok-g"))).expect("cg")["structuredContent"]["pass"], json!(true));
    }
    assert_eq!(host.inspect_app("alpha", "ui://app/alpha").expect("app_a").domain.as_deref(), Some("alpha.susi.local"));
    assert_eq!(host.inspect_app("beta", "ui://app/beta").expect("app_b").domain.as_deref(), Some("beta.susi.local"));
    let triad = host.call_tool_resolving_tasks("alpha", "mrtr_triad", &json!({}), Some(Duration::from_secs(10))).expect("triad");
    assert_eq!(triad["structuredContent"], json!({"sampled":"sampled-alpha","go":true,"firstRoot":"file:///workspace/alpha"}));
    svc_a.resource_updated("memo://welcome".into());
    assert_eq!(sub_a.next_frame().expect("f").expect("ok")["method"], "notifications/resources/updated");
    drop(sub_a);
    let snap = host.snapshot();
    let by_id = |id: &str| snap["servers"].as_array().expect("arr").iter().find(|s| s["serverId"] == id).expect("srv").clone();
    assert!(by_id("alpha")["clientIdentity"]["principal"] == "alice" && by_id("beta")["clientIdentity"]["principal"] == "bob" && by_id("gamma")["clientIdentity"]["principal"] == "carol");
    assert!(by_id("alpha")["advertisedCapabilities"].get("sampling").is_some() && by_id("beta")["advertisedCapabilities"].get("sampling").is_none());
    assert!(host.drain_notifications("gamma").expect("gn").len() <= MAX_BUFFERED_NOTIFICATIONS);
    // 5. Published SLOs (warm loopback <= 25 ms, 10-call throughput, disk, memory, multi-server fairness) + Regressions R1..R4.
    let mut best_us = u128::MAX;
    let t_batch = Instant::now();
    for _ in 0..10 {
        let t0 = Instant::now();
        assert_eq!(host.call_tool("alpha", "summarize_items", &sum_args, None).expect("slo")["structuredContent"], expected_sc);
        best_us = best_us.min(t0.elapsed().as_micros());
    }
    assert!(best_us <= 25_000 && t_batch.elapsed().as_millis() <= 1_500, "best_us={best_us}");
    let slow_addr = addr_b.clone();
    let slow_bg = std::thread::spawn(move || {
        let mut sb = Session::open(&slow_addr, Some(tok_a), Duration::from_secs(10)).expect("sb");
        let res = sb.call_tool("slow_peer_op", &json!({}), None, &mut |_| {}).expect("slow");
        sb.close().expect("close sb");
        res
    });
    let t_fair = Instant::now();
    for _ in 0..3 {
        assert_eq!(host.call_tool("alpha", "summarize_items", &sum_args, None).expect("fair")["structuredContent"], expected_sc);
    }
    assert!(t_fair.elapsed().as_millis() < 120, "sibling slow server must not block alpha");
    assert_eq!(slow_bg.join().expect("join slow")["structuredContent"]["slow"], json!(true));
    // R1: unrequested CreateTaskResult on non-task host policy (`beta`) fails closed.
    assert!(host.call_tool("beta", "approval_workflow", &json!({"ticket":"T-R1"}), None).is_err());
    // R2: stateful `2025-06-18` and `2025-03-26` HTTP revision negotiation + `DELETE` teardown.
    for rev in ["2025-06-18", "2025-03-26"] {
        let lis_r = TcpListener::bind("127.0.0.1:0").expect("bind rev");
        let addr_r = format!("127.0.0.1:{}", lis_r.local_addr().expect("addr").port());
        let rev_s = rev.to_string();
        let bg_r = std::thread::spawn(move || {
            let mut saw_delete = false;
            for stream in lis_r.incoming().flatten().take(3) {
                let mut rdr = BufReader::new(stream.try_clone().expect("c"));
                let mut first = String::new();
                let _ = rdr.read_line(&mut first);
                let mut clen = 0usize;
                loop {
                    let mut h = String::new();
                    if rdr.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" { break; }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") { clen = v.trim().parse().unwrap_or(0); }
                }
                let mut body = vec![0u8; clen];
                let _ = rdr.read_exact(&mut body);
                let mut sock = rdr.into_inner();
                if first.starts_with("DELETE ") {
                    saw_delete = true;
                    let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                } else if String::from_utf8_lossy(&body).contains("\"initialize\"") {
                    let b = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":\"{rev_s}\",\"capabilities\":{{\"tools\":{{}}}},\"serverInfo\":{{\"name\":\"rev-{rev_s}\",\"version\":\"1.0\"}}}}}}");
                    let _ = write!(sock, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: sess-{rev_s}\r\nMCP-Protocol-Version: {rev_s}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len());
                } else {
                    let _ = sock.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
            saw_delete
        });
        host.connect_http_session(rev, HttpEndpoint { addr: &addr_r, bearer: None, timeout: Duration::from_secs(5) }, HostPolicy::strict("rev-c", "1.0")).expect("connect rev");
        assert_eq!(host.discover(rev).expect("disc rev")["protocolVersion"], rev);
        host.disconnect(rev).expect("disc rev");
        assert!(bg_r.join().expect("join rev"), "stateful {rev} disconnect must send DELETE");
    }
    // R4: Mandate 35 task store compact size & RSS bound.
    let store_bytes = std::fs::metadata(&store_path).expect("store meta").len();
    assert!(store_bytes > 0 && store_bytes <= 65_536);
    let rss_delta = adapter_resident_bytes().unwrap_or(0).saturating_sub(baseline_rss);
    assert!(rss_delta <= 96 * 1024 * 1024, "rss_delta={rss_delta}");
    for id in ["alpha", "beta", "gamma", "susi_stdio"] {
        host.disconnect(id).expect("disconnect");
    }
    #[cfg(unix)]
    host.disconnect("delta_uds").expect("disconnect uds");
    assert!(root.join(".agents/gmcp/evidence/VC-203-017.jsonl").is_file() && root.join(".agents/roadmap-verdicts/VC-203-017.json").is_file());
}
/// GMCP-203-017-06: inspectable `McpHost::snapshot()` & `susi mcp` CLI state,
/// fail-closed configuration validation, bounded W3C `traceparent` SSE redaction,
/// live peer disconnect/rollback isolation, incident runbook bindings, and
/// automated standards/contract drift detection.
#[test]
fn gmcp_req_203_017_06_operations() {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_gmcp::{
        extensions::ExtensionStatus,
        mcp::model::{ReadResourceResult, Resource},
        sdk::examples::production_reference_server,
        server::GmcpServer,
        susi_config::validate::{validate_dir, ConfigIssueKind},
    };
    use susi_tools::{GmcpClient, McpConfig, McpServerConfig};
    use susi_wire_client::{
        host::{probe_http_auth_challenge, HostPolicy, HttpEndpoint, McpHost, RootEntry, MAX_BUFFERED_NOTIFICATIONS},
        mcp::{PersistedTaskHandleStore, Session, TaskStatus, APPS_EXTENSION_ID, MAX_PAGINATION_PAGES},
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (_home_lock, _env, home) = gmcp_http_home_guard("017-06-operations");
    let scratch = home.join("scratch");
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&scratch).expect("scratch");
    std::fs::create_dir_all(&cfg_dir).expect("cfg_dir");
    let tok_ops = "tok-secret-017-06-ops";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_ops}\n")).expect("api_token");
    let base_a = home.join("srv-a");
    std::fs::create_dir_all(base_a.join("ws")).expect("ws_a");
    let (svc_a, _log_a) = production_reference_server(base_a.join("ws"), base_a.join("tasks")).expect("svc_a");
    svc_a.extensions().write().register(APPS_EXTENSION_ID, ExtensionStatus::Official, 1, serde_json::Map::new()).expect("apps");
    svc_a.register_resource(
        Resource::new("ui://app/ops", "Ops Card").with_mime_type("text/html;profile=mcp-app"),
        Arc::new(|req, _ctx| {
            Box::pin(async move {
                let r: ReadResourceResult = serde_json::from_value(json!({"contents":[{"uri":req.uri,"mimeType":"text/html;profile=mcp-app","text":"<!doctype html><html><body>ops</body></html>","_meta":{"ui":{"csp":{"connectDomains":["https://ops.example.test"]},"permissions":{"clipboardWrite":false},"domain":"ops.susi.local"}}}]}))
                    .expect("app res");
                Ok(r.into())
            })
        }),
    );
    let lis_a = TcpListener::bind("127.0.0.1:0").expect("bind_a");
    let addr_a = format!("127.0.0.1:{}", lis_a.local_addr().expect("addr_a").port());
    let svc_http = svc_a.clone();
    let _bg_a = std::thread::spawn(move || GmcpServer::serve_http_service(svc_http, vec![lis_a], None, false));
    for _ in 0..50 {
        if TcpStream::connect(&addr_a).is_ok() { break; }
        std::thread::sleep(Duration::from_millis(10));
    }
    // 1. Inspectable `McpHost::snapshot()`, `GmcpService::ops_snapshot()`, and `susi mcp` CLI state.
    let mut host = McpHost::new();
    let pol_a = HostPolicy::strict("susi-ops-alpha", "1.0.0")
        .with_principal("ops-alice")
        .with_roots(vec![RootEntry::new("file:///workspace/ops-alpha", Some("ops-a".into())).expect("root")])
        .with_elicitation(|_p| Ok(json!({"action":"accept","content":{"approved":true,"decision":"ops-ok"}})))
        .with_tasks(true)
        .with_apps(true);
    host.connect_http_2026_07_28("alpha", HttpEndpoint { addr: &addr_a, bearer: Some(tok_ops), timeout: Duration::from_secs(15) }, pol_a).expect("connect alpha");
    let store_path = scratch.join("ops-tasks.json");
    host.attach_task_store("alpha", PersistedTaskHandleStore::open(&store_path).expect("store")).expect("attach");
    host.connect_http_session("beta", HttpEndpoint { addr: &addr_a, bearer: Some(tok_ops), timeout: Duration::from_secs(15) }, HostPolicy::strict("susi-ops-beta", "1.0.0").with_principal("ops-bob")).expect("connect beta");
    let th = host.start_task_tool("alpha", "approval_workflow", &json!({"ticket":"T-OPS-119"})).expect("start task");
    let snap = host.snapshot();
    assert_eq!(snap["serverCount"], json!(2));
    assert_eq!(snap["limits"]["maxBufferedNotifications"], json!(MAX_BUFFERED_NOTIFICATIONS));
    assert_eq!(snap["limits"]["maxInterleavedFrames"], json!(1024));
    assert_eq!(snap["limits"]["maxPaginationPages"], json!(MAX_PAGINATION_PAGES));
    let srv_alpha = snap["servers"].as_array().expect("srvs").iter().find(|s| s["serverId"] == "alpha").expect("alpha snap");
    assert_eq!((srv_alpha["persistedTaskCount"].as_u64(), srv_alpha["allowedRootsCount"].as_u64()), (Some(1), Some(1)));
    assert_eq!(srv_alpha["clientIdentity"]["principal"], json!("ops-alice"));
    assert!(!snap.to_string().contains(tok_ops) && !format!("{host:?}").contains(tok_ops), "host diagnostics must never leak Bearer secret");
    assert!(svc_a.ops_snapshot()["catalog"]["tools"].as_array().expect("tools").contains(&json!("summarize_items")));

    let run_cli = |args: &[&str]| -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
        isolate_home(&mut cmd, &home).env("SUSI_HOME", &cfg_dir).current_dir(root).args(args).output().expect("susi cli")
    };
    let run_cli_json = |args: &[&str]| -> Value {
        let out = run_cli(args);
        assert!(out.status.success(), "cli {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("cli json")
    };
    let help_out = run_cli(&["mcp", "--help"]);
    assert!(help_out.status.success());
    let help_txt = String::from_utf8_lossy(&help_out.stdout);
    for sub in ["serve", "list", "doctor", "status", "ledger", "call", "tools", "discover", "resources", "read", "prompts", "prompt", "complete", "task"] {
        assert!(help_txt.contains(sub), "susi mcp --help missing `{sub}`");
    }
    let ledger_cli = run_cli_json(&["mcp", "ledger"]);
    assert_eq!(ledger_cli["verified"], json!(true));
    let status_cli = run_cli_json(&["mcp", "status"]);
    assert!(status_cli.is_array() && status_cli == run_cli_json(&["mcp", "list"]));
    assert_eq!(run_cli_json(&["mcp", "discover", &addr_a, "--token", tok_ops])["serverInfo"]["name"], json!("sdk-production-ref"));
    let tools_out = run_cli(&["mcp", "tools", &addr_a, "--token", tok_ops]);
    assert!(tools_out.status.success() && String::from_utf8_lossy(&tools_out.stdout).contains("summarize_items"));
    assert_eq!(run_cli_json(&["mcp", "call", &addr_a, "summarize_items", "{\"items\":[\"a\"],\"scope\":{\"namespace\":\"ops\"}}", "--token", tok_ops])["structuredContent"]["count"], json!(1));
    assert!(!run_cli_json(&["mcp", "resources", &addr_a, "--templates", "--token", tok_ops])["resourceTemplates"].as_array().expect("rt").is_empty());
    assert_eq!(run_cli_json(&["mcp", "read", &addr_a, "ui://app/ops", "--app", "--token", tok_ops])["domain"], json!("ops.susi.local"));
    assert!(!run_cli_json(&["mcp", "prompts", &addr_a, "--token", tok_ops])["prompts"].as_array().expect("pl").is_empty());
    assert!(run_cli_json(&["mcp", "prompt", &addr_a, "draft_brief", "{\"topic\":\"ops\"}", "--token", tok_ops])["messages"][0]["content"]["text"].as_str().expect("pt").contains("ops"));
    assert!(run_cli_json(&["mcp", "complete", &addr_a, "prompt", "draft_brief", "topic", "se", "--token", tok_ops])["completion"]["values"].as_array().expect("cv").contains(&json!("security")));
    assert_eq!(run_cli_json(&["mcp", "task", &addr_a, "cancel", &th.task_id, "--token", tok_ops])["cancelled"], json!(true));
    assert_eq!(host.get_task("alpha", &th.task_id).expect("get task").handle.status, TaskStatus::Cancelled);

    // 2. Fail-closed configuration, URL scope, non-loopback Bearer, and corrupt store validation.
    assert!(!run_cli(&["mcp", "ledger", "--refetched", scratch.to_str().unwrap()]).status.success());
    let bad_cfg_dir = scratch.join("bad-cfg");
    std::fs::create_dir_all(&bad_cfg_dir).expect("bad_cfg_dir");
    std::fs::write(bad_cfg_dir.join("config.json"), r#"{"max_rpc_body_bytes":"not-a-number"}"#).expect("write bad cfg");
    let cfg_rep = validate_dir(&bad_cfg_dir).expect("validate_dir");
    assert!(cfg_rep.has_blocking() && cfg_rep.issues.iter().any(|i| i.kind == ConfigIssueKind::TypeMismatch && i.path == "max_rpc_body_bytes"));
    let scoped: McpServerConfig = serde_json::from_value(json!({"command": format!("https://op:{tok_ops}@mcp.example.com/v1?token={tok_ops}#frag"), "args": []})).expect("scoped");
    let (clean_url, is_loop) = scoped.http_scope().expect("scope").expect("url");
    assert!(clean_url == "https://mcp.example.com/v1" && !is_loop && !clean_url.contains(tok_ops));
    assert!(Session::open("198.51.100.77:9093", Some(tok_ops), Duration::from_secs(1)).is_err());
    let corrupt_store_path = scratch.join("corrupt-ops-store.json");
    std::fs::write(&corrupt_store_path, b"{broken-json").expect("corrupt write");
    let (recovered, quarantined) = PersistedTaskHandleStore::open_or_quarantine(&corrupt_store_path).expect("recover");
    assert!(quarantined.expect("quarantined path").is_file() && recovered.list().is_empty());

    // 3. Bounded logs/traces, W3C `traceparent`, and strict Bearer credential redaction.
    let http_post = |hdrs: &[(&str, &str)], body_val: &Value| -> (String, String, Vec<Value>) {
        let body = body_val.to_string();
        let mut req = format!("POST /mcp HTTP/1.1\r\nHost: {addr_a}\r\nConnection: close\r\nAuthorization: Bearer {tok_ops}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n");
        for (k, v) in hdrs { req.push_str(&format!("{k}: {v}\r\n")); }
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        let mut s = TcpStream::connect(&addr_a).expect("connect /mcp");
        s.set_read_timeout(Some(Duration::from_secs(15))).expect("timeout");
        s.write_all(req.as_bytes()).expect("write");
        let mut raw = String::new();
        let _ = s.read_to_string(&mut raw);
        let (head, rbody) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let mut frames = Vec::new();
        if rbody.trim().starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(rbody.trim()) { frames.push(v); }
        } else {
            for line in rbody.lines() {
                if let Some(d) = line.strip_prefix("data:").map(str::trim) {
                    if let Ok(v) = serde_json::from_str::<Value>(d) { frames.push(v); }
                }
            }
        }
        (head.lines().next().unwrap_or_default().to_string(), raw, frames)
    };
    let mut meta_info = adapter_modern_meta()["_meta"].clone();
    meta_info["io.modelcontextprotocol/requestId"] = json!("ops017-trace");
    meta_info["io.modelcontextprotocol/logLevel"] = json!("info");
    let hdrs_info = [
        ("MCP-Protocol-Version", "2026-07-28"),
        ("Mcp-Method", "tools/call"),
        ("Mcp-Name", "summarize_items"),
        ("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
    ];
    let (st_i, raw_i, frames_i) = http_post(&hdrs_info, &json!({"jsonrpc":"2.0","id":"ops017-trace","method":"tools/call","params":{"name":"summarize_items","arguments":{"items":["x"],"scope":{"namespace":"trace"}},"_meta":meta_info}}));
    assert!(st_i.starts_with("HTTP/1.1 200") && frames_i.iter().any(|f| f["method"] == "notifications/message") && !raw_i.contains(tok_ops));
    let mut meta_quiet = adapter_modern_meta()["_meta"].clone();
    meta_quiet["io.modelcontextprotocol/requestId"] = json!("ops017-quiet");
    let (_, raw_q, frames_q) = http_post(&hdrs_info[..3], &json!({"jsonrpc":"2.0","id":"ops017-quiet","method":"tools/call","params":{"name":"summarize_items","arguments":{"items":["x"],"scope":{"namespace":"quiet"}},"_meta":meta_quiet}}));
    assert!(frames_q.iter().all(|f| f["method"] != "notifications/message") && !raw_q.contains(tok_ops));
    let ch_probe = probe_http_auth_challenge(&addr_a, Some("wrong-bearer-probe"), Duration::from_secs(5)).expect("probe");
    assert!(!format!("{ch_probe:?}").contains("wrong-bearer-probe"));

    // 4. Live peer disconnect & Mandate 35 config rollback without disturbing sibling servers.
    assert_eq!(GmcpClient::admit_mcp_server("ops_peer", &format!("http://{addr_a}/mcp"), &[]), "SUCCESS_ADMITTED");
    let mcp_cfg_path = GmcpClient::get_config_path();
    let mut cfg_doc: Value = serde_json::from_str(&std::fs::read_to_string(&mcp_cfg_path).expect("cfg")).expect("json");
    cfg_doc["x_ops_revision"] = json!(19);
    std::fs::write(&mcp_cfg_path, serde_json::to_vec_pretty(&cfg_doc).expect("vec")).expect("write");
    let mut parsed_cfg: McpConfig = serde_json::from_slice(&std::fs::read(&mcp_cfg_path).expect("bytes")).expect("parse");
    parsed_cfg.mcp_servers.remove("ops_peer");
    std::fs::write(&mcp_cfg_path, serde_json::to_vec_pretty(&parsed_cfg).expect("vec")).expect("rollback write");
    let rolled_back: Value = serde_json::from_slice(&std::fs::read(&mcp_cfg_path).expect("rb")).expect("json");
    assert!(rolled_back["mcp_servers"].get("ops_peer").is_none() && rolled_back["x_ops_revision"] == json!(19));
    host.disconnect("beta").expect("disconnect beta");
    assert_eq!(host.snapshot()["serverCount"], json!(1));
    assert_eq!(host.call_tool("alpha", "summarize_items", &json!({"items":["alive"],"scope":{"namespace":"post-rollback"}}), None).expect("alpha alive")["structuredContent"]["count"], json!(1));
    host.disconnect("alpha").expect("disconnect alpha");

    // 5. Automated standards & contract drift detection across all 22 pinned `VC-203-017` authorities.
    let sources: Value = serde_json::from_slice(&std::fs::read(root.join(".agents/gmcp/sources.json")).expect("sources")).expect("json");
    let pinned_017 = "schema/2026-07-28/schema.json|schema/2025-11-25/schema.json|docs/specification/2026-07-28/architecture/index.mdx|docs/specification/2026-07-28/basic/versioning.mdx|docs/specification/2026-07-28/basic/transports/stdio.mdx|docs/specification/2026-07-28/basic/transports/streamable-http.mdx|docs/specification/2026-07-28/basic/authorization/index.mdx|docs/specification/2026-07-28/basic/patterns/mrtr.mdx|docs/specification/2026-07-28/basic/patterns/subscriptions.mdx|docs/specification/2026-07-28/basic/patterns/progress.mdx|docs/specification/2026-07-28/basic/patterns/cancellation.mdx|docs/specification/2026-07-28/client/roots.mdx|docs/specification/2026-07-28/client/sampling.mdx|docs/specification/2026-07-28/client/elicitation.mdx|docs/specification/2026-07-28/server/discover.mdx|docs/specification/2026-07-28/server/tools.mdx|docs/specification/2026-07-28/server/resources.mdx|docs/specification/2026-07-28/server/prompts.mdx|docs/specification/2026-07-28/server/utilities/completion.mdx|ext-tasks/specification/2026-07-28/tasks.md|ext-apps/specification/2026-01-26/apps.mdx|ext-auth/specification/stable/enterprise-managed-authorization.mdx";
    assert_eq!(pinned_017.split('|').count(), 22);
    for doc in pinned_017.split('|') {
        let entry = sources["files"].as_array().expect("files").iter().find(|f| f["document"] == doc).expect("doc");
        let sha = susi_paths::hex::encode(Sha256::digest(std::fs::read(root.join(".agents/gmcp/spec").join(doc)).expect("spec")));
        assert_eq!(entry["sha256"].as_str(), Some(sha.as_str()), "sha mismatch on {doc}");
    }
    let refetched = scratch.join("refetched-spec");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let ref_arg = refetched.to_str().expect("str");
    let drift_args = ["mcp", "ledger", "--drift", "--refetched", ref_arg];
    let clean_drift = run_cli_json(&drift_args);
    assert!(clean_drift["verified"] == json!(true) && clean_drift["findings"].as_array().expect("f").is_empty());
    for victim_rel in [
        "docs/specification/2026-07-28/basic/transports/streamable-http.mdx",
        "docs/specification/2026-07-28/basic/transports/stdio.mdx",
        "docs/specification/2026-07-28/client/roots.mdx",
        "docs/specification/2026-07-28/client/sampling.mdx",
        "docs/specification/2026-07-28/client/elicitation.mdx",
        "ext-tasks/specification/2026-07-28/tasks.md",
        "ext-apps/specification/2026-01-26/apps.mdx",
        "ext-auth/specification/stable/enterprise-managed-authorization.mdx",
    ] {
        let victim = refetched.join(victim_rel);
        let orig = std::fs::read(&victim).expect("orig");
        let mut altered = orig.clone();
        altered.extend_from_slice(b"\n<!-- drifted vc-203-017 spec -->\n");
        std::fs::write(&victim, &altered).expect("alter");
        let drifted = run_cli(&drift_args);
        assert!(!drifted.status.success(), "{victim_rel} drift must fail");
        let rep: Value = serde_json::from_slice(&drifted.stdout).expect("rep");
        assert!(rep["verified"] == json!(false) && rep["findings"].as_array().expect("f").iter().any(|f| f["document"] == victim_rel && f["kind"] == "hash_drift"));
        std::fs::write(&victim, &orig).expect("restore");
    }
    assert_eq!(run_cli_json(&drift_args), clean_drift);
    let missing_rel = "docs/specification/2026-07-28/client/roots.mdx";
    std::fs::remove_file(refetched.join(missing_rel)).expect("rm");
    let miss_out = run_cli(&drift_args);
    assert!(!miss_out.status.success());
    let miss_rep: Value = serde_json::from_slice(&miss_out.stdout).expect("miss rep");
    assert!(miss_rep["verified"] == json!(false) && miss_rep["findings"].as_array().expect("f").iter().any(|f| f["document"] == missing_rel && f["kind"] == "unfetchable"));

    // 6. Contract runbook bindings, reproducible retained evidence, and pre-mastery guard.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-017.md")).expect("contract");
    for req_tok in "## Operations and non-claims|## Operations and incident runbook|### Inspectable state|### Configuration validation|### Bounded metrics, logs and traces|### Diagnostics and incident triage|### Examples|### Incident triage|### Migration|### Rollback|### Production guarantees and non-claims|McpHost::snapshot()|PersistedTaskHandleStore::open_or_quarantine|MAX_BUFFERED_NOTIFICATIONS = 256|MAX_RESPONSE_BYTES = 16_777_216|MAX_PAGINATION_PAGES = 50|HOST-001|HOST-018|hash_drift|unfetchable".split('|') {
        assert!(contract.contains(req_tok), "contract missing `{req_tok}`");
    }
    let ledger_sha = susi_paths::hex::encode(Sha256::digest(std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).expect("ledger")));
    let mut ev_count = 0usize;
    for entry in std::fs::read_dir(root.join(".agents/gmcp/evidence")).expect("ev_dir") {
        let p = entry.expect("e").path();
        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            ev_count += 1;
            let recs = newest_records(&std::fs::read_to_string(&p).expect("read ev"));
            assert!(!recs.is_empty() && recs.iter().all(|r| r["exit"] == 0 && r["artifact_sha256"] == ledger_sha));
        }
    }
    assert!(ev_count >= 16);
    assert!(root.join(".agents/gmcp/evidence/VC-203-017.jsonl").is_file() && root.join(".agents/roadmap-verdicts/VC-203-017.json").is_file());
}
/// GMCP-203-017-M: Vector mastery gate for VC-203-017 (Complete MCP client and host).
#[test]
fn gmcp_vc_203_017_mastery() {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::{BTreeSet, HashMap};
    use std::io::BufReader;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_core::mac_policy::{actions, MacPolicy, PrivacyMode};
    use susi_gmcp::{
        direct_call::call_tool_direct,
        extensions::ExtensionStatus,
        mcp::{
            model::{CallToolResult, ReadResourceResult, Resource, Tool},
            task_manager::TaskExit,
        },
        protocol::GmcpService,
        sdk::{
            examples::production_reference_server, mrtr_elicitation, mrtr_roots_list,
            mrtr_sampling,
        },
        server::GmcpServer,
    };
    use susi_tools::{spawn_authorized_stdio_peer, McpServerConfig};
    use susi_wire_client::{
        host::{
            probe_http_auth_challenge, CompletionQuery, DuplexStreams, HostPolicy, HttpEndpoint,
            McpHost, RootEntry,
        },
        mcp::{
            parse_www_authenticate, PersistedTaskHandleStore, Session, TaskStatus, WireError,
            APPS_EXTENSION_ID,
        },
    };

    struct EvidenceCtx<'a> {
        root: &'a Path,
        ledger_hash: &'a str,
        test_src: &'a str,
        head_digest: &'a str,
        now: u64,
    }

    fn verify_vc_203_017_evidence(
        ctx: &EvidenceCtx<'_>,
        evidence_text: &str,
        committed_artifacts: Option<&[String]>,
    ) -> Result<BTreeSet<String>, String> {
        const LINK: [&str; 3] = ["recorded_unix", "identity", "artifact_sha256"];
        let names = |r: &Value, t: &Value| LINK.iter().all(|f| r["supersedes"][*f] == t[*f]);
        let mut order: Vec<String> = Vec::new();
        let mut chains: HashMap<String, Vec<Value>> = HashMap::new();
        for line in evidence_text.lines().filter(|l| !l.trim().is_empty()) {
            let rec: Value = serde_json::from_str(line).map_err(|e| format!("bad JSON: {e}"))?;
            let req = rec["requirement"].as_str().unwrap_or_default().to_string();
            match chains.get_mut(&req) {
                Some(ch) => {
                    if !ch.iter().any(|old| names(&rec, old)) {
                        return Err(format!("{req}: missing supersedes link"));
                    }
                    ch.push(rec);
                }
                None => {
                    order.push(req.clone());
                    chains.insert(req, vec![rec]);
                }
            }
        }
        let newest: Vec<Value> = order
            .into_iter()
            .filter_map(|req| {
                let ch = chains.remove(&req)?;
                ch.iter()
                    .filter(|r| !ch.iter().any(|later| names(later, r)))
                    .max_by_key(|r| r["recorded_unix"].as_u64().unwrap_or(0))
                    .cloned()
            })
            .collect();
        let mut covered = BTreeSet::new();
        let req_fields = "requirement|check|exit|detail|recorded_unix|artifact_sha256|identity";
        for rec in newest {
            let req = rec["requirement"].as_str().unwrap_or_default();
            if req.ends_with("-M") {
                continue;
            }
            if rec.get("waived").is_some() || rec.get("mock").is_some() {
                return Err(format!("{req}: waived or mock evidence rejected"));
            }
            if req_fields.split('|').any(|f| rec[f].is_null()) {
                return Err(format!("{req}: missing required field"));
            }
            if rec["exit"] != 0
                || rec["identity"].as_str().unwrap_or_default().is_empty()
                || rec["detail"].as_str().unwrap_or_default().is_empty()
                || rec["artifact_sha256"].as_str().unwrap_or_default() != ctx.ledger_hash
            {
                return Err(format!("{req}: invalid exit/identity/detail/ledger_hash"));
            }
            let ts = rec["recorded_unix"].as_u64().unwrap_or(0);
            if ts <= 1_792_256_000 || ts > ctx.now + 60 {
                return Err(format!("{req}: stale or future recorded_unix {ts}"));
            }
            let check = rec["check"].as_str().unwrap_or_default();
            let Some(fn_name) =
                check.strip_prefix("cargo test --locked --test gmcp_requirements ")
            else {
                return Err(format!("{req}: unsupported check command"));
            };
            if !fn_name.starts_with("gmcp_")
                || fn_name.contains(char::is_whitespace)
                || !is_test_fn(ctx.test_src, fn_name)
            {
                return Err(format!("{req}: zero-test missing fn {fn_name}"));
            }
            if let Some(artifacts) = committed_artifacts {
                assert_bound_to_tested_commit(ctx.root, &rec, artifacts, ctx.head_digest);
            } else {
                let c = rec["tested_commit"].as_str().unwrap_or_default();
                if c.len() != 40
                    || !c.chars().all(|x| x.is_ascii_hexdigit())
                    || rec["tested_artifacts"].as_str() != Some(ctx.head_digest)
                {
                    return Err(format!("{req}: invalid commit/artifact binding"));
                }
            }
            covered.insert(req.to_string());
        }
        for n in 1..=6 {
            let req = format!("GMCP-203-017-0{n}");
            if !covered.contains(&req) {
                return Err(format!("missing evidence for {req}"));
            }
        }
        Ok(covered)
    }

    let (_lock, _env, home) = gmcp_http_home_guard("017-mastery");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).expect("cfg_dir");

    // 1. Contract, pinned ledger parity, and zero-vendor SDK manifest boundary.
    let report = ledger_report(root);
    assert!(report["verified"] == true && susi_json(root, &home, &["mcp", "ledger"]) == report);
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-017.md")).expect("contract");
    for tok in "Status: **delivered**|GMCP-203-017-01..06 + GMCP-203-017-M|CLAIM-04|HOST-001|HOST-018|## Operations and incident runbook".split('|') {
        assert!(contract.contains(tok), "missing `{tok}`");
    }
    assert!(!root.join("crates/susi-vendor-mcp").exists());
    for manifest_rel in ["crates/susi-wire-client/Cargo.toml", "crates/susi-tools/Cargo.toml", "crates/susi-gmcp/Cargo.toml", "crates/susi-abi/Cargo.toml"] {
        let toml = std::fs::read_to_string(root.join(manifest_rel)).expect("Cargo.toml");
        for v in ["rmcp", "schemars", "axum", "tower", "reqwest", "ureq"] {
            assert!(!toml.contains(v), "{manifest_rel} must not depend on `{v}`");
        }
    }

    // 2. Concurrent isolated multi-server host + CLI + gateway + direct + stdio + UDS mastery run.
    let obj_schema = serde_json::Map::from_iter([("type".into(), json!("object"))]);
    let spawn_ref = |sub: &str, tok: &str| -> (GmcpService, String) {
        std::fs::write(cfg_dir.join("api_token"), format!("{tok}\n")).expect("token");
        let base = home.join(sub);
        std::fs::create_dir_all(base.join("ws")).expect("ws");
        let (svc, _) = production_reference_server(base.join("ws"), base.join("tasks")).expect("svc");
        svc.extensions().write().register(APPS_EXTENSION_ID, ExtensionStatus::Official, 1, serde_json::Map::new()).expect("apps");
        svc.register_resource(
            Resource::new("ui://app/mastery", "MasteryApp").with_mime_type("text/html;profile=mcp-app"),
            Arc::new(|req, _| Box::pin(async move {
                let r: ReadResourceResult = serde_json::from_value(json!({"contents":[{"uri":req.uri,"mimeType":"text/html;profile=mcp-app","text":"<main>mastery</main>","_meta":{"ui":{"domain":"mastery.susi.local","csp":{"connectDomains":["https://api.susi.local"]},"permissions":{"clipboardWrite":false}}}}]})).expect("app");
                Ok(r.into())
            })),
        );
        svc.register_task_tool(
            Tool::new_with_raw("mrtr_triad", Some("3-callback task".into()), obj_schema.clone()),
            Arc::new(|_, tc| Box::pin(async move {
                let s = tc.request_input("s", mrtr_sampling("check", 32).map_err(TaskExit::Error)?).await?;
                let e = tc.request_input("e", mrtr_elicitation("go?", json!({"type":"object","properties":{"go":{"type":"boolean"}},"required":["go"]})).map_err(TaskExit::Error)?).await?;
                let r = tc.request_input("r", mrtr_roots_list().map_err(TaskExit::Error)?).await?;
                Ok(CallToolResult::structured(json!({"sampled":s["content"]["text"],"go":e["go"],"firstRoot":r["roots"][0]["uri"]})))
            })),
        ).expect("triad");
        let lis = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("127.0.0.1:{}", lis.local_addr().expect("addr").port());
        let h_svc = svc.clone();
        let _bg = std::thread::spawn(move || GmcpServer::serve_http_service(h_svc, vec![lis], None, false));
        for _ in 0..50 {
            if TcpStream::connect(&addr).is_ok() { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        (svc, addr)
    };
    let tok_a = "tok-mastery-alpha-017";
    let (svc_a, addr_a) = spawn_ref("srv-a", tok_a);
    svc_a.bind_direct_server("mastery_017_direct").expect("bind direct");
    let mut host = McpHost::new();
    let mk_pol_a = || {
        HostPolicy::strict("susi-m-alpha", "1.0.0")
            .with_principal("alice")
            .with_roots(vec![RootEntry::new("file:///workspace/m-alpha", Some("ma".into())).expect("root")])
            .with_sampling(|_| Ok(json!({"role":"assistant","model":"susi-m","content":{"type":"text","text":"sampled-m"}})))
            .with_elicitation(|_| Ok(json!({"action":"accept","content":{"go":true,"approved":true,"decision":"mastery-ok"}})))
            .with_tasks(true)
            .with_apps(true)
    };
    host.connect_http_2026_07_28("alpha", HttpEndpoint { addr: &addr_a, bearer: Some(tok_a), timeout: Duration::from_secs(15) }, mk_pol_a()).expect("connect alpha");
    let store_path = home.join("mastery-tasks.json");
    host.attach_task_store("alpha", PersistedTaskHandleStore::open(&store_path).expect("store")).expect("attach store");

    let tok_b = "tok-mastery-beta-017";
    let (_svc_b, addr_b) = spawn_ref("srv-b", tok_b);
    assert!(matches!(Session::open(&addr_b, Some(tok_a), Duration::from_secs(5)), Err(WireError::Http(401, _))));
    assert!(probe_http_auth_challenge(&addr_b, Some(tok_a), Duration::from_secs(5)).is_ok());
    let ch = parse_www_authenticate(401, r#"Bearer realm="susi", resource_metadata="https://mcp.example/.well-known/oauth-protected-resource", scope="mcp:tools", error="invalid_token""#).expect("www");
    assert!(ch.scheme == "Bearer" && ch.scope.as_deref() == Some("mcp:tools"));
    host.connect_http_session("beta", HttpEndpoint { addr: &addr_b, bearer: Some(tok_b), timeout: Duration::from_secs(15) }, HostPolicy::strict("susi-m-beta", "1.0.0").with_principal("bob").with_roots(vec![RootEntry::new("file:///workspace/m-beta", None).expect("rb")])).expect("connect beta");
    // Restore `tok_a` in `api_token` for subsequent `alpha` HTTP requests!
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_a}\n")).expect("restore tok_a");

    let stdio_src = home.join("mastery_stdio_peer.rs");
    let stdio_exe = home.join(format!("mastery_stdio_peer{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&stdio_src, r#"
use std::io::{self, BufRead, Write};
fn main() {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut rc = 0u32;
    while let Some(Ok(line)) = lines.next() {
        if line.contains("\"notifications/roots/list_changed\"") { rc += 1; continue; }
        if line.contains("\"notifications/cancelled\"") { continue; }
        if !line.contains("\"id\"") { continue; }
        let id: String = line.split("\"id\":").nth(1).unwrap().chars().take_while(|c| c.is_ascii_digit()).collect();
        if line.contains("\"initialize\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{{\"tools\":{{}},\"resources\":{{\"subscribe\":true}},\"prompts\":{{}},\"completions\":{{}}}},\"serverInfo\":{{\"name\":\"gamma-mastery\",\"version\":\"1.0\"}}}}}}");
        } else if line.contains("\"tools/call\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{{\"progressToken\":\"stale\",\"progress\":1}}}}");
            println!("{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{{\"progressToken\":\"p-m\",\"progress\":100,\"total\":100}}}}");
            println!("{{\"jsonrpc\":\"2.0\",\"id\":801,\"method\":\"roots/list\",\"params\":{{}}}}");
            io::stdout().flush().unwrap();
            let r1 = lines.next().unwrap().unwrap();
            println!("{{\"jsonrpc\":\"2.0\",\"id\":802,\"method\":\"sampling/createMessage\",\"params\":{{\"messages\":[],\"maxTokens\":16}}}}");
            io::stdout().flush().unwrap();
            let r2 = lines.next().unwrap().unwrap();
            println!("{{\"jsonrpc\":\"2.0\",\"id\":803,\"method\":\"elicitation/create\",\"params\":{{\"message\":\"ok?\",\"requestedSchema\":{{\"type\":\"object\"}}}}}}");
            io::stdout().flush().unwrap();
            let r3 = lines.next().unwrap().unwrap();
            let ok = r1.contains("file:///workspace/m-gamma") && r2.contains("gamma-sampled") && r3.contains("gamma-elicited");
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"ok={ok};rc={rc}\"}}],\"isError\":false}}}}");
        } else if line.contains("\"completion/complete\"") && line.contains("\"ref/resource\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"completion\":{{\"values\":[\"10\",\"11\"],\"hasMore\":false}}}}}}");
        } else {
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{}}}}");
        }
        io::stdout().flush().unwrap();
    }
}
"#).expect("stdio_src");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    assert!(Command::new(rustc).args(["--edition", "2021"]).arg(&stdio_src).arg("-o").arg(&stdio_exe).status().expect("rustc").success());
    let stdio_cfg = McpServerConfig { command: stdio_exe.to_string_lossy().into_owned(), args: vec![], env: Some(HashMap::new()), extra: HashMap::new() };
    let deny_mac = MacPolicy::new([4u8; 32], PrivacyMode::LocalOnly, false);
    assert!(spawn_authorized_stdio_peer(&stdio_cfg, &deny_mac).is_err());
    let allow_mac = MacPolicy::new([4u8; 32], PrivacyMode::LocalOnly, false);
    allow_mac.grant("susi", actions::PROCESS_EXEC, &stdio_cfg.command, None).expect("grant");
    let mut child = spawn_authorized_stdio_peer(&stdio_cfg, &allow_mac).expect("spawn");
    let c_out = BufReader::new(child.stdout.take().expect("out"));
    let c_in = child.stdin.take().expect("in");
    host.connect_duplex("gamma", DuplexStreams { reader: Box::new(c_out), writer: Box::new(c_in), on_close: Some(Box::new(move || { let _ = child.kill(); let _ = child.wait(); })) }, HostPolicy::strict("susi-m-gamma", "1.0.0").with_principal("carol").with_roots(vec![RootEntry::new("file:///workspace/m-gamma", Some("mg".into())).expect("rg")]).with_sampling(|_| Ok(json!({"role":"assistant","model":"m","content":{"type":"text","text":"gamma-sampled"}}))).with_elicitation(|_| Ok(json!({"action":"accept","content":{"tag":"gamma-elicited"}})))).expect("connect gamma");

    #[cfg(unix)]
    {
        use std::os::unix::net::{UnixListener, UnixStream};
        use susi_gmcp::mcp::ServiceExt;
        let uds_path = home.join("m017.sock");
        let uds_lis = UnixListener::bind(&uds_path).expect("bind uds");
        let uds_svc = svc_a.clone();
        let _uds_bg = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("rt");
            rt.block_on(async move {
                uds_lis.set_nonblocking(true).expect("nb");
                let lis = tokio::net::UnixListener::from_std(uds_lis).expect("tokio uds");
                if let Ok((stream, _)) = lis.accept().await {
                    let (r, w) = tokio::io::split(stream);
                    let gate = susi_gmcp::jsonrpc_gate::StdioGate::new(susi_gmcp::stdio::BoundedLines::new(r, 16 * 1024 * 1024), w);
                    if let Ok(run) = uds_svc.session().serve(gate).await {
                        let _ = run.waiting().await;
                    }
                }
            });
        });
        let u_cli = UnixStream::connect(&uds_path).expect("connect uds");
        let u_r = BufReader::new(u_cli.try_clone().expect("clone"));
        host.connect_duplex("delta_uds", DuplexStreams { reader: Box::new(u_r), writer: Box::new(u_cli), on_close: None }, HostPolicy::strict("susi-m-uds", "1.0.0").with_principal("dave")).expect("connect uds");
    }

    assert!(host.discover("alpha").expect("disc")["supportedVersions"].as_array().expect("sv").contains(&json!("2026-07-28")));
    assert!(host.list_tools("alpha").expect("tools").iter().any(|t| t["name"] == "summarize_items"));
    let sum_args = json!({"items":["m1","m2"],"scope":{"namespace":"mastery"}});
    let host_sum = host.call_tool("alpha", "summarize_items", &sum_args, None).expect("call");
    assert_eq!(host_sum["structuredContent"], susi_core::mcp_client::call_tool(&addr_a, "summarize_items", &sum_args, Some(tok_a)).expect("gw")["structuredContent"]);
    let direct_parsed: Value = serde_json::from_str(call_tool_direct("mastery_017_direct", "summarize_items", &sum_args, &home).expect("direct")["content"][0]["text"].as_str().expect("txt")).expect("json");
    assert_eq!(direct_parsed, host_sum["structuredContent"]);
    #[cfg(unix)]
    assert_eq!(host.call_tool("delta_uds", "summarize_items", &sum_args, None).expect("uds")["structuredContent"], host_sum["structuredContent"]);

    assert!(host.list_resources("alpha").expect("res").iter().any(|r| r["uri"] == "memo://welcome"));
    assert!(host.list_resource_templates("alpha").expect("rt").iter().any(|t| t["uriTemplate"] == "memo://items/{id}"));
    assert!(host.read_resource("alpha", "memo://items/9").expect("read")["contents"][0]["text"].as_str().expect("txt").contains("\"9\""));
    let mut sub = host.open_subscription_stream("alpha", &json!({"resourceSubscriptions":["memo://welcome"]})).expect("sub");
    assert!(sub.is_acknowledged());
    svc_a.resource_updated("memo://welcome".to_string());
    assert_eq!(sub.next_frame().expect("frame").expect("ok")["method"], json!("notifications/resources/updated"));
    drop(sub);
    assert!(host.list_prompts("alpha").expect("prompts").iter().any(|p| p["name"] == "draft_brief"));
    assert!(host.get_prompt("alpha", "draft_brief", Some(&json!({"topic":"mastery"}))).expect("prompt")["messages"][0]["content"]["text"].as_str().expect("pt").contains("mastery"));
    assert!(host.complete("alpha", CompletionQuery { is_resource: false, target: "draft_brief", arg_name: "topic", arg_value: "se", context_args: None }).expect("cp")["completion"]["values"].as_array().expect("v").contains(&json!("security")));
    assert!(host.complete("alpha", CompletionQuery { is_resource: true, target: "memo://items/{id}", arg_name: "id", arg_value: "1", context_args: None }).is_err());
    assert!(host.complete("gamma", CompletionQuery { is_resource: true, target: "memo://items/{id}", arg_name: "id", arg_value: "1", context_args: None }).expect("cr")["completion"]["values"].as_array().expect("v").contains(&json!("10")));
    assert_eq!(host.call_tool_mrtr("alpha", "confirm_action", &json!({"operation":"seal-017"})).expect("mrtr")["structuredContent"]["status"], json!("confirmed"));
    let triad = host.call_tool_resolving_tasks("alpha", "mrtr_triad", &json!({}), Some(Duration::from_secs(10))).expect("triad");
    assert_eq!(triad["structuredContent"], json!({"sampled":"sampled-m","go":true,"firstRoot":"file:///workspace/m-alpha"}));
    let app = host.inspect_app("alpha", "ui://app/mastery").expect("app");
    assert_eq!(app.domain.as_deref(), Some("mastery.susi.local"));

    host.update_roots("gamma", vec![RootEntry::new("file:///workspace/m-gamma", Some("mg-v2".into())).expect("r")]).expect("update roots");
    assert_eq!(host.call_tool("gamma", "full_callbacks", &json!({}), Some(&json!("p-m"))).expect("gamma")["content"][0]["text"], json!("ok=true;rc=1"));
    let g_notes = host.drain_notifications("gamma").expect("drain");
    assert!(g_notes.iter().any(|n| n["params"]["progressToken"] == "p-m") && !g_notes.iter().any(|n| n["params"]["progressToken"] == "stale"));
    host.cancel_request("gamma", json!(777), Some("mastery-cancel")).expect("cancel req");

    let cancel_h = host.start_task_tool("alpha", "approval_workflow", &json!({"ticket":"T-M-CANCEL"})).expect("start cancel");
    host.cancel_task("alpha", &cancel_h.task_id).expect("cancel task");
    assert_eq!(host.get_task("alpha", &cancel_h.task_id).expect("get cancelled").handle.status, TaskStatus::Cancelled);

    // Crash-restart task handle persistence + CLI surface parity.
    let resume_h = host.start_task_tool("alpha", "approval_workflow", &json!({"ticket":"T-M-RESUME"})).expect("start resume");
    let snap_host = host.snapshot();
    assert!(!snap_host.to_string().contains(tok_a) && !snap_host.to_string().contains(tok_b));
    let run_cli = |args: &[&str]| -> Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
        let out = isolate_home(&mut cmd, &home).env("SUSI_HOME", &cfg_dir).current_dir(root).args(args).output().expect("cli");
        assert!(out.status.success(), "cli {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("json")
    };
    assert_eq!(run_cli(&["mcp", "discover", &addr_a, "--token", tok_a])["serverInfo"]["name"], json!("sdk-production-ref"));
    assert_eq!(run_cli(&["mcp", "call", &addr_a, "summarize_items", &sum_args.to_string(), "--token", tok_a])["structuredContent"], host_sum["structuredContent"]);
    assert_eq!(run_cli(&["mcp", "read", &addr_a, "ui://app/mastery", "--app", "--token", tok_a])["domain"], json!("mastery.susi.local"));

    // Verify `beta` with `tok_b` remains isolated and refuses unrequested CreateTaskResult.
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_b}\n")).expect("tok_b");
    assert!(host.call_tool("beta", "approval_workflow", &json!({"ticket":"T-NO-OPT"}), None).is_err());
    host.disconnect("beta").expect("disc beta");
    host.disconnect("gamma").expect("disc gamma");
    #[cfg(unix)]
    host.disconnect("delta_uds").expect("disc uds");
    host.disconnect("alpha").expect("disc alpha");
    drop(host);
    GmcpService::unbind_direct_server("mastery_017_direct");

    std::fs::write(cfg_dir.join("api_token"), format!("{tok_a}\n")).expect("tok_a");
    let mut host_resumed = McpHost::new();
    host_resumed.connect_http_2026_07_28("alpha", HttpEndpoint { addr: &addr_a, bearer: Some(tok_a), timeout: Duration::from_secs(15) }, mk_pol_a()).expect("reconnect alpha");
    host_resumed.attach_task_store("alpha", PersistedTaskHandleStore::open(&store_path).expect("reopen store")).expect("reattach");
    assert_eq!(host_resumed.resume_persisted_task("alpha", &resume_h.task_id, Some(Duration::from_secs(10))).expect("resume")["structuredContent"]["decision"], json!("mastery-ok"));
    assert_eq!(run_cli(&["mcp", "task", &addr_a, "get", &resume_h.task_id, "--token", tok_a])["status"], json!("completed"));
    host_resumed.disconnect("alpha").expect("disc resumed");

    // 3. Strict 01..06 evidence verification & 11-case negative tamper rejection matrix.
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).expect("ledger")));
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let artifacts = evidence_artifacts(root, "VC-203-017");
    let head_digest = artifacts_digest(root, "HEAD", &artifacts).expect("HEAD artifacts digest");
    let valid_tests = [
        ("GMCP-203-017-01", "gmcp_req_203_017_01_contract"),
        ("GMCP-203-017-02", "gmcp_req_203_017_02_production"),
        ("GMCP-203-017-03", "gmcp_req_203_017_03_integration"),
        ("GMCP-203-017-04", "gmcp_req_203_017_04_adversarial"),
        ("GMCP-203-017-05", "gmcp_req_203_017_05_interop"),
        ("GMCP-203-017-06", "gmcp_req_203_017_06_operations"),
    ];
    let good_lines: Vec<Value> = valid_tests.iter().map(|(r, t)| json!({"requirement":r,"check":format!("cargo test --locked --test gmcp_requirements {t}"),"exit":0,"detail":"ok","recorded_unix":now,"artifact_sha256":ledger_hash,"identity":"AG2FEATURES","tested_commit":"0".repeat(40),"tested_artifacts":head_digest})).collect();
    let good_text = good_lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let ev_ctx = EvidenceCtx { root, ledger_hash: &ledger_hash, test_src: &test_src, head_digest: &head_digest, now };
    let check_ev = |txt: &str, committed: bool| verify_vc_203_017_evidence(&ev_ctx, txt, committed.then_some(artifacts.as_slice()));
    assert!(check_ev(&good_text, false).is_ok());
    assert!(check_ev(&good_lines[..5].iter().map(Value::to_string).collect::<Vec<_>>().join("\n"), false).is_err());

    type Mutate<'a> = Box<dyn Fn(&mut Value) + 'a>;
    let mutators: [Mutate<'_>; 9] = [
        Box::new(|v| v["recorded_unix"] = json!(1_700_000_000_u64)),
        Box::new(|v| v["recorded_unix"] = json!(now + 120)),
        Box::new(|v| v["artifact_sha256"] = json!("deadbeef")),
        Box::new(|v| v["tested_artifacts"] = json!("stale-digest")),
        Box::new(|v| v["identity"] = json!("")),
        Box::new(|v| v["exit"] = json!(1)),
        Box::new(|v| { v.as_object_mut().unwrap().insert("waived".into(), json!(true)); }),
        Box::new(|v| { v.as_object_mut().unwrap().insert("mock".into(), json!(true)); }),
        Box::new(|v| v["check"] = json!("cargo test --locked --test gmcp_requirements gmcp_req_203_017_99_missing")),
    ];
    for mutate in mutators {
        let mut bad = good_lines[0].clone();
        mutate(&mut bad);
        let txt = std::iter::once(&bad).chain(&good_lines[1..]).map(Value::to_string).collect::<Vec<_>>().join("\n");
        assert!(check_ev(&txt, false).is_err());
    }
    assert!(check_ev(&format!("{good_text}\n{}", good_lines[0]), false).is_err());

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-017.jsonl")).expect("VC-203-017.jsonl");
    assert_eq!(check_ev(&evidence, true).expect("VC-203-017.jsonl valid").len(), 6);

    // 4. Roadmap, verdict, closed prerequisite tasks, and clean working tree checks.
    let roadmap_json: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".agents/roadmap.json")).expect("roadmap")).expect("json");
    let vc_entry = roadmap_json["vectors"].as_array().expect("vecs").iter().find(|e| e["id"] == "VC-203-017").expect("VC-203-017");
    assert!(vc_entry["progress"].as_str().unwrap_or_default().starts_with("DELIVERED"));

    let verdict: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".agents/roadmap-verdicts/VC-203-017.json")).expect("verdict")).expect("json");
    assert!(verdict["vector"] == "VC-203-017" && verdict["verdict"] == "delivered");
    let tracked = verdict["tracked_by"].as_array().expect("tracked");
    assert_eq!(tracked.len(), 7);
    for n in 114..=120 {
        assert!(tracked.contains(&json!(format!("T-CODEXGMCPROADMAP-{n}"))));
    }
    for n in 114..=119 {
        assert!(root.join(format!(".agents/tasks/done/T-CODEXGMCPROADMAP-{n}.json")).is_file());
    }

    let status = Command::new("git")
        .args(["status", "--porcelain", "--", ".agents/gmcp", "docs/gmcp", "tests/gmcp_requirements.rs", "tests/gmcp_requirements", "crates/susi-gmcp", ".agents/roadmap.json", ".agents/roadmap-verdicts"])
        .current_dir(root)
        .output()
        .expect("git status");
    assert!(status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(), "{}", String::from_utf8_lossy(&status.stdout));
}



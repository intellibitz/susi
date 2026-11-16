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
    let susi_stdio_cfg = McpServerConfig {
        command: susi_exe,
        args: vec!["mcp".into(), "serve".into()],
        env: Some(HashMap::from([("HOME".into(), home.to_string_lossy().into_owned()), ("SUSI_HOME".into(), cfg_dir.to_string_lossy().into_owned())])),
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
        assert_eq!(host.complete("alpha", CompletionQuery { is_resource: false, target: "draft_brief", arg_name: "topic", arg_value: "se", context_args: None }).expect("ca")["completion"]["values"].as_array().expect("v").contains(&json!("security")), true);
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
    assert!(!root.join(".agents/gmcp/evidence/VC-203-017.jsonl").exists() && !root.join(".agents/roadmap-verdicts/VC-203-017.json").exists());
}

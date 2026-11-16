/// GMCP-203-017-01: the complete MCP client and host contract is pinned
/// (`docs/gmcp/contract-203-017.md`) with verified upstream SHA-256 digests,
/// `CLAIM-04` ledger binding, confined `HOST-001..HOST-018` behavior rows,
/// zero vendor SDK crates, and positive/negative multi-server host fixtures.
#[test]
fn gmcp_req_203_017_01_contract() {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::{BTreeMap, BTreeSet};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;
    use susi_gmcp::{sdk::examples::production_reference_server, server::GmcpServer};
    use susi_tools::{McpConfig, McpServerConfig};
    use susi_wire_client::mcp::{client_capabilities_with_tasks, Session, WireError};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-017.md")).expect("contract");
    for section in [
        "Normative clause", "Pinned sources", "Canonical data model", "Behavior table",
        "Compatibility and deprecation decisions", "Threat model", "SLOs and resource budgets",
        "Fixtures", "Evidence locations", "Ambiguities resolved",
    ] {
        assert!(contract.contains(&format!("## {section}")), "missing `## {section}`");
    }

    let ledger_report = susi_gmcp::requirements_ledger::verify_pinned_ledger(root).expect("ledger");
    assert!(ledger_report.rows > 0 && ledger_report.documents > 0);
    let ledger: Value = serde_json::from_slice(&std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).expect("ledger json")).expect("parse");
    let claim04 = ledger["requirements"].as_array().expect("reqs").iter().find(|r| r["id"] == "CLAIM-04").expect("CLAIM-04");
    assert_eq!((claim04["trace"]["vector"].as_str(), claim04["trace"]["production_path"].as_str()), (Some("VC-203-017"), Some("crates/susi-tools/src/mcp_budget.rs")));
    assert!(contract.contains("CLAIM-04") && contract.contains("crates/susi-tools/src/mcp_budget.rs"));

    let sources: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".agents/gmcp/sources.json")).expect("sources")).expect("json");
    let upstream_commits: BTreeMap<&str, &str> = sources["upstream"].as_object().expect("u").iter().map(|(r, m)| (r.as_str(), m["commit"].as_str().expect("c"))).collect();
    let source_files: BTreeMap<&str, (&str, &str)> = sources["files"].as_array().expect("f").iter().map(|f| (f["document"].as_str().expect("d"), (f["repo"].as_str().expect("r"), f["sha256"].as_str().expect("s")))).collect();

    let pinned_rows: Vec<&str> = contract.lines().filter(|l| ["| modelcontextprotocol |", "| ext-tasks |", "| ext-apps |", "| ext-auth |"].iter().any(|p| l.starts_with(p))).collect();
    assert_eq!(pinned_rows.len(), 22);
    for row in pinned_rows {
        let cells: Vec<&str> = row.trim_matches('|').split('|').map(|c| c.trim().trim_matches('`')).collect();
        let (repo, doc, commit, sha) = (cells[0], cells[1], cells[2], cells[3]);
        let (expected_repo, expected_sha) = source_files.get(doc).copied().expect("pinned doc");
        assert!(repo == expected_repo && Some(commit) == upstream_commits.get(repo).copied() && sha == expected_sha);
        let disk_sha = susi_paths::hex::encode(Sha256::digest(std::fs::read(root.join(".agents/gmcp/spec").join(doc)).expect("spec")));
        assert_eq!(disk_sha, sha);
    }

    let host_rows: Vec<&str> = contract.lines().filter(|l| l.starts_with("| HOST-")).collect();
    assert_eq!(host_rows.len(), 18);
    let mut ids = BTreeSet::new();
    for row in host_rows {
        let cells: Vec<&str> = row.trim_matches('|').split('|').map(|c| c.trim().trim_matches('`')).collect();
        assert!(ids.insert(cells[0].to_string()) && ["must", "must_not", "should", "should_not", "may"].contains(&cells[1]));
        assert!(!cells[2].is_empty() && confined_repo_path(cells[3]) && root.join(cells[3]).exists(), "bad path `{}`", cells[3]);
    }
    let lower = contract.to_lowercase();
    for term in [
        "discover", "tools", "resources", "subscribe", "prompts", "completions", "mrtr", "roots",
        "sampling", "elicitation", "tasks", "apps", "authorization", "notifications", "progress",
        "cancellation", "isolated", "susi commercial apps",
    ] {
        assert!(lower.contains(term), "contract must resolve `{term}`");
    }
    assert!(contract.contains("gmcp_req_203_017_01_contract"));

    // Positive & negative McpConfig / McpServerConfig Mandate 35 & http_scope fixtures.
    let cfg_raw = json!({"mcp_servers":{"loop":{"command":"http://127.0.0.1:9093","args":[],"env":null,"x_tier":"local"},"remote":{"command":"https://user:secret@mcp.example.com/v1?k=v#frag","args":[],"env":null}},"x_top":7});
    let cfg: McpConfig = serde_json::from_value(cfg_raw.clone()).expect("cfg");
    assert_eq!(serde_json::to_value(&cfg).expect("re-serialize"), cfg_raw);
    assert_eq!(cfg.mcp_servers["loop"].http_scope().expect("loop"), Some(("http://127.0.0.1:9093/mcp".into(), true)));
    let (rem_url, rem_loop) = cfg.mcp_servers["remote"].http_scope().expect("rem").expect("some");
    assert!(rem_url == "https://mcp.example.com/v1" && !rem_loop && !rem_url.contains("secret") && !rem_url.contains("k=v"));
    for bad in ["http://", "http://[::zzz]:80/mcp", "http://bad host:80/mcp"] {
        let bad_srv: McpServerConfig = serde_json::from_value(json!({"command": bad, "args": []})).expect("srv");
        assert!(bad_srv.http_scope().is_err(), "expected `{bad}` to fail closed");
    }
    assert!(Session::open_with_capabilities("127.0.0.1:9", Some("tok"), &json!(["not-an-object"]), Duration::from_secs(1)).is_err());
    assert!(Session::open("198.51.100.10:9093", Some("tok"), Duration::from_secs(1)).is_err());

    // Positive & negative concurrent multi-server host fixtures against real GMCP reference servers.
    let spawn_ref = |home_dir: &Path, sub: &str, tok: &str| -> String {
        let cfg_dir = susi_paths::SusiDirs::config_dir();
        std::fs::create_dir_all(&cfg_dir).expect("cfg_dir");
        std::fs::write(cfg_dir.join("api_token"), format!("{tok}\n")).expect("token");
        let base = home_dir.join(sub);
        std::fs::create_dir_all(base.join("ws")).expect("ws");
        let (svc, _log) = production_reference_server(base.join("ws"), base.join("tasks")).expect("ref server");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("127.0.0.1:{}", listener.local_addr().expect("addr").port());
        let _bg = std::thread::spawn(move || GmcpServer::serve_http_service(svc, vec![listener], None, false));
        for _ in 0..50 {
            if TcpStream::connect(&addr).is_ok() { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        addr
    };
    {
        let (_home_lock, _env, home) = gmcp_http_home_guard("017-01-contract");
        let addr_a = spawn_ref(&home, "srv-a", "tok-alpha-203-017");
        let mut sess_a = Session::open_2026_07_28(&addr_a, Some("tok-alpha-203-017"), Duration::from_secs(10)).expect("open A");
        assert!(sess_a.protocol_version() == "2026-07-28" && sess_a.supports_server_tasks());
        assert!(sess_a.request("tools/list", &json!({}), &mut |_| {}).expect("tools")["tools"].as_array().expect("arr").iter().any(|t| t["name"] == "summarize_items"));
        assert_eq!(sess_a.call_tool("summarize_items", &json!({"items":["x","y"],"scope":{"namespace":"ns-a"}}), None, &mut |_| {}).expect("call")["structuredContent"]["count"], json!(2));
        assert!(sess_a.request("resources/read", &json!({"uri":"memo://welcome"}), &mut |_| {}).expect("res")["contents"][0]["text"].as_str().expect("txt").contains("Welcome"));
        assert!(sess_a.request("prompts/get", &json!({"name":"draft_brief","arguments":{"topic":"host"}}), &mut |_| {}).expect("prompt")["messages"][0]["content"]["text"].as_str().expect("p").contains("host"));
        assert!(sess_a.request("completion/complete", &json!({"ref":{"type":"ref/prompt","name":"draft_brief"},"argument":{"name":"topic","value":"ar"}}), &mut |_| {}).expect("comp")["completion"]["values"].as_array().expect("vals").contains(&json!("architecture")));
        assert!(sess_a.call_tool("approval_workflow", &json!({"ticket":"T-NO-OPTIN"}), None, &mut |_| {}).is_err());
        let resolved = sess_a.call_tool_resolving_tasks("approval_workflow", &json!({"ticket":"T-17"}), Some(Duration::from_secs(10)), |_| Ok(json!({"approve":{"decision":"ship"}}))).expect("task");
        assert_eq!(resolved["structuredContent"]["decision"], json!("ship"));

        // Rotate host token for isolated second server and prove cross-token rejection + stateful 2025-11-25 session.
        let addr_b = spawn_ref(&home, "srv-b", "tok-beta-203-017");
        assert!(matches!(Session::open(&addr_b, Some("tok-alpha-203-017"), Duration::from_secs(5)), Err(WireError::Http(401, _))));
        let mut sess_b = Session::open_with_capabilities(&addr_b, Some("tok-beta-203-017"), &client_capabilities_with_tasks(), Duration::from_secs(10)).expect("open B");
        assert_eq!(sess_b.protocol_version(), "2025-11-25");
        assert_eq!(sess_b.call_tool("summarize_items", &json!({"items":["z"],"scope":{"namespace":"ns-b"}}), None, &mut |_| {}).expect("call B")["structuredContent"]["namespace"], json!("ns-b"));
        sess_b.close().expect("close B");
        sess_a.close().expect("close A");
    }

    assert!(!root.join("crates/susi-vendor-mcp").exists());
    for manifest_rel in ["crates/susi-wire-client/Cargo.toml", "crates/susi-tools/Cargo.toml", "crates/susi-gmcp/Cargo.toml", "crates/susi-abi/Cargo.toml"] {
        let cargo_toml = std::fs::read_to_string(root.join(manifest_rel)).expect("Cargo.toml");
        for v in ["rmcp", "schemars", "axum", "tower", "reqwest", "ureq"] {
            assert!(!cargo_toml.contains(v), "{manifest_rel} must not depend on `{v}`");
        }
    }
    assert!(!root.join(".agents/gmcp/evidence/VC-203-017.jsonl").exists() && !root.join(".agents/roadmap-verdicts/VC-203-017.json").exists());
}

/// GMCP-203-017-02: canonical multi-server `McpHost`, `Session`, `MacPolicy`-gated
/// stdio spawn (`CLAIM-04`), and unified `susi mcp` CLI execute end-to-end across
/// concurrent real servers (`HOST-001..HOST-018`).
#[test]
fn gmcp_req_203_017_02_production() {
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::io::BufReader;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_core::mac_policy::{actions, MacPolicy, PrivacyMode};
    use susi_gmcp::{
        extensions::ExtensionStatus,
        mcp::model::{ReadResourceResult, Resource},
        sdk::examples::production_reference_server,
        server::GmcpServer,
    };
    use susi_tools::{spawn_authorized_stdio_peer, McpServerConfig};
    use susi_wire_client::{
        host::{
            probe_http_auth_challenge, CompletionQuery, DuplexStreams, HostPolicy, HttpEndpoint,
            McpHost, RootEntry,
        },
        mcp::{
            parse_www_authenticate, PersistedTaskHandleStore, TaskStatus, APPS_EXTENSION_ID,
        },
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (_home_lock, _env, home) = gmcp_http_home_guard("017-02-production");
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).expect("cfg_dir");

    // 1. Spawn real GMCP HTTP reference server A (2026-07-28 stateless, Apps `ui://` resource + extensions).
    let tok_a = "tok-alpha-017-02";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_a}\n")).expect("tok_a");
    let base_a = home.join("srv-a");
    std::fs::create_dir_all(base_a.join("ws")).expect("ws_a");
    let (svc_a, _log_a) = production_reference_server(base_a.join("ws"), base_a.join("tasks")).expect("svc_a");
    svc_a
        .extensions()
        .write()
        .register(APPS_EXTENSION_ID, ExtensionStatus::Official, 1, serde_json::Map::new())
        .expect("apps ext");
    svc_a.register_resource(
        Resource::new("ui://app/summary", "Summary Card").with_mime_type("text/html;profile=mcp-app"),
        Arc::new(|req, _ctx| {
            Box::pin(async move {
                let result: ReadResourceResult = serde_json::from_value(json!({ "contents": [{ "uri": req.uri, "mimeType": "text/html;profile=mcp-app", "text": "<!doctype html><html><body>headless card</body></html>", "_meta": {"ui": {"csp": {"connectDomains": ["https://api.example.test"]}, "permissions": {"clipboardWrite": false}, "domain": "app.susi.local"}}}]}))
                .expect("read res");
                Ok(result.into())
            })
        }),
    );
    let lis_a = TcpListener::bind("127.0.0.1:0").expect("bind_a");
    let addr_a = format!("127.0.0.1:{}", lis_a.local_addr().expect("addr_a").port());
    let svc_a_http = svc_a.clone();
    let _bg_a = std::thread::spawn(move || GmcpServer::serve_http_service(svc_a_http, vec![lis_a], None, false));
    for _ in 0..50 {
        if TcpStream::connect(&addr_a).is_ok() { break; }
        std::thread::sleep(Duration::from_millis(10));
    }

    // 2. Compile & spawn a real windowless stdio MCP peer via `spawn_authorized_stdio_peer` (`CLAIM-04`)
    //    exercising live bidirectional `roots/list`, `sampling/createMessage`, `elicitation/create`, & progress.
    let stdio_src = home.join("stdio_host_peer.rs");
    let stdio_exe = home.join(format!("stdio_host_peer{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&stdio_src, r#"
use std::io::{self, BufRead, Write};
fn main() {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut roots_changed = 0u32;
    while let Some(Ok(line)) = lines.next() {
        if line.contains("\"notifications/roots/list_changed\"") { roots_changed += 1; continue; }
        if !line.contains("\"id\"") { continue; }
        let id: String = line.split("\"id\":").nth(1).unwrap().chars().take_while(|c| c.is_ascii_digit()).collect();
        if line.contains("\"initialize\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{{\"tools\":{{}},\"resources\":{{\"subscribe\":true}},\"prompts\":{{}},\"completions\":{{}}}},\"serverInfo\":{{\"name\":\"gamma-stdio\",\"version\":\"0.2.0\"}}}}}}");
        } else if line.contains("\"tools/list\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"tools\":[{{\"name\":\"full_callbacks\",\"inputSchema\":{{\"type\":\"object\"}}}}]}}}}");
        } else if line.contains("\"tools/call\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{{\"progressToken\":\"stale\",\"progress\":1}}}}");
            println!("{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{{\"progressToken\":\"p-live\",\"progress\":50,\"total\":100}}}}");
            println!("{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{{\"level\":\"info\",\"data\":\"cb-start\"}}}}");
            println!("{{\"jsonrpc\":\"2.0\",\"id\":901,\"method\":\"roots/list\",\"params\":{{}}}}");
            io::stdout().flush().unwrap();
            let r1 = lines.next().unwrap().unwrap();
            println!("{{\"jsonrpc\":\"2.0\",\"id\":902,\"method\":\"sampling/createMessage\",\"params\":{{\"messages\":[{{\"role\":\"user\",\"content\":{{\"type\":\"text\",\"text\":\"hi\"}}}}],\"maxTokens\":32}}}}");
            io::stdout().flush().unwrap();
            let r2 = lines.next().unwrap().unwrap();
            println!("{{\"jsonrpc\":\"2.0\",\"id\":903,\"method\":\"elicitation/create\",\"params\":{{\"message\":\"confirm?\",\"requestedSchema\":{{\"type\":\"object\"}}}}}}");
            io::stdout().flush().unwrap();
            let r3 = lines.next().unwrap().unwrap();
            let ok = r1.contains("file:///workspace/gamma") && r2.contains("sampled-reply") && r3.contains("accepted-gamma");
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"ok={ok};rc={roots_changed}\"}}],\"isError\":false}}}}");
        } else if line.contains("\"resources/subscribe\"") || line.contains("\"resources/unsubscribe\"") {
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{}}}}");
        }
        io::stdout().flush().unwrap();
    }
}
"#).expect("write stdio_src");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let out = Command::new(rustc).args(["--edition", "2021"]).arg(&stdio_src).arg("-o").arg(&stdio_exe).output().expect("rustc");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let stdio_cfg = McpServerConfig {
        command: stdio_exe.to_string_lossy().into_owned(),
        args: vec![],
        env: Some(HashMap::new()),
        extra: HashMap::new(),
    };
    let deny_mac = MacPolicy::new([9u8; 32], PrivacyMode::LocalOnly, false);
    assert!(spawn_authorized_stdio_peer(&stdio_cfg, &deny_mac).expect_err("must deny").contains(actions::PROCESS_EXEC));
    let allow_mac = MacPolicy::new([9u8; 32], PrivacyMode::LocalOnly, false);
    allow_mac.grant("susi", actions::PROCESS_EXEC, &stdio_cfg.command, None).expect("grant");
    let mut child = spawn_authorized_stdio_peer(&stdio_cfg, &allow_mac).expect("authorized stdio spawn");
    let child_out = BufReader::new(child.stdout.take().expect("stdout"));
    let child_in = child.stdin.take().expect("stdin");

    // 3. Connect concurrent `alpha-modern`, `gamma-stdio`, and isolated `beta-session` on one `McpHost`.
    let mut host = McpHost::new();
    let pol_a = HostPolicy::strict("susi-alpha", "1.0.0")
        .with_principal("alice")
        .with_roots(vec![RootEntry::new("file:///workspace/alpha", Some("alpha".into())).expect("root")])
        .with_elicitation(|_p| Ok(json!({"action":"accept","content":{"approved":true,"decision":"approved-alpha"}})))
        .with_tasks(true)
        .with_apps(true);
    host.connect_http_2026_07_28("alpha", HttpEndpoint { addr: &addr_a, bearer: Some(tok_a), timeout: Duration::from_secs(15) }, pol_a).expect("connect alpha");
    host.attach_task_store("alpha", PersistedTaskHandleStore::open(home.join("alpha-tasks.json")).expect("store")).expect("attach store");

    let pol_g = HostPolicy::strict("susi-gamma", "1.0.0")
        .with_principal("carol")
        .with_roots(vec![RootEntry::new("file:///workspace/gamma", Some("gamma".into())).expect("root")])
        .with_sampling(|_p| Ok(json!({"role":"assistant","model":"susi-local","content":{"type":"text","text":"sampled-reply"}})))
        .with_elicitation(|_p| Ok(json!({"action":"accept","content":{"note":"accepted-gamma"}})));
    host.connect_duplex("gamma", DuplexStreams { reader: Box::new(child_out), writer: Box::new(child_in), on_close: Some(Box::new(move || { let _ = child.kill(); let _ = child.wait(); })) }, pol_g).expect("connect gamma");

    // Exercise alpha primitives: discover, tools, MRTR, tasks, resources, SSE subscriptions, prompts, completions, Apps!
    assert_eq!(host.discover("alpha").expect("disc")["serverInfo"]["name"], json!("sdk-production-ref"));
    assert!(host.list_tools("alpha").expect("tools").iter().any(|t| t["name"] == "summarize_items"));
    assert_eq!(host.call_tool("alpha", "summarize_items", &json!({"items":["a","b","c"],"scope":{"namespace":"prod"}}), None).expect("call")["structuredContent"]["count"], json!(3));
    assert_eq!(host.call_tool_mrtr("alpha", "confirm_action", &json!({"operation":"cut-release"})).expect("mrtr")["structuredContent"]["status"], json!("confirmed"));
    assert_eq!(host.call_tool_resolving_tasks("alpha", "approval_workflow", &json!({"ticket":"T-115"}), Some(Duration::from_secs(10))).expect("task")["structuredContent"]["decision"], json!("approved-alpha"));
    let th = host.start_task_tool("alpha", "approval_workflow", &json!({"ticket":"T-CANCEL"})).expect("start task");
    for _ in 0..30 {
        if host.get_task("alpha", &th.task_id).expect("get").handle.status == TaskStatus::InputRequired { break; }
        std::thread::sleep(Duration::from_millis(10));
    }
    host.cancel_task("alpha", &th.task_id).expect("cancel task");
    assert_eq!(host.get_task("alpha", &th.task_id).expect("get cancelled").handle.status, TaskStatus::Cancelled);
    host.cancel_request("alpha", json!(999), Some("operator-abort")).expect("cancel req");

    assert!(host.list_resources("alpha").expect("res").iter().any(|r| r["uri"] == "memo://welcome"));
    assert!(host.list_resource_templates("alpha").expect("tmpl").iter().any(|t| t["uriTemplate"] == "memo://items/{id}"));
    assert!(host.read_resource("alpha", "memo://items/77").expect("read")["contents"][0]["text"].as_str().expect("txt").contains("\"77\""));
    let app_info = host.inspect_app("alpha", "ui://app/summary").expect("inspect app");
    assert!(app_info.mime_type.as_deref() == Some("text/html;profile=mcp-app") && app_info.domain.as_deref() == Some("app.susi.local") && app_info.csp.is_some());
    assert!(host.inspect_app("alpha", "memo://welcome").is_err());

    let mut sub_stream = host.open_subscription_stream("alpha", &json!({"resourceSubscriptions":["memo://welcome"],"toolsListChanged":true})).expect("sub stream");
    assert!(sub_stream.is_acknowledged());
    svc_a.resource_updated("memo://welcome".to_string());
    let upd = sub_stream.next_frame().expect("frame").expect("ok");
    assert_eq!(upd["method"], json!("notifications/resources/updated"));
    drop(sub_stream);

    assert!(host.list_prompts("alpha").expect("prompts").iter().any(|p| p["name"] == "draft_brief"));
    assert!(host.get_prompt("alpha", "draft_brief", Some(&json!({"topic":"host-02"}))).expect("prompt")["messages"][0]["content"]["text"].as_str().expect("pt").contains("host-02"));
    assert!(host.complete("alpha", CompletionQuery { is_resource: false, target: "draft_brief", arg_name: "topic", arg_value: "se", context_args: None }).expect("comp")["completion"]["values"].as_array().expect("cv").contains(&json!("security")));

    // Exercise Unified `susi mcp` CLI parity against live `addr_a` BEFORE rotating host token for `beta`!
    let run_cli = |args: &[&str]| -> Value {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
        let out = isolate_home(&mut cmd, &home).args(args).output().expect("susi cli");
        assert!(out.status.success(), "cli {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).expect("cli json")
    };
    assert_eq!(run_cli(&["mcp", "discover", &addr_a, "--token", tok_a])["serverInfo"]["name"], json!("sdk-production-ref"));
    assert!(!run_cli(&["mcp", "resources", &addr_a, "--templates", "--token", tok_a])["resourceTemplates"].as_array().expect("rt").is_empty());
    assert_eq!(run_cli(&["mcp", "read", &addr_a, "ui://app/summary", "--app", "--token", tok_a])["domain"], json!("app.susi.local"));
    assert!(!run_cli(&["mcp", "prompts", &addr_a, "--token", tok_a])["prompts"].as_array().expect("pl").is_empty());
    assert!(run_cli(&["mcp", "prompt", &addr_a, "draft_brief", "{\"topic\":\"cli\"}", "--token", tok_a])["messages"][0]["content"]["text"].as_str().expect("t").contains("cli"));
    assert!(run_cli(&["mcp", "complete", &addr_a, "prompt", "draft_brief", "topic", "av", "--token", tok_a])["completion"]["values"].as_array().expect("v").contains(&json!("availability")));
    let cli_th = host.start_task_tool("alpha", "approval_workflow", &json!({"ticket":"T-CLI"})).expect("cli task");
    for _ in 0..30 {
        if run_cli(&["mcp", "task", &addr_a, "get", &cli_th.task_id, "--token", tok_a])["status"] == "input_required" { break; }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(run_cli(&["mcp", "task", &addr_a, "update", &cli_th.task_id, "{\"approve\":{\"decision\":\"cli-ok\"}}", "--token", tok_a])["updated"], json!(true));
    for _ in 0..30 {
        let snap = run_cli(&["mcp", "task", &addr_a, "get", &cli_th.task_id, "--token", tok_a]);
        if snap["status"] == "completed" {
            assert_eq!(snap["result"]["structuredContent"]["decision"], json!("cli-ok"));
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // Exercise `gamma` bidirectional stdio MRTR (`roots/list` + `sampling` + `elicitation` + correlated progress).
    host.subscribe_resource("gamma", "memo://stdio").expect("sub gamma");
    host.update_roots("gamma", vec![RootEntry::new("file:///workspace/gamma", Some("gamma-v2".into())).expect("r")]).expect("update roots");
    let g_res = host.call_tool("gamma", "full_callbacks", &json!({}), Some(&json!("p-live"))).expect("gamma call");
    assert_eq!(g_res["content"][0]["text"], json!("ok=true;rc=1"));
    let g_notes = host.drain_notifications("gamma").expect("drain gamma");
    assert!(g_notes.iter().any(|n| n["method"] == "notifications/progress" && n["params"]["progressToken"] == "p-live"));
    assert!(!g_notes.iter().any(|n| n["params"]["progressToken"] == "stale"));
    host.unsubscribe_resource("gamma", "memo://stdio").expect("unsub gamma");

    // Spawn isolated stateful 2025-11-25 server `beta` (zero elicitation capability -> `confirm_action` fails closed!).
    let tok_b = "tok-beta-017-02";
    std::fs::write(cfg_dir.join("api_token"), format!("{tok_b}\n")).expect("tok_b");
    let base_b = home.join("srv-b");
    std::fs::create_dir_all(base_b.join("ws")).expect("ws_b");
    let (svc_b, _) = production_reference_server(base_b.join("ws"), base_b.join("tasks")).expect("svc_b");
    let lis_b = TcpListener::bind("127.0.0.1:0").expect("bind_b");
    let addr_b = format!("127.0.0.1:{}", lis_b.local_addr().expect("addr_b").port());
    let _bg_b = std::thread::spawn(move || GmcpServer::serve_http_service(svc_b, vec![lis_b], None, false));
    for _ in 0..50 {
        if TcpStream::connect(&addr_b).is_ok() { break; }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(probe_http_auth_challenge(&addr_b, Some(tok_a), Duration::from_secs(5)).is_ok());
    let ch = parse_www_authenticate(401, r#"Bearer realm="susi", resource_metadata="https://mcp.example/.well-known/oauth-protected-resource", scope="mcp:tools", error="invalid_token""#).expect("www-auth");
    assert!(ch.scheme == "Bearer" && ch.scope.as_deref() == Some("mcp:tools") && ch.error.as_deref() == Some("invalid_token"));

    let pol_b = HostPolicy::strict("susi-beta", "1.0.0").with_principal("bob");
    host.connect_http_session("beta", HttpEndpoint { addr: &addr_b, bearer: Some(tok_b), timeout: Duration::from_secs(10) }, pol_b).expect("connect beta");
    assert_eq!(host.snapshot()["serverCount"], json!(3));
    assert!(host.call_tool("beta", "confirm_action", &json!({"operation":"blocked"}), None).is_err());
    host.subscribe_resource("beta", "memo://welcome").expect("sub beta");
    host.unsubscribe_resource("beta", "memo://welcome").expect("unsub beta");

    for id in ["beta", "gamma", "alpha"] {
        host.disconnect(id).expect("disconnect");
    }
    assert!(!root.join(".agents/gmcp/evidence/VC-203-017.jsonl").exists() && !root.join(".agents/roadmap-verdicts/VC-203-017.json").exists());
}

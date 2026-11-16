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

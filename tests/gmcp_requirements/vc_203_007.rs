#[test]
fn gmcp_req_203_007_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-007.md"))
        .expect("contract document must exist");
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
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-007")
        .collect();
    assert_eq!(rows.len(), 80, "the contract binds 80 VC-203-007 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
}

/// GMCP-203-007-02: the shipped GMCP runtime serves the production path
/// through the canonical transport SPI, with in-process transport and
/// stdio transport behaving as the same protocol.
#[test]
fn gmcp_req_203_007_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-007-02-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut wire = StdioSession::spawn_raw(root, &home);
    let initialize = wire
        .request_verbatim(
            &serde_json::json!({ "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "gmcp-007-02-prod", "version": "1"}}}),
            std::time::Duration::from_secs(300),
        )
        .expect("initialize must be answered on the production stdio path");
    assert!(
        initialize["result"]["capabilities"].is_object(),
        "initialize must return a capabilities object: {initialize}"
    );
    wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let tools = wire.request("tools/list", serde_json::json!({}));
    assert!(
        tools["tools"].is_array(),
        "production path must serve tools/list: {tools}"
    );

    let ping = wire.request("ping", serde_json::json!({}));
    assert_eq!(
        ping,
        serde_json::json!({}),
        "production path must answer ping"
    );

    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-007-03: one implementation behind every transport adapter.
///
/// The transport SPI (`susi_gmcp::mcp::transport`) admits any byte pipe, so
/// this test serves the production `GmcpService` behind the production
/// envelope gate on two adapters the shipped binary does not use itself, an
/// embedded in-process duplex and a Unix-domain socket pair, and drives the
/// shipped stdio entry point (`susi mcp serve`) beside them. One script runs
/// on all three and their transcripts must be equal: the handshake, the
/// tool registry, ordering of pipelined requests, cancellation, and every
/// error code. The per-message limit is the adapter's own, so the two SPI
/// adapters are given the same small limit and must refuse an oversized
/// line identically and keep serving. Closing the client's side ends each
/// served session within a bound, with no cancellation needed.
///
/// Named pipes and WebSocket are not served here: core ships no adapter
/// for either, and this test does not claim one.
#[test]
fn gmcp_req_203_007_03_integration() {
    let (_lock, _env, home) = gmcp_http_home_guard("007-int");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime = adapter_runtime(&home);

    let (mut embedded, embedded_session) = embedded_adapter(&runtime, root);
    let embedded_transcript = transport_adapter_script(&mut embedded);
    let embedded_limit = adapter_limit_step(&mut embedded);
    drop(embedded);
    adapter_session_ended(&runtime, embedded_session, "in-process");

    #[cfg(unix)]
    {
        let (mut socket, socket_session) = socket_adapter(&runtime, root);
        let socket_transcript = transport_adapter_script(&mut socket);
        assert_eq!(
            socket_transcript, embedded_transcript,
            "the Unix-domain socket adapter must answer exactly as the in-process one"
        );
        assert_eq!(
            adapter_limit_step(&mut socket),
            embedded_limit,
            "both adapters must refuse an oversized line with the same code"
        );
        drop(socket);
        adapter_session_ended(&runtime, socket_session, "unix socket");
    }

    // The shipped entry point: `susi mcp serve` over real stdio.
    let mut child = susi_command(root, &home, &["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the susi binary must spawn `mcp serve`");
    let mut stdin = child.stdin.take().expect("stdin");
    let rx = adapter_lines(child.stdout.take().expect("stdout"));
    let mut stdio = AdapterClient {
        write: Box::new(move |bytes| {
            stdin
                .write_all(bytes)
                .and_then(|()| stdin.flush())
                .expect("the server must accept stdin");
        }),
        rx,
        pending: Vec::new(),
    };
    let stdio_transcript = transport_adapter_script(&mut stdio);
    // Dropping the client closes stdin: the shipped server must exit by
    // itself, without a kill.
    drop(stdio);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if std::time::Instant::now() > deadline {
            stop_test_server(&mut child);
            panic!("`susi mcp serve` must exit when its stdin closes");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(status.success(), "a closed stdin is a clean exit: {status}");
    assert_eq!(
        stdio_transcript, embedded_transcript,
        "the shipped stdio entry point must answer exactly as the embedded adapter"
    );

    assert!(
        embedded_limit.is_i64(),
        "an oversized line earns a typed error: {embedded_limit}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// GMCP-203-007-04: adversarial and failure behavior of the transport
/// adapters.
///
/// The vector's threat model, replayed on the in-process and Unix-domain
/// socket adapters behind the production gate: a request before the
/// handshake, truncated, non-UTF-8, scalar, batch, wrong-version and
/// method-less frames, a flood of oversized lines carrying a smuggled
/// request, a replayed request id, a forged response, a spoofed
/// cancellation and a cancellation in flight. Each attack earns a typed
/// answer, the same on both adapters; none is served, repeated or left to
/// stall the session, and the session answers `ping` after every one. A
/// connection torn mid-frame, and one closed before any handshake, end
/// their sessions within a bound and leave an independent session on the
/// same runtime serving. No adapter advertises itself as MCP surface.
#[test]
fn gmcp_req_203_007_04_adversarial() {
    let (_lock, _env, home) = gmcp_http_home_guard("007-adv");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime = adapter_runtime(&home);

    // A bystander session: faults on other connections must not reach it.
    let (mut bystander, bystander_session) = embedded_adapter(&runtime, root);
    let hello = bystander.call(
        "init",
        "initialize",
        serde_json::json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "gmcp-007-04-bystander", "version": "1"}}),
    );
    assert!(
        hello["result"].is_object(),
        "the bystander must be served: {hello}"
    );
    bystander.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let (mut embedded, embedded_session) = embedded_adapter(&runtime, root);
    let embedded_transcript = transport_adapter_attacks(&mut embedded);
    assert!(
        embedded_transcript.len() == 11,
        "every attack must be recorded: {embedded_transcript:?}"
    );
    // Torn mid-frame: half a request, then the pipe closes.
    (embedded.write)(b"{\"jsonrpc\":\"2.0\",\"id\":\"torn\",\"method\":\"pi");
    drop(embedded);
    adapter_session_ended(&runtime, embedded_session, "in-process, torn frame");

    #[cfg(unix)]
    {
        let (mut socket, socket_session) = socket_adapter(&runtime, root);
        let socket_transcript = transport_adapter_attacks(&mut socket);
        assert_eq!(
            socket_transcript, embedded_transcript,
            "every attack must earn the same answer on the Unix-domain socket adapter"
        );
        (socket.write)(b"{\"jsonrpc\":\"2.0\",\"id\":\"torn\",\"method\":\"pi");
        drop(socket);
        adapter_session_ended(&runtime, socket_session, "unix socket, torn frame");

        // Closed before any handshake: the session ends, it does not wait.
        let (silent, silent_session) = socket_adapter(&runtime, root);
        drop(silent);
        let outcome = runtime
            .block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(30), silent_session).await
            })
            .expect("a connection closed before its handshake must not hold a session open")
            .expect("the session task must not panic");
        assert!(
            outcome.is_err() || matches!(outcome, Ok(susi_gmcp::mcp::service::QuitReason::Closed)),
            "a handshake that never came is not a served session: {outcome:?}"
        );
    }

    let alive = bystander.call("bystander-ping", "ping", serde_json::json!({}));
    assert_eq!(
        alive["result"],
        serde_json::json!({}),
        "faults on other connections must not reach a bystander: {alive}"
    );
    drop(bystander);
    adapter_session_ended(&runtime, bystander_session, "bystander");
    let _ = std::fs::remove_dir_all(&home);
}

/// GMCP-203-007-05: interoperability and quality of the transport adapters.
///
/// The pinned reference fixtures (one wire message each, positive and
/// negative) are replayed on the in-process adapter, the Unix-domain socket
/// adapter and the shipped stdio entry point, on a compatibility-era and a
/// modern-era session of each. Every fixture must earn what it expects, with
/// its expected error code, and the same answer on all three adapters. The envelope gate
/// the adapters stand behind and the gate of the HTTP gateway must give a
/// malformed frame the same error code, and that code is what a live adapter
/// session answers. A deterministic pipelined run answers every request
/// exactly once, identically on both SPI adapters. Startup, request latency,
/// pipelined throughput, fairness between two sessions, memory growth and
/// disk growth are held to the ceilings the vector's contract publishes.
///
/// Core serves the server role only on this SPI: it ships no MCP client on
/// a byte-pipe adapter, so the client role is not claimed here. Named pipes
/// and WebSocket have no adapter in core and are not claimed either.
#[test]
fn gmcp_req_203_007_05_interop() {
    use std::time::{Duration, Instant};
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};

    let (_lock, _env, home) = gmcp_http_home_guard("007-interop");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let budgets = adapter_budgets(root);
    let within = |name: &str, measured: u128| {
        let ceiling = budgets[name];
        assert!(
            measured <= ceiling,
            "{name}: measured {measured}, the published ceiling is {ceiling}"
        );
    };
    let runtime = adapter_runtime(&home);
    let fixtures = adapter_wire_fixtures(root);
    let disk_before = adapter_tree_bytes(&home);

    // ── 1. Startup, then the reference fixtures on every adapter ────────
    let opened = Instant::now();
    let (mut embedded, embedded_session) = embedded_adapter(&runtime, root);
    adapter_handshake(&mut embedded, "gmcp-007-05-embedded");
    within("startup_ms", opened.elapsed().as_millis());
    let legacy_replay = adapter_fixture_replay(&mut embedded, &fixtures, "legacy");
    assert_eq!(
        legacy_replay,
        adapter_fixture_replay(&mut embedded, &fixtures, "legacy"),
        "a fixture replay must be deterministic"
    );
    let (mut modern, modern_session) = embedded_adapter(&runtime, root);
    adapter_modern_open(&mut modern);
    let modern_replay = adapter_fixture_replay(&mut modern, &fixtures, "modern");
    drop(modern);
    adapter_session_ended(&runtime, modern_session, "in-process, modern");
    assert!(
        legacy_replay.len() >= 4 && modern_replay.len() >= 10,
        "both eras must replay their fixtures: {} legacy, {} modern",
        legacy_replay.len(),
        modern_replay.len()
    );

    #[cfg(unix)]
    let (mut socket, socket_session) = {
        let (mut modern, modern_session) = socket_adapter(&runtime, root);
        adapter_modern_open(&mut modern);
        assert_eq!(
            adapter_fixture_replay(&mut modern, &fixtures, "modern"),
            modern_replay,
            "the Unix-domain socket adapter must answer every modern fixture as the in-process one"
        );
        drop(modern);
        adapter_session_ended(&runtime, modern_session, "unix socket, modern");

        let opened = Instant::now();
        let (mut socket, session) = socket_adapter(&runtime, root);
        adapter_handshake(&mut socket, "gmcp-007-05-socket");
        within("startup_ms", opened.elapsed().as_millis());
        assert_eq!(
            adapter_fixture_replay(&mut socket, &fixtures, "legacy"),
            legacy_replay,
            "the Unix-domain socket adapter must answer every legacy fixture as the in-process one"
        );
        (socket, session)
    };

    // The shipped entry point answers the same fixtures the same way, in
    // both eras, and exits by itself when its client closes stdin.
    for (era, expected) in [("legacy", &legacy_replay), ("modern", &modern_replay)] {
        let mut child = susi_command(root, &home, &["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the susi binary must spawn `mcp serve`");
        let mut stdin = child.stdin.take().expect("stdin");
        let rx = adapter_lines(child.stdout.take().expect("stdout"));
        let mut stdio = AdapterClient {
            write: Box::new(move |bytes| {
                stdin
                    .write_all(bytes)
                    .and_then(|()| stdin.flush())
                    .expect("the server must accept stdin");
            }),
            rx,
            pending: Vec::new(),
        };
        if era == "modern" {
            adapter_modern_open(&mut stdio);
        } else {
            adapter_handshake(&mut stdio, "gmcp-007-05-stdio");
        }
        let stdio_replay = adapter_fixture_replay(&mut stdio, &fixtures, era);
        drop(stdio);
        let deadline = Instant::now() + Duration::from_secs(30);
        while child.try_wait().expect("wait").is_none() {
            if Instant::now() > deadline {
                stop_test_server(&mut child);
                panic!("`susi mcp serve` must exit when its stdin closes");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            &stdio_replay, expected,
            "the shipped stdio entry point must answer every {era} fixture as the embedded adapter"
        );
    }

    // ── 2. Direct gate versus gateway gate, and the live adapter ────────
    for (name, line) in [
        ("batch", br#"[]"#.as_slice()),
        (
            "null-id",
            br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
        ),
        (
            "non-2.0",
            br#"{"jsonrpc":"1.0","id":null,"method":"ping"}"#.as_slice(),
        ),
        ("truncated", br#"{"jsonrpc":"2.0","#.as_slice()),
    ] {
        let mut era = EraState::default();
        let direct = match classify_line(line, &mut era) {
            GateVerdict::Reject { code, .. } => code,
            GateVerdict::Forward => panic!("the gate must reject {name}, got Forward"),
            GateVerdict::Drop => panic!("the gate must reject {name}, got Drop"),
        };
        let gateway = http_envelope_rejection(line)
            .unwrap_or_else(|| panic!("the HTTP gate must reject {name}"))["error"]["code"]
            .as_i64()
            .unwrap();
        assert_eq!(
            direct, gateway,
            "{name}: direct and gateway must give the same error code"
        );
        let mut framed = line.to_vec();
        framed.push(b'\n');
        (embedded.write)(&framed);
        let live = embedded.rejection(name);
        assert_eq!(
            live["error"]["code"].as_i64(),
            Some(direct),
            "{name}: a live adapter session answers with the gate's code: {live}"
        );
    }

    // ── 3. Request latency ───────────────────────────────────────────────
    let mut worst = 0;
    for i in 0..20 {
        let asked = Instant::now();
        let answer = embedded.call(&format!("lat-{i}"), "ping", serde_json::json!({}));
        assert_eq!(answer["result"], serde_json::json!({}), "{answer}");
        worst = worst.max(asked.elapsed().as_millis());
    }
    within("request_ms", worst);

    // ── 4. Pipelined property run: exactly-once, same on both adapters ──
    let resident_before = adapter_resident_bytes();
    let asked = Instant::now();
    let embedded_answers = adapter_property_run(&mut embedded, 400);
    within("pipeline_ms", asked.elapsed().as_millis());
    assert_eq!(embedded_answers.len(), 400);
    #[cfg(unix)]
    {
        let asked = Instant::now();
        let socket_answers = adapter_property_run(&mut socket, 400);
        within("pipeline_ms", asked.elapsed().as_millis());
        assert_eq!(
            socket_answers, embedded_answers,
            "the pipelined run must be answered identically on both adapters"
        );
    }
    if let (Some(before), Some(after)) = (resident_before, adapter_resident_bytes()) {
        within(
            "memory_growth_mib",
            u128::from(after.saturating_sub(before) / (1024 * 1024)),
        );
    }

    // ── 5. Fairness: a flooded session does not starve its neighbour ────
    let (mut neighbour, neighbour_session) = embedded_adapter(&runtime, root);
    adapter_handshake(&mut neighbour, "gmcp-007-05-neighbour");
    let mut flood = Vec::new();
    for i in 0..300 {
        flood.extend_from_slice(
            format!("{{\"jsonrpc\":\"2.0\",\"id\":\"f{i}\",\"method\":\"tools/list\",\"params\":{{}}}}\n")
                .as_bytes(),
        );
    }
    (embedded.write)(&flood);
    let asked = Instant::now();
    let answer = neighbour.call("fair", "ping", serde_json::json!({}));
    assert_eq!(answer["result"], serde_json::json!({}), "{answer}");
    within("fairness_ms", asked.elapsed().as_millis());
    for i in 0..300 {
        let answer = embedded.answer(&format!("f{i}"));
        assert!(
            answer["result"]["tools"].is_array(),
            "the flood itself is served in full: {answer}"
        );
    }

    // ── 6. Clean shutdown and disk growth ───────────────────────────────
    drop(neighbour);
    adapter_session_ended(&runtime, neighbour_session, "neighbour");
    drop(embedded);
    adapter_session_ended(&runtime, embedded_session, "in-process");
    #[cfg(unix)]
    {
        drop(socket);
        adapter_session_ended(&runtime, socket_session, "unix socket");
    }
    within(
        "disk_growth_kib",
        u128::from(adapter_tree_bytes(&home).saturating_sub(disk_before) / 1024),
    );

    // ── 7. The contract binds every ledger row of the vector ────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-007.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-007")
        .collect();
    assert_eq!(rows.len(), 80, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// GMCP-203-007-06: operations, configuration validation, diagnostics, runbook
/// and drift detection for the transport adapter SPI and embedded operation.
#[test]
fn gmcp_req_203_007_06_operations() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::susi_config::validate::{validate_dir, ConfigIssueKind};

    let (_lock, _env, home) = gmcp_http_home_guard("007-ops");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    let op_home = scratch.join("home");
    std::fs::create_dir_all(&op_home).unwrap();

    // 1. Inspectable CLI state: ledger report & `susi mcp status` / `ledger`.
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

    let cli_ledger = susi_json(root, &op_home, &["mcp", "ledger"]);
    assert_eq!(cli_ledger["verified"], true);
    assert_eq!(cli_ledger["rows"], report["rows"]);
    assert_eq!(cli_ledger["vectors"], report["vectors"]);

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

    // 2. Inspectable API state across transport adapters (embedded in-process,
    // Unix-domain socket on Unix, and served stdio): identical identity,
    // discovery metadata, and `ledger_verify` tool report.
    let runtime = adapter_runtime(&home);
    let (mut embedded, embedded_session) = embedded_adapter(&runtime, root);
    let discover_probe = |client: &mut AdapterClient, label: &str| {
        let resp = client.call(
            &format!("discover-{label}"),
            "server/discover",
            adapter_modern_meta(),
        );
        assert!(
            resp.get("error").is_none_or(serde_json::Value::is_null),
            "{label} server/discover must succeed: {resp}"
        );
        resp["result"].clone()
    };
    let embedded_discover = discover_probe(&mut embedded, "embedded");
    assert_eq!(
        embedded_discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "susi-gmcp"
    );
    assert!(embedded_discover["ttlMs"].is_number());
    adapter_handshake(&mut embedded, "ops-embedded");
    let embedded_tool = embedded.call(
        "verify-embedded",
        "tools/call",
        serde_json::json!({ "name": "ledger_verify", "arguments": {}, "_meta": adapter_modern_meta()["_meta"]}),
    );
    let embedded_tool_report = call_tool_report(&embedded_tool["result"]);
    assert_eq!(embedded_tool_report["verified"], true);
    assert_eq!(embedded_tool_report["rows"], report["rows"]);
    assert_eq!(embedded_tool_report["vectors"], report["vectors"]);
    drop(embedded);
    adapter_session_ended(&runtime, embedded_session, "in-process");

    #[cfg(unix)]
    {
        let (mut socket, socket_session) = socket_adapter(&runtime, root);
        let socket_discover = discover_probe(&mut socket, "socket");
        assert_eq!(
            socket_discover, embedded_discover,
            "Unix socket and in-process adapters must expose identical discovery state"
        );
        // A served session ends as `Closed` only once it was negotiated, as
        // on the in-process adapter above; a client that closes before
        // `initialize` ends it as a failed negotiation.
        adapter_handshake(&mut socket, "ops-socket");
        drop(socket);
        adapter_session_ended(&runtime, socket_session, "unix socket");
    }

    let mut served = StdioSession::spawn_raw(root, &op_home);
    let served_discover = served
        .request_verbatim(
            &serde_json::json!({ "jsonrpc": "2.0", "id": "discover-stdio", "method": "server/discover", "params": adapter_modern_meta(),}),
            std::time::Duration::from_secs(120),
        )
        .expect("served stdio server/discover must answer");
    assert_eq!(
        served_discover["result"], embedded_discover,
        "served stdio and in-process adapters must expose identical discovery state"
    );
    drop(served);

    // 3. Configuration validation & bounded diagnostics on transport limits.
    let invalid_cli = susi_command(
        root,
        &op_home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(!invalid_cli.status.success());

    let cfg_home = scratch.join("cfg-valid");
    let cfg_susi = cfg_home.join(".susi");
    std::fs::create_dir_all(&cfg_susi).unwrap();
    std::fs::write(
        cfg_susi.join("config.json"),
        r#"{"max_rpc_body_bytes": 1024}"#,
    )
    .unwrap();
    let valid_cfg = validate_dir(&cfg_susi).expect("valid config must validate");
    assert!(
        !valid_cfg.has_blocking(),
        "numeric max_rpc_body_bytes must have no blocking schema issues: {:?}",
        valid_cfg.issues
    );

    let bad_type_home = scratch.join("cfg-bad-type");
    let bad_type_susi = bad_type_home.join(".susi");
    std::fs::create_dir_all(&bad_type_susi).unwrap();
    std::fs::write(
        bad_type_susi.join("config.json"),
        r#"{"max_rpc_body_bytes": "huge"}"#,
    )
    .unwrap();
    let bad_type_report = validate_dir(&bad_type_susi).expect("typed mismatch must report");
    assert!(bad_type_report.has_blocking());
    assert!(bad_type_report.issues.iter().any(|issue| {
        issue.path == "max_rpc_body_bytes" && issue.kind == ConfigIssueKind::TypeMismatch
    }));

    // Zero frame limit is refused loudly on stderr with zero corrupt bytes on stdout.
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
        .expect("spawn stdio with zero limit");
    drop(zero_child.stdin.take());
    let zero_out = zero_child
        .wait_with_output()
        .expect("zero limit stdio exit");
    let zero_stderr = String::from_utf8_lossy(&zero_out.stderr);
    assert!(
        zero_out.stdout.is_empty(),
        "refused zero-limit startup must emit zero stdout bytes, got: {}",
        String::from_utf8_lossy(&zero_out.stdout)
    );
    assert!(
        zero_stderr.contains("max_rpc_body_bytes") && zero_stderr.contains("must be > 0"),
        "zero-limit startup must diagnose invalid max_rpc_body_bytes on stderr, got: {zero_stderr}"
    );

    // Corrupt config logs a diagnostic on stderr and falls back safely to defaults
    // while keeping stdout strictly valid newline-delimited JSON-RPC.
    let corrupt_home = scratch.join("cfg-corrupt");
    let corrupt_susi = corrupt_home.join(".susi");
    std::fs::create_dir_all(&corrupt_susi).unwrap();
    std::fs::write(corrupt_susi.join("config.json"), b"{corrupt").unwrap();
    let mut corrupt_child = susi_command(root, &corrupt_home, &["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stdio with corrupt config");
    {
        use std::io::Write;
        let stdin = corrupt_child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            serde_json::json!({ "jsonrpc": "2.0", "id": "ops-corrupt-ping", "method": "ping", "params": adapter_modern_meta()})
        )
        .unwrap();
    }
    drop(corrupt_child.stdin.take());
    let corrupt_out = corrupt_child
        .wait_with_output()
        .expect("corrupt config stdio exit");
    let corrupt_stderr = String::from_utf8_lossy(&corrupt_out.stderr);
    assert!(
        corrupt_stderr.contains("Config load failed"),
        "corrupt config must emit diagnostic on stderr, got: {corrupt_stderr}"
    );
    assert!(
        corrupt_stderr.contains("max_rpc_body_bytes="),
        "startup stderr must report active max_rpc_body_bytes bound, got: {corrupt_stderr}"
    );
    let stdout_text = String::from_utf8(corrupt_out.stdout).expect("stdout must be UTF-8");
    let lines: Vec<&str> = stdout_text.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "stdout must contain only MCP JSON-RPC: {stdout_text}"
    );
    let ping_resp: serde_json::Value =
        serde_json::from_str(lines[0]).expect("every stdout line must parse as JSON-RPC");
    assert_eq!(ping_resp["id"], "ops-corrupt-ping");
    assert!(ping_resp["result"].is_object());

    // 4. Deterministic drift detection invalidates stale proof for BOTH pinned
    // VC-203-007 transport specifications.
    let clean = susi_json(
        root,
        &op_home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            ".agents/gmcp/spec",
        ],
    );
    assert_eq!(clean["drift"], true);
    assert_eq!(clean["verified"], true);
    assert_eq!(clean["checked"], clean["matched"]);
    assert!(clean["findings"].as_array().unwrap().is_empty());

    let pinned_docs = [
        "docs/specification/2025-11-25/basic/transports.mdx",
        "docs/specification/2026-07-28/basic/transports/index.mdx",
    ];
    for (idx, transport_doc) in pinned_docs.iter().enumerate() {
        let refetched = scratch.join(format!("refetched-{idx}"));
        copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
        let victim = refetched.join(transport_doc);
        let mut bytes = std::fs::read(&victim).unwrap();
        bytes.push(b'!');
        std::fs::write(&victim, &bytes).unwrap();
        let drifted = susi_command(
            root,
            &op_home,
            &[
                "mcp",
                "ledger",
                "--drift",
                "--refetched",
                refetched.to_str().unwrap(),
            ],
        )
        .output()
        .unwrap();
        assert!(!drifted.status.success());
        let drift_report: serde_json::Value =
            serde_json::from_slice(&drifted.stdout).expect("drift failure must remain JSON");
        assert_eq!(drift_report["verified"], false);
        assert!(drift_report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["document"] == *transport_doc && f["kind"] == "hash_drift"));

        std::fs::remove_file(&victim).unwrap();
        let missing = susi_command(
            root,
            &op_home,
            &[
                "mcp",
                "ledger",
                "--drift",
                "--refetched",
                refetched.to_str().unwrap(),
            ],
        )
        .output()
        .unwrap();
        assert!(!missing.status.success());
        let missing_report: serde_json::Value =
            serde_json::from_slice(&missing.stdout).expect("missing source must remain JSON");
        assert!(missing_report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["document"] == *transport_doc && f["kind"] == "unfetchable"));
    }

    // 5. Contract carries the complete operations, examples, and incident runbook.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-007.md"))
        .expect("contract must exist");
    for token in [
        "Operations and incident runbook",
        "Inspectable state",
        "Configuration validation",
        "Bounded metrics, logs and traces",
        "Diagnostics and incident triage",
        "Incident triage",
        "Examples",
        "Migration (re-pinning",
        "Rollback",
        "hash_drift",
        "unfetchable",
        "max_rpc_body_bytes",
        "BoundedLines",
        "StdioGate",
        "-32600",
        "-32700",
        "2025-11-25/basic/transports.mdx",
        "2026-07-28/basic/transports/index.mdx",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // 6. Reproducible evidence artifacts & ledger binding: every VC-203-007 row
    // points to `.agents/gmcp/evidence/VC-203-007.jsonl`, the recorder script
    // is present, and existing retained evidence records match the schema.
    let ledger_raw = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap();
    let ledger: serde_json::Value = serde_json::from_slice(&ledger_raw).unwrap();
    let vc_rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-007")
        .collect();
    assert_eq!(vc_rows.len(), 80);
    assert!(vc_rows
        .iter()
        .all(|row| { row["trace"]["evidence"] == ".agents/gmcp/evidence/VC-203-007.jsonl" }));
    let recorder = std::fs::read_to_string(root.join("scripts/gmcp-record-evidence.sh"))
        .expect("reproducible evidence recorder must exist");
    assert!(recorder.contains("exec cargo local-release record-gmcp \"$@\""));
    let native_recorder = std::fs::read_to_string(root.join("xtask/src/gmcp_evidence.rs")).unwrap();
    assert!(native_recorder.contains(".susi/records/gmcp/evidence")
        && native_recorder.contains("requirements_sha256")
        && native_recorder.contains("tested_artifacts"));

    let ledger_hash = susi_paths::hex::encode(Sha256::digest(&ledger_raw));
    let vc7_evidence_path = gmcp_evidence_path(root, ".agents/gmcp/evidence/VC-203-007.jsonl");
    if vc7_evidence_path.exists() {
        let evidence = std::fs::read_to_string(&vc7_evidence_path).unwrap();
        if let Some(record) = newest_records(&evidence)
            .into_iter()
            .find(|record| record["requirement"] == "GMCP-203-007-06")
        {
            assert_eq!(
                record["check"],
                "cargo test --locked --test gmcp_requirements gmcp_req_203_007_06_operations"
            );
            assert_eq!(record["exit"], 0);
            assert_eq!(record["artifact_sha256"], ledger_hash);
            assert!(!record["detail"].as_str().unwrap_or_default().is_empty());
            assert!(!record["identity"].as_str().unwrap_or_default().is_empty());
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// VC-203-007 mastery: transport adapter SPI and embedded/in-process operation.
///
/// Proves end-to-end across shipped production surfaces, live transport
/// adapters (`embedded_adapter`, `#[cfg(unix)] socket_adapter`, `susi mcp serve`),
/// direct/HTTP JSON-RPC gates, fail-closed startup configuration validation,
/// pinned specification verification, retained evidence records, roadmap/verdict
/// state, and committed clean artifacts.
#[test]
fn gmcp_vc_203_007_mastery() {
    use sha2::{Digest, Sha256};
    use std::io::Write as _;
    use std::time::{Duration, Instant};
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // ── 1. Shipped production path: CLI and tool surfaces emit the
    //    identical canonical report and cover all 80 VC-203-007 rows ────
    let cli = ledger_report(root);
    assert_eq!(cli["verified"], true, "production path must verify");
    assert!(
        cli["rows"].as_u64().unwrap_or(0) >= 80,
        "shipped ledger must cover VC-203-007"
    );
    assert_eq!(
        ledger_tool_report(root),
        cli,
        "ledger_verify tool and CLI must emit identical canonical reports"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-007.md"))
        .expect("contract-203-007.md must exist");
    let ledger_bytes =
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).expect("ledger readable");
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(&ledger_bytes));
    let ledger: serde_json::Value =
        serde_json::from_slice(&ledger_bytes).expect("canonical ledger must parse");
    let vc_rows: Vec<_> = ledger["requirements"]
        .as_array()
        .expect("ledger requirements must be an array")
        .iter()
        .filter(|row| row["trace"]["vector"] == "VC-203-007")
        .collect();
    assert_eq!(
        vc_rows.len(),
        80,
        "VC-203-007 must bind exactly 80 normative requirements"
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
        assert_eq!(production, "crates/susi-gmcp/src/server.rs");
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        assert_eq!(
            row["trace"]["positive_test"],
            "tests/gmcp_requirements.rs::gmcp_req_203_007_03_integration"
        );
        assert_eq!(
            row["trace"]["negative_test"],
            "tests/gmcp_requirements.rs::gmcp_req_203_007_04_adversarial"
        );
        assert_eq!(
            row["trace"]["evidence"],
            ".agents/gmcp/evidence/VC-203-007.jsonl"
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
            "contract-203-007.md must bind row {id}"
        );
    }

    // ── 2. Multi-adapter SPI parity, gate equivalence, fail-closed ops ─
    let (_lock, _env, home) = gmcp_http_home_guard("007-mastery");
    let runtime = adapter_runtime(&home);
    let budgets = adapter_budgets(root);

    // Embedded adapter transcript + frame limit + clean session shutdown.
    let opened = Instant::now();
    let (mut embedded, embedded_session) = embedded_adapter(&runtime, root);
    let embedded_script = transport_adapter_script(&mut embedded);
    let startup_ms = opened.elapsed().as_millis();
    assert!(
        startup_ms <= budgets["startup_ms"],
        "startup_ms {startup_ms} exceeds budget {}",
        budgets["startup_ms"]
    );
    assert!(
        embedded_script.len() >= 5,
        "adapter script must exercise multiple protocol stages"
    );
    let embedded_limit = adapter_limit_step(&mut embedded);
    assert!(
        embedded_limit.is_i64(),
        "an oversized line must earn a typed error code: {embedded_limit}"
    );

    // Direct JSON-RPC gate vs HTTP envelope gate vs live embedded adapter.
    for (name, line) in [
        ("batch", br#"[]"#.as_slice()),
        (
            "null-id",
            br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
        ),
        (
            "non-2.0",
            br#"{"jsonrpc":"1.0","id":null,"method":"ping"}"#.as_slice(),
        ),
        ("truncated", br#"{"jsonrpc":"2.0","#.as_slice()),
    ] {
        let mut era = EraState::default();
        let direct = match classify_line(line, &mut era) {
            GateVerdict::Reject { code, .. } => code,
            other @ (GateVerdict::Forward | GateVerdict::Drop) => {
                panic!("gate must reject {name}, got {other:?}")
            }
        };
        let gateway = http_envelope_rejection(line)
            .unwrap_or_else(|| panic!("HTTP gate must reject {name}"))["error"]["code"]
            .as_i64()
            .unwrap();
        assert_eq!(direct, gateway, "{name}: direct and HTTP gates must agree");
        let mut framed = line.to_vec();
        framed.push(b'\n');
        (embedded.write)(&framed);
        let live = embedded.rejection(name);
        assert_eq!(
            live["error"]["code"].as_i64(),
            Some(direct),
            "{name}: live adapter rejection must match gate"
        );
    }

    // Bystander isolation during adversarial attack battery on embedded adapter.
    let (mut bystander, bystander_session) = embedded_adapter(&runtime, root);
    adapter_handshake(&mut bystander, "gmcp-007-mastery-bystander");
    let (mut attacked, attacked_session) = embedded_adapter(&runtime, root);
    let attacks = transport_adapter_attacks(&mut attacked);
    assert_eq!(attacks.len(), 11, "all 11 adversarial vectors must answer");
    assert_eq!(
        bystander.call("bystander-ping", "ping", serde_json::json!({}))["result"],
        serde_json::json!({})
    );
    drop(attacked);
    adapter_session_ended(&runtime, attacked_session, "embedded attacked");
    drop(bystander);
    adapter_session_ended(&runtime, bystander_session, "embedded bystander");
    drop(embedded);
    adapter_session_ended(&runtime, embedded_session, "embedded primary");

    // Unix-domain socket parity against the in-process adapter on Unix targets.
    #[cfg(unix)]
    {
        let (mut socket, socket_session) = socket_adapter(&runtime, root);
        assert_eq!(
            transport_adapter_script(&mut socket),
            embedded_script,
            "Unix-domain socket adapter must match embedded adapter script verbatim"
        );
        assert_eq!(
            adapter_limit_step(&mut socket),
            embedded_limit,
            "Unix-domain socket adapter must match embedded adapter limit error"
        );
        drop(socket);
        adapter_session_ended(&runtime, socket_session, "unix socket mastery");
    }

    // Shipped `susi mcp serve` matches embedded adapter across both eras.
    let fixtures = adapter_wire_fixtures(root);
    let (mut modern_emb, modern_emb_session) = embedded_adapter(&runtime, root);
    adapter_modern_open(&mut modern_emb);
    let modern_expected = adapter_fixture_replay(&mut modern_emb, &fixtures, "modern");
    drop(modern_emb);
    adapter_session_ended(&runtime, modern_emb_session, "embedded modern fixtures");

    let (mut legacy_emb, legacy_emb_session) = embedded_adapter(&runtime, root);
    adapter_handshake(&mut legacy_emb, "gmcp-007-mastery-legacy");
    let legacy_expected = adapter_fixture_replay(&mut legacy_emb, &fixtures, "legacy");
    drop(legacy_emb);
    adapter_session_ended(&runtime, legacy_emb_session, "embedded legacy fixtures");

    for (era, expected) in [("legacy", &legacy_expected), ("modern", &modern_expected)] {
        let mut child = susi_command(root, &home, &["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("susi mcp serve must spawn");
        let mut stdin = child.stdin.take().expect("stdin");
        let rx = adapter_lines(child.stdout.take().expect("stdout"));
        let mut stdio = AdapterClient {
            write: Box::new(move |bytes| {
                stdin
                    .write_all(bytes)
                    .and_then(|()| stdin.flush())
                    .expect("stdin write");
            }),
            rx,
            pending: Vec::new(),
        };
        if era == "modern" {
            adapter_modern_open(&mut stdio);
        } else {
            adapter_handshake(&mut stdio, "gmcp-007-mastery-stdio");
        }
        assert_eq!(
            &adapter_fixture_replay(&mut stdio, &fixtures, era),
            expected,
            "shipped stdio entry must match embedded adapter on {era} fixtures"
        );
        drop(stdio);
        let deadline = Instant::now() + Duration::from_secs(30);
        while child.try_wait().expect("try_wait").is_none() {
            if Instant::now() > deadline {
                stop_test_server(&mut child);
                panic!("`susi mcp serve` did not exit on stdin EOF");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // Fail-closed configuration validation: max_rpc_body_bytes = 0 refuses startup.
    let zero_home = home.join("zero-limit");
    std::fs::create_dir_all(zero_home.join(".susi")).unwrap();
    std::fs::write(
        zero_home.join(".susi/config.json"),
        serde_json::json!({ "max_rpc_body_bytes": 0 }).to_string(),
    )
    .unwrap();
    let mut zero_child = susi_command(root, &zero_home, &["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn zero-limit susi mcp serve");
    if let Some(stdin) = zero_child.stdin.as_mut() {
        let _ = writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": "zero-ping", "method": "ping", "params": {}})
        );
    }
    drop(zero_child.stdin.take());
    let zero_out = zero_child.wait_with_output().expect("wait zero-limit");
    assert!(
        zero_out.stdout.is_empty(),
        "zero-limit startup must refuse requests and emit zero stdout bytes, got: {}",
        String::from_utf8_lossy(&zero_out.stdout)
    );
    let zero_stderr = String::from_utf8_lossy(&zero_out.stderr);
    assert!(
        zero_stderr.contains("max_rpc_body_bytes") && zero_stderr.contains("must be > 0"),
        "zero-limit startup must diagnose max_rpc_body_bytes on stderr: {zero_stderr}"
    );

    // ── 3. Contract sections, explicit non-claims, and pinned specs ──
    for token in [
        "Operations and incident runbook",
        "Configuration validation",
        "## Production guarantees and non-claims",
        "Not covered:",
    ] {
        assert!(
            contract.contains(token),
            "contract-203-007.md missing required section/token: {token}"
        );
    }
    for rel in [
        ".agents/gmcp/spec/docs/specification/2025-11-25/basic/transports.mdx",
        ".agents/gmcp/spec/docs/specification/2026-07-28/basic/transports/index.mdx",
    ] {
        let bytes = std::fs::read(root.join(rel))
            .unwrap_or_else(|e| panic!("pinned spec {rel} missing: {e}"));
        let hex = susi_paths::hex::encode(Sha256::digest(&bytes));
        assert!(
            contract.contains(&hex),
            "contract-203-007.md must bind live sha256 {hex} of {rel}"
        );
    }

    // ── 4. Evidence verification: complete, bound to HEAD commit,
    //    signed, exit 0, matching current ledger hash ────────────────
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let evidence = std::fs::read_to_string(gmcp_evidence_path(root, ".agents/gmcp/evidence/VC-203-007.jsonl"))
        .expect("VC-203-007 must retain an evidence artifact");
    let artifacts = evidence_artifacts(root, "VC-203-007");
    let head_digest =
        artifacts_digest(root, "HEAD", &artifacts).expect("HEAD must list VC-203-007 artifacts");
    let mut covered = std::collections::BTreeSet::new();
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
        "GMCP-203-007-01",
        "GMCP-203-007-02",
        "GMCP-203-007-03",
        "GMCP-203-007-04",
        "GMCP-203-007-05",
        "GMCP-203-007-06",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── 5. Roadmap and verdict delivery verification ────────────────
    let roadmap_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/roadmap.json"))
            .expect("roadmap.json must be readable"),
    )
    .expect("roadmap.json must parse");
    let vc_entry = roadmap_json["vectors"]
        .as_array()
        .expect("roadmap.json vectors must be an array")
        .iter()
        .find(|entry| entry["id"] == "VC-203-007")
        .expect("VC-203-007 must exist in roadmap.json");
    assert!(
        vc_entry["progress"]
            .as_str()
            .unwrap_or_default()
            .starts_with("DELIVERED"),
        "VC-203-007 must be marked DELIVERED in roadmap.json"
    );

    let verdict: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/roadmap-verdicts/VC-203-007.json"))
            .expect("VC-203-007.json verdict must exist"),
    )
    .expect("verdict must parse");
    assert_eq!(verdict["vector"], "VC-203-007");
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
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert_eq!(tracked.len(), 7, "VC-203-007 must track all 7 tasks");
    for num in 86..=92 {
        let tid = format!("T-CODEXGMCPROADMAP-{num}");
        assert!(
            tracked.contains(&tid.as_str()),
            "verdict missing tracked task {tid}"
        );
    }

    // ── 6. Committed proof ──────────────────────────────────────────
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

    let _ = std::fs::remove_dir_all(&home);
}

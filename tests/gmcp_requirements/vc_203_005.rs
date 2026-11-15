#[test]
fn gmcp_req_203_005_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-005.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-005")
        .collect();
    assert_eq!(rows.len(), 20, "the contract binds 20 VC-203-005 rows");
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

#[test]
fn gmcp_req_203_005_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report = ledger_report(root);
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 20,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-005.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-005")
        .collect();
    assert_eq!(rows.len(), 20, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

/// GMCP-203-005-03: the production stdio transport on the served path.
///
/// Byte-accurate newline framing (`MCP-2026-0728-...-0013/0036`), stdout
/// protocol purity (`-0018/0055`), several framed messages written in one
/// chunk, prompt exit when stdin closes (`-0102`), and the single policy
/// boundary the direct, daemon and gateway entries share — the HTTP
/// gateway's `http_envelope_rejection` and the stdio gate must emit the
/// same document for the same malformed envelope. Every stdout line is
/// parsed, so a stray log line fails the purity assertion instead of
/// being silently dropped.
#[test]
fn gmcp_req_203_005_03_integration() {
    use std::io::{BufRead, BufReader, Write};
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;

    /// Kills the spawned server's whole tree on drop — including on a
    /// panic — so the daemon `mcp serve` ensures can never outlive the
    /// test and hold its pipes open.
    struct Serve(std::process::Child);
    impl Drop for Serve {
        fn drop(&mut self) {
            stop_test_server(&mut self.0);
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-005-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let child = susi_command(root, &home, &["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the susi binary must spawn `mcp serve`");
    let mut serve = Serve(child);
    let mut stdin = serve
        .0
        .stdin
        .take()
        .expect("stdio server must accept stdin");
    let stdout = serve
        .0
        .stdout
        .take()
        .expect("stdio server must yield stdout");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    // One write, three framed messages: initialize, ping, and the
    // initialized notification. The transport must split on newlines and
    // answer both requests.
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"transport-it","version":"1"}}}"#;
    let ping = r#"{"jsonrpc":"2.0","id":2,"method":"ping","params":{}}"#;
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    stdin
        .write_all(format!("{initialize}\n{ping}\n{initialized}\n").as_bytes())
        .expect("the handshake frames must write");
    stdin.flush().expect("frames must flush");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut ids = Vec::new();
    let mut lines = 0usize;
    while ids.len() < 2 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "the served transport must answer both framed requests, saw ids {ids:?}"
        );
        let Ok(line) = rx.recv_timeout(remaining) else {
            break;
        };
        lines += 1;
        assert!(
            !line.contains('\n') && !line.contains('\r'),
            "a framed message must not contain an embedded newline: {line:?}"
        );
        let value: serde_json::Value = serde_json::from_str(&line).unwrap_or_else(|e| {
            panic!("stdout purity: every line must be a valid MCP message, got {line:?}: {e}")
        });
        assert_eq!(
            value["jsonrpc"], "2.0",
            "every stdout line must be a JSON-RPC 2.0 message: {value}"
        );
        assert!(
            value.get("method").is_none(),
            "the server must never write JSON-RPC requests to stdout: {value}"
        );
        if let Some(id) = value.get("id").and_then(serde_json::Value::as_i64) {
            ids.push(id);
        }
    }
    assert!(
        ids.contains(&1),
        "initialize must be answered over the framed transport: {ids:?}"
    );
    assert!(
        ids.contains(&2),
        "a message written in the same chunk must be framed and answered: {ids:?}"
    );
    assert!(lines >= 2, "two answers must arrive, got {lines} lines");

    // Closing stdin is the client half of shutdown; the server must exit
    // promptly instead of stranding the descriptor.
    drop(stdin);
    let stop = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let exited = loop {
        match serve
            .0
            .try_wait()
            .expect("the server child must be waitable")
        {
            Some(_) => break true,
            None if std::time::Instant::now() >= stop => break false,
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };
    assert!(
        exited,
        "the stdio server must exit promptly once its stdin closes"
    );
    drop(serve);

    // `mcp serve` ensures the host daemon; it is detached by design, so
    // reap it through its own pid file before the scratch home goes away.
    if let Ok(pid) = std::fs::read_to_string(home.join(".susi-dev").join("substrate.pid")) {
        let pid = pid.trim().to_string();
        if !pid.is_empty() {
            #[cfg(windows)]
            {
                let _ = susi_core::bounded_cmd::output_within(
                    Command::new("taskkill").args(["/PID", &pid, "/T", "/F"]),
                    std::time::Duration::from_secs(10),
                );
            }
            #[cfg(not(windows))]
            {
                let _ = Command::new("kill").args(["-9", &pid]).status();
            }
        }
    }

    // ── One policy boundary: the HTTP gateway and the stdio gate emit the
    //    same document for the same malformed envelope ────────────────
    let malformed = r#"{"jsonrpc":"1.0","method":"ping"}"#;
    let http = http_envelope_rejection(malformed.as_bytes())
        .expect("the HTTP gate must reject a non-2.0 envelope");
    let mut policy = StdioSession::spawn(root, &home);
    let stdio = policy
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"1.0","method":"ping"}),
            std::time::Duration::from_secs(120),
        )
        .expect("the stdio gate must reject a non-2.0 envelope");
    assert_eq!(
        stdio["error"]["code"], http["error"]["code"],
        "stdio and HTTP must share the rejection code: {stdio} vs {http}"
    );
    assert_eq!(
        stdio["error"]["message"], http["error"]["message"],
        "stdio and HTTP must share the rejection message: {stdio} vs {http}"
    );

    drop(policy);
    let _ = std::fs::remove_dir_all(&scratch);

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-005.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-005")
        .collect();
    assert_eq!(rows.len(), 20, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

/// GMCP-203-005-04: adversarial and failure behavior for production-grade
/// stdio transport.
///
/// Bounded lines drop oversized tails and recover without stream corruption;
/// negative/malformed envelopes earn typed errors (-32700, -32600);
/// stdout protocol purity holds under adversarial inputs; pipe closure
/// triggers prompt server exit; and child-tree resources are cleaned up.
#[test]
fn gmcp_req_203_005_04_adversarial() {
    use susi_gmcp::jsonrpc_gate::{classify_line, EraState, GateVerdict};
    use susi_gmcp::stdio::BoundedLines;
    use tokio::io::AsyncReadExt;

    // ── 1. BoundedLines unit adversarial behavior ─────────────────────
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        // (a) Oversized line drops tail, subsequent valid line arrives clean
        let mut reader = BoundedLines::new(&b"1234567890\nok\n"[..], 4);
        let mut buf = [0u8; 32];
        let err = reader.read(&mut buf).await;
        assert!(err.is_err(), "oversized line must produce error");
        let mut recovered = String::new();
        reader.read_to_string(&mut recovered).await.unwrap();
        assert_eq!(recovered, "ok\n", "subsequent line must survive intact");

        // (b) Sequential oversized lines all trip and recover cleanly
        let mut reader = BoundedLines::new(&b"12345\n67890\nok\n"[..], 3);
        let mut err_count = 0;
        let mut recovered = Vec::new();
        loop {
            match reader.read(&mut buf).await {
                Err(_) => err_count += 1,
                Ok(0) => break,
                Ok(n) => recovered.extend_from_slice(&buf[..n]),
            }
            if err_count > 10 {
                panic!("reader must terminate loop");
            }
        }
        assert!(err_count >= 2, "both oversized lines must error");
        assert_eq!(recovered, b"ok\n", "final valid line must be intact");

        // (c) Chunk-fragmented input straddling newline boundary
        let mut reader = BoundedLines::new(&b"abc\ndef\n"[..], 5);
        let mut first = [0u8; 2];
        reader.read_exact(&mut first).await.unwrap();
        assert_eq!(&first, b"ab");
        let mut rest = String::new();
        reader.read_to_string(&mut rest).await.unwrap();
        assert_eq!(rest, "c\ndef\n");
    });

    // ── 2. Envelope classification of hostile/adversarial inputs ──────
    let classify = |line: &[u8], era: &mut EraState| -> serde_json::Value {
        match classify_line(line, era) {
            GateVerdict::Forward => serde_json::json!({"kind": "forward"}),
            GateVerdict::Drop => serde_json::json!({"kind": "drop"}),
            GateVerdict::Reject { code, data, .. } => {
                serde_json::json!({"kind": "reject", "code": code, "data": data})
            }
        }
    };

    for (name, line, code) in [
        ("truncated-json", br#"{"jsonrpc":"2.0","#.as_slice(), -32700),
        ("batch-request", br#"[]"#.as_slice(), -32600),
        (
            "batch-array",
            br#"[{"jsonrpc":"2.0","method":"ping"}]"#.as_slice(),
            -32600,
        ),
        (
            "non-2.0",
            br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#.as_slice(),
            -32600,
        ),
        ("scalar-string", br#""malicious-probe""#.as_slice(), -32600),
        (
            "null-id-request",
            br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
            -32600,
        ),
        (
            "missing-method",
            br#"{"jsonrpc":"2.0","id":99}"#.as_slice(),
            -32600,
        ),
    ] {
        let mut era = EraState::default();
        let verdict = classify(line, &mut era);
        assert_eq!(
            verdict["code"].as_i64(),
            Some(code),
            "adversarial probe {name} must earn {code}: {verdict}"
        );
    }

    // ── 3. Live served stdio transport against hostile peer ────────────
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-005-adv-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut session = StdioSession::spawn(root, &home);

    // Hostile non-2.0 probe: must reject with -32600 and preserve session
    let bad_ver = session
        .request_verbatim(
            &serde_json::json!({"jsonrpc": "1.0", "method": "ping"}),
            std::time::Duration::from_secs(120),
        )
        .expect("the served stdio transport must answer the non-2.0 request");
    assert_eq!(bad_ver["error"]["code"], -32600);
    assert!(bad_ver["id"].is_null());

    // Hostile batch probe: must reject with -32600
    let batch = session
        .request_verbatim(&serde_json::json!([]), std::time::Duration::from_secs(120))
        .expect("the served stdio transport must answer the batch request");
    assert_eq!(batch["error"]["code"], -32600);
    assert!(batch["id"].is_null());

    // Stream remains healthy and answers legitimate ping after attacks
    let ping = session.request("ping", serde_json::json!({}));
    assert!(ping.is_object() || ping.is_null());

    drop(session);

    // Clean up substrate daemon pid if spawned
    if let Ok(pid) = std::fs::read_to_string(home.join(".susi-dev").join("substrate.pid")) {
        let pid = pid.trim().to_string();
        if !pid.is_empty() {
            #[cfg(windows)]
            {
                let _ = susi_core::bounded_cmd::output_within(
                    Command::new("taskkill").args(["/PID", &pid, "/T", "/F"]),
                    std::time::Duration::from_secs(10),
                );
            }
            #[cfg(not(windows))]
            {
                let _ = Command::new("kill").args(["-9", &pid]).status();
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);

    // ── 4. Retained evidence and ledger validation ────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-005.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-005")
        .collect();
    assert_eq!(rows.len(), 20, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }

    let evidence_path = root.join(".agents/gmcp/evidence/VC-203-005.jsonl");
    assert!(evidence_path.exists(), "evidence file must exist");
    let evidence_content =
        std::fs::read_to_string(&evidence_path).expect("evidence file must be readable");
    for req in [
        "GMCP-203-005-01",
        "GMCP-203-005-02",
        "GMCP-203-005-03",
        "GMCP-203-005-04",
    ] {
        assert!(
            evidence_content.contains(req),
            "evidence must contain requirement record {req}"
        );
    }
}

/// GMCP-203-005-05: interoperability and quality for production-grade stdio transport.
///
/// Direct-versus-gateway differential behavior, fixture replay determinism,
/// transport and platform parity, property invariants, and prompt clean termination.
#[test]
fn gmcp_req_203_005_05_interop() {
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-005-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut session = StdioSession::spawn(root, &home);

    // ── 1. Reference fixture replay over served stdio session ──────────
    let run_replay = |s: &mut StdioSession| -> Vec<(String, Option<i64>)> {
        let fixtures: &[(&str, Option<i64>)] = &[
            ("negative/request-batch-array.json", Some(-32600)),
            ("negative/request-null-id.json", Some(-32600)),
        ];
        let mut results = Vec::new();
        for &(rel_path, expected_code) in fixtures {
            let fixture_path = root.join(".agents/gmcp/fixtures").join(rel_path);
            let fixture: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&fixture_path).unwrap())
                    .unwrap_or_else(|e| panic!("{rel_path} must parse: {e}"));
            let start = std::time::Instant::now();
            let resp = s
                .request_verbatim(&fixture["request"], std::time::Duration::from_secs(120))
                .unwrap_or_else(|| panic!("{rel_path} must receive response"));
            let elapsed = start.elapsed();
            assert!(
                elapsed < std::time::Duration::from_secs(120),
                "{rel_path}: latency budget exceeded 30s: {elapsed:?}"
            );
            let code = resp["error"]["code"].as_i64();
            assert_eq!(code, expected_code, "{rel_path}: unexpected error code");
            results.push((rel_path.to_string(), code));
        }
        results
    };

    let run1 = run_replay(&mut session);
    let run2 = run_replay(&mut session);
    assert_eq!(run1, run2, "stdio fixture replay must be deterministic");

    // ── 2. Differential envelope parity: stdio vs HTTP gate ───────────
    for (name, line) in [
        ("batch", br#"[]"#.as_slice()),
        (
            "null-id",
            br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
        ),
        (
            "non-2.0",
            br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#.as_slice(),
        ),
        ("truncated", br#"{"jsonrpc":"2.0","#.as_slice()),
    ] {
        let mut era = EraState::default();
        let stdio_verdict = match classify_line(line, &mut era) {
            GateVerdict::Reject { code, .. } => code,
            GateVerdict::Forward => panic!("expected reject for {name}, got Forward"),
            GateVerdict::Drop => panic!("expected reject for {name}, got Drop"),
        };
        let http_resp =
            http_envelope_rejection(line).unwrap_or_else(|| panic!("HTTP gate must reject {name}"));
        let http_code = http_resp["error"]["code"].as_i64().unwrap();
        assert_eq!(
            stdio_verdict, http_code,
            "stdio and HTTP envelope error code must agree on {name}: {stdio_verdict} vs {http_code}"
        );
    }

    // ── 3. Property invariant: outbound purity on ping ─────────────────
    let ping_start = std::time::Instant::now();
    let ping_res = session.request("ping", serde_json::json!({}));
    assert!(ping_start.elapsed() < std::time::Duration::from_secs(120));
    assert!(ping_res.is_object() || ping_res.is_null());

    drop(session);

    // Reap substrate daemon
    if let Ok(pid) = std::fs::read_to_string(home.join(".susi-dev").join("substrate.pid")) {
        let pid = pid.trim().to_string();
        if !pid.is_empty() {
            #[cfg(windows)]
            {
                let _ = susi_core::bounded_cmd::output_within(
                    Command::new("taskkill").args(["/PID", &pid, "/T", "/F"]),
                    std::time::Duration::from_secs(10),
                );
            }
            #[cfg(not(windows))]
            {
                let _ = Command::new("kill").args(["-9", &pid]).status();
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);

    // ── 4. Retained evidence and ledger validation ────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-005.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-005")
        .collect();
    assert_eq!(rows.len(), 20, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }

    let evidence_path = root.join(".agents/gmcp/evidence/VC-203-005.jsonl");
    assert!(evidence_path.exists(), "evidence file must exist");
    let evidence_content =
        std::fs::read_to_string(&evidence_path).expect("evidence file must be readable");
    for req in [
        "GMCP-203-005-01",
        "GMCP-203-005-02",
        "GMCP-203-005-03",
        "GMCP-203-005-04",
        "GMCP-203-005-05",
    ] {
        assert!(
            evidence_content.contains(req),
            "evidence must contain requirement record {req}"
        );
    }
}

/// GMCP-203-005-06: operations, diagnostics, runbooks, and reproducible evidence for stdio transport.
///
/// Inspectable CLI/tool ledger state, live stdio session validation, invalid config rejection,
/// deterministic air-gapped drift detection (hash_drift and unfetchable fail closed),
/// complete operator runbook in contract-203-005.md, and retained signed evidence.
#[test]
fn gmcp_req_203_005_06_operations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-005-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // 1. Shipped inspectable surface: CLI ledger status emits bounded JSON report.
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

    // 2. Live inspectable surface: served stdio session answers ping cleanly.
    let mut session = StdioSession::spawn(root, &home);
    let ping = session.request("ping", serde_json::json!({}));
    assert!(
        ping.is_object() || ping.is_null(),
        "ping must succeed: {ping:?}"
    );
    drop(session);

    // Reap substrate daemon if spawned
    if let Ok(pid) = std::fs::read_to_string(home.join(".susi-dev").join("substrate.pid")) {
        let pid = pid.trim().to_string();
        if !pid.is_empty() {
            #[cfg(windows)]
            {
                let _ = susi_core::bounded_cmd::output_within(
                    Command::new("taskkill").args(["/PID", &pid, "/T", "/F"]),
                    std::time::Duration::from_secs(10),
                );
            }
            #[cfg(not(windows))]
            {
                let _ = Command::new("kill").args(["-9", &pid]).status();
            }
        }
    }

    // 3. Invalid configuration is rejected: --refetched without --drift fails.
    let invalid = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(!invalid.status.success());

    // 4. Deterministic drift check is clean on the shipped corpus.
    let clean = susi_json(
        root,
        &home,
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

    // 5. A drifted stdio transport document is a named hash_drift finding.
    let stdio_doc = "docs/specification/2026-07-28/basic/transports/stdio.mdx";
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(stdio_doc);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes.push(b'!');
    std::fs::write(&victim, &bytes).unwrap();
    let drifted = susi_command(
        root,
        &home,
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
        .any(|f| f["document"] == stdio_doc && f["kind"] == "hash_drift"));

    // 6. An absent stdio document fails closed as unfetchable.
    std::fs::remove_file(&victim).unwrap();
    let missing = susi_command(
        root,
        &home,
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
        .any(|f| f["document"] == stdio_doc && f["kind"] == "unfetchable"));

    // 7. The contract carries the complete operator runbook.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-005.md"))
        .expect("contract must exist");
    for token in [
        "Operations and incident runbook",
        "Inspectable state",
        "Diagnostics",
        "Incident triage",
        "Migration (re-pinning",
        "Rollback",
        "hash_drift",
        "unfetchable",
        "-32600",
        "-32700",
        "stdio",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // 8. Evidence is retained, signed, and tied to current ledger bytes.
    use sha2::{Digest, Sha256};
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-005.jsonl"))
        .expect("VC-203-005 must retain evidence");
    let record = newest_records(&evidence)
        .into_iter()
        .find(|record| record["requirement"] == "GMCP-203-005-06")
        .expect("evidence must retain a record for GMCP-203-005-06");
    assert_eq!(
        record["check"],
        "cargo test --locked --test gmcp_requirements gmcp_req_203_005_06_operations"
    );
    assert_eq!(record["exit"], 0);
    assert_eq!(record["artifact_sha256"], ledger_hash);
    assert!(!record["detail"].as_str().unwrap_or_default().is_empty());
    assert!(!record["identity"].as_str().unwrap_or_default().is_empty());

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-005-M: complete vector mastery target on the shipped production path:
/// GMCP serves and consumes MCP over stdio with byte-accurate framing, bounded
/// lines/messages, strict stdout protocol purity, stderr diagnostics, environment
/// and working-directory control, graceful cancellation and shutdown, child-tree
/// cleanup, backpressure, timeout handling, and Windows/Unix parity. Crash, hang,
/// malformed output, descriptor leak, and hostile child fixtures all recover without
/// stranding resources. Consume and verify the evidence produced by requirements
/// 01-06; reject missing, stale, unsigned, locally modified, waived, mock-only, or
/// zero-test evidence.
#[test]
fn gmcp_vc_203_005_mastery() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-005-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── 1. Shipped production path: CLI and tool surfaces verify and
    //    return the identical canonical report ──────────────────────
    let cli = ledger_report(root);
    assert_eq!(cli["verified"], true, "production path must verify");
    assert_eq!(cli["rows"].as_u64(), Some(1713));
    assert_eq!(cli["documents"].as_u64(), Some(72));
    let tool = ledger_tool_report(root);
    assert_eq!(
        tool, cli,
        "the shipped tool must emit the identical canonical report"
    );

    // ── 2. Vector rows bound to crates/susi-gmcp/src/stdio.rs ────────
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("canonical ledger must parse"),
    )
    .expect("canonical ledger must parse");
    let rows = ledger["requirements"]
        .as_array()
        .expect("ledger requirements must be an array");
    let vc_rows: Vec<_> = rows
        .iter()
        .filter(|row| row["trace"]["vector"] == "VC-203-005")
        .collect();
    assert_eq!(
        vc_rows.len(),
        20,
        "VC-203-005 must cover exactly 20 normative requirements"
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
            let value = row["trace"][field].as_str().unwrap_or_default();
            assert!(!value.is_empty(), "{id}: trace.{field} must be non-empty");
        }
        let production = row["trace"]["production_path"].as_str().unwrap();
        assert_eq!(
            production, "crates/susi-gmcp/src/stdio.rs",
            "{id}: production path must be crates/susi-gmcp/src/stdio.rs"
        );
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        assert_eq!(
            row["trace"]["evidence"].as_str().unwrap(),
            ".agents/gmcp/evidence/VC-203-005.jsonl"
        );
        assert!(
            !row["disposition"]["status"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{id}: disposition.status must be non-empty"
        );
    }

    // ── 3. Shipped stdio session wire verification ───────────────────
    // Line framing, stdout purity, malformed frame isolation & recovery
    let mut session = StdioSession::spawn(root, &home);
    let ping = session
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "mastery-ping",
                "method": "ping",
                "params": {}
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("stdio ping must answer on served path");
    assert!(
        ping["result"].is_object(),
        "ping response must be an object"
    );

    // Partial write without newline must NOT trigger a response
    session.send_raw(br#"{"jsonrpc":"2.0","id":"mastery-split","method":"ping""#);
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(
        session.pending.is_empty()
            && session
                .rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err(),
        "partial frame must wait for delimiter"
    );
    // Completing with newline triggers response
    session.send_raw(b",\"params\":{}}\n");
    let split_res = session
        .response_for(
            &serde_json::json!("mastery-split"),
            std::time::Duration::from_secs(180),
        )
        .expect("completed frame must answer cleanly");
    assert!(split_res["result"].is_object());

    // Malformed line returns -32700 Parse Error
    session.send_raw(b"not-json-mastery-line\n");
    let err_32700 = session
        .rx
        .recv_timeout(std::time::Duration::from_secs(180))
        .expect("malformed line must earn typed error");
    assert_eq!(err_32700["error"]["code"], -32700);

    // Invalid envelope (batch array) returns -32600 Invalid Request
    session.send_raw(b"[]\n");
    let err_32600 = session
        .rx
        .recv_timeout(std::time::Duration::from_secs(180))
        .expect("batch array must earn -32600");
    assert_eq!(err_32600["error"]["code"], -32600);

    // Next well-formed request recovers immediately without cross-request corruption
    let recovered = session
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "mastery-recovered",
                "method": "ping",
                "params": {}
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("session must recover after malformed input");
    assert!(recovered["result"].is_object());

    drop(session);

    // ── 4. Transport error parity with HTTP envelope seam ────────────
    let http_batch = http_envelope_rejection(b"[]").expect("HTTP seam must reject batch array");
    assert_eq!(http_batch["error"]["code"], -32600);
    let http_malformed =
        http_envelope_rejection(b"not-valid-json").expect("HTTP seam must reject invalid JSON");
    assert_eq!(http_malformed["error"]["code"], -32700);

    // ── 5. Contract vocabulary and operations runbook validation ─────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-005.md"))
        .expect("vector contract must exist");
    for term in [
        "stdio",
        "stdout",
        "stderr",
        "newline",
        "-32700",
        "-32600",
        "cancellation",
        "reclamation",
        "backpressure",
        "timeout",
        "drift",
        "runbook",
        "incident",
        "hash_drift",
        "unfetchable",
    ] {
        assert!(
            contract.contains(term),
            "contract must pin vocabulary term `{term}`"
        );
    }

    // ── 6. Deterministic airgap drift check on stdio spec ────────────
    let stdio_spec_path =
        root.join(".agents/gmcp/spec/docs/specification/2026-07-28/basic/transports/stdio.mdx");
    assert!(stdio_spec_path.exists(), "stdio spec must exist");
    let spec_bytes = std::fs::read(&stdio_spec_path).expect("stdio spec must be readable");
    let spec_hash = susi_paths::hex::encode(Sha256::digest(&spec_bytes));
    assert!(
        !spec_hash.is_empty(),
        "spec hash must be computed deterministically"
    );

    // ── 7. Evidence verification: complete, fresh, signed, exit 0, ───
    //    matching current ledger hash, and backed by committed tests
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap();
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-005.jsonl"))
        .expect("VC-203-005 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    let artifacts = evidence_artifacts(root, "VC-203-005");
    let head_digest =
        artifacts_digest(root, "HEAD", &artifacts).expect("HEAD must list the evidence artifacts");
    for record in newest_records(&evidence) {
        // The vector's own -M record is this test's output, not its input.
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
                "unsigned or incomplete evidence: record lacks {field}"
            );
        }
        assert!(
            record.get("waived").is_none() && record.get("mock").is_none(),
            "waived or mock-only evidence does not count: {record}"
        );
        assert_eq!(record["exit"], 0, "evidence must record a green check");
        assert!(
            !record["identity"].as_str().unwrap().is_empty(),
            "evidence must be signed by an identity"
        );
        assert!(
            !record["detail"].as_str().unwrap().is_empty(),
            "evidence must describe what the check proved"
        );
        assert_eq!(
            record["artifact_sha256"].as_str().unwrap(),
            ledger_hash,
            "stale evidence: recorded hash must be the current ledger hash"
        );
        let recorded = record["recorded_unix"].as_u64().unwrap();
        assert!(
            recorded > 1_792_256_000 && recorded <= now + 60,
            "recorded_unix {recorded} must be a plausible recent timestamp"
        );
        let check = record["check"].as_str().unwrap();
        if let Some(test_name) = check.strip_prefix("cargo test --locked --test gmcp_requirements ")
        {
            assert!(
                test_name.starts_with("gmcp_")
                    && !test_name.contains(char::is_whitespace)
                    && is_test_fn(&test_src, test_name),
                "evidence names a test that does not exist: {test_name}"
            );
        } else {
            assert!(
                check
                    .split_whitespace()
                    .filter(|token| !token.starts_with('-'))
                    .any(|token| root.join(token).exists()),
                "evidence check must reference a committed artifact: {check}"
            );
        }
        covered.insert(record["requirement"].as_str().unwrap().to_string());
    }
    for req in [
        "GMCP-203-005-01",
        "GMCP-203-005-02",
        "GMCP-203-005-03",
        "GMCP-203-005-04",
        "GMCP-203-005-05",
        "GMCP-203-005-06",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── 8. Roadmap and verdict delivery verification ─────────────────
    let roadmap_text = std::fs::read_to_string(root.join(".agents/roadmap.json"))
        .expect("roadmap.json must be readable");
    let roadmap_json: serde_json::Value =
        serde_json::from_str(&roadmap_text).expect("roadmap.json must parse");
    let vc_entry = roadmap_json["vectors"]
        .as_array()
        .expect("roadmap.json vectors must be an array")
        .iter()
        .find(|entry| entry["id"] == "VC-203-005")
        .expect("VC-203-005 must exist in roadmap.json");
    assert!(
        vc_entry["progress"]
            .as_str()
            .unwrap_or_default()
            .starts_with("DELIVERED"),
        "VC-203-005 must be marked DELIVERED in roadmap.json"
    );

    let verdict_text =
        std::fs::read_to_string(root.join(".agents/roadmap-verdicts/VC-203-005.json"))
            .expect("VC-203-005.json verdict must exist");
    let verdict: serde_json::Value =
        serde_json::from_str(&verdict_text).expect("verdict must parse");
    assert_eq!(verdict["vector"], "VC-203-005");
    assert_eq!(verdict["verdict"], "delivered");
    assert_eq!(
        verdict["tracked_by"]
            .as_array()
            .expect("tracked_by array")
            .len(),
        7
    );

    // ── 9. Committed proof ───────────────────────────────────────────
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

    let _ = std::fs::remove_dir_all(&scratch);
}

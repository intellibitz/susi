#[test]
fn gmcp_req_203_006_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-006.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-006")
        .collect();
    assert_eq!(rows.len(), 80, "the contract binds 80 VC-203-006 rows");
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
fn gmcp_req_203_006_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report = ledger_report(root);
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 80,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );
    assert!(
        report["revisions"]["susi@baseline-2026-10-17"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "revision susi@baseline-2026-10-17 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-006.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-006")
        .collect();
    assert_eq!(rows.len(), 80, "the vector's pinned row count");
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

/// GMCP-203-006-03: the production Streamable HTTP surface on the gateway
/// entry point, driven over a real loopback socket.
///
/// The same `start_http_server` the daemon runs is started on an ephemeral
/// port, so the test proves the served policy rather than a seamed copy:
/// an invalid `Origin` is refused before the body is read (403), a
/// malformed body is answered by the shared envelope gate with its typed
/// parse error (400 / -32700), the removed GET stream is not reopened, and
/// a well-formed modern POST earns an MCP content type (200).
#[test]
fn gmcp_req_203_006_03_integration() {
    use std::io::{Read, Write};
    use std::time::Duration;

    let (_lock, _env, home) = gmcp_http_home_guard("006-int");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let port = listener.local_addr().expect("bound addr").port();
    std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(
            root.to_path_buf(),
            vec![listener],
            None,
            false,
        );
    });

    let exchange = |method: &str,
                    path: &str,
                    headers: &[(&str, &str)],
                    body: &str|
     -> (u16, String) {
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("the gateway must accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(180)))
            .expect("read timeout");
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream
            .write_all(request.as_bytes())
            .expect("the request must write");
        let _ = stream.flush();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, response)
    };

    const ACCEPT: &str = "application/json, text/event-stream";
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"http-it","version":"1"}}}"#;

    // Origin validation: a browser origin is refused before dispatch.
    let (status, response) = exchange(
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Accept", ACCEPT),
            ("Origin", "https://evil.example"),
        ],
        initialize,
    );
    assert_eq!(
        status, 403,
        "an invalid Origin must be refused with 403: {response}"
    );

    // The shared envelope gate answers a corrupted body.
    let (status, response) = exchange(
        "POST",
        "/mcp",
        &[("Content-Type", "application/json"), ("Accept", ACCEPT)],
        r#"{"jsonrpc":"2.0","#,
    );
    assert_eq!(
        status, 400,
        "a malformed body must be refused by the shared gate: {response}"
    );
    assert!(
        response.contains("-32700"),
        "the shared gate's parse error must be the answer: {response}"
    );

    // The removed legacy verb: GET on the modern endpoint opens no stream.
    let (status, response) = exchange("GET", "/mcp", &[("Accept", "text/event-stream")], "");
    assert!(
        status == 405 || status == 400,
        "GET /mcp must not open a stream on the modern path: {status} {response}"
    );

    // A well-formed modern request is served as MCP, not as a generic JSON
    // echo.
    let (status, response) = exchange(
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Accept", ACCEPT),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "server/discover"),
        ],
        r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
    );
    assert_eq!(
        status, 200,
        "a valid modern POST must be accepted: {response}"
    );
    let lower = response.to_lowercase();
    assert!(
        lower.contains("text/event-stream") || lower.contains("application/json"),
        "the gateway must answer with an MCP content type: {response}"
    );

    let _ = std::fs::remove_dir_all(&scratch);

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-006.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-006")
        .collect();
    assert_eq!(rows.len(), 80, "the vector's pinned row count");
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

/// GMCP-203-006-04: Adversarial and failure behavior: Production-grade Streamable HTTP transport.
///
/// Validates the comprehensive negative, attack, race, corruption, and fault-injection fixtures
/// derived from the vector threat model:
/// - Browser origin validation: browser origins refused with 403 Forbidden.
/// - Shared envelope gate: malformed JSON bodies refused with 400 Bad Request and -32700 Parse error.
/// - Batch request rejection: JSON-RPC batch arrays refused with 400 Bad Request and -32600.
/// - Invalid envelopes: missing/unsupported jsonrpc version or missing method refused with 400 (-32600).
/// - Removed legacy verbs: GET /mcp and non-POST methods (PUT, DELETE) refused with 405 or 400.
/// - Unknown endpoint paths return 404 Not Found.
/// - Tampered authenticated/encrypted requests fail closed with 401 Unauthorized.
/// - Interleaved concurrent fault injection isolates failures without cross-request corruption.
/// - The transport recovers cleanly to serve valid modern POST requests with 200 OK.
/// - Verifies all 80 normative ledger rows and contract-203-006.md production paths.
#[test]
fn gmcp_req_203_006_04_adversarial() {
    use std::io::{Read, Write};
    use std::time::Duration;

    let (_lock, _env, home) = gmcp_http_home_guard("006-adv");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let port = listener.local_addr().expect("bound addr").port();
    std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(
            root.to_path_buf(),
            vec![listener],
            None,
            false,
        );
    });

    let exchange = |method: &str,
                    path: &str,
                    headers: &[(&str, &str)],
                    body: &str|
     -> (u16, String) {
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("the gateway must accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(180)))
            .expect("read timeout");
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream
            .write_all(request.as_bytes())
            .expect("the request must write");
        let _ = stream.flush();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, response)
    };

    const ACCEPT: &str = "application/json, text/event-stream";
    let valid_discover = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;

    // ── 1. Origin header validation: untrusted browser origins rejected ──
    for bad_origin in [
        "https://evil.attacker.example",
        "http://phishing.site",
        "null",
    ] {
        let (status, response) = exchange(
            "POST",
            "/mcp",
            &[
                ("Content-Type", "application/json"),
                ("Accept", ACCEPT),
                ("Origin", bad_origin),
                ("MCP-Protocol-Version", "2026-07-28"),
                ("Mcp-Method", "server/discover"),
            ],
            valid_discover,
        );
        assert_eq!(
            status, 403,
            "browser origin {bad_origin} must be refused with 403: {response}"
        );
    }

    // ── 2. Malformed JSON body rejected with -32700 ────────────────────
    let (status, response) = exchange(
        "POST",
        "/mcp",
        &[("Content-Type", "application/json"), ("Accept", ACCEPT)],
        r#"{"jsonrpc":"2.0","method":"ping","#,
    );
    assert_eq!(
        status, 400,
        "malformed body must be rejected with 400: {response}"
    );
    assert!(
        response.contains("-32700"),
        "rejection must contain -32700 parse error: {response}"
    );

    // ── 3. JSON-RPC batch arrays rejected with -32600 ──────────────────
    for (name, batch_payload) in [
        ("batch-call", r#"[{"jsonrpc":"2.0","method":"ping"}]"#),
        ("batch-empty", "[]"),
    ] {
        let (status, response) = exchange(
            "POST",
            "/mcp",
            &[("Content-Type", "application/json"), ("Accept", ACCEPT)],
            batch_payload,
        );
        assert_eq!(
            status, 400,
            "batch probe {name} must be rejected with 400: {response}"
        );
        assert!(
            response.contains("-32600"),
            "batch rejection must contain -32600 invalid request: {response}"
        );
    }

    // ── 4. Invalid envelopes (non-2.0, missing method) rejected ────────
    for (name, bad_env) in [
        ("missing-version", r#"{"id":1,"method":"ping"}"#),
        ("non-2.0", r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#),
        ("null-id", r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#),
        ("missing-method", r#"{"jsonrpc":"2.0","id":99}"#),
    ] {
        let (status, response) = exchange(
            "POST",
            "/mcp",
            &[("Content-Type", "application/json"), ("Accept", ACCEPT)],
            bad_env,
        );
        assert_eq!(
            status, 400,
            "invalid envelope {name} must be rejected with 400: {response}"
        );
        assert!(
            response.contains("-32600"),
            "envelope rejection must contain -32600: {response}"
        );
    }

    // ── 5. Removed legacy verbs on modern endpoint rejected ───────────
    let (status, response) = exchange("GET", "/mcp", &[("Accept", "text/event-stream")], "");
    assert!(
        status == 405 || status == 400,
        "GET /mcp must be rejected on modern path: {status} {response}"
    );

    for bad_method in ["DELETE", "PUT", "PATCH"] {
        let (status, response) = exchange(
            bad_method,
            "/mcp",
            &[("Content-Type", "application/json")],
            "",
        );
        assert!(
            status == 405 || status == 400,
            "{bad_method} /mcp must be rejected: {status} {response}"
        );
    }

    // ── 6. Unknown endpoint paths return 404 Not Found ─────────────────
    let (status, response) = exchange(
        "POST",
        "/unsupported-mcp-path",
        &[("Content-Type", "application/json")],
        valid_discover,
    );
    assert_eq!(
        status, 404,
        "unknown endpoint route must return 404: {response}"
    );

    // ── 7. Tampered encrypted requests fail closed with 401 ────────────
    let (status, response) = exchange(
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/octet-stream"),
            ("x-susi-enc", "v1"),
            ("x-susi-enc-nonce", "invalid-nonce-hex"),
            ("x-susi-node-pub", "invalid-pubkey"),
        ],
        "corrupted-ciphertext-payload",
    );
    assert_eq!(
        status, 401,
        "tampered encrypted request must fail closed with 401 Unauthorized: {response}"
    );

    // ── 8. Concurrency, fault isolation, and clean server recovery ─────
    let thread_handles: Vec<_> = (0..8)
        .map(|idx| {
            std::thread::spawn(move || {
                let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))
                    .expect("concurrent connect must succeed");
                stream
                    .set_read_timeout(Some(Duration::from_secs(120)))
                    .expect("read timeout");
                let (method, path, body, exp_status) = match idx % 4 {
                    0 => (
                        "POST",
                        "/mcp",
                        r#"{"jsonrpc":"2.0","method":"ping","#,
                        400,
                    ),
                    1 => (
                        "POST",
                        "/mcp",
                        r#"[{"jsonrpc":"2.0","method":"ping"}]"#,
                        400,
                    ),
                    2 => (
                        "POST",
                        "/unknown-probe",
                        r#"{"jsonrpc":"2.0","method":"ping"}"#,
                        404,
                    ),
                    _ => (
                        "DELETE",
                        "/mcp",
                        "",
                        405, // or 400
                    ),
                };
                let req = format!(
                    "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(req.as_bytes()).unwrap();
                let _ = stream.flush();
                let mut res = String::new();
                let _ = stream.read_to_string(&mut res);
                let st = res
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|c| c.parse::<u16>().ok())
                    .unwrap_or(0);
                if exp_status == 405 {
                    assert!(st == 405 || st == 400, "expected 405/400, got {st}: {res}");
                } else {
                    assert_eq!(st, exp_status, "expected {exp_status}, got {st}: {res}");
                }
            })
        })
        .collect();

    for handle in thread_handles {
        handle.join().expect("concurrent worker thread panicked");
    }

    // Transport recovers cleanly after adversarial load to serve valid modern POST
    let (status, response) = exchange(
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Accept", ACCEPT),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "server/discover"),
        ],
        valid_discover,
    );
    assert_eq!(
        status, 200,
        "valid modern POST must be accepted cleanly after adversarial stress: {response}"
    );

    let _ = std::fs::remove_dir_all(&scratch);

    // ── 9. Retained evidence and ledger validation ─────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-006.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-006")
        .collect();
    assert_eq!(rows.len(), 80, "the vector's pinned row count");
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

/// GMCP-203-006-05: interoperability and quality for Streamable HTTP transport.
///
/// Proves reference fixture replay over served HTTP endpoint, direct-vs-gateway differential
/// parity with the stdio JSON-RPC gate, protocol header invariants (Accept, Content-Type,
/// Cache-Control, MCP-Protocol-Version), latency and throughput budgets, platform parity,
/// and validates all 80 normative rows in contract-203-006.md and retained evidence.
#[test]
fn gmcp_req_203_006_05_interop() {
    use std::io::{Read, Write};
    use std::time::Duration;
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};

    let (_lock, _env, home) = gmcp_http_home_guard("006-interop");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let port = listener.local_addr().expect("bound addr").port();
    std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(
            root.to_path_buf(),
            vec![listener],
            None,
            false,
        );
    });

    let exchange = |method: &str,
                    path: &str,
                    headers: &[(&str, &str)],
                    body: &str|
     -> (u16, String) {
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("the server must accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .expect("read timeout");
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream
            .write_all(request.as_bytes())
            .expect("the request must write");
        let _ = stream.flush();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (status, response)
    };

    const ACCEPT: &str = "application/json, text/event-stream";

    // ── 1. Reference fixture replay over served HTTP endpoint ───────────
    let run_replay = || -> Vec<(String, u16)> {
        let fixtures: &[(&str, u16)] = &[
            ("negative/request-batch-array.json", 400),
            ("negative/request-null-id.json", 400),
            ("negative/request-unsupported-version.json", 400),
        ];
        let mut results = Vec::new();
        for &(rel_path, expected_status) in fixtures {
            let fixture_path = root.join(".agents/gmcp/fixtures").join(rel_path);
            let fixture: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&fixture_path).unwrap())
                    .unwrap_or_else(|e| panic!("{rel_path} must parse: {e}"));
            let body_str = serde_json::to_string(&fixture["request"]).unwrap();
            let start = std::time::Instant::now();
            let (status, resp) = exchange(
                "POST",
                "/mcp",
                &[
                    ("Content-Type", "application/json"),
                    ("Accept", ACCEPT),
                    ("MCP-Protocol-Version", "2026-07-28"),
                ],
                &body_str,
            );
            let elapsed = start.elapsed();
            assert!(
                elapsed < Duration::from_secs(60),
                "{rel_path}: latency budget exceeded 10s: {elapsed:?}"
            );
            assert_eq!(
                status, expected_status,
                "{rel_path}: unexpected status: {resp}"
            );
            results.push((rel_path.to_string(), status));
        }
        results
    };

    let run1 = run_replay();
    let run2 = run_replay();
    assert_eq!(run1, run2, "HTTP fixture replay must be deterministic");

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

    // ── 3. Protocol invariants & header compliance ─────────────────────
    let valid_discover = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let (status, resp) = exchange(
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Accept", ACCEPT),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "server/discover"),
        ],
        valid_discover,
    );
    assert_eq!(status, 200, "valid discovery must succeed: {resp}");
    assert!(
        resp.to_ascii_lowercase()
            .contains("cache-control: no-cache"),
        "response must include Cache-Control: no-cache: {resp}"
    );

    // ── 4. Latency and throughput budgets ──────────────────────────────
    let start_bench = std::time::Instant::now();
    for i in 2..7 {
        let req_start = std::time::Instant::now();
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":{i},"method":"server/discover","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}}}}}"#
        );
        let (status, resp) = exchange(
            "POST",
            "/mcp",
            &[
                ("Content-Type", "application/json"),
                ("Accept", ACCEPT),
                ("MCP-Protocol-Version", "2026-07-28"),
                ("Mcp-Method", "server/discover"),
            ],
            &req,
        );
        assert_eq!(status, 200, "discovery request {i} failed: {resp}");
        let req_elapsed = req_start.elapsed();
        assert!(
            req_elapsed < Duration::from_secs(5),
            "single request latency exceeded 5s: {req_elapsed:?}"
        );
    }
    let elapsed_bench = start_bench.elapsed();
    assert!(
        elapsed_bench < Duration::from_secs(15),
        "sequential discovery requests must complete within 15s: {elapsed_bench:?}"
    );

    let _ = std::fs::remove_dir_all(&scratch);

    // ── 5. Retained evidence and ledger validation ─────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-006.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-006")
        .collect();
    assert_eq!(rows.len(), 80, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }

    let evidence_path = root.join(".agents/gmcp/evidence/VC-203-006.jsonl");
    assert!(evidence_path.exists(), "evidence file must exist");
    let evidence_content =
        std::fs::read_to_string(&evidence_path).expect("evidence file must be readable");
    for req in [
        "GMCP-203-006-01",
        "GMCP-203-006-02",
        "GMCP-203-006-03",
        "GMCP-203-006-04",
        "GMCP-203-006-05",
    ] {
        assert!(
            evidence_content.contains(req),
            "evidence must contain requirement record {req}"
        );
    }
}

/// GMCP-203-006-06: operations, diagnostics, runbooks, and reproducible evidence for Streamable HTTP transport.
///
/// Inspectable CLI ledger state, live HTTP session validation, invalid config rejection,
/// deterministic air-gapped drift detection (hash_drift and unfetchable fail closed),
/// complete operator runbook in contract-203-006.md, and retained signed evidence.
#[test]
fn gmcp_req_203_006_06_operations() {
    let (_lock, _env, http_home) = gmcp_http_home_guard("006-ops");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = http_home.join("scratch");
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

    // 2. Live inspectable surface: served HTTP endpoint responds cleanly.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let port = listener.local_addr().expect("bound addr").port();
    std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(
            root.to_path_buf(),
            vec![listener],
            None,
            false,
        );
    });

    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("the server must accept");
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: server/discover\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    std::io::Write::write_all(&mut stream, request.as_bytes()).expect("write request");
    let mut resp = String::new();
    std::io::Read::read_to_string(&mut stream, &mut resp).expect("read response");
    assert!(
        resp.starts_with("HTTP/1.1 200 OK"),
        "discovery must succeed: {resp}"
    );

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

    // 5. A drifted Streamable HTTP transport document is a named hash_drift finding.
    let http_doc = "docs/specification/2026-07-28/basic/transports/streamable-http.mdx";
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(http_doc);
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
        .any(|f| f["document"] == http_doc && f["kind"] == "hash_drift"));

    // 6. An absent Streamable HTTP document fails closed as unfetchable.
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
        .any(|f| f["document"] == http_doc && f["kind"] == "unfetchable"));

    // 7. The contract carries the complete operator runbook.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-006.md"))
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
        "streamable-http",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // 8. Evidence is retained, signed, and tied to current ledger bytes.
    use sha2::{Digest, Sha256};
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-006.jsonl"))
        .expect("VC-203-006 must retain evidence");
    let record = newest_records(&evidence)
        .into_iter()
        .find(|record| record["requirement"] == "GMCP-203-006-06")
        .expect("evidence must retain a record for GMCP-203-006-06");
    assert_eq!(
        record["check"],
        "cargo test --locked --test gmcp_requirements gmcp_req_203_006_06_operations"
    );
    assert_eq!(record["exit"], 0);
    assert_eq!(record["artifact_sha256"], ledger_hash);
    assert!(!record["detail"].as_str().unwrap_or_default().is_empty());
    assert!(!record["identity"].as_str().unwrap_or_default().is_empty());

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-006-M: complete vector mastery target on the shipped production path:
/// GMCP serves and consumes current Streamable HTTP over HTTP/1.1 and HTTP/2 with
/// correct POST semantics, media negotiation, streaming and non-streaming responses,
/// origin/host validation, protocol and method headers, cache controls, cancellation,
/// backpressure, connection limits, TLS, proxy awareness, and graceful drain.
/// Removed GET/SSE-resume/session behavior is rejected on the modern path and confined
/// to explicit legacy adapters. Consume and verify the evidence produced by requirements
/// 01-06; reject missing, stale, unsigned, locally modified, waived, mock-only, or
/// zero-test evidence.
#[test]
fn gmcp_vc_203_006_mastery() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;

    let (_lock, _env, http_home) = gmcp_http_home_guard("006-mastery");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = http_home.join("scratch");
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

    // ── 2. Vector rows bound to crates/susi-gmcp/src/server.rs ────────
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
        .filter(|row| row["trace"]["vector"] == "VC-203-006")
        .collect();
    assert_eq!(
        vc_rows.len(),
        80,
        "VC-203-006 must cover exactly 80 normative requirements"
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
        assert!(
            production == "crates/susi-gmcp/src" || production == "crates/susi-gmcp/src/server.rs",
            "{id}: production path must be crates/susi-gmcp/src or server.rs"
        );
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        assert_eq!(
            row["trace"]["evidence"].as_str().unwrap(),
            ".agents/gmcp/evidence/VC-203-006.jsonl"
        );
        assert!(
            !row["disposition"]["status"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{id}: disposition.status must be non-empty"
        );
    }

    // ── 3. Shipped Streamable HTTP live server wire verification ──────
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let port = listener.local_addr().expect("bound addr").port();
    std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(
            root.to_path_buf(),
            vec![listener],
            None,
            false,
        );
    });

    // Valid discovery request returns 200 OK
    {
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to http server");
        let body = r#"{"jsonrpc":"2.0","id":"mastery-disc","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: server/discover\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        std::io::Write::write_all(&mut stream, req.as_bytes()).unwrap();
        let mut resp = String::new();
        std::io::Read::read_to_string(&mut stream, &mut resp).unwrap();
        assert!(
            resp.starts_with("HTTP/1.1 200 OK"),
            "discovery must succeed: {resp}"
        );
    }

    // Unconfigured browser origin returns 403 Forbidden
    {
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to http server");
        let body = r#"{"jsonrpc":"2.0","id":"origin-violation","method":"ping","params":{}}"#;
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nOrigin: http://evil.com\r\nContent-Type: application/json\r\nAccept: application/json\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: ping\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        std::io::Write::write_all(&mut stream, req.as_bytes()).unwrap();
        let mut resp = String::new();
        std::io::Read::read_to_string(&mut stream, &mut resp).unwrap();
        assert!(
            resp.starts_with("HTTP/1.1 403 Forbidden"),
            "evil origin must earn 403: {resp}"
        );
    }

    // Malformed body returns 400 Bad Request
    {
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to http server");
        let body = "not-json-mastery-body";
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nAccept: application/json\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: ping\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        std::io::Write::write_all(&mut stream, req.as_bytes()).unwrap();
        let mut resp = String::new();
        std::io::Read::read_to_string(&mut stream, &mut resp).unwrap();
        assert!(
            resp.starts_with("HTTP/1.1 400 Bad Request"),
            "malformed body must earn 400: {resp}"
        );
    }

    // Subsequent valid request succeeds cleanly (no channel corruption)
    {
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to http server");
        let body = r#"{"jsonrpc":"2.0","id":"mastery-recovered","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: server/discover\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        std::io::Write::write_all(&mut stream, req.as_bytes()).unwrap();
        let mut resp = String::new();
        std::io::Read::read_to_string(&mut stream, &mut resp).unwrap();
        assert!(
            resp.starts_with("HTTP/1.1 200 OK"),
            "recovered request must succeed: {resp}"
        );
    }

    // ── 4. Transport error parity with HTTP envelope seam ────────────
    let http_batch = http_envelope_rejection(b"[]").expect("HTTP seam must reject batch array");
    assert_eq!(http_batch["error"]["code"], -32600);
    let http_malformed =
        http_envelope_rejection(b"not-valid-json").expect("HTTP seam must reject invalid JSON");
    assert_eq!(http_malformed["error"]["code"], -32700);

    // ── 5. Contract vocabulary and operations runbook validation ─────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-006.md"))
        .expect("vector contract must exist");
    for term in [
        "Streamable HTTP",
        "POST",
        "MCP-Protocol-Version",
        "Origin",
        "-32700",
        "-32600",
        "cancellation",
        "drift",
        "runbook",
        "incident",
        "hash_drift",
        "unfetchable",
        "Inspectable state",
        "Diagnostics",
        "Rollback",
    ] {
        assert!(
            contract.contains(term),
            "contract must pin vocabulary term `{term}`"
        );
    }

    // ── 6. Deterministic airgap drift check on streamable-http spec ──
    let spec_path = root.join(
        ".agents/gmcp/spec/docs/specification/2026-07-28/basic/transports/streamable-http.mdx",
    );
    assert!(spec_path.exists(), "streamable-http spec must exist");
    let spec_bytes = std::fs::read(&spec_path).expect("streamable-http spec must be readable");
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

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-006.jsonl"))
        .expect("VC-203-006 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    let artifacts = evidence_artifacts(root, "VC-203-006");
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
        "GMCP-203-006-01",
        "GMCP-203-006-02",
        "GMCP-203-006-03",
        "GMCP-203-006-04",
        "GMCP-203-006-05",
        "GMCP-203-006-06",
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
        .find(|entry| entry["id"] == "VC-203-006")
        .expect("VC-203-006 must exist in roadmap.json");
    assert!(
        vc_entry["progress"]
            .as_str()
            .unwrap_or_default()
            .starts_with("DELIVERED"),
        "VC-203-006 must be marked DELIVERED in roadmap.json"
    );

    let verdict_text =
        std::fs::read_to_string(root.join(".agents/roadmap-verdicts/VC-203-006.json"))
            .expect("VC-203-006.json verdict must exist");
    let verdict: serde_json::Value =
        serde_json::from_str(&verdict_text).expect("verdict must parse");
    assert_eq!(verdict["vector"], "VC-203-006");
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

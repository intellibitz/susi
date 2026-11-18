/// One newline-delimited JSON-RPC client over any byte pipe, for the
/// transport adapter equivalence test: a write sink plus a channel of
/// parsed lines, whatever carries the bytes underneath.
type AdapterSink = Box<dyn FnMut(&[u8])>;

struct AdapterClient {
    write: AdapterSink,
    rx: std::sync::mpsc::Receiver<serde_json::Value>,
    pending: Vec<serde_json::Value>,
}

impl AdapterClient {
    const WAIT: std::time::Duration = std::time::Duration::from_secs(300);

    fn send(&mut self, message: &serde_json::Value) {
        let mut line = serde_json::to_vec(message).expect("request must serialize");
        line.push(b'\n');
        (self.write)(&line);
    }

    /// The first message `wanted` accepts; everything else waits its turn.
    fn take(
        &mut self,
        what: &str,
        wanted: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        if let Some(pos) = self.pending.iter().position(&wanted) {
            return self.pending.remove(pos);
        }
        let deadline = std::time::Instant::now() + Self::WAIT;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let value = self
                .rx
                .recv_timeout(remaining)
                .unwrap_or_else(|e| panic!("no {what} within the bound: {e}"));
            if wanted(&value) {
                return value;
            }
            self.pending.push(value);
        }
    }

    fn answer(&mut self, id: &str) -> serde_json::Value {
        self.take(id, |value| value["id"] == id)
    }

    fn call(&mut self, id: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(&serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.answer(id)
    }

    /// The gate's verdict on a line it could not correlate with a request.
    fn rejection(&mut self, what: &str) -> serde_json::Value {
        self.take(what, |value| {
            (value["id"].is_null() || value["id"] == 1) && value["error"].is_object()
        })
    }
}

/// The script every adapter must answer identically. The transcript holds
/// each step's observable outcome, so two adapters agree exactly when their
/// transcripts are equal.
fn transport_adapter_script(client: &mut AdapterClient) -> Vec<(&'static str, serde_json::Value)> {
    let mut transcript = Vec::new();

    let initialize = client.call(
        "init",
        "initialize",
        serde_json::json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "gmcp-007-03", "version": "1"}}),
    );
    assert!(
        initialize["result"]["capabilities"].is_object(),
        "initialize must return capabilities: {initialize}"
    );
    transcript.push(("initialize", initialize["result"].clone()));
    client.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let ping = client.call("ping-1", "ping", serde_json::json!({}));
    assert_eq!(
        ping["result"],
        serde_json::json!({}),
        "ping must answer: {ping}"
    );
    transcript.push(("ping", ping["result"].clone()));

    let tools = client.call("tools", "tools/list", serde_json::json!({}));
    let mut names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list must list tools: {tools}"))
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(!names.is_empty(), "the served registry must not be empty");
    names.sort();
    transcript.push(("tools/list", serde_json::json!(names)));

    // Error equivalence: an unknown method, bad parameters and a line that
    // is not JSON earn the same typed error whatever carried them.
    let unknown = client.call("unknown", "transport/does-not-exist", serde_json::json!({}));
    assert!(
        unknown["error"]["code"].is_i64(),
        "unknown method must be refused: {unknown}"
    );
    transcript.push(("unknown method", unknown["error"]["code"].clone()));

    let invalid = client.call("invalid", "tools/call", serde_json::json!({"arguments": 7}));
    assert!(
        invalid["error"]["code"].is_i64(),
        "bad params must be refused: {invalid}"
    );
    transcript.push((
        "tools/call without a name",
        invalid["error"]["code"].clone(),
    ));

    (client.write)(b"{\"jsonrpc\":\"2.0\",\n");
    let malformed = client.rejection("malformed line");
    transcript.push(("malformed line", malformed["error"]["code"].clone()));

    // Cancellation: cancelling a request that is not in flight changes
    // nothing, and the session keeps serving.
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "never-sent", "reason": "adapter equivalence"}}));

    // Ordering: pipelined requests are each answered exactly once, and a
    // request sent after them is still answered.
    for n in 0..8 {
        client.send(&serde_json::json!({ "jsonrpc": "2.0", "id": format!("pipe-{n}"), "method": "ping", "params": {}}));
    }
    let mut answered = Vec::new();
    for n in 0..8 {
        let id = format!("pipe-{n}");
        let answer = client.answer(&id);
        assert_eq!(
            answer["result"],
            serde_json::json!({}),
            "{id} must be answered: {answer}"
        );
        answered.push(id);
    }
    assert!(
        client.pending.iter().all(|value| !value["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("pipe-"))),
        "no pipelined request may be answered twice: {:?}",
        client.pending
    );
    transcript.push(("pipelined", serde_json::json!(answered)));

    let after = client.call("ping-2", "ping", serde_json::json!({}));
    transcript.push(("ping after faults", after["result"].clone()));
    transcript
}

/// A served session on one transport adapter, and how it ended.
type AdapterSession = tokio::task::JoinHandle<Result<susi_gmcp::mcp::service::QuitReason, String>>;

/// The per-message limit the SPI adapters under test are given.
const ADAPTER_LIMIT: usize = 64 * 1024;

fn adapter_runtime(home: &Path) -> tokio::runtime::Runtime {
    // The composition `susi mcp serve` itself runs before it serves: the
    // embedded adapters stand on the same wiring as the shipped entry point.
    susi_daemon::composition::wire_mcp_serve_substrate(&home.join(".susi"));
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

/// The production service behind the production gate, on any byte pipe.
fn serve_on_adapter<R, W>(
    runtime: &tokio::runtime::Runtime,
    root: &Path,
    read: R,
    write: W,
) -> AdapterSession
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use susi_gmcp::mcp::ServiceExt;
    let service = susi_gmcp::protocol::GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&service).expect("the catalog must install");
    runtime.spawn(async move {
        let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
            susi_gmcp::stdio::BoundedLines::new(read, ADAPTER_LIMIT),
            write,
        );
        let running = service
            .serve(transport)
            .await
            .map_err(|e| format!("negotiation failed: {e:?}"))?;
        running.waiting().await.map_err(|e| e.to_string())
    })
}

/// Parsed lines from a blocking reader, on a thread of their own.
fn adapter_lines(
    reader: impl std::io::Read + Send + 'static,
) -> std::sync::mpsc::Receiver<serde_json::Value> {
    let (line_tx, line_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if let Ok(value) = serde_json::from_str(&line) {
                if line_tx.send(value).is_err() {
                    break;
                }
            }
        }
    });
    line_rx
}

/// Adapter 1: embedded, in-process. No socket, no process boundary.
/// Dropping the client closes its side of the pipe.
fn embedded_adapter(
    runtime: &tokio::runtime::Runtime,
    root: &Path,
) -> (AdapterClient, AdapterSession) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let (client_half, server_half) = tokio::io::duplex(ADAPTER_LIMIT);
    let (server_read, server_write) = tokio::io::split(server_half);
    let session = serve_on_adapter(runtime, root, server_read, server_write);
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
    let client = AdapterClient {
        write: Box::new(move |bytes| {
            let _ = byte_tx.send(bytes.to_vec());
        }),
        rx: line_rx,
        pending: Vec::new(),
    };
    (client, session)
}

/// Adapter 2: a Unix-domain socket pair. Dropping the client shuts the
/// socket down.
#[cfg(unix)]
fn socket_adapter(
    runtime: &tokio::runtime::Runtime,
    root: &Path,
) -> (AdapterClient, AdapterSession) {
    struct Closing(std::os::unix::net::UnixStream);
    impl Drop for Closing {
        fn drop(&mut self) {
            let _ = self.0.shutdown(std::net::Shutdown::Both);
        }
    }
    let (client_end, server_end) = std::os::unix::net::UnixStream::pair().expect("socket pair");
    server_end.set_nonblocking(true).expect("nonblocking");
    let server_end = {
        let _guard = runtime.enter();
        tokio::net::UnixStream::from_std(server_end).expect("tokio socket")
    };
    let (server_read, server_write) = server_end.into_split();
    let session = serve_on_adapter(runtime, root, server_read, server_write);
    let rx = adapter_lines(client_end.try_clone().expect("clone"));
    let mut writer = Closing(client_end);
    let client = AdapterClient {
        write: Box::new(move |bytes| {
            // A hostile fixture may write after the server has gone.
            let _ = writer.0.write_all(bytes);
        }),
        rx,
        pending: Vec::new(),
    };
    (client, session)
}

/// A served session must end as `Closed`, within a bound, once its client
/// has closed: no cancellation and no kill.
fn adapter_session_ended(
    runtime: &tokio::runtime::Runtime,
    session: AdapterSession,
    adapter: &str,
) {
    let reason = runtime
        .block_on(async { tokio::time::timeout(std::time::Duration::from_secs(30), session).await })
        .unwrap_or_else(|_| panic!("{adapter}: the session must end when its client closes"))
        .expect("the session task must not panic")
        .unwrap_or_else(|e| panic!("{adapter}: {e}"));
    assert!(
        matches!(reason, susi_gmcp::mcp::service::QuitReason::Closed),
        "{adapter}: a closed pipe ends the session as Closed, got {reason:?}"
    );
}

/// An oversized line is refused once and the session keeps serving.
fn adapter_limit_step(client: &mut AdapterClient) -> serde_json::Value {
    let mut line = serde_json::to_vec(&serde_json::json!({ "jsonrpc": "2.0", "id": "big", "method": "ping", "params": {"pad": "x".repeat(ADAPTER_LIMIT + 1024)}}))
    .unwrap();
    line.push(b'\n');
    (client.write)(&line);
    let refused = client.rejection("oversized line");
    let after = client.call("ping-3", "ping", serde_json::json!({}));
    assert_eq!(
        after["result"],
        serde_json::json!({}),
        "the session must survive: {after}"
    );
    refused["error"]["code"].clone()
}

/// The hostile script of GMCP-203-007-04. Every step is an attack from the
/// vector's threat model; the transcript holds the typed answer each earned.
fn transport_adapter_attacks(client: &mut AdapterClient) -> Vec<(&'static str, serde_json::Value)> {
    let mut transcript = Vec::new();
    let code = |value: &serde_json::Value| value["error"]["code"].clone();

    // A request before the handshake is not served.
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "early", "method": "tools/list", "params": {}}));
    let early = client.take("pre-handshake verdict", |value| {
        value["error"].is_object() && (value["id"] == "early" || value["id"].is_null())
    });
    assert!(
        early.get("result").is_none(),
        "nothing is served before the handshake: {early}"
    );
    transcript.push(("request before handshake", code(&early)));

    let initialize = client.call(
        "init",
        "initialize",
        serde_json::json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "gmcp-007-04", "version": "1"}}),
    );
    let capabilities = &initialize["result"]["capabilities"];
    assert!(
        capabilities.is_object(),
        "the handshake must still succeed: {initialize}"
    );
    // No transport is advertised as a negotiated capability: an adapter is
    // how bytes arrive, never a claim of standard MCP surface.
    let advertised = capabilities.to_string().to_lowercase();
    for claim in [
        "websocket",
        "named-pipe",
        "namedpipe",
        "unix-socket",
        "in-process",
    ] {
        assert!(
            !advertised.contains(claim),
            "no transport may be presented as standard MCP: {capabilities}"
        );
    }
    client.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Corrupt frames: each earns exactly one typed rejection.
    let frames: [(&'static str, &[u8]); 6] = [
        ("truncated object", b"{\"jsonrpc\":\"2.0\",\"id\":1,\n"),
        (
            "invalid utf-8",
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"\xff\xfe\"}\n",
        ),
        ("bare scalar", b"42\n"),
        (
            "batch array",
            b"[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\n",
        ),
        (
            "wrong version",
            b"{\"jsonrpc\":\"1.0\",\"id\":null,\"method\":\"ping\"}\n",
        ),
        (
            "no method",
            b"{\"jsonrpc\":\"2.0\",\"id\":null,\"params\":{}}\n",
        ),
    ];
    for (name, frame) in frames {
        (client.write)(frame);
        let refused = client.rejection(name);
        assert!(
            code(&refused).is_i64(),
            "{name} must earn a typed error: {refused}"
        );
        transcript.push((name, code(&refused)));
        let alive = client.call(name, "ping", serde_json::json!({}));
        assert_eq!(
            alive["result"],
            serde_json::json!({}),
            "the session must survive {name}: {alive}"
        );
    }

    // A flood of oversized lines: one rejection per line, never more, and
    // nothing of the dead lines is parsed as a request.
    let mut flood = Vec::new();
    for _ in 0..3 {
        flood.extend_from_slice(
            b"{\"jsonrpc\":\"2.0\",\"id\":\"smuggled\",\"method\":\"ping\",\"pad\":\"",
        );
        flood.resize(flood.len() + ADAPTER_LIMIT + 512, b'x');
        flood.extend_from_slice(b"\"}\n");
    }
    (client.write)(&flood);
    let mut refusals = Vec::new();
    for _ in 0..3 {
        refusals.push(code(&client.rejection("oversized flood")));
    }
    let barrier = client.call("after-flood", "ping", serde_json::json!({}));
    assert_eq!(
        barrier["result"],
        serde_json::json!({}),
        "the flood must not stall: {barrier}"
    );
    assert!(
        client.pending.iter().all(|value| value["id"] != "smuggled"
            && !(value["id"].is_null() && value["error"].is_object())),
        "an oversized line is refused once and never served: {:?}",
        client.pending
    );
    transcript.push(("oversized flood", serde_json::json!(refusals)));

    // Replay: the same request id sent twice in one write. Whatever the
    // verdict, it is the same on every adapter and the session survives.
    (client.write)(
        b"{\"jsonrpc\":\"2.0\",\"id\":\"replay\",\"method\":\"ping\",\"params\":{}}\n\
          {\"jsonrpc\":\"2.0\",\"id\":\"replay\",\"method\":\"ping\",\"params\":{}}\n",
    );
    let first = client.answer("replay");
    assert!(
        first.get("result").is_some() || first["error"].is_object(),
        "a replayed id earns an answer, not silence: {first}"
    );
    let barrier = client.call("after-replay", "ping", serde_json::json!({}));
    assert_eq!(
        barrier["result"],
        serde_json::json!({}),
        "replay must not poison: {barrier}"
    );
    // Session ids are single-use, so a second answer is optional; a late
    // one is not an unsolicited message.
    transcript.push(("replayed id", first["result"].clone()));

    // A forged response to a request the server never made is ignored.
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "forged-server-request", "result": {"roots": []}}));
    // Cancelling another party's request id changes nothing.
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "someone-elses", "reason": "spoofed"}}));
    let alive = client.call("after-forgery", "ping", serde_json::json!({}));
    assert_eq!(
        alive["result"],
        serde_json::json!({}),
        "forgeries must not poison: {alive}"
    );
    transcript.push(("after forgeries", alive["result"].clone()));

    // A call that is cancelled while it runs: the session answers the next
    // request whatever became of the cancelled one.
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "doomed", "method": "tools/list", "params": {}}));
    client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "doomed", "reason": "client gave up"}}));
    let alive = client.call("after-cancel", "ping", serde_json::json!({}));
    assert_eq!(
        alive["result"],
        serde_json::json!({}),
        "cancellation must not poison: {alive}"
    );
    transcript.push(("after cancellation in flight", alive["result"].clone()));
    client
        .pending
        .retain(|value| value["id"] != "doomed" && value["id"] != "replay");
    assert!(
        client.pending.is_empty(),
        "no unsolicited message may be left over: {:?}",
        client.pending
    );
    transcript
}

/// The compatibility handshake every adapter session of GMCP-203-007-05
/// starts with.
fn adapter_handshake(client: &mut AdapterClient, name: &str) {
    let initialize = client.call(
        "init",
        "initialize",
        serde_json::json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": name, "version": "1"}}),
    );
    assert!(
        initialize["result"]["capabilities"].is_object(),
        "{name}: the handshake must succeed: {initialize}"
    );
    client.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
}

/// One pinned reference fixture that is a single wire message.
struct WireFixture {
    name: String,
    request: serde_json::Value,
    /// `modern`, `legacy`, or neither: the era the expectation holds in.
    era: Option<String>,
    /// `error`, `result` or `none`.
    expect: String,
    expect_code: Option<i64>,
}

/// The pinned reference fixtures that are one wire message, in name order.
fn adapter_wire_fixtures(root: &Path) -> Vec<WireFixture> {
    let mut fixtures = Vec::new();
    for kind in ["negative", "positive"] {
        let dir = root.join(".agents/gmcp/fixtures").join(kind);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .expect("the fixture directory must exist")
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| {
                name.starts_with("request-")
                    || name.starts_with("notification-")
                    || name.starts_with("server-discover-")
            })
            .collect();
        names.sort();
        for name in names {
            let fixture: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(dir.join(&name)).unwrap())
                    .unwrap_or_else(|e| panic!("{kind}/{name} must parse: {e}"));
            fixtures.push(WireFixture {
                name: format!("{kind}/{name}"),
                request: fixture["request"].clone(),
                era: fixture["era"].as_str().map(str::to_string),
                expect: fixture["expect"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{kind}/{name} must state what it expects"))
                    .to_string(),
                expect_code: fixture["expect_code"].as_i64(),
            });
        }
    }
    assert!(
        fixtures.len() >= 10,
        "the pinned wire fixtures must be present: {}",
        fixtures.len()
    );
    // A retained divergence is the expectation: the fixture stays verbatim
    // and every adapter must show the recorded outcome, no other.
    let divergences: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/fixtures/divergences.json"))
            .expect("the divergence record must exist"),
    )
    .expect("the divergence record must parse");
    for divergence in divergences["divergences"].as_array().into_iter().flatten() {
        let Some(fixture) = fixtures
            .iter_mut()
            .find(|fixture| divergence["fixture"] == fixture.name.as_str())
        else {
            continue;
        };
        fixture.expect = divergence["observed"]["kind"]
            .as_str()
            .expect("a divergence records what was observed")
            .to_string();
        fixture.expect_code = divergence["observed"]["code"].as_i64();
    }
    fixtures
}

/// The `_meta` every request of a modern-era session carries.
fn adapter_modern_meta() -> serde_json::Value {
    serde_json::json!({"_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}})
}

/// Replay, on one session of the given era, every wire fixture whose
/// expectation holds in that era. The transcript holds what each earned:
/// `error <code>`, `result`, or `silence` for a notification; each must be
/// what the fixture expects.
fn adapter_fixture_replay(
    client: &mut AdapterClient,
    fixtures: &[WireFixture],
    era: &str,
) -> Vec<(String, String)> {
    // `ping` is not a method of an established modern session; discovery
    // answers in every state of both eras.
    let (barrier_method, barrier_params) = if era == "modern" {
        ("server/discover", adapter_modern_meta())
    } else {
        ("ping", serde_json::json!({}))
    };
    let mut transcript = Vec::new();
    for (index, fixture) in fixtures.iter().enumerate() {
        if fixture.era.as_deref().is_some_and(|own| own != era) {
            continue;
        }
        let name = &fixture.name;
        let request = &fixture.request;
        client.send(request);
        let id = request.get("id").filter(|id| !id.is_null()).cloned();
        let outcome = if fixture.expect == "none" {
            "silence".to_string()
        } else {
            let answer = client.take(name, |value| {
                id.as_ref().is_some_and(|id| &value["id"] == id)
                    || (value["id"].is_null() && value["error"].is_object())
            });
            match answer["error"]["code"].as_i64() {
                Some(code) => {
                    assert!(
                        fixture.expect_code.is_none_or(|expected| expected == code),
                        "{name}: expected error {:?}, got {answer}",
                        fixture.expect_code
                    );
                    format!("error {code}")
                }
                None => {
                    assert!(
                        answer.get("result").is_some(),
                        "{name}: an answer is a result or a typed error: {answer}"
                    );
                    "result".to_string()
                }
            }
        };
        let earned = outcome.split(' ').next().unwrap_or_default();
        let expected = if fixture.expect == "none" {
            "silence"
        } else {
            fixture.expect.as_str()
        };
        assert_eq!(
            earned, expected,
            "{name}: the {era} session must answer as the fixture expects, got {outcome}"
        );
        // The session serves the next request whatever the fixture was.
        let barrier = client.call(
            &format!("fixture-{index}"),
            barrier_method,
            barrier_params.clone(),
        );
        assert!(
            barrier["result"].is_object(),
            "{name}: the session must survive the fixture: {barrier}"
        );
        transcript.push((name.clone(), outcome));
    }
    assert!(
        client.pending.is_empty(),
        "a fixture replay leaves no unsolicited message: {:?}",
        client.pending
    );
    transcript
}

/// Open a modern-era session: no `initialize`, discovery with `_meta`.
fn adapter_modern_open(client: &mut AdapterClient) {
    let discovered = client.call("open", "server/discover", adapter_modern_meta());
    assert!(
        discovered["result"].is_object(),
        "a modern session opens with server/discover: {discovered}"
    );
}

/// A deterministic pipelined run: `count` requests in bursts of varying
/// size, each burst one write. Returns, per request id, what answered it;
/// every id must be answered exactly once.
fn adapter_property_run(
    client: &mut AdapterClient,
    count: usize,
) -> std::collections::BTreeMap<String, String> {
    let mut seed: u64 = 0x2030_0705;
    let mut next = || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as usize
    };
    let mut answers = std::collections::BTreeMap::new();
    let mut sent = 0;
    while sent < count {
        let burst = (1 + next() % 16).min(count - sent);
        let mut bytes = Vec::new();
        let mut ids = Vec::new();
        for _ in 0..burst {
            let id = format!("p{sent}");
            let method = if next() % 8 == 0 {
                "tools/list"
            } else {
                "ping"
            };
            bytes.extend_from_slice(
                &serde_json::to_vec(&serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": {}}))
                .unwrap(),
            );
            bytes.push(b'\n');
            ids.push((id, method));
            sent += 1;
        }
        (client.write)(&bytes);
        for (id, method) in ids {
            let answer = client.answer(&id);
            let outcome = if method == "ping" {
                assert_eq!(
                    answer["result"],
                    serde_json::json!({}),
                    "{id}: ping must be served: {answer}"
                );
                "ping".to_string()
            } else {
                let tools = answer["result"]["tools"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{id}: tools/list must be served: {answer}"));
                format!("tools {}", tools.len())
            };
            assert!(
                answers.insert(id.clone(), outcome).is_none(),
                "{id} was answered twice"
            );
        }
    }
    assert!(
        client.pending.is_empty(),
        "every answer belongs to a request: {:?}",
        client.pending
    );
    answers
}

/// The ceilings `docs/gmcp/contract-203-007.md` publishes, by name.
fn adapter_budgets(root: &Path) -> std::collections::BTreeMap<String, u128> {
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-007.md"))
        .expect("the vector's contract must exist");
    let budgets: std::collections::BTreeMap<String, u128> = contract
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- budget: "))
        .filter_map(|rest| {
            let (name, rest) = rest.split_once(" <= ")?;
            let value = rest.split_whitespace().next()?.parse().ok()?;
            Some((name.to_string(), value))
        })
        .collect();
    for name in [
        "startup_ms",
        "request_ms",
        "pipeline_ms",
        "fairness_ms",
        "memory_growth_mib",
        "disk_growth_kib",
    ] {
        assert!(
            budgets.contains_key(name),
            "the contract must publish the {name} budget"
        );
    }
    budgets
}

fn adapter_tree_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => adapter_tree_bytes(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Resident memory of this process in bytes, where the host reports it.
fn adapter_resident_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

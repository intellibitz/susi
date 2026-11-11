//! Wire golden for the harness's three HTTP surfaces: the metering gateway,
//! the public ingress and the harness API (cockpit). A scripted set of raw
//! HTTP/1.x exchanges runs against the public `serve` functions the binaries
//! call, and every response is compared byte for byte (status line, header
//! names, values and order, framing, chunk boundaries, body) with the
//! transcript in `golden/harness_http_wire.json`.
//!
//! The transcript was recorded from the axum 0.8 / hyper 1 stack before it
//! left the harness (`SUSI_HARNESS_GOLDEN=capture cargo test -p susi-harness
//! http_wire`), so the in-tree server is held to the bytes axum put on the
//! wire. Only values that differ run to run are normalized: the `date`
//! header reads `<date>` and the cockpit's `generated_unix_ms` reads
//! `<ms>` (13 digits either way, so the framing is unchanged).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/tests/golden/harness_http_wire.json"
);

/// A provider stand-in: answers every call on a fresh connection and closes
/// it, so no pooled connection outlives an exchange. The body decides the
/// answer: a streamed call gets SSE lines, `fail-me` a 500, anything else a
/// priced completion.
fn fake_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || answer_upstream(stream));
        }
    });
    address
}

fn answer_upstream(stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).unwrap();
    let body = String::from_utf8_lossy(&body).into_owned();
    let mut stream = stream;
    let reply = if body.contains("\"stream\":true") {
        concat!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            "data: {\"id\":\"cmpl-s\",\"created\":1700000000,\"choices\":[{\"delta\":{\"content\":\"O\"}}]}\n\n",
            "data: {\"id\":\"cmpl-s\",\"created\":1700000000,\"choices\":[{\"delta\":{\"content\":\"K\"}}]}\n\n",
            "data: {\"id\":\"cmpl-s\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string()
    } else if body.contains("fail-me") {
        let payload = r#"{"error":{"message":"boom"}}"#;
        format!(
            "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        )
    } else {
        let payload = r#"{"id":"cmpl-1","object":"chat.completion","created":1700000000,"choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":3}}"#;
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        )
    };
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

fn gateway_state(upstream: SocketAddr, dir: &Path) -> Arc<crate::gateway::GatewayState> {
    Arc::new(crate::gateway::GatewayState {
        catalogue: crate::catalog::PriceCatalogue::from_json(
            r#"{"schema":"susi/harness/prices/v1","models":{}}"#,
        )
        .unwrap(),
        ledger_path: dir.join("ledger.jsonl"),
        upstream_chat_url: format!("http://{upstream}/v1/chat/completions"),
        provider: "golden-provider".into(),
        api_key: Some("test-key".into()),
        timeout_secs: 10,
        max_output_tokens: None,
        cache: None,
    })
}

fn cockpit(dir: &Path) -> crate::cockpit::Cockpit {
    crate::cockpit::Cockpit {
        ledger_path: dir.join("cockpit-ledger.jsonl"),
        events_path: dir.join("events.jsonl"),
        state_path: None,
        plan_path: None,
        controls_path: None,
        catalogue_path: None,
        graph_path: Some(dir.join("graph.jsonl")),
        budget: None,
        mission: None,
    }
}

struct Servers {
    gateway: SocketAddr,
    ingress: SocketAddr,
    cockpit: SocketAddr,
}

fn start(runtime: &tokio::runtime::Runtime, dir: &Path) -> Servers {
    let upstream = fake_upstream();
    let state = gateway_state(upstream, dir);
    let accounts = Arc::new(crate::ingress::Ingress::new(vec![
        crate::ingress::Account::new("golden-token", "golden-account"),
    ]));
    let authorize: crate::cockpit::CommandAuthorizer =
        Arc::new(|context: &crate::cockpit::CommandAuthContext<'_>| {
            context.header("authorization") == Some("Bearer golden-token")
        });
    let cockpit = Arc::new(cockpit(dir));
    runtime.block_on(async move {
        let bind = || async { tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap() };
        let (gateway, ingress, api) = (bind().await, bind().await, bind().await);
        let servers = Servers {
            gateway: gateway.local_addr().unwrap(),
            ingress: ingress.local_addr().unwrap(),
            cockpit: api.local_addr().unwrap(),
        };
        let ingress_state = Arc::clone(&state);
        tokio::spawn(crate::gateway::serve(gateway, state));
        tokio::spawn(crate::ingress::serve(ingress, ingress_state, accounts));
        tokio::spawn(crate::cockpit::serve(api, cockpit, authorize));
        servers
    })
}

/// How far to read the answer to a request.
#[derive(Clone, Copy, PartialEq)]
enum Until {
    /// Every response the request draws, framed by its own headers.
    Framed,
    /// The head and the first body chunk (an endless SSE stream).
    FirstChunk,
}

fn connect(address: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
}

fn read_line(reader: &mut BufReader<TcpStream>, out: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    match reader.read_until(b'\n', &mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            out.extend_from_slice(&line);
            Some(line)
        }
    }
}

/// One response, raw, as the client read it; `None` when the connection
/// closed before a status line arrived.
fn read_response(
    reader: &mut BufReader<TcpStream>,
    head_request: bool,
    until: Until,
) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let status_line = read_line(reader, &mut out)?;
    let status: u16 = String::from_utf8_lossy(&status_line)
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let mut length = None;
    let mut chunked = false;
    loop {
        let line = read_line(reader, &mut out)?;
        if line == b"\r\n" {
            break;
        }
        let text = String::from_utf8_lossy(&line).to_ascii_lowercase();
        if let Some(value) = text.strip_prefix("content-length:") {
            length = value.trim().parse::<usize>().ok();
        }
        if text.starts_with("transfer-encoding:") && text.contains("chunked") {
            chunked = true;
        }
    }
    if head_request || (100..200).contains(&status) || status == 204 || status == 304 {
        return Some(out);
    }
    if chunked {
        loop {
            let size_line = read_line(reader, &mut out)?;
            let size = usize::from_str_radix(
                String::from_utf8_lossy(&size_line)
                    .trim()
                    .split(';')
                    .next()
                    .unwrap_or("0"),
                16,
            )
            .unwrap_or(0);
            if size == 0 {
                read_line(reader, &mut out)?;
                return Some(out);
            }
            let mut data = vec![0u8; size + 2];
            reader.read_exact(&mut data).ok()?;
            out.extend_from_slice(&data);
            if until == Until::FirstChunk {
                return Some(out);
            }
        }
    }
    if let Some(length) = length {
        let mut data = vec![0u8; length];
        reader.read_exact(&mut data).ok()?;
        out.extend_from_slice(&data);
        return Some(out);
    }
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
    out.extend_from_slice(&rest);
    Some(out)
}

/// Send `request` and read every response it draws (an interim `100` and
/// its final answer, or one answer per pipelined request), then whether the
/// server closed the connection.
fn exchange(address: SocketAddr, request: &[u8], expected: usize, until: Until) -> String {
    let stream = connect(address);
    let head_request = request.starts_with(b"HEAD ");
    // A large body is written from its own thread: the server may answer
    // and close before it has read all of it.
    let writer = {
        let mut stream = stream.try_clone().unwrap();
        let request = request.to_vec();
        std::thread::spawn(move || {
            let _ = stream.write_all(&request);
        })
    };
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut transcript = Vec::new();
    let mut finals = 0;
    while finals < expected {
        let Some(response) = read_response(&mut reader, head_request, until) else {
            break;
        };
        if !response.starts_with(b"HTTP/1.1 100") {
            finals += 1;
        }
        transcript.extend_from_slice(&response);
    }
    if until == Until::Framed {
        // Whether the server ends the connection after its answer: a quiet
        // half second reads as kept open.
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut rest = [0u8; 1];
        let state: &[u8] = match reader.read(&mut rest) {
            Ok(0) => b"<closed>",
            Ok(_) => b"<more>",
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                b"<open>"
            }
            Err(_) => b"<reset>",
        };
        transcript.extend_from_slice(state);
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = writer.join();
    normalize(&String::from_utf8(transcript).expect("responses are UTF-8"))
}

/// Send a request whose body waits for the server's `100 Continue`.
fn exchange_expecting_continue(address: SocketAddr, head: &str, body: &str) -> String {
    let mut stream = connect(address);
    stream.write_all(head.as_bytes()).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut transcript = Vec::new();
    // The interim answer comes before the body is sent, or not at all.
    let mut first = Vec::new();
    let _ = reader.read_until(b'\n', &mut first);
    transcript.extend_from_slice(&first);
    if first.starts_with(b"HTTP/1.1 100") {
        let mut blank = Vec::new();
        let _ = reader.read_until(b'\n', &mut blank);
        transcript.extend_from_slice(&blank);
    }
    stream.write_all(body.as_bytes()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    if !first.starts_with(b"HTTP/1.1 100") && !first.is_empty() {
        // That was the final answer's status line.
        let mut rest = Vec::new();
        let _ = reader.read_to_end(&mut rest);
        transcript.extend_from_slice(&rest);
    } else if let Some(response) = read_response(&mut reader, false, Until::Framed) {
        transcript.extend_from_slice(&response);
    }
    normalize(&String::from_utf8(transcript).expect("responses are UTF-8"))
}

fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\r\ndate: ") {
        let start = at + "\r\ndate: ".len();
        out.push_str(&rest[..start]);
        out.push_str("<date>");
        let end = rest[start..]
            .find("\r\n")
            .map_or(rest.len(), |len| start + len);
        rest = &rest[end..];
    }
    out.push_str(rest);
    let mut normalized = String::with_capacity(out.len());
    let mut rest = out.as_str();
    let key = "\"generated_unix_ms\":";
    while let Some(at) = rest.find(key) {
        let start = at + key.len();
        normalized.push_str(&rest[..start]);
        let digits = rest[start..].bytes().take_while(u8::is_ascii_digit).count();
        normalized.push_str(&"<ms>".repeat(usize::from(digits > 0)));
        normalized.push_str(&"_".repeat(digits.saturating_sub(4)));
        rest = &rest[start + digits..];
    }
    normalized.push_str(rest);
    normalized
}

fn post(path: &str, extra: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\nhost: localhost\r\n{extra}content-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn get(path: &str, extra: &str) -> Vec<u8> {
    format!("GET {path} HTTP/1.1\r\nhost: localhost\r\n{extra}\r\n").into_bytes()
}

const MODEL: &str = "golden-probe-model";

fn scripted(servers: &Servers) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut record = |name: &str, transcript: String| out.push((name.to_string(), transcript));
    let (g, i, c) = (servers.gateway, servers.ingress, servers.cockpit);
    let one = |address, request: &[u8]| exchange(address, request, 1, Until::Framed);
    let completion = |extra: &str| {
        json!({"model": MODEL, "messages": [{"role": "user", "content": format!("hi{extra}")}]})
            .to_string()
    };

    // Gateway routing and framing.
    record("gateway GET /health", one(g, &get("/health", "")));
    record(
        "gateway HEAD /health",
        one(g, b"HEAD /health HTTP/1.1\r\nhost: localhost\r\n\r\n"),
    );
    record(
        "gateway GET /health?probe=1",
        one(g, &get("/health?probe=1", "")),
    );
    record("gateway POST /health", one(g, &post("/health", "", "x")));
    record("gateway GET /nope", one(g, &get("/nope", "")));
    record("gateway GET /health/", one(g, &get("/health/", "")));
    record(
        "gateway GET /v1/chat/completions",
        one(g, &get("/v1/chat/completions", "")),
    );
    record(
        "gateway PUT /v1/chat/completions",
        one(
            g,
            b"PUT /v1/chat/completions HTTP/1.1\r\nhost: localhost\r\ncontent-length: 0\r\n\r\n",
        ),
    );
    record(
        "gateway keep-alive then close",
        exchange(
            g,
            b"GET /health HTTP/1.1\r\nhost: localhost\r\n\r\nGET /health HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n",
            2,
            Until::Framed,
        ),
    );
    record(
        "gateway refused completion then keep-alive",
        exchange(
            g,
            b"POST /v1/chat/completions HTTP/1.1\r\nhost: localhost\r\ncontent-length: 3\r\n\r\nabcGET /health HTTP/1.1\r\nhost: localhost\r\n\r\n",
            2,
            Until::Framed,
        ),
    );
    record(
        "gateway POST /health then keep-alive",
        exchange(
            g,
            b"POST /health HTTP/1.1\r\nhost: localhost\r\ncontent-length: 3\r\n\r\nabcGET /health HTTP/1.1\r\nhost: localhost\r\n\r\n",
            2,
            Until::Framed,
        ),
    );
    record(
        "gateway HTTP/1.0 GET /health",
        one(g, b"GET /health HTTP/1.0\r\n\r\n"),
    );
    record(
        "gateway HTTP/1.0 keep-alive GET /health",
        one(g, b"GET /health HTTP/1.0\r\nconnection: keep-alive\r\n\r\n"),
    );
    record("gateway garbage request", one(g, b"GARBAGE\r\n\r\n"));
    record(
        "gateway oversized header",
        one(
            g,
            format!(
                "GET /health HTTP/1.1\r\nhost: localhost\r\nx-big: {}\r\n\r\n",
                "a".repeat(512 * 1024)
            )
            .as_bytes(),
        ),
    );

    // Gateway refusals before any provider is called.
    record(
        "gateway completion not JSON",
        one(g, &post("/v1/chat/completions", "", "not json")),
    );
    record(
        "gateway completion without model",
        one(g, &post("/v1/chat/completions", "", r#"{"messages":[]}"#)),
    );
    record(
        "gateway completion stream not boolean",
        one(
            g,
            &post(
                "/v1/chat/completions",
                "",
                &json!({"model": MODEL, "stream": "true", "messages": []}).to_string(),
            ),
        ),
    );
    let mut invalid =
        b"POST /v1/chat/completions HTTP/1.1\r\nhost: localhost\r\ncontent-length: 4\r\n\r\n"
            .to_vec();
    invalid.extend_from_slice(b"a\xffbc");
    record("gateway completion not UTF-8", one(g, &invalid));
    let huge = "x".repeat(2 * 1024 * 1024 + 1);
    record(
        "gateway completion over 2 MiB",
        one(g, &post("/v1/chat/completions", "", &huge)),
    );
    let exact = format!("{}{}", "{", " ".repeat(2 * 1024 * 1024 - 1));
    record(
        "gateway completion of exactly 2 MiB",
        one(g, &post("/v1/chat/completions", "", &exact)),
    );
    record(
        "gateway completion chunked request body",
        one(
            g,
            b"POST /v1/chat/completions HTTP/1.1\r\nhost: localhost\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
        ),
    );

    // Gateway calls that reach the provider stand-in.
    record(
        "gateway buffered completion",
        one(
            g,
            &post(
                "/v1/chat/completions",
                "content-type: application/json\r\n",
                &completion(""),
            ),
        ),
    );
    let streamed =
        json!({"model": MODEL, "stream": true, "messages": [{"role": "user", "content": "hi"}]})
            .to_string();
    record(
        "gateway streamed completion",
        one(g, &post("/v1/chat/completions", "", &streamed)),
    );
    record(
        "gateway HTTP/1.0 streamed completion",
        one(
            g,
            format!(
                "POST /v1/chat/completions HTTP/1.0\r\ncontent-length: {}\r\n\r\n{streamed}",
                streamed.len()
            )
            .as_bytes(),
        ),
    );
    record(
        "gateway upstream failure",
        one(
            g,
            &post("/v1/chat/completions", "", &completion(" fail-me")),
        ),
    );
    let body = completion(" continue");
    record(
        "gateway expect 100-continue",
        exchange_expecting_continue(
            g,
            &format!(
                "POST /v1/chat/completions HTTP/1.1\r\nhost: localhost\r\nexpect: 100-continue\r\ncontent-length: {}\r\n\r\n",
                body.len()
            ),
            &body,
        ),
    );

    // Ingress.
    record("ingress GET /health", one(i, &get("/health", "")));
    record(
        "ingress GET /v1/models without a token",
        one(i, &get("/v1/models", "")),
    );
    record(
        "ingress GET /v1/models with a wrong token",
        one(i, &get("/v1/models", "authorization: Bearer nope\r\n")),
    );
    record(
        "ingress completion with a wrong key",
        one(
            i,
            &post(
                "/v1/chat/completions",
                "x-api-key: nope\r\n",
                &completion(""),
            ),
        ),
    );
    record(
        "ingress completion admitted",
        one(
            i,
            &post(
                "/v1/chat/completions",
                "authorization: Bearer golden-token\r\n",
                &completion(""),
            ),
        ),
    );
    record(
        "ingress POST /v1/models",
        one(i, &post("/v1/models", "", "")),
    );

    // Harness API (cockpit).
    let bearer = "authorization: Bearer golden-token\r\n";
    let json_type = "content-type: application/json\r\n";
    record("cockpit GET /health", one(c, &get("/health", "")));
    record("cockpit GET /", one(c, &get("/", "")));
    record("cockpit GET /api/status", one(c, &get("/api/status", "")));
    record(
        "cockpit GET /api/events first event",
        exchange(c, &get("/api/events", ""), 1, Until::FirstChunk),
    );
    record("cockpit GET /api/command", one(c, &get("/api/command", "")));
    record(
        "cockpit GET /api/command with a token",
        one(
            c,
            &get("/api/command", "authorization: Bearer golden-token\r\n"),
        ),
    );
    record(
        "cockpit GET /api/command over 64 KiB",
        one(
            c,
            format!(
                "GET /api/command HTTP/1.1\r\nhost: localhost\r\ncontent-length: 65537\r\n\r\n{}",
                " ".repeat(65_537)
            )
            .as_bytes(),
        ),
    );
    let pause = r#"{"command":"pause","mission_id":"m-1"}"#;
    record(
        "cockpit command without a token",
        one(c, &post("/api/command", json_type, pause)),
    );
    record(
        "cockpit command over 64 KiB",
        one(
            c,
            &post(
                "/api/command",
                &format!("{bearer}{json_type}"),
                &" ".repeat(64 * 1024 + 1),
            ),
        ),
    );
    record(
        "cockpit command without content type",
        one(c, &post("/api/command", bearer, pause)),
    );
    record(
        "cockpit command as text/json",
        one(
            c,
            &post(
                "/api/command",
                &format!("{bearer}content-type: text/json\r\n"),
                pause,
            ),
        ),
    );
    record(
        "cockpit command as cloudevents+json",
        one(
            c,
            &post(
                "/api/command",
                &format!("{bearer}content-type: application/cloudevents+json; charset=utf-8\r\n"),
                pause,
            ),
        ),
    );
    for (name, body) in [
        ("accepted", pause),
        (
            "syntax error at the root",
            "{\"command\":\"pause\" \"mission_id\":\"m\"}",
        ),
        (
            "syntax error in a value",
            "{\"command\":\"pause\",\"mission_id\":}",
        ),
        (
            "syntax error in a nested value",
            "{\"command\":\"start_mission\",\"mission\":{\"id\":tru}}",
        ),
        (
            "syntax error in an array",
            "{\"command\":\"start_mission\",\"mission\":{\"tasks\":[1,}}",
        ),
        ("unterminated", "{\"command\":\"pause\""),
        ("trailing comma", "{\"command\":\"pause\",}"),
        ("empty body", ""),
        ("not an object", "[1,2]"),
        ("missing tag", "{\"mission_id\":\"m\"}"),
        (
            "duplicate tag",
            "{\"command\":\"pause\",\"command\":\"pause\"}",
        ),
        (
            "unknown variant",
            "{\"command\":\"launch\",\"mission_id\":\"m\"}",
        ),
        ("tag of the wrong type", "{\"command\":5}"),
        ("missing field", "{\"command\":\"pause\"}"),
        (
            "field of the wrong type",
            "{\"command\":\"redirect\",\"mission_id\":\"m\",\"priority\":\"high\"}",
        ),
        (
            "trailing characters",
            "{\"command\":\"pause\",\"mission_id\":\"m\"} x",
        ),
    ] {
        record(
            &format!("cockpit command {name}"),
            one(
                c,
                &post("/api/command", &format!("{bearer}{json_type}"), body),
            ),
        );
    }
    out
}

#[test]
fn the_http_surfaces_answer_byte_for_byte_as_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let servers = start(&runtime, dir.path());
    let transcript: Vec<Value> = scripted(&servers)
        .into_iter()
        .map(|(name, response)| json!({"exchange": name, "response": response}))
        .collect();
    let path = PathBuf::from(GOLDEN);
    if std::env::var("SUSI_HARNESS_GOLDEN").as_deref() == Ok("capture") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = serde_json::to_string_pretty(&transcript).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        return;
    }
    let golden: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(golden.len(), transcript.len(), "exchange count");
    for (want, got) in golden.iter().zip(&transcript) {
        assert_eq!(want["exchange"], got["exchange"]);
        assert_eq!(
            want["response"].as_str().unwrap(),
            got["response"].as_str().unwrap(),
            "{}",
            want["exchange"]
        );
    }
    runtime.shutdown_timeout(Duration::from_secs(1));
}

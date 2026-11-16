use super::*;
use std::io::Write;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
fn sse(body: &str, id: u64) -> (Result<Value, WireError>, Vec<Value>) {
    let mut seen = Vec::new();
    let result = sse_result(body.as_bytes(), id, &mut |n| seen.push(n.clone()));
    (result, seen)
}
#[test]
fn sse_picks_matching_id_skips_control_frames_and_relays_progress() {
    let body = "data: \nid: 0\nretry: 3000\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progress\":0.5}}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{\"data\":\"hi\"}}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}\n\n";
    let (result, seen) = sse(body, 2);
    assert_eq!(
        result
            .unwrap()
            .pointer("/content/0/text")
            .and_then(Value::as_str),
        Some("ok")
    );
    let methods: Vec<_> = seen.iter().map(|n| n["method"].clone()).collect();
    assert_eq!(
        methods,
        vec![
            json!("notifications/progress"),
            json!("notifications/message")
        ]
    );
}
#[test]
fn sse_surfaces_rpc_errors() {
    let body = "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32602,\"message\":\"Unknown tool\"}}";
    match sse(body, 2).0 {
        Err(WireError::Rpc(code, message)) => {
            assert_eq!(code, -32602);
            assert_eq!(message, "Unknown tool");
        }
        other => panic!("expected an rpc error, got {other:?}"),
    }
}
#[test]
fn sse_misses_when_no_frame_matches() {
    let (result, _) = sse("data: {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{}}", 2);
    assert!(matches!(result, Err(WireError::Protocol(_))));
}
#[test]
fn sse_joins_multiline_data_fields() {
    let body = "data: {\"jsonrpc\":\"2.0\",\ndata: \"id\":2,\"result\":{\"ok\":true}}\n\n";
    assert_eq!(sse(body, 2).0.unwrap(), json!({"ok": true}));
}
#[test]
fn server_requests_sharing_an_id_are_not_responses() {
    let body = "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"elicitation/create\"}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n\n";
    let (result, seen) = sse(body, 2);
    assert_eq!(result.unwrap(), json!({}));
    assert!(seen.is_empty());
}
#[test]
fn json_bodies_are_matched_and_sniffed() {
    let read =
        |raw: &'static [u8]| read_result(http::read_response(raw, "POST").unwrap(), 2, &mut |_| {});
    let json_reply =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}";
    assert_eq!(read(json_reply).unwrap(), json!({"tools": []}));
    let other_id =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{}}";
    assert!(read(other_id).is_err());
    let sniffed =
        b"HTTP/1.1 200 OK\r\n\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"x\":1}}\n\n";
    assert_eq!(read(sniffed).unwrap(), json!({"x": 1}));
}
#[test]
fn negotiation_accepts_only_spoken_versions() {
    assert_eq!(
        negotiated_version(&json!({"protocolVersion": "2025-06-18"})).unwrap(),
        "2025-06-18"
    );
    assert!(negotiated_version(&json!({"protocolVersion": "1999-01-01"})).is_err());
    assert!(negotiated_version(&json!({})).is_err());
}
#[test]
fn addresses_split_into_host_and_port() {
    assert_eq!(
        split_addr("127.0.0.1:9093").unwrap(),
        ("127.0.0.1".into(), 9093)
    );
    assert_eq!(split_addr("[::1]:9093").unwrap(), ("::1".into(), 9093));
    assert!(split_addr("127.0.0.1").is_err());
    assert!(split_addr(":9093").is_err());
    assert!(split_addr("host:port").is_err());
}
#[test]
fn only_loopback_addresses_are_accepted() {
    assert!(ensure_loopback("127.0.0.1", 9093).is_ok());
    assert!(ensure_loopback("::1", 9093).is_ok());
    for remote in ["192.0.2.1", "10.0.0.1", "0.0.0.0"] {
        let err = ensure_loopback(remote, 9093).unwrap_err();
        assert!(err.to_string().contains("not a loopback"), "{err}");
    }
    let err = Session::open("192.0.2.1:9093", Some("tok"), Duration::from_secs(5)).unwrap_err();
    assert!(matches!(err, WireError::Protocol(_)), "{err}");
}
/// The release reads only the status: a huge error body costs a short
/// excerpt, not the whole body.
#[test]
fn release_reads_only_a_short_excerpt_of_the_reply() {
    let huge: &'static str = Box::leak(
        format!(
            "HTTP/1.1 500 Oops\r\nContent-Length: 4000000\r\n\r\n{}",
            "x".repeat(4_000_000)
        )
        .into_boxed_str(),
    );
    let (addr, _, server) = scripted(vec![
        "HTTP/1.1 200 OK\r\nMcp-Session-Id: s-3\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\"}}",
        "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n",
        huge,
    ]);
    let session = Session::open(&addr, None, Duration::from_secs(5)).unwrap();
    match session.close() {
        Err(WireError::Http(500, body)) => assert!(body.len() <= 204, "{}", body.len()),
        other => panic!("expected HTTP 500, got {other:?}"),
    }
    drop(server);
}
#[test]
fn errors_display_their_cause() {
    assert_eq!(WireError::Http(401, String::new()).to_string(), "HTTP 401");
    assert_eq!(
        WireError::Http(404, "not found".into()).to_string(),
        "HTTP 404: not found"
    );
    assert_eq!(
        WireError::Rpc(-32602, "Unknown tool".into()).to_string(),
        "rpc error -32602: Unknown tool"
    );
}
/// One recorded request: method, headers (lower-cased names), body.
type Seen = Arc<Mutex<Vec<(String, Vec<(String, String)>, String)>>>;
/// A scripted Streamable HTTP server: answers each connection with the
/// next canned reply and records what the client sent.
fn scripted(replies: Vec<&'static str>) -> (String, Seen, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let handle = std::thread::spawn(move || {
        for reply in replies {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut head = Vec::new();
            loop {
                let mut line = String::new();
                io::BufRead::read_line(&mut reader, &mut line).unwrap();
                let line = line.trim_end().to_string();
                if line.is_empty() {
                    break;
                }
                head.push(line);
            }
            let method = head[0].split(' ').next().unwrap().to_string();
            let headers: Vec<(String, String)> = head[1..]
                .iter()
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            let length = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .map_or(0, |(_, v)| v.parse().unwrap());
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            log.lock()
                .unwrap()
                .push((method, headers, String::from_utf8(body).unwrap()));
            let mut stream = reader.into_inner();
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    (addr, seen, handle)
}
#[test]
fn session_handshakes_calls_with_progress_and_releases() {
    let (addr, seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nMcp-Session-Id: s-1\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-06-18\"}}",
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n\
             47\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n\r\n\
             31\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"a\":1}}\n\n\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ]);
    let mut session = Session::open(&addr, Some("tok"), Duration::from_secs(5)).unwrap();
    assert_eq!(session.protocol_version(), "2025-06-18");
    let mut progress = 0;
    let result = session
        .call_tool("echo", &json!({"x": 1}), Some(json!("p1")), &mut |_| {
            progress += 1
        })
        .unwrap();
    assert_eq!(result, json!({"a": 1}));
    assert_eq!(progress, 1);
    session.close().unwrap();
    server.join().unwrap();
    let seen = seen.lock().unwrap();
    let methods: Vec<&str> = seen.iter().map(|(m, _, _)| m.as_str()).collect();
    assert_eq!(methods, ["POST", "POST", "POST", "DELETE"]);
    let header = |i: usize, name: &str| {
        seen[i]
            .1
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(header(0, "authorization").as_deref(), Some("Bearer tok"));
    assert_eq!(header(0, "mcp-session-id"), None);
    for i in 1..4 {
        assert_eq!(header(i, "mcp-session-id").as_deref(), Some("s-1"));
        assert_eq!(
            header(i, "mcp-protocol-version").as_deref(),
            Some("2025-06-18")
        );
    }
    let call: Value = serde_json::from_str(&seen[2].2).unwrap();
    assert_eq!(call["method"], "tools/call");
    assert_eq!(call["params"]["name"], "echo");
    assert_eq!(call["params"]["_meta"]["progressToken"], "p1");
}
#[test]
fn unsupported_version_fails_and_still_releases_the_session() {
    let (addr, seen, server) = scripted(vec![
        "HTTP/1.1 200 OK\r\nMcp-Session-Id: s-2\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"1999-01-01\"}}",
        "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n",
    ]);
    let err = Session::open(&addr, None, Duration::from_secs(5)).unwrap_err();
    assert!(err.to_string().contains("unsupported"), "{err}");
    server.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.last().map(|(m, _, _)| m.as_str()), Some("DELETE"));
    assert!(seen[0].1.iter().all(|(k, _)| k != "authorization"));
}
#[test]
fn http_errors_carry_status_and_body() {
    let (addr, _, server) = scripted(vec![
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\n\r\nbad token\r\n\r\n",
    ]);
    match Session::open(&addr, Some("wrong"), Duration::from_secs(5)) {
        Err(WireError::Http(401, body)) => assert_eq!(body, "bad token"),
        other => panic!("expected HTTP 401, got {other:?}"),
    }
    server.join().unwrap();
}
/// A server that keeps the stream alive with comments but never answers
/// is cut off at the call's total budget, not kept open by each byte.
#[test]
fn keep_alive_comments_do_not_extend_the_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: s\r\n\r\n",
        );
        for _ in 0..100 {
            if stream.write_all(b": keep-alive\n\n").is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    });
    let started = std::time::Instant::now();
    let err = Session::open(&addr, None, Duration::from_millis(120)).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(err.to_string().contains("time budget"), "{err}");
}
/// A peer that accepts and never answers costs the caller's short
/// budget, not the 20-second handshake default.
#[test]
fn a_short_budget_bounds_the_handshake_too() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut sink = Vec::new();
            let _ = stream.read_to_end(&mut sink);
        }
    });
    let started = std::time::Instant::now();
    assert!(call_tool(&addr, "runtime_models_list", &json!({}), None, 1).is_err());
    assert!(
        started.elapsed() < DEFAULT_TIMEOUT,
        "{:?}",
        started.elapsed()
    );
}
#[test]
fn open_with_capabilities_exchanges_and_retains_server_extensions() {
    let (addr, seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nMcp-Session-Id: ext-1\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{\"list\":{}},\"vendor.example/custom\":{\"version\":2}}}}}",
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ]);
    let client_caps = json!({ "extensions": { "io.modelcontextprotocol/tasks": {}, "vendor.example/client-only": {"enabled": true}}});
    let session =
        Session::open_with_capabilities(&addr, None, &client_caps, Duration::from_secs(5)).unwrap();
    let exts = session
        .server_extensions()
        .expect("server extensions must be retained");
    assert!(exts.contains_key("io.modelcontextprotocol/tasks"));
    assert_eq!(exts["vendor.example/custom"]["version"], 2);
    assert!(session.server_capabilities().get("extensions").is_some());
    session.close().unwrap();
    server.join().unwrap();
    let seen = seen.lock().unwrap();
    let init: Value = serde_json::from_str(&seen[0].2).unwrap();
    assert_eq!(init["params"]["capabilities"], client_caps);
    assert!(Session::open_with_capabilities(
        &addr,
        None,
        &json!("not-an-object"),
        Duration::from_secs(1),
    )
    .is_err());
}
#[test]
fn open_2026_07_28_discovers_statelessly_and_detects_tasks_extension() {
    let (addr, seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2026-07-28\",\"2025-11-25\"],\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{}}},\"ttlMs\":1000,\"cacheScope\":\"public\"}}",
        ]);
    let session = Session::open_2026_07_28(&addr, Some("tok-1"), Duration::from_secs(5)).unwrap();
    assert_eq!(session.protocol_version(), PROTOCOL_VERSION_2026_07_28);
    assert!(session.supports_server_tasks());
    drop(session); // Must not issue DELETE /mcp when stateless.
    server.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "POST");
    let hdrs = &seen[0].1;
    assert!(hdrs
        .iter()
        .any(|(k, v)| k == "mcp-protocol-version" && v == "2026-07-28"));
    assert!(hdrs
        .iter()
        .any(|(k, v)| k == "mcp-method" && v == "server/discover"));
    assert!(!hdrs.iter().any(|(k, _)| k == "mcp-session-id"));
    let req: Value = serde_json::from_str(&seen[0].2).unwrap();
    assert_eq!(req["method"], "server/discover");
    assert_eq!(
        req["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-07-28"
    );
    assert_eq!(
        req["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"],
        json!({})
    );
}
#[test]
fn open_2026_07_28_rejects_missing_version_advertisement() {
    let (addr, _seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2025-11-25\"],\"capabilities\":{}}}",
        ]);
    let err = Session::open_2026_07_28(&addr, None, Duration::from_secs(5)).unwrap_err();
    assert!(matches!(err, WireError::Protocol(_)), "{err}");
    server.join().unwrap();
}
#[test]
fn call_tool_with_tasks_gates_capability_and_rejects_unexpected_task_result() {
    let (addr, seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2026-07-28\"],\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{}}}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"resultType\":\"task\",\"taskId\":\"task-abc\",\"status\":\"working\",\"createdAt\":\"2026-11-15T00:00:00Z\",\"lastUpdatedAt\":\"2026-11-15T00:00:00Z\",\"ttlMs\":5000,\"pollIntervalMs\":50}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"resultType\":\"task\",\"taskId\":\"task-forged\",\"status\":\"working\",\"createdAt\":\"2026-11-15T00:00:00Z\",\"lastUpdatedAt\":\"2026-11-15T00:00:00Z\"}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"resultType\":\"task\",\"taskId\":\"task-list\",\"status\":\"working\",\"createdAt\":\"2026-11-15T00:00:00Z\",\"lastUpdatedAt\":\"2026-11-15T00:00:00Z\"}}",
        ]);
    let mut session = Session::open_2026_07_28(&addr, None, Duration::from_secs(5)).unwrap();
    let outcome = session
        .call_tool_with_tasks("mission_run", &json!({"x": 1}), true, &mut |_| {})
        .unwrap();
    assert!(matches!(outcome, CallToolOutcome::Task(ref h) if h.task_id == "task-abc"));
    let forged_err = session
        .call_tool_with_tasks("mission_run", &json!({"x": 2}), false, &mut |_| {})
        .unwrap_err();
    assert!(matches!(forged_err, WireError::Protocol(_)), "{forged_err}");
    let list_err = session
        .request("tools/list", &json!({}), &mut |_| {})
        .unwrap_err();
    assert!(matches!(list_err, WireError::Protocol(_)), "{list_err}");
    server.join().unwrap();
    let seen = seen.lock().unwrap();
    let req2: Value = serde_json::from_str(&seen[1].2).unwrap();
    let req3: Value = serde_json::from_str(&seen[2].2).unwrap();
    assert!(
        req2["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"]
            .get(TASKS_EXTENSION_ID)
            .is_some()
    );
    assert!(seen[1]
        .1
        .iter()
        .any(|(k, v)| k == "mcp-method" && v == "tools/call"));
    assert!(seen[1]
        .1
        .iter()
        .any(|(k, v)| k == "mcp-name" && v == "mission_run"));
    assert_eq!(
        req3["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"],
        json!({})
    );
}
#[test]
fn task_rpcs_inject_mcp_name_and_retry_only_idempotent_transient_failures() {
    let (addr, seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2026-07-28\"],\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{}}}}}",
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\n\r\nbusy",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-9\",\"status\":\"working\",\"createdAt\":\"2026-11-15T00:00:00Z\",\"lastUpdatedAt\":\"2026-11-15T00:00:01Z\"}}",
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\n\r\nbusy",
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\n\r\nbusy",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":6,\"result\":{\"resultType\":\"complete\"}}",
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":7,\"error\":{\"code\":-32021,\"message\":\"Missing required client capability\"}}",
        ]);
    let mut session = Session::open_2026_07_28(&addr, None, Duration::from_secs(5)).unwrap();
    let snap = session.get_task("t-9").unwrap();
    assert_eq!(snap.handle.status, TaskStatus::Working);
    let update_err = session
        .update_task("t-9", &json!({"req-1": {"ok": true}}))
        .unwrap_err();
    assert!(
        matches!(update_err, WireError::Http(503, _)),
        "{update_err}"
    );
    session.cancel_task("t-9").unwrap();
    let rpc_err = session.get_task("t-9").unwrap_err();
    assert!(matches!(rpc_err, WireError::Rpc(-32021, _)), "{rpc_err}");
    server.join().unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 7);
    for entry in &seen[1..] {
        assert!(
            entry.1.iter().any(|(k, v)| k == "mcp-name" && v == "t-9"),
            "expected Mcp-Name: t-9 in {:?}",
            entry.1
        );
    }
}
#[test]
fn poll_task_until_done_handles_mrtr_honors_poll_interval_and_rejects_reuse_or_ttl() {
    let (addr, _seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2026-07-28\"],\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{}}}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-mrtr\",\"status\":\"working\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t0\",\"ttlMs\":5000,\"pollIntervalMs\":15}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-mrtr\",\"status\":\"input_required\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t1\",\"ttlMs\":5000,\"pollIntervalMs\":15,\"inputRequests\":{\"k-roots\":{\"method\":\"roots/list\"}}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"resultType\":\"complete\"}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-mrtr\",\"status\":\"completed\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t2\",\"ttlMs\":5000,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}],\"isError\":false}}}",
        ]);
    let mut session = Session::open_2026_07_28(&addr, None, Duration::from_secs(5)).unwrap();
    let handle = TaskHandle {
        task_id: "t-mrtr".into(),
        status: TaskStatus::Working,
        status_message: None,
        created_at: "t0".into(),
        last_updated_at: "t0".into(),
        ttl_ms: Some(5000),
        poll_interval_ms: Some(15),
    };
    let done = session
        .poll_task_until_done(&handle, Some(Duration::from_secs(5)), |reqs| {
            assert_eq!(reqs.len(), 1);
            assert_eq!(reqs[0].method, "roots/list");
            Ok(json!({ &reqs[0].key: {"roots": []} }))
        })
        .unwrap();
    assert_eq!(done.handle.status, TaskStatus::Completed);
    assert_eq!(done.result.unwrap()["content"][0]["text"], "done");
    server.join().unwrap();
    let (addr2, _seen2, server2) = scripted(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2026-07-28\"],\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{}}}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-dup\",\"status\":\"input_required\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t1\",\"pollIntervalMs\":5,\"inputRequests\":{\"k-same\":{\"method\":\"elicitation/create\",\"params\":{\"message\":\"ok\"}}}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"resultType\":\"complete\"}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-dup\",\"status\":\"input_required\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t2\",\"pollIntervalMs\":5,\"inputRequests\":{\"k-same\":{\"method\":\"elicitation/create\",\"params\":{\"message\":\"again\"}}}}}",
        ]);
    let mut session2 = Session::open_2026_07_28(&addr2, None, Duration::from_secs(5)).unwrap();
    let reuse_err = session2
        .poll_task_until_done(
            &TaskHandle {
                task_id: "t-dup".into(),
                ..handle.clone()
            },
            None,
            |reqs| Ok(json!({ &reqs[0].key: {"action": "accept"} })),
        )
        .unwrap_err();
    assert!(matches!(reuse_err, WireError::Protocol(_)), "{reuse_err}");
    server2.join().unwrap();
    let bad_snap = json!({ "resultType": "complete", "taskId": "t-bad", "status": "input_required", "createdAt": "t0", "lastUpdatedAt": "t1", "inputRequests": { "evil": { "method": "tools/call", "params": {}}}});
    assert!(TaskSnapshot::from_get_result(&bad_snap).is_err());
    let (addr3, _seen3, server3) = scripted(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2026-07-28\"],\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{}}}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-ttl\",\"status\":\"working\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t1\",\"ttlMs\":20,\"pollIntervalMs\":500}}",
        ]);
    let mut session3 = Session::open_2026_07_28(&addr3, None, Duration::from_secs(5)).unwrap();
    let ttl_err = session3
        .poll_task_until_done(
            &TaskHandle {
                task_id: "t-ttl".into(),
                ttl_ms: Some(20),
                poll_interval_ms: Some(500),
                ..handle
            },
            None,
            |_| Ok(json!({})),
        )
        .unwrap_err();
    assert!(matches!(ttl_err, WireError::Protocol(_)), "{ttl_err}");
    server3.join().unwrap();
}
#[test]
fn persisted_task_handle_store_survives_restart_and_fails_closed_on_corruption() {
    let dir = std::env::temp_dir().join(format!(
        "susi_wire_client_tasks_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let path = dir.join("handles.json");
    let mut store = PersistedTaskHandleStore::open(&path).unwrap();
    assert!(store.list().is_empty());
    let handle = TaskHandle {
        task_id: "task-persist-1".into(),
        status: TaskStatus::Working,
        status_message: Some("running".into()),
        created_at: "2026-11-15T00:00:00Z".into(),
        last_updated_at: "2026-11-15T00:00:00Z".into(),
        ttl_ms: Some(60_000),
        poll_interval_ms: Some(250),
    };
    store
        .save(PersistedTaskHandle::from_handle(
            "127.0.0.1:8080",
            PROTOCOL_VERSION_2026_07_28,
            &handle,
        ))
        .unwrap();
    drop(store);
    let mut reopened = PersistedTaskHandleStore::open(&path).unwrap();
    let entry = reopened.get("task-persist-1").unwrap();
    assert_eq!(entry.addr, "127.0.0.1:8080");
    assert_eq!(entry.status, TaskStatus::Working);
    let snap = TaskSnapshot::from_get_result(&json!({ "resultType": "complete", "taskId": "task-persist-1", "status": "completed", "statusMessage": "done", "createdAt": "2026-11-15T00:00:00Z", "lastUpdatedAt": "2026-11-15T00:00:05Z", "ttlMs": 60_000, "result": {"ok": true}}))
    .unwrap();
    assert!(reopened.update_from_snapshot(&snap).unwrap());
    assert_eq!(
        PersistedTaskHandleStore::open(&path)
            .unwrap()
            .get("task-persist-1")
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    assert!(reopened.remove("task-persist-1").unwrap());
    assert!(PersistedTaskHandleStore::open(&path)
        .unwrap()
        .list()
        .is_empty());
    fs::write(&path, b"{broken json").unwrap();
    assert!(matches!(
        PersistedTaskHandleStore::open(&path),
        Err(WireError::Protocol(_))
    ));
    let _ = fs::remove_dir_all(&dir);
}
#[test]
fn call_tool_resolving_tasks_bridges_sync_completed_is_error_failed_and_cancelled() {
    let (addr, _seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2026-07-28\"],\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{}}}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"resultType\":\"complete\",\"content\":[{\"type\":\"text\",\"text\":\"sync-direct\"}],\"isError\":false}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"resultType\":\"task\",\"taskId\":\"t-tool-err\",\"status\":\"working\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t0\",\"ttlMs\":5000,\"pollIntervalMs\":5}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-tool-err\",\"status\":\"completed\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t1\",\"ttlMs\":5000,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"tool-fault\"}],\"isError\":true}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{\"resultType\":\"task\",\"taskId\":\"t-rpc-err\",\"status\":\"working\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t0\",\"ttlMs\":5000,\"pollIntervalMs\":5}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":6,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-rpc-err\",\"status\":\"failed\",\"statusMessage\":\"rpc crashed\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t1\",\"ttlMs\":5000,\"error\":{\"code\":-32603,\"message\":\"downstream crash\"}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"resultType\":\"task\",\"taskId\":\"t-cancel\",\"status\":\"working\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t0\",\"ttlMs\":5000,\"pollIntervalMs\":5}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":8,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-cancel\",\"status\":\"cancelled\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t1\",\"ttlMs\":5000}}",
        ]);
    let mut session = Session::open_2026_07_28(&addr, None, Duration::from_secs(5)).unwrap();
    let direct = session
        .call_tool_resolving_tasks("echo", &json!({}), None, |_| Ok(json!({})))
        .unwrap();
    assert_eq!(direct["content"][0]["text"], "sync-direct");
    let tool_err = session
        .call_tool_resolving_tasks("echo", &json!({}), None, |_| Ok(json!({})))
        .unwrap();
    assert_eq!(tool_err["isError"], json!(true));
    assert_eq!(tool_err["content"][0]["text"], "tool-fault");
    let rpc_err = session
        .call_tool_resolving_tasks("echo", &json!({}), None, |_| Ok(json!({})))
        .unwrap_err();
    assert!(
        matches!(&rpc_err, WireError::Rpc(-32603, msg) if msg == "downstream crash"),
        "{rpc_err:?}"
    );
    let cancel_err = session
        .call_tool_resolving_tasks("echo", &json!({}), None, |_| Ok(json!({})))
        .unwrap_err();
    assert!(
        matches!(&cancel_err, WireError::Protocol(msg) if msg.contains("cancelled")),
        "{cancel_err:?}"
    );
    server.join().unwrap();
}
#[test]
fn task_ack_and_task_id_validation_reject_forged_result_types_and_control_chars() {
    let (addr, _seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"supportedVersions\":[\"2026-07-28\"],\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{}}}}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"resultType\":\"input_required\"}}",
        ]);
    let mut session = Session::open_2026_07_28(&addr, None, Duration::from_secs(5)).unwrap();
    for bad_id in ["", "bad\r\nEvil: 1", "bad\u{0000}id"] {
        assert!(matches!(
            session.get_task(bad_id),
            Err(WireError::Protocol(_))
        ));
        assert!(matches!(
            session.update_task(bad_id, &json!({})),
            Err(WireError::Protocol(_))
        ));
        assert!(matches!(
            session.cancel_task(bad_id),
            Err(WireError::Protocol(_))
        ));
    }
    assert!(matches!(
        session.update_task("valid-id", &json!({})),
        Err(WireError::Protocol(_))
    ));
    assert!(matches!(
        session.cancel_task("valid-id"),
        Err(WireError::Protocol(_))
    ));
    server.join().unwrap();
}

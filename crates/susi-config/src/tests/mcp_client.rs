use super::*;
use std::collections::VecDeque;
use std::io::{BufReader, Cursor};
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::time::Duration;
struct Scripted {
    responses: VecDeque<Value>,
    requests: Vec<Value>,
}
impl Scripted {
    fn new(responses: Vec<Value>) -> Self {
        Self {
            responses: responses.into(),
            requests: Vec::new(),
        }
    }
}
impl Transport for Scripted {
    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        self.requests.push(request.clone());
        self.responses
            .pop_front()
            .ok_or_else(|| EaiError::internal("scripted transport exhausted"))
    }
}
fn initialized() -> Value {
    response(
        1,
        json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}}),
    )
}
fn response(id: u64, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn error_response(id: u64, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
#[test]
fn mcp_lists_tools_after_initialize() {
    let transport = Scripted::new(vec![
        initialized(),
        response(
            2,
            json!({"tools":[{"name":"search","description":"Search","inputSchema":{"type":"object"}},{"name":"fetch","description":"Fetch","inputSchema":{"type":"object"}}]}),
        ),
    ]);
    let mut client = McpClient::new(transport);
    let tools = client.list_tools().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "search");
    assert_eq!(client.transport.requests[0]["method"], "initialize");
    assert_eq!(client.transport.requests[1]["method"], "tools/list");
}
#[test]
fn mcp_list_tools_follows_next_cursor_until_absent() {
    let transport = Scripted::new(vec![
        initialized(),
        response(2, json!({"tools": [{"name": "a"}], "nextCursor": "rev:1"})),
        response(3, json!({"tools": [{"name": "b"}], "nextCursor": "rev:2"})),
        response(4, json!({"tools": [{"name": "c"}]})),
    ]);
    let mut client = McpClient::new(transport);
    let names: Vec<String> = client
        .list_tools()
        .unwrap()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(names, ["a", "b", "c"]);
    let requests = &client.transport.requests;
    assert_eq!(requests.len(), 4);
    assert!(requests[1]["params"].get("cursor").is_none());
    assert_eq!(requests[2]["params"]["cursor"], "rev:1");
    assert_eq!(requests[3]["params"]["cursor"], "rev:2");
}
#[test]
fn mcp_list_tools_surfaces_a_cursor_error_and_bounds_paging() {
    let transport = Scripted::new(vec![
        initialized(),
        response(2, json!({"tools": [{"name": "a"}], "nextCursor": "rev:1"})),
        error_response(3, -32602, "Invalid cursor"),
    ]);
    let mut client = McpClient::new(transport);
    let error = client.list_tools().unwrap_err().to_string();
    assert!(error.contains("Invalid cursor"), "{error}");
    let mut endless = vec![initialized()];
    endless.extend((0..=MAX_LIST_PAGES).enumerate().map(|(index, n)| {
        response(
            index as u64 + 2,
            json!({"tools": [], "nextCursor": format!("rev:{n}")}),
        )
    }));
    let mut client = McpClient::new(Scripted::new(endless));
    let error = client.list_tools().unwrap_err().to_string();
    assert!(error.contains("still paging"), "{error}");
}
#[test]
fn mcp_calls_a_tool_and_joins_text_blocks() {
    let transport = Scripted::new(vec![
        initialized(),
        response(
            2,
            json!({"content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]}),
        ),
    ]);
    let mut client = McpClient::new(transport);
    let text = client.call("search", &json!({"q": "rust"})).unwrap();
    assert_eq!(text, "first\nsecond");
    let call = &client.transport.requests[1];
    assert_eq!(call["method"], "tools/call");
    assert_eq!(call["params"]["name"], "search");
    assert_eq!(call["params"]["arguments"]["q"], "rust");
}
#[test]
fn mcp_surfaces_a_tool_result_error_instead_of_returning_success_text() {
    let transport = Scripted::new(vec![
        initialized(),
        response(
            2,
            json!({"isError":true,"content":[{"type":"text","text":"permission denied"}]}),
        ),
    ]);
    let mut client = McpClient::new(transport);
    let error = client
        .call("read_file", &json!({"path": "secret.txt"}))
        .unwrap_err();
    let error = error.to_string();
    assert!(error.contains("tools/call 'read_file' failed"), "{error}");
    assert!(error.contains("permission denied"), "{error}");
}

#[test]
fn raw_tool_result_preserves_structured_content_empty_text_and_errors() {
    for result in [
        json!({"content":[{"type":"text","text":""}],"structuredContent":{"text":""},"isError":false}),
        json!({"content":[{"type":"text","text":"{\"kind\":\"Config\"}"}],"isError":true}),
    ] {
        let mut client = McpClient::new(Scripted::new(vec![
            initialized(),
            response(2, result.clone()),
        ]));
        assert_eq!(client.call_result("tool", &json!({})).unwrap(), result);
        assert_eq!(client.transport.requests.len(), 2);
    }
}

#[test]
fn stdio_skips_notifications_before_the_json_rpc_response() {
    let stream = concat!(
        "\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n"
    );
    let mut reader = BufReader::new(Cursor::new(stream));
    let response = read_stdio_response(&mut reader).unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["ok"], true);
}
#[test]
fn stdio_frame_limit_includes_the_newline_and_does_not_overread() {
    let response = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":null}\n";
    let limit = MAX_STDIO_FRAME_BYTES as usize;
    let mut bytes = vec![b' '; limit - response.len()];
    bytes.extend_from_slice(response.as_bytes());
    assert_eq!(
        read_stdio_response(&mut Cursor::new(&bytes)).unwrap()["id"],
        1
    );
    bytes.insert(0, b' ');
    let mut reader = Cursor::new(bytes);
    assert!(read_stdio_response(&mut reader)
        .unwrap_err()
        .to_string()
        .contains("line too long"));
    assert_eq!(reader.position(), MAX_STDIO_FRAME_BYTES);
}
#[test]
fn stdio_rejects_partial_eof_and_invalid_utf8_frames() {
    for bytes in [
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":null}".as_slice(),
        b"{\"method\":\"notifications/progress\"}",
        b" \r",
        b"\xff\n",
    ] {
        assert!(read_stdio_response(&mut Cursor::new(bytes)).is_err());
    }
}
#[cfg(unix)]
fn isolated_stdio_fixture(script: &str, timeout: Duration) -> StdioTransport {
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", script])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    StdioTransport::from_child(child)
        .unwrap()
        .with_timeout(timeout)
}
#[cfg(unix)]
#[test]
fn stdio_nonreading_child_times_out_large_requests_and_notifications() {
    for notification in [false, true] {
        let mut transport =
            isolated_stdio_fixture("while :; do :; done", Duration::from_millis(150));
        transport.set_call_timeout(None);
        let mut message = json!({"jsonrpc":"2.0", "method":"fixture", "params":{
            "text": "x".repeat(256 * 1024)
        }});
        if !notification {
            message["id"] = json!(1);
        }
        let started = Instant::now();
        let result = if notification {
            transport.notify(&message)
        } else {
            transport.exchange(&message).map(|_| ())
        };
        let error = result.unwrap_err().to_string();
        assert!(error.contains("mcp write timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(transport.child.try_wait().unwrap().is_some());
        assert!(transport.writes.is_none());
        assert!(transport.responses.is_none());
        assert!(transport
            .notify(&json!({"method":"must-not-be-retried"}))
            .unwrap_err()
            .to_string()
            .contains("closed"));
    }
}
#[cfg(unix)]
#[test]
fn stdio_optional_call_budget_preserves_long_running_native_operations() {
    let mut transport = isolated_stdio_fixture(
        concat!(
            "IFS= read -r _line; /bin/sleep 0.25; ",
            "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":true}'"
        ),
        Duration::from_millis(100),
    );
    assert_eq!(transport.call_timeout, Some(Duration::from_millis(100)));
    transport.set_call_timeout(None);
    let started = Instant::now();
    assert_eq!(
        transport.exchange(&json!({"id":1})).unwrap()["result"],
        true
    );
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(transport.timeout, Duration::from_millis(100));
    transport.set_call_timeout(Some(Duration::from_millis(50)));
    assert_eq!(transport.call_timeout, Some(Duration::from_millis(50)));
}
#[cfg(unix)]
#[test]
fn stdio_optional_call_budget_still_bounds_a_started_response_frame() {
    let mut transport = isolated_stdio_fixture(
        "IFS= read -r _line; printf '%s' '{'; while :; do :; done",
        Duration::from_millis(100),
    );
    transport.set_call_timeout(None);
    let started = Instant::now();
    let error = transport
        .exchange(&json!({"id":1}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("mcp response frame timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(transport.child.try_wait().unwrap().is_some());
}
#[cfg(unix)]
#[test]
fn stdio_notifications_never_extend_an_explicit_call_budget() {
    let mut transport = isolated_stdio_fixture(
        concat!(
            "IFS= read -r _line; while :; do printf '%s\\n' ",
            "'{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}'; ",
            "/bin/sleep 0.025; done"
        ),
        Duration::from_millis(150),
    );
    let started = Instant::now();
    let error = transport
        .exchange(&json!({"id":1}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("mcp response timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(transport.child.try_wait().unwrap().is_some());
}
#[cfg(unix)]
#[test]
fn stdio_malformed_response_aborts_a_still_running_owned_child() {
    let mut transport = isolated_stdio_fixture(
        "IFS= read -r _line; printf '%s\\n' 'invalid'; while :; do :; done",
        Duration::from_secs(2),
    );
    let error = transport.exchange(&json!({"id":1})).unwrap_err();
    assert!(error.to_string().contains("mcp response"));
    assert!(transport.child.try_wait().unwrap().is_some());
    assert!(transport.writes.is_none());
    assert!(transport.responses.is_none());
}
#[cfg(unix)]
#[test]
fn stdio_write_time_consumes_the_response_deadline_without_joining_pipe_holders() {
    let mut transport = isolated_stdio_fixture(
        concat!(
            "/bin/sleep 0.25; IFS= read -r _line; /bin/sleep 0.5; ",
            "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":null}'"
        ),
        Duration::from_millis(600),
    );
    let started = Instant::now();
    let error = transport
        .exchange(&json!({"id":1, "text":"x".repeat(256 * 1024)}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("mcp response timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(transport.child.try_wait().unwrap().is_some());
    // The finite sleep descendant inherited the pipes. Closing must not join
    // either worker even while that descendant keeps the pipe handles open.
    transport.close().unwrap();
    assert!(transport.reader.is_none());
    assert!(transport.writer.is_none());
}
#[cfg(unix)]
#[test]
fn stdio_transport_reads_a_response_from_the_reader_worker() {
    let script = concat!(
        "IFS= read -r _line && printf '%s\\n%s\\n' '",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/log\"}",
        "' '",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}",
        "'"
    );
    let mut transport = isolated_stdio_fixture(script, Duration::from_secs(5));
    let response = transport
        .exchange(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["ok"], true);
}
#[cfg(unix)]
#[test]
fn stdio_transport_times_out_and_reaps_an_unresponsive_child() {
    let script = "while IFS= read -r line; do :; done";
    let mut transport = isolated_stdio_fixture(script, Duration::from_millis(50));
    let error = transport
        .exchange(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("mcp response timed out after"), "{error}");
}
#[cfg(unix)]
#[test]
fn closing_an_mcp_client_reaps_its_stdio_child_immediately() {
    let script = "while IFS= read -r line; do :; done";
    let transport = isolated_stdio_fixture(script, Duration::from_secs(60));
    let mut client = McpClient::new(transport);
    client.close().unwrap();
    assert!(client.closed);
    assert!(client.transport.child.try_wait().unwrap().is_some());
    client.close().unwrap();
}
#[test]
fn mcp_surfaces_a_json_rpc_error() {
    let transport = Scripted::new(vec![
        initialized(),
        error_response(2, -32601, "method not found"),
    ]);
    let mut client = McpClient::new(transport);
    let error = client.list_tools().unwrap_err().to_string();
    assert!(error.contains("method not found"), "{error}");
}
#[test]
fn supported_versions_list_the_modern_revisions_and_keep_the_offered_one() {
    assert_eq!(PROTOCOL_VERSION, "2025-11-25");
    assert_eq!(
        SUPPORTED_PROTOCOL_VERSIONS,
        &["2026-07-28", "2025-11-25", "2024-11-05"]
    );
    assert!(is_supported_protocol_version(PROTOCOL_VERSION));
    assert!(is_supported_protocol_version("2026-07-28"));
    assert!(!is_supported_protocol_version("1999-01-01"));
}
#[test]
fn mcp_records_the_negotiated_protocol_version() {
    let transport = Scripted::new(vec![
        response(
            1,
            json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
        ),
        response(2, json!({"tools": []})),
    ]);
    let mut client = McpClient::new(transport);
    assert_eq!(client.negotiated_protocol_version(), None);
    assert!(!client.negotiated_supported());
    client.list_tools().unwrap();
    assert_eq!(client.negotiated_protocol_version(), Some("2025-11-25"));
    assert!(client.negotiated_supported());
    assert_eq!(
        client.transport().requests[0]["params"]["protocolVersion"],
        PROTOCOL_VERSION
    );
    let mut legacy = McpClient::new(Scripted::new(vec![
        response(1, json!({"protocolVersion": "2025-06-18"})),
        response(2, json!({"tools": []})),
    ]));
    legacy.list_tools().unwrap();
    assert_eq!(legacy.negotiated_protocol_version(), Some("2025-06-18"));
    assert!(!legacy.negotiated_supported());
}
#[test]
fn mcp_rejects_initialize_without_a_protocol_version_before_ready_notification() {
    let transport = Scripted::new(vec![response(1, json!({"capabilities": {}}))]);
    let mut client = McpClient::new(transport);
    let error = client.initialize().unwrap_err().to_string();
    assert!(error.contains("no non-empty protocolVersion"), "{error}");
    assert_eq!(client.transport.requests.len(), 1);
    assert!(!client.initialized);
}
#[test]
fn mcp_rejects_a_response_for_another_request_id() {
    let transport = Scripted::new(vec![
        json!({"jsonrpc":"2.0","id":99,"result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{}}}),
    ]);
    let mut client = McpClient::new(transport);
    let error = client.initialize().unwrap_err().to_string();
    assert!(
        error.contains("response id does not match request id 1"),
        "{error}"
    );
}
#[test]
fn mcp_rejects_a_response_without_the_json_rpc_version() {
    let transport = Scripted::new(vec![
        json!({"id":1,"result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{}}}),
    ]);
    let mut client = McpClient::new(transport);
    let error = client.initialize().unwrap_err().to_string();
    assert!(error.contains("invalid jsonrpc version"), "{error}");
}

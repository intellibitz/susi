use super::*;
use std::collections::VecDeque;
use std::io::{BufReader, Cursor};

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
            json!({"tools": [
                {"name": "search", "description": "Search", "inputSchema": {"type": "object"}},
                {"name": "fetch", "description": "Fetch", "inputSchema": {"type": "object"}}
            ]}),
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
            json!({"content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ]}),
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
            json!({
                "isError": true,
                "content": [{"type": "text", "text": "permission denied"}]
            }),
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
fn mcp_tools_turn_into_registry_specs() {
    let tool = McpTool {
        name: "search".into(),
        description: "Search".into(),
        schema: json!({"type": "object"}),
    };
    let spec = tool.to_spec(Policy::Ask);
    assert_eq!(spec.name, "search");
    assert_eq!(spec.policy, Policy::Ask);
}

#[test]
fn supported_versions_list_the_modern_revisions_and_keep_the_offered_one() {
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
    let transport = Scripted::new(vec![json!({
        "jsonrpc": "2.0",
        "id": 99,
        "result": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}}
    })]);
    let mut client = McpClient::new(transport);
    let error = client.initialize().unwrap_err().to_string();
    assert!(
        error.contains("response id does not match request id 1"),
        "{error}"
    );
}

#[test]
fn mcp_rejects_a_response_without_the_json_rpc_version() {
    let transport = Scripted::new(vec![json!({
        "id": 1,
        "result": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}}
    })]);
    let mut client = McpClient::new(transport);
    let error = client.initialize().unwrap_err().to_string();
    assert!(error.contains("invalid jsonrpc version"), "{error}");
}

#[test]
fn http_endpoints_gain_the_gmcp_path_only_when_they_have_none() {
    assert_eq!(
        HttpSocketTransport::new("http://127.0.0.1:9090").url(),
        "http://127.0.0.1:9090/mcp"
    );
    assert_eq!(
        HttpSocketTransport::new("http://127.0.0.1:9093/").url(),
        "http://127.0.0.1:9093/mcp"
    );
    assert_eq!(
        HttpSocketTransport::new("http://127.0.0.1:9090/messages").url(),
        "http://127.0.0.1:9090/messages"
    );
    let socket = crate::adaptive::SusiSocketEndpoints::default();
    assert_eq!(
        HttpSocketTransport::for_socket(&socket).url(),
        "http://127.0.0.1:9090/mcp"
    );
}

#[test]
fn event_streams_yield_the_response_matching_the_request_id() {
    let stream = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\r\n\r\n\
                  data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n\n\
                  data: {\"jsonrpc\":\"2.0\",\"id\":8,\"result\":{\"ok\":false}}\n";
    let answer = response_from_event_stream(stream, Some(&json!(7))).unwrap();
    assert_eq!(answer["result"]["ok"], true);
    let last = response_from_event_stream(stream, Some(&json!(99))).unwrap();
    assert_eq!(last["id"], 8);
    assert!(response_from_event_stream(": keep-alive\n\n", None).is_err());
}

/// One recorded HTTP request: lowercase header map and body.
struct Recorded {
    request_line: String,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Serve one canned response per connection, in order, and hand back
/// every request it read. Each request is read in full (head and
/// `Content-Length` body) before the reply, so Windows never resets the
/// socket with unread bytes.
fn serve(replies: Vec<String>) -> (String, std::thread::JoinHandle<Vec<Recorded>>) {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let mut recorded = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0_u8; 4096];
            let head_end = loop {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "client closed before sending a request");
                raw.extend_from_slice(&buffer[..read]);
                if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
            let mut lines = head.split("\r\n");
            let request_line = lines.next().unwrap().to_string();
            let headers: Vec<(String, String)> = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            let length: usize = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .map_or(0, |(_, v)| v.parse().unwrap());
            while raw.len() < head_end + length {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "client closed mid-body");
                raw.extend_from_slice(&buffer[..read]);
            }
            let body = serde_json::from_slice(&raw[head_end..head_end + length]).unwrap();
            stream.write_all(reply.as_bytes()).unwrap();
            stream.flush().unwrap();
            recorded.push(Recorded {
                request_line,
                headers,
                body,
            });
        }
        recorded
    });
    (url, handle)
}

fn reply(status: &str, extra_headers: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n{extra_headers}\r\n{body}",
        body.len()
    )
}

#[test]
fn http_socket_transport_runs_the_session_handshake_over_json_rpc() {
    let init = json!({
        "jsonrpc": "2.0", "id": 1,
        "result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}}
    })
    .to_string();
    let list = format!(
        "event: message\ndata: {}\n\n",
        json!({"jsonrpc": "2.0", "id": 2, "result": {"tools": [
            {"name": "susi.status", "description": "Status", "inputSchema": {"type": "object"}}
        ]}})
    );
    let call = json!({
        "jsonrpc": "2.0", "id": 3,
        "result": {"content": [{"type": "text", "text": "healthy"}]}
    })
    .to_string();
    let (url, server) = serve(vec![
        reply(
            "200 OK",
            "Mcp-Session-Id: sess-42\r\n",
            "application/json",
            &init,
        ),
        reply("202 Accepted", "", "application/json", ""),
        reply("200 OK", "", "text/event-stream", &list),
        reply("200 OK", "", "application/json", &call),
    ]);

    let transport = HttpSocketTransport::new(&url)
        .with_timeout_secs(10)
        .with_bearer("token-1");
    let mut client = McpClient::new(transport);
    let tools = client.list_tools().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "susi.status");
    assert_eq!(client.negotiated_protocol_version(), Some("2025-11-25"));
    assert_eq!(client.transport().session_id(), Some("sess-42"));
    let answer = client.call("susi.status", &json!({})).unwrap();
    assert_eq!(answer, "healthy");

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    for request in &requests {
        assert_eq!(request.request_line, "POST /mcp HTTP/1.1");
        assert_eq!(request.body["jsonrpc"], "2.0");
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert_eq!(
            request.header("accept"),
            Some("application/json, text/event-stream")
        );
        assert_eq!(request.header("authorization"), Some("Bearer token-1"));
    }
    let init = &requests[0];
    assert_eq!(init.body["method"], "initialize");
    assert_eq!(init.body["id"], 1);
    assert_eq!(init.body["params"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(init.header("mcp-session-id"), None);
    assert_eq!(init.header("mcp-protocol-version"), None);

    let initialized = &requests[1];
    assert_eq!(initialized.body["method"], "notifications/initialized");
    assert!(initialized.body.get("id").is_none());
    for later in &requests[1..] {
        assert_eq!(later.header("mcp-session-id"), Some("sess-42"));
        assert_eq!(later.header("mcp-protocol-version"), Some("2025-11-25"));
    }
    assert_eq!(requests[2].body["method"], "tools/list");
    assert_eq!(requests[2].body["id"], 2);
    assert_eq!(requests[3].body["method"], "tools/call");
    assert_eq!(requests[3].body["params"]["name"], "susi.status");
}

#[test]
fn http_socket_transport_surfaces_http_errors_and_unreachable_sockets() {
    let (url, server) = serve(vec![reply(
        "500 Internal Server Error",
        "",
        "text/plain",
        "boom",
    )]);
    let mut client = McpClient::new(HttpSocketTransport::new(&url).with_timeout_secs(10));
    let error = client.initialize().unwrap_err().to_string();
    assert!(
        error.contains("HTTP 500") && error.contains("boom"),
        "{error}"
    );
    server.join().unwrap();

    let mut closed =
        McpClient::new(HttpSocketTransport::new("http://127.0.0.1:1").with_timeout_secs(2));
    assert!(closed.initialize().is_err());
}

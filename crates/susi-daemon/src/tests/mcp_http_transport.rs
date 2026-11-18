//! The streamable-HTTP MCP transport, moved with its tests from `susi-harness`
//! `mcp.rs` (Mandate 84): header rules, endpoint paths, event streams, the
//! session handshake, session close and HTTP errors, over a canned server. The
//! transport carries no bearer, as the one `susi-harness` registered did not.

use super::*;
use serde_json::json;
use std::io::Write;
use susi_config::mcp_client::{McpClient, PROTOCOL_VERSION};

#[test]
fn configured_http_headers_cannot_override_protocol_or_frame_messages() {
    for (name, value) in [
        ("MCP-Session-Id", "foreign"),
        ("Content-Length", "0"),
        ("Authorization", "Bearer bad\r\nInjected: true"),
        ("bad header", "value"),
    ] {
        assert!(
            HttpTransport::new("http://127.0.0.1:9090/mcp")
                .with_headers(vec![(name.into(), value.into())])
                .is_err()
        );
    }
    let transport = HttpTransport::new("http://127.0.0.1:9090/mcp")
        .with_headers(vec![
            ("Authorization".into(), "Bearer fixture".into()),
            ("X-Client".into(), "fixture".into()),
        ])
        .unwrap();
    assert_eq!(transport.headers.len(), 2);
}
#[test]
fn http_endpoints_gain_the_gmcp_path_only_when_they_have_none() {
    assert_eq!(
        HttpTransport::new("http://127.0.0.1:9090").url(),
        "http://127.0.0.1:9090/mcp"
    );
    assert_eq!(
        HttpTransport::new("http://127.0.0.1:9093/").url(),
        "http://127.0.0.1:9093/mcp"
    );
    assert_eq!(
        HttpTransport::new("http://127.0.0.1:9090/messages").url(),
        "http://127.0.0.1:9090/messages"
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

#[test]
fn event_streams_keep_first_match_and_last_fallback_with_lenient_cr_lines() {
    let stream = "data: broken\n\ndata: \nretry: 100\n\n\
                  data: {\"id\":7,\r\r\ndata: \"result\":\"first\"}\r\r\n\r\r\n\
                  data: {\"id\":7,\"result\":\"second\"}\n\n\
                  data: {\"id\":9,\"method\":\"notifications/progress\"}\n\n\
                  data: {\"id\":8,\"error\":null}";
    assert_eq!(
        response_from_event_stream(stream, Some(&json!(7))).unwrap()["result"],
        "first"
    );
    for id in [None, Some(json!(99))] {
        assert_eq!(
            response_from_event_stream(stream, id.as_ref()).unwrap(),
            json!({"id": 8, "error": null})
        );
    }
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
            let body = if length == 0 {
                Value::Null
            } else {
                serde_json::from_slice(&raw[head_end..head_end + length]).unwrap()
            };
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
    let init = json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}}}})
    .to_string();
    let list = format!(
        "event: message\ndata: {}\n\n",
        json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"susi.status","description":"Status","inputSchema":{"type":"object"}}]}})
    );
    let call =
        json!({"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"healthy"}]}})
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
    let transport = HttpTransport::new(&url).with_timeout_secs(10);
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
        assert_eq!(request.header("authorization"), None);
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
fn http_socket_transport_closes_a_session_once_and_carries_session_headers() {
    let init =
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{}}})
            .to_string();
    let (url, server) = serve(vec![
        reply(
            "200 OK",
            "Mcp-Session-Id: close-42\r\n",
            "application/json",
            &init,
        ),
        reply("202 Accepted", "", "application/json", ""),
        reply("204 No Content", "", "application/json", ""),
    ]);
    let mut client = McpClient::new(HttpTransport::new(&url).with_timeout_secs(10));
    client.initialize().unwrap();
    client.close().unwrap();
    client.close().unwrap();
    assert!(client.initialize().is_err());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    let delete = &requests[2];
    assert_eq!(delete.request_line, "DELETE /mcp HTTP/1.1");
    assert_eq!(delete.body, Value::Null);
    assert_eq!(delete.header("mcp-session-id"), Some("close-42"));
    assert_eq!(delete.header("mcp-protocol-version"), Some("2025-11-25"));
    assert_eq!(delete.header("authorization"), None);
}
#[test]
fn dropping_an_initialized_http_client_releases_its_session() {
    let init =
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{}}})
            .to_string();
    let (url, server) = serve(vec![
        reply(
            "200 OK",
            "Mcp-Session-Id: drop-42\r\n",
            "application/json",
            &init,
        ),
        reply("202 Accepted", "", "application/json", ""),
        reply("204 No Content", "", "application/json", ""),
    ]);
    {
        let mut client = McpClient::new(HttpTransport::new(&url).with_timeout_secs(10));
        client.initialize().unwrap();
    }
    let requests = server.join().unwrap();
    assert_eq!(requests[2].request_line, "DELETE /mcp HTTP/1.1");
    assert_eq!(requests[2].header("mcp-session-id"), Some("drop-42"));
}
#[test]
fn http_socket_transport_surfaces_http_errors_and_unreachable_sockets() {
    let (url, server) = serve(vec![reply(
        "500 Internal Server Error",
        "",
        "text/plain",
        "boom",
    )]);
    let mut client = McpClient::new(HttpTransport::new(&url).with_timeout_secs(10));
    let error = client.initialize().unwrap_err().to_string();
    assert!(
        error.contains("HTTP 500") && error.contains("boom"),
        "{error}"
    );
    server.join().unwrap();
    let mut closed = McpClient::new(HttpTransport::new("http://127.0.0.1:1").with_timeout_secs(2));
    assert!(closed.initialize().is_err());
}

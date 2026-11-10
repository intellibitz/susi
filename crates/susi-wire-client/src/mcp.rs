//! Blocking MCP Streamable HTTP client for the local daemon's socket,
//! built on [`crate::http`] and [`crate::sse`].
//!
//! ```text
//! POST /mcp  initialize                  -> Mcp-Session-Id header + protocolVersion
//! POST /mcp  notifications/initialized   (session + protocol headers) -> 202
//! POST /mcp  tools/call ...              -> application/json or text/event-stream
//! DELETE /mcp                            (session headers) -> session released
//! ```
//!
//! It mirrors the behaviour of `susi_core::mcp_client` (same protocol
//! revisions, same response matching) without its peer features: requests
//! are never signed and bodies are never sealed, so it is only for a
//! loopback daemon that accepts the host bearer.

use crate::http::{self, Request};
use serde_json::{json, Value};
use std::fmt;
use std::io::{self, BufReader, Read};
use std::time::{Duration, Instant};

/// Protocol revision this client requests. A copy of
/// `susi_core::mcp_client::PROTOCOL_VERSION` (susi-core is not a
/// dependency); the susi-gmcp parity test keeps the two in step.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// Revisions whose `initialize` and `tools/call` shapes this client speaks.
const SUPPORTED_VERSIONS: [&str; 3] = [PROTOCOL_VERSION, "2025-06-18", "2025-03-26"];

/// Cap on one response body.
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Receive window for the handshake and the closing `DELETE`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Why a call failed.
#[derive(Debug)]
pub enum WireError {
    /// Connect, send or read failure.
    Io(String),
    /// A non-2xx status, with an excerpt of the body.
    Http(u16, String),
    /// The server answered with a JSON-RPC error.
    Rpc(i64, String),
    /// The reply did not follow the protocol.
    Protocol(String),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Http(status, body) if body.is_empty() => write!(f, "HTTP {status}"),
            Self::Http(status, body) => write!(f, "HTTP {status}: {body}"),
            Self::Rpc(code, message) => write!(f, "rpc error {code}: {message}"),
            Self::Protocol(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// An initialized MCP session. Dropping it sends a best-effort `DELETE`;
/// [`Session::close`] does the same and reports the outcome.
#[derive(Debug)]
pub struct Session {
    host: String,
    port: u16,
    bearer: Option<String>,
    id: String,
    version: String,
    timeout: Duration,
    next_id: u64,
    open: bool,
}

impl Session {
    /// Connect to `addr` (`host:port`), send `initialize` and
    /// `notifications/initialized`, and keep the session id and the
    /// negotiated protocol revision. `timeout` bounds each request. An
    /// address that resolves to anything but loopback is refused before a
    /// byte is sent: the bearer travels in plaintext.
    pub fn open(addr: &str, bearer: Option<&str>, timeout: Duration) -> Result<Self, WireError> {
        let (host, port) = split_addr(addr)?;
        ensure_loopback(&host, port)?;
        let mut session = Self {
            host,
            port,
            bearer: bearer.map(str::to_string),
            id: String::new(),
            version: String::new(),
            timeout,
            next_id: 1,
            open: false,
        };
        let init = session.post(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "susi", "version": env!("CARGO_PKG_VERSION")}
            }
        }))?;
        session.next_id = 2;
        session.id = init
            .header("mcp-session-id")
            .map(str::to_string)
            .ok_or_else(|| WireError::Protocol(format!("{addr} returned no Mcp-Session-Id")))?;
        // From here on the server holds a session: drop releases it.
        session.open = true;
        let result = read_result(init, 1, &mut |_| {})?;
        session.version = negotiated_version(&result)?;
        // Only the status matters (GMCP answers 202 with no body); the
        // reply is dropped unread, as the peer client does.
        session.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(session)
    }

    /// The protocol revision the server chose.
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.version
    }

    /// Change the receive window for later requests.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Call `name` with `args` and return the raw `tools/call` result.
    /// With a `progress_token` the request asks for progress, and every
    /// `notifications/progress` or `notifications/message` frame that
    /// arrives before the result is passed to `on_notification`.
    pub fn call_tool(
        &mut self,
        name: &str,
        args: &Value,
        progress_token: Option<Value>,
        on_notification: &mut dyn FnMut(&Value),
    ) -> Result<Value, WireError> {
        let mut params = json!({"name": name, "arguments": args});
        if let Some(token) = progress_token {
            params["_meta"] = json!({"progressToken": token});
        }
        self.request("tools/call", &params, on_notification)
    }

    /// Any session-scoped JSON-RPC request (`tools/list`, `ping`, ...);
    /// returns its `result`.
    pub fn request(
        &mut self,
        method: &str,
        params: &Value,
        on_notification: &mut dyn FnMut(&Value),
    ) -> Result<Value, WireError> {
        let id = self.next_id;
        self.next_id += 1;
        let response = self.post(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        read_result(response, id, on_notification)
    }

    /// Release the session with `DELETE`. A 405 (the server does not let
    /// clients end sessions) counts as success: the server reclaims it.
    pub fn close(mut self) -> Result<(), WireError> {
        self.delete()
    }

    fn delete(&mut self) -> Result<(), WireError> {
        if !std::mem::take(&mut self.open) {
            return Ok(());
        }
        let headers = self.headers(false);
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let budget = self.timeout.min(DEFAULT_TIMEOUT);
        let mut response = http::request_streaming_by(
            &self.host,
            self.port,
            Request {
                method: "DELETE",
                path: "/mcp",
                headers: &refs,
                body: None,
            },
            budget,
            Some(Instant::now() + budget),
        )
        .map_err(WireError::Io)?;
        // Only the status matters: the body is never read whole, at most a
        // short excerpt for the error.
        if (200..300).contains(&response.status) || response.status == 405 {
            Ok(())
        } else {
            let mut head = Vec::new();
            let _ = (&mut response.body).take(512).read_to_end(&mut head);
            Err(WireError::Http(response.status, excerpt(&head)))
        }
    }

    fn headers(&self, with_body: bool) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        if with_body {
            headers.push((
                "Accept".to_string(),
                "application/json, text/event-stream".to_string(),
            ));
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
        }
        if let Some(token) = &self.bearer {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        if !self.id.is_empty() {
            headers.push(("Mcp-Session-Id".to_string(), self.id.clone()));
        }
        if !self.version.is_empty() {
            headers.push(("MCP-Protocol-Version".to_string(), self.version.clone()));
        }
        headers
    }

    /// POST one JSON-RPC message; a non-2xx status is an error. The
    /// timeout is the request's total budget, not only a per-read window.
    fn post(&self, message: &Value) -> Result<http::StreamingResponse, WireError> {
        let body = serde_json::to_vec(message).map_err(|e| WireError::Protocol(e.to_string()))?;
        let headers = self.headers(true);
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let mut response = http::request_streaming_by(
            &self.host,
            self.port,
            Request {
                method: "POST",
                path: "/mcp",
                headers: &refs,
                body: Some(&body),
            },
            self.timeout,
            Some(Instant::now() + self.timeout),
        )
        .map_err(WireError::Io)?;
        if !(200..300).contains(&response.status) {
            let mut head = Vec::new();
            let _ = (&mut response.body).take(512).read_to_end(&mut head);
            return Err(WireError::Http(response.status, excerpt(&head)));
        }
        Ok(response)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.delete();
    }
}

/// One `tools/call` through a fresh session, released afterwards: the
/// counterpart of `susi_core::mcp_client::call_tool_with_timeout`.
/// `timeout_secs` is the receive window for the call itself; the handshake
/// and the release use the shorter of it and 20 seconds.
pub fn call_tool(
    addr: &str,
    name: &str,
    args: &Value,
    bearer: Option<&str>,
    timeout_secs: u64,
) -> Result<Value, WireError> {
    let call_timeout = Duration::from_secs(timeout_secs.max(1));
    let handshake = call_timeout.min(DEFAULT_TIMEOUT);
    let mut session = Session::open(addr, bearer, handshake)?;
    session.set_timeout(call_timeout);
    let result = session.call_tool(name, args, None, &mut |_| {});
    session.set_timeout(handshake);
    let _ = session.close();
    result
}

/// Split `host:port`, accepting a bracketed IPv6 host.
fn split_addr(addr: &str) -> Result<(String, u16), WireError> {
    let bad = || WireError::Protocol(format!("address {addr:?} is not host:port"));
    let (host, port) = addr.rsplit_once(':').ok_or_else(bad)?;
    let port = port.parse().map_err(|_| bad())?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Err(bad());
    }
    Ok((host.to_string(), port))
}

/// Refuse a host that resolves to no address or to any non-loopback one.
fn ensure_loopback(host: &str, port: u16) -> Result<(), WireError> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| WireError::Io(format!("resolve {host}:{port}: {e}")))?
        .collect();
    if addrs.is_empty() || !addrs.iter().all(|a| a.ip().is_loopback()) {
        return Err(WireError::Protocol(format!(
            "{host}:{port} is not a loopback address; this client only talks to the local daemon"
        )));
    }
    Ok(())
}

/// The server's chosen revision from an `initialize` result, refused when
/// this client does not speak it.
fn negotiated_version(result: &Value) -> Result<String, WireError> {
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            WireError::Protocol("initialize result carries no protocolVersion".into())
        })?;
    if SUPPORTED_VERSIONS.contains(&version) {
        Ok(version.to_string())
    } else {
        Err(WireError::Protocol(format!(
            "unsupported MCP protocol version {version}"
        )))
    }
}

/// What one JSON-RPC message means for the request `id`.
enum Frame {
    /// The response to `id`: its result or its error.
    Answer(Result<Value, WireError>),
    /// A progress or log notification for the caller.
    Notification,
    /// Anything else: other ids, server requests, other notifications.
    Other,
}

fn classify(message: &Value, id: u64) -> Frame {
    let method = message.get("method").and_then(Value::as_str);
    if let Some(method) = method {
        let notification = message.get("id").is_none()
            && matches!(method, "notifications/progress" | "notifications/message");
        return if notification {
            Frame::Notification
        } else {
            Frame::Other
        };
    }
    if message.get("id").and_then(Value::as_u64) != Some(id) {
        return Frame::Other;
    }
    if let Some(error) = message.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| error.to_string(), str::to_string);
        return Frame::Answer(Err(WireError::Rpc(code, text)));
    }
    Frame::Answer(
        message
            .get("result")
            .cloned()
            .ok_or_else(|| WireError::Protocol("response carries neither result nor error".into())),
    )
}

/// The result for `id` from a reply that is either one JSON message or an
/// event stream. An event stream is read as it arrives, so notifications
/// reach `on_notification` before the result does. Without a
/// `text/event-stream` content type the body is read whole and sniffed:
/// JSON first, then SSE.
fn read_result<R: io::BufRead>(
    response: http::StreamingResponse<R>,
    id: u64,
    on_notification: &mut dyn FnMut(&Value),
) -> Result<Value, WireError> {
    let streaming = response
        .header("content-type")
        .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("text/event-stream"));
    if streaming {
        // An event stream may run long (progress on a long mission): each
        // event is capped by the SSE reader instead of the whole stream.
        return sse_result(BufReader::new(response.body), id, on_notification);
    }
    let mut bytes = Vec::new();
    BufReader::new(response.body.take(MAX_RESPONSE_BYTES + 1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(WireError::Protocol(format!(
            "response exceeds {MAX_RESPONSE_BYTES} bytes"
        )));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(message) => match classify(&message, id) {
            Frame::Answer(result) => result,
            Frame::Notification | Frame::Other => Err(WireError::Protocol(
                "JSON response does not answer the request id".into(),
            )),
        },
        Err(_) => sse_result(&bytes[..], id, on_notification),
    }
}

/// Walk an event stream until the response for `id`. Events whose data is
/// not JSON (the empty priming event) are skipped.
fn sse_result<R: io::BufRead>(
    reader: R,
    id: u64,
    on_notification: &mut dyn FnMut(&Value),
) -> Result<Value, WireError> {
    for event in crate::sse::SseReader::new(reader) {
        let event = event?;
        let Ok(message) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match classify(&message, id) {
            Frame::Answer(result) => return result,
            Frame::Notification => on_notification(&message),
            Frame::Other => {}
        }
    }
    Err(WireError::Protocol(
        "no matching response frame in SSE stream".into(),
    ))
}

fn excerpt(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    match text.char_indices().nth(200) {
        Some((cut, _)) => format!("{}...", &text[..cut]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
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
        let read = |raw: &'static [u8]| {
            read_result(http::read_response(raw, "POST").unwrap(), 2, &mut |_| {})
        };
        let json_reply =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}";
        assert_eq!(read(json_reply).unwrap(), json!({"tools": []}));
        let other_id =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{}}";
        assert!(read(other_id).is_err());
        // No event-stream content type, but the body is SSE: sniffed.
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
        // Refused before any connection: nothing listens on TEST-NET-1.
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
}

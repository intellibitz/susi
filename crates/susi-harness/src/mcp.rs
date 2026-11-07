//! The MCP client: tools that live outside the binary.
//!
//! MCP is the tool protocol of this harness, the way OpenAI-compatible chat is
//! the model protocol. A client speaks JSON-RPC 2.0 over a transport; the
//! transport is a trait so the protocol is testable without a process, and
//! [`StdioTransport`] is the production one for a child server.
//!
//! The client does the minimum a tool caller needs: `initialize`, `tools/list`
//! and `tools/call`, with request ids and error surfacing.

use crate::tools::{Policy, ToolSpec};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use susi_error::{EaiError, EaiResult};

/// The protocol revision this client offers in `initialize`.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Protocol revisions this client recognises in a server's `initialize`
/// answer, newest first.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2026-07-28", "2025-11-25", "2024-11-05"];

/// Whether `version` is one of [`SUPPORTED_PROTOCOL_VERSIONS`].
#[must_use]
pub fn is_supported_protocol_version(version: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
}

/// Default GMCP streamable-HTTP path on the Susi OS Socket.
pub const GMCP_HTTP_PATH: &str = "/mcp";

/// Largest response body [`HttpSocketTransport`] reads.
pub const MAX_HTTP_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

/// One tool an MCP server advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTool {
    /// Name to call.
    pub name: String,
    /// Description to show the model.
    pub description: String,
    /// JSON schema of the arguments.
    pub schema: Value,
}

impl McpTool {
    /// Turn the server's advertisement into a registry entry.
    #[must_use]
    pub fn to_spec(&self, policy: Policy) -> ToolSpec {
        ToolSpec::mcp(&self.name, &self.description, self.schema.clone(), policy)
    }

    /// Verify structural invariants of an advertised MCP tool.
    ///
    /// # Errors
    ///
    /// Returns [`EaiError::Config`] when the tool name is blank, the input
    /// schema is not a JSON object, or the schema's `"type"` field is present
    /// and not `"object"`.
    pub fn verify_tool_invariants(&self) -> EaiResult<()> {
        if self.name.trim().is_empty() {
            return Err(EaiError::config("mcp tool name must not be empty"));
        }
        if !self.schema.is_object() {
            return Err(EaiError::config(format!(
                "mcp tool '{}' schema must be a JSON object",
                self.name
            )));
        }
        if let Some(schema_type) = self.schema.get("type") {
            if !matches!(schema_type, Value::String(t) if t == "object") {
                return Err(EaiError::config(format!(
                    "mcp tool '{}' schema type must be \"object\"",
                    self.name
                )));
            }
        }
        Ok(())
    }
}

/// A JSON-RPC channel to one MCP server.
pub trait Transport: Send {
    /// Send one request and read its response.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the server cannot be reached, the stream
    /// closes, or the response is not JSON.
    fn exchange(&mut self, request: &Value) -> EaiResult<Value>;

    /// Send a notification (no id, no response).
    ///
    /// # Errors
    ///
    /// Returns a transport error when the notification cannot be written.
    fn notify(&mut self, _notification: &Value) -> EaiResult<()> {
        Ok(())
    }

    /// Learn the protocol revision the server chose, so a transport that
    /// carries it on the wire (HTTP's `MCP-Protocol-Version`) can send it.
    fn set_protocol_version(&mut self, _version: &str) {}
}

/// Upper bound on `tools/list` pages one listing follows.
pub const MAX_LIST_PAGES: usize = 50;

/// A client for one MCP server.
#[derive(Debug)]
pub struct McpClient<T: Transport> {
    transport: T,
    next_id: u64,
    initialized: bool,
    negotiated_version: Option<String>,
}

impl<T: Transport> McpClient<T> {
    /// A client over an already-connected transport.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 0,
            initialized: false,
            negotiated_version: None,
        }
    }

    /// The protocol revision the server answered `initialize` with, once the
    /// handshake has run.
    #[must_use]
    pub fn negotiated_protocol_version(&self) -> Option<&str> {
        self.negotiated_version.as_deref()
    }

    /// Whether the server chose a revision in [`SUPPORTED_PROTOCOL_VERSIONS`].
    ///
    /// A server answering with an unlisted revision is still used — the
    /// three calls this client makes are stable across revisions — but a
    /// caller that needs a newer feature can refuse it here.
    #[must_use]
    pub fn negotiated_supported(&self) -> bool {
        self.negotiated_version
            .as_deref()
            .is_some_and(is_supported_protocol_version)
    }

    /// The underlying transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Perform the MCP handshake once.
    ///
    /// # Errors
    ///
    /// Returns the server's error or a transport error.
    pub fn initialize(&mut self) -> EaiResult<Value> {
        if self.initialized {
            return Ok(json!({}));
        }
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "susi-harness", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        if let Some(version) = result.get("protocolVersion").and_then(Value::as_str) {
            self.transport.set_protocol_version(version);
            self.negotiated_version = Some(version.to_string());
        }
        self.transport
            .notify(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        self.initialized = true;
        Ok(result)
    }

    /// The tools the server advertises, across every `tools/list` page.
    ///
    /// The listing follows `nextCursor` until the server omits it, bounded by
    /// [`MAX_LIST_PAGES`] so a misbehaving server cannot loop the client.
    ///
    /// # Errors
    ///
    /// Returns an error when the handshake or any page of the listing fails,
    /// when a page carries a non-string cursor, or when the server keeps
    /// paging past the bound.
    pub fn list_tools(&mut self) -> EaiResult<Vec<McpTool>> {
        self.initialize()?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
            let result = self.request("tools/list", params)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| EaiError::internal("mcp tools/list returned no tools array"))?;
            tools.extend(page.iter().cloned());
            cursor = match result.get("nextCursor") {
                None | Some(Value::Null) => return Ok(Self::parse_tools(&tools)),
                Some(Value::String(next)) => Some(next.clone()),
                Some(_) => {
                    return Err(EaiError::internal(
                        "mcp tools/list returned a non-string nextCursor",
                    ))
                }
            };
        }
        Err(EaiError::internal(format!(
            "mcp tools/list still paging after {MAX_LIST_PAGES} pages"
        )))
    }

    fn parse_tools(tools: &[Value]) -> Vec<McpTool> {
        tools
            .iter()
            .filter_map(|tool| {
                let name = tool.get("name").and_then(Value::as_str)?;
                Some(McpTool {
                    name: name.to_string(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                })
            })
            .collect()
    }

    /// Call one tool and return its text content.
    ///
    /// Non-text content is returned as its JSON form so a caller never gets an
    /// empty answer for a tool that answered.
    ///
    /// # Errors
    ///
    /// Returns the server's error or a transport error.
    pub fn call(&mut self, name: &str, arguments: &Value) -> EaiResult<String> {
        self.initialize()?;
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        let mut text = String::new();
        if let Some(blocks) = result.get("content").and_then(Value::as_array) {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(chunk);
                        }
                    }
                    Some(_) => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&block.to_string());
                    }
                    None => {}
                }
            }
        }
        if text.is_empty() {
            Ok(result.to_string())
        } else {
            Ok(text)
        }
    }

    fn request(&mut self, method: &str, params: Value) -> EaiResult<Value> {
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self.transport.exchange(&request)?;
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(EaiError::internal(format!("mcp {method}: {message}")));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| EaiError::internal(format!("mcp {method}: response has no result")))
    }
}

/// A JSON-RPC transport over a child process's stdio.
#[derive(Debug)]
pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioTransport {
    /// Spawn an MCP server and connect to it.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the process cannot be spawned or its pipes
    /// cannot be taken.
    pub fn spawn(program: &str, args: &[String]) -> EaiResult<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| EaiError::io(format!("mcp spawn {program}: {error}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| EaiError::internal("mcp: child has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EaiError::internal("mcp: child has no stdout"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

impl Transport for StdioTransport {
    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        let line = serde_json::to_string(request)
            .map_err(|error| EaiError::internal(format!("mcp request: {error}")))?;
        writeln!(self.stdin, "{line}")
            .map_err(|error| EaiError::io(format!("mcp write: {error}")))?;
        self.stdin
            .flush()
            .map_err(|error| EaiError::io(format!("mcp flush: {error}")))?;
        let mut response = String::new();
        self.stdout
            .read_line(&mut response)
            .map_err(|error| EaiError::io(format!("mcp read: {error}")))?;
        if response.trim().is_empty() {
            return Err(EaiError::io("mcp server closed the stream"));
        }
        serde_json::from_str(&response)
            .map_err(|error| EaiError::internal(format!("mcp response: {error}")))
    }

    fn notify(&mut self, notification: &Value) -> EaiResult<()> {
        let line = serde_json::to_string(notification)
            .map_err(|error| EaiError::internal(format!("mcp notification: {error}")))?;
        writeln!(self.stdin, "{line}")
            .map_err(|error| EaiError::io(format!("mcp write: {error}")))?;
        self.stdin
            .flush()
            .map_err(|error| EaiError::io(format!("mcp flush: {error}")))
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A JSON-RPC transport over MCP streamable HTTP, for the GMCP socket of the
/// Susi AI OS (`http://127.0.0.1:9090/mcp` by default).
///
/// Each message is one `POST`. The `Mcp-Session-Id` the server returns on
/// `initialize` is echoed on every later message, and once the client has
/// negotiated a revision it is sent as `MCP-Protocol-Version`. A response may
/// be plain JSON or a `text/event-stream`; the JSON-RPC answer is taken from
/// the stream's `data:` lines.
#[derive(Debug, Clone)]
pub struct HttpSocketTransport {
    url: String,
    timeout_secs: u64,
    bearer: Option<String>,
    session_id: Option<String>,
    protocol_version: Option<String>,
}

impl HttpSocketTransport {
    /// A transport to `url`. A URL with no path (`http://127.0.0.1:9090`) gets
    /// the GMCP path [`GMCP_HTTP_PATH`] appended; any explicit path is kept.
    #[must_use]
    pub fn new(url: &str) -> Self {
        Self {
            url: endpoint_url(url),
            timeout_secs: 60,
            bearer: None,
            session_id: None,
            protocol_version: None,
        }
    }

    /// The transport for the Susi OS Socket's GMCP endpoint.
    #[must_use]
    pub fn for_socket(endpoints: &crate::adaptive::SusiSocketEndpoints) -> Self {
        Self::new(&endpoints.gmcp_http_url)
    }

    /// Set the per-message deadline (at least one second).
    #[must_use]
    pub fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs.max(1);
        self
    }

    /// Send `Authorization: Bearer <token>` on every message.
    #[must_use]
    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }

    /// The endpoint every message is posted to.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The session the server assigned, once it has assigned one.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Verify structural invariants of this HTTP socket transport.
    ///
    /// # Errors
    ///
    /// Returns [`EaiError::Config`] when the URL scheme, host, port, or path
    /// is invalid, the timeout is zero, or a negotiated protocol version is not
    /// in [`SUPPORTED_PROTOCOL_VERSIONS`].
    pub fn verify_transport_invariants(&self) -> EaiResult<()> {
        let (after_scheme, default_port) = if let Some(rest) = self.url.strip_prefix("http://") {
            (rest, 80u16)
        } else if let Some(rest) = self.url.strip_prefix("https://") {
            (rest, 443u16)
        } else {
            return Err(EaiError::config(format!(
                "mcp http transport url '{}' must start with http:// or https://",
                self.url
            )));
        };

        let (authority, path) = match after_scheme.find('/') {
            Some(idx) => (&after_scheme[..idx], &after_scheme[idx..]),
            None => (after_scheme, ""),
        };

        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => {
                let parsed = p.parse::<u16>().map_err(|_| {
                    EaiError::config(format!(
                        "mcp http transport url '{}' has invalid port '{p}'",
                        self.url
                    ))
                })?;
                (h, parsed)
            }
            None => (authority, default_port),
        };

        if host.trim().is_empty() {
            return Err(EaiError::config(format!(
                "mcp http transport url '{}' must have a non-empty host",
                self.url
            )));
        }
        if port == 0 {
            return Err(EaiError::config(format!(
                "mcp http transport url '{}' must have a port > 0",
                self.url
            )));
        }
        if !path.starts_with('/') {
            return Err(EaiError::config(format!(
                "mcp http transport url '{}' path must start with '/'",
                self.url
            )));
        }
        if self.timeout_secs == 0 {
            return Err(EaiError::config(
                "mcp http transport timeout_secs must be greater than zero",
            ));
        }
        if let Some(ref version) = self.protocol_version {
            if !is_supported_protocol_version(version) {
                return Err(EaiError::config(format!(
                    "mcp http transport protocol_version '{version}' is not supported"
                )));
            }
        }
        Ok(())
    }

    fn post(&mut self, message: &Value) -> EaiResult<susi_http_transport::HttpCall> {
        let body = serde_json::to_vec(message)
            .map_err(|error| EaiError::internal(format!("mcp http request: {error}")))?;
        let authorization = self.bearer.as_ref().map(|token| format!("Bearer {token}"));
        let mut headers: Vec<(&str, &str)> = vec![
            ("content-type", "application/json"),
            ("accept", "application/json, text/event-stream"),
        ];
        if let Some(session) = self.session_id.as_deref() {
            headers.push(("mcp-session-id", session));
        }
        if let Some(version) = self.protocol_version.as_deref() {
            headers.push(("mcp-protocol-version", version));
        }
        if let Some(authorization) = authorization.as_deref() {
            headers.push(("authorization", authorization));
        }
        let call = susi_http_transport::http_call_with_body(
            "POST",
            &self.url,
            &headers,
            Some(&body),
            self.timeout_secs,
            0,
        )
        .map_err(|error| EaiError::network(format!("mcp http {}: {error}", self.url)))?;
        if let Some(session) = call.header("mcp-session-id") {
            self.session_id = Some(session.to_string());
        }
        Ok(call)
    }
}

impl Transport for HttpSocketTransport {
    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        let call = self.post(request)?;
        let status = call.status;
        let event_stream = call
            .header("content-type")
            .is_some_and(|kind| kind.to_ascii_lowercase().contains("text/event-stream"));
        let text = call
            .into_utf8(MAX_HTTP_RESPONSE_BYTES)
            .map_err(|error| EaiError::network(format!("mcp http body: {error}")))?;
        if !(200..300).contains(&status) {
            return Err(EaiError::network(format!(
                "mcp http {} returned HTTP {status}: {}",
                self.url,
                text.chars().take(500).collect::<String>()
            )));
        }
        if event_stream {
            return response_from_event_stream(&text, request.get("id"));
        }
        if text.trim().is_empty() {
            return Err(EaiError::network("mcp http response has no body"));
        }
        serde_json::from_str(&text)
            .map_err(|error| EaiError::internal(format!("mcp http response: {error}")))
    }

    fn notify(&mut self, notification: &Value) -> EaiResult<()> {
        let call = self.post(notification)?;
        if (200..300).contains(&call.status) {
            Ok(())
        } else {
            Err(EaiError::network(format!(
                "mcp http {} refused a notification with HTTP {}",
                self.url, call.status
            )))
        }
    }

    fn set_protocol_version(&mut self, version: &str) {
        self.protocol_version = Some(version.to_string());
    }
}

/// `url` with the GMCP path appended when it names only a host and port.
fn endpoint_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let after_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
    if after_scheme.contains('/') {
        trimmed.to_string()
    } else {
        format!("{trimmed}{GMCP_HTTP_PATH}")
    }
}

/// The JSON-RPC response in a server-sent event stream: the event whose `id`
/// matches the request's, else the last JSON message carrying a result or an
/// error.
fn response_from_event_stream(text: &str, id: Option<&Value>) -> EaiResult<Value> {
    let mut events: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if !current.is_empty() {
                events.push(std::mem::take(&mut current));
            }
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    if !current.is_empty() {
        events.push(current);
    }
    let answers: Vec<Value> = events
        .iter()
        .filter_map(|event| serde_json::from_str::<Value>(event).ok())
        .filter(|message| message.get("result").is_some() || message.get("error").is_some())
        .collect();
    let matching = id.and_then(|id| {
        answers
            .iter()
            .find(|message| message.get("id") == Some(id))
            .cloned()
    });
    matching
        .or_else(|| answers.last().cloned())
        .ok_or_else(|| EaiError::network("mcp event stream carried no JSON-RPC response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

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
        json!({"result": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}}})
    }

    #[test]
    fn mcp_lists_tools_after_initialize() {
        let transport = Scripted::new(vec![
            initialized(),
            json!({"result": {"tools": [
                {"name": "search", "description": "Search", "inputSchema": {"type": "object"}},
                {"name": "fetch", "description": "Fetch", "inputSchema": {"type": "object"}}
            ]}}),
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
            json!({"result": {"tools": [{"name": "a"}], "nextCursor": "rev:1"}}),
            json!({"result": {"tools": [{"name": "b"}], "nextCursor": "rev:2"}}),
            json!({"result": {"tools": [{"name": "c"}]}}),
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
            json!({"result": {"tools": [{"name": "a"}], "nextCursor": "rev:1"}}),
            json!({"error": {"code": -32602, "message": "Invalid cursor"}}),
        ]);
        let mut client = McpClient::new(transport);
        let error = client.list_tools().unwrap_err().to_string();
        assert!(error.contains("Invalid cursor"), "{error}");

        let mut endless = vec![initialized()];
        endless.extend(
            (0..=MAX_LIST_PAGES)
                .map(|n| json!({"result": {"tools": [], "nextCursor": format!("rev:{n}")}})),
        );
        let mut client = McpClient::new(Scripted::new(endless));
        let error = client.list_tools().unwrap_err().to_string();
        assert!(error.contains("still paging"), "{error}");
    }

    #[test]
    fn mcp_calls_a_tool_and_joins_text_blocks() {
        let transport = Scripted::new(vec![
            initialized(),
            json!({"result": {"content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ]}}),
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
    fn mcp_surfaces_a_json_rpc_error() {
        let transport = Scripted::new(vec![
            initialized(),
            json!({"error": {"code": -32601, "message": "method not found"}}),
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
            json!({"result": {"protocolVersion": "2025-11-25", "capabilities": {}}}),
            json!({"result": {"tools": []}}),
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
            json!({"result": {"protocolVersion": "2025-06-18"}}),
            json!({"result": {"tools": []}}),
        ]));
        legacy.list_tools().unwrap();
        assert_eq!(legacy.negotiated_protocol_version(), Some("2025-06-18"));
        assert!(!legacy.negotiated_supported());
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

    #[test]
    fn mcp_tool_and_http_socket_transport_verify_invariants_reject_corrupt_state() {
        let valid_tool = McpTool {
            name: "susi.status".to_string(),
            description: "Inspect Susi status".to_string(),
            schema: json!({"type": "object", "properties": {}}),
        };
        assert!(valid_tool.verify_tool_invariants().is_ok());

        let untyped_object_tool = McpTool {
            name: "susi.ping".to_string(),
            description: "Ping".to_string(),
            schema: json!({"properties": {}}),
        };
        assert!(untyped_object_tool.verify_tool_invariants().is_ok());

        let blank_name = McpTool {
            name: "   ".to_string(),
            ..valid_tool.clone()
        };
        assert!(blank_name.verify_tool_invariants().is_err());

        let non_object_schema = McpTool {
            schema: json!(["not", "an", "object"]),
            ..valid_tool.clone()
        };
        assert!(non_object_schema.verify_tool_invariants().is_err());

        let wrong_schema_type = McpTool {
            schema: json!({"type": "array"}),
            ..valid_tool.clone()
        };
        assert!(wrong_schema_type.verify_tool_invariants().is_err());

        let non_string_schema_type = McpTool {
            schema: json!({"type": 123}),
            ..valid_tool
        };
        assert!(non_string_schema_type.verify_tool_invariants().is_err());

        let mut valid_transport =
            HttpSocketTransport::new("http://127.0.0.1:9090").with_timeout_secs(15);
        assert!(valid_transport.verify_transport_invariants().is_ok());
        valid_transport.set_protocol_version("2025-11-25");
        assert!(valid_transport.verify_transport_invariants().is_ok());

        let mut empty_host = valid_transport.clone();
        empty_host.url = "http://:9090/mcp".to_string();
        assert!(empty_host.verify_transport_invariants().is_err());

        let mut zero_port = valid_transport.clone();
        zero_port.url = "http://127.0.0.1:0/mcp".to_string();
        assert!(zero_port.verify_transport_invariants().is_err());

        let mut missing_path = valid_transport.clone();
        missing_path.url = "http://127.0.0.1:9090".to_string();
        assert!(missing_path.verify_transport_invariants().is_err());

        let mut zero_timeout = valid_transport.clone();
        zero_timeout.timeout_secs = 0;
        assert!(zero_timeout.verify_transport_invariants().is_err());

        let mut bad_protocol = valid_transport;
        bad_protocol.protocol_version = Some("1999-01-01".to_string());
        assert!(bad_protocol.verify_transport_invariants().is_err());
    }
}

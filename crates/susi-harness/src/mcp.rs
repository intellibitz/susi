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
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use susi_error::{EaiError, EaiResult};

/// The protocol revision this client offers in `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

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

/// Default deadline for one MCP stdio response.
pub const DEFAULT_STDIO_TIMEOUT_SECS: u64 = 60;

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

    /// Release any session owned by this transport.
    ///
    /// # Errors
    ///
    /// Returns a transport error when an owned session cannot be released.
    fn close(&mut self) -> EaiResult<()> {
        Ok(())
    }
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
    closed: bool,
}

impl<T: Transport> McpClient<T> {
    /// A client over an already-connected transport.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 0,
            initialized: false,
            negotiated_version: None,
            closed: false,
        }
    }

    /// Release the underlying MCP session. The operation is idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error when the transport cannot release its session.
    pub fn close(&mut self) -> EaiResult<()> {
        if self.closed {
            return Ok(());
        }
        self.transport.close()?;
        self.closed = true;
        self.initialized = false;
        self.negotiated_version = None;
        Ok(())
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
        if self.closed {
            return Err(EaiError::internal("mcp client is closed"));
        }
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
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .filter(|version| !version.is_empty())
            .ok_or_else(|| {
                EaiError::internal("mcp initialize: response has no non-empty protocolVersion")
            })?;
        self.transport.set_protocol_version(version);
        self.negotiated_version = Some(version.to_string());
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
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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
            text = result.to_string();
        }
        if is_error {
            Err(EaiError::internal(format!(
                "mcp tools/call '{name}' failed: {text}"
            )))
        } else {
            Ok(text)
        }
    }

    fn request(&mut self, method: &str, params: Value) -> EaiResult<Value> {
        if self.closed {
            return Err(EaiError::internal("mcp client is closed"));
        }
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self.transport.exchange(&request)?;
        if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(EaiError::internal(format!(
                "mcp {method}: response has invalid jsonrpc version"
            )));
        }
        if response.get("id") != Some(&Value::from(id)) {
            return Err(EaiError::internal(format!(
                "mcp {method}: response id does not match request id {id}"
            )));
        }
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

impl<T: Transport> Drop for McpClient<T> {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.transport.close();
        }
    }
}

/// Read the next response from an MCP stdio stream.
///
/// Servers may send JSON-RPC notifications (for example progress or log
/// events) between a request and its response. Notifications have a method
/// but no id and never carry a result or error, so they are not candidates for
/// the request's response. A message with an id, malformed JSON, or a
/// response-shaped message without an id is returned to the client layer,
/// which preserves its normal correlation checks.
fn read_stdio_response<R: BufRead>(reader: &mut R) -> EaiResult<Value> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| EaiError::io(format!("mcp read: {error}")))?;
        if read == 0 {
            return Err(EaiError::io("mcp server closed the stream"));
        }
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line)
            .map_err(|error| EaiError::internal(format!("mcp response: {error}")))?;
        let notification = message.get("id").is_none()
            && message.get("method").and_then(Value::as_str).is_some()
            && message.get("result").is_none()
            && message.get("error").is_none();
        if notification {
            continue;
        }
        return Ok(message);
    }
}

type StdioResponses = Receiver<EaiResult<Value>>;
type StdioReader = JoinHandle<()>;

fn start_stdio_reader(
    mut stdout: BufReader<ChildStdout>,
) -> EaiResult<(StdioResponses, StdioReader)> {
    let (sender, receiver) = mpsc::channel();
    let reader = thread::Builder::new()
        .name("susi-mcp-stdio-reader".to_string())
        .spawn(move || loop {
            let result = read_stdio_response(&mut stdout);
            let finished = result.is_err();
            if sender.send(result).is_err() || finished {
                break;
            }
        })
        .map_err(|error| EaiError::io(format!("mcp reader thread: {error}")))?;
    Ok((receiver, reader))
}

/// A JSON-RPC transport over a child process's stdio.
#[derive(Debug)]
pub struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<EaiResult<Value>>,
    reader: Option<JoinHandle<()>>,
    timeout: Duration,
}

impl StdioTransport {
    /// Spawn an MCP server and connect to it.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the process cannot be spawned or its pipes
    /// cannot be taken.
    pub fn spawn(program: &str, args: &[String]) -> EaiResult<Self> {
        let mut cmd = susi_paths::console::background_command(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in susi_config::cloud_env_overlay() {
            if std::env::var_os(&k).is_none() {
                cmd.env(k, v);
            }
        }
        cmd.env("SUSI_UNATTENDED", "1");
        susi_paths::console::apply_non_interactive_env(&mut cmd);
        let mut child = cmd
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
        let (responses, reader) = match start_stdio_reader(BufReader::new(stdout)) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            stdin,
            responses,
            reader: Some(reader),
            timeout: Duration::from_secs(DEFAULT_STDIO_TIMEOUT_SECS),
        })
    }

    /// Set the per-response deadline.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.max(Duration::from_millis(1));
        self
    }

    /// Set the per-response deadline in seconds, with a one-second minimum.
    #[must_use]
    pub fn with_timeout_secs(self, timeout_secs: u64) -> Self {
        self.with_timeout(Duration::from_secs(timeout_secs.max(1)))
    }

    fn stop_child(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Do not join the reader: a child can pass stdout to a grandchild,
        // which keeps the pipe open after the child itself is reaped.
        let _ = self.reader.take();
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
        match self.responses.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                let timeout_ms = self.timeout.as_millis();
                self.stop_child();
                Err(EaiError::io(format!(
                    "mcp response timed out after {timeout_ms} ms"
                )))
            }
            Err(RecvTimeoutError::Disconnected) => Err(EaiError::io("mcp stdio reader stopped")),
        }
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

    fn close(&mut self) -> EaiResult<()> {
        self.stop_child();
        Ok(())
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.stop_child();
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

    fn close(&mut self) -> EaiResult<()> {
        let Some(session) = self.session_id.take() else {
            return Ok(());
        };
        let authorization = self.bearer.as_ref().map(|token| format!("Bearer {token}"));
        let mut headers: Vec<(&str, &str)> = vec![("mcp-session-id", session.as_str())];
        if let Some(version) = self.protocol_version.as_deref() {
            headers.push(("mcp-protocol-version", version));
        }
        if let Some(authorization) = authorization.as_deref() {
            headers.push(("authorization", authorization));
        }
        let call = susi_http_transport::http_call_with_body(
            "DELETE",
            &self.url,
            &headers,
            None,
            self.timeout_secs,
            0,
        )
        .map_err(|error| EaiError::network(format!("mcp http {}: {error}", self.url)));
        match call {
            Ok(call) if (200..300).contains(&call.status) || call.status == 405 => {
                self.protocol_version = None;
                Ok(())
            }
            Ok(call) => {
                self.session_id = Some(session);
                Err(EaiError::network(format!(
                    "mcp http {} refused session close with HTTP {}",
                    self.url, call.status
                )))
            }
            Err(error) => {
                self.session_id = Some(session);
                Err(error)
            }
        }
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
#[path = "tests/mcp.rs"]
mod tests;

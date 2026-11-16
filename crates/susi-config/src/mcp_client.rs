//! MCP JSON-RPC client and bounded, owned process transport.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
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

/// Largest incoming MCP stdio frame, including the terminating newline.
pub const MAX_STDIO_FRAME_BYTES: u64 = 16 * 1024 * 1024;

/// Default deadline for writing one MCP stdio message and reading its response.
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

    /// Set the deadline for subsequent messages.
    fn set_timeout(&mut self, _timeout: Duration) {}

    /// Set the call-response budget. Transports supporting `None` may wait
    /// until completion or closure while retaining bounded writes. Other
    /// transports retain their current timeout when `None` is requested.
    fn set_call_timeout(&mut self, timeout: Option<Duration>) {
        if let Some(timeout) = timeout {
            self.set_timeout(timeout);
        }
    }

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

/// Software identity presented during MCP initialization.
#[derive(Debug, Clone, Copy)]
pub struct ClientInfo {
    pub name: &'static str,
    pub version: &'static str,
}
impl Default for ClientInfo {
    fn default() -> Self {
        Self {
            name: "susi-mcp",
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

/// A client for one MCP server.
#[derive(Debug)]
pub struct McpClient<T: Transport> {
    transport: T,
    client_info: ClientInfo,
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
            client_info: ClientInfo::default(),
            next_id: 0,
            initialized: false,
            negotiated_version: None,
            closed: false,
        }
    }

    /// Set initialization metadata before the first operation.
    pub fn with_client_info(mut self, client_info: ClientInfo) -> Self {
        self.client_info = client_info;
        self
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

    /// Set the transport's call-response budget after completing discovery.
    /// `None` permits unbounded stdio response waits; HTTP retains its timeout.
    pub fn set_call_timeout(&mut self, timeout: Option<Duration>) {
        self.transport.set_call_timeout(timeout);
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
                "clientInfo": {"name": self.client_info.name, "version": self.client_info.version}
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

    /// Change the deadline for subsequent MCP exchanges.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.transport.set_timeout(timeout);
    }

    /// Call once and preserve the MCP result, including structured content and
    /// explicit tool errors. Transport errors remain errors; no call is replayed.
    pub fn call_result(&mut self, name: &str, arguments: &Value) -> EaiResult<Value> {
        self.initialize()?;
        self.request("tools/call", json!({"name":name,"arguments":arguments}))
    }

    /// Read a resource through this same authorized MCP session.
    pub fn read_resource(&mut self, uri: &str) -> EaiResult<Value> {
        self.initialize()?;
        self.request("resources/read", json!({"uri":uri}))
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
        let result = self.call_result(name, arguments)?;
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

/// Read one bounded MCP stdio frame, distinguishing skipped notifications.
///
/// Servers may send JSON-RPC notifications (for example progress or log
/// events) between a request and its response. Notifications have a method
/// but no id and never carry a result or error, so they are not candidates for
/// the request's response. A message with an id, malformed JSON, or a
/// response-shaped message without an id is returned to the client layer,
/// which preserves its normal correlation checks.
fn read_stdio_message<R: BufRead>(reader: &mut R) -> EaiResult<Option<Value>> {
    let mut line = Vec::new();
    susi_paths::http_body::read_line_capped(reader, &mut line, MAX_STDIO_FRAME_BYTES)
        .map_err(|error| EaiError::io(format!("mcp read: {error}")))?;
    if line.is_empty() {
        return Err(EaiError::io("mcp server closed the stream"));
    }
    if !line.ends_with(b"\n") {
        return Err(EaiError::io("mcp server closed an incomplete frame"));
    }
    let line =
        std::str::from_utf8(&line).map_err(|error| EaiError::io(format!("mcp read: {error}")))?;
    if line.trim().is_empty() {
        return Ok(None);
    }
    let message: Value = crate::protocol_json::parse(line.as_bytes())
        .map_err(|error| EaiError::internal(format!("mcp response: {error}")))?;
    let notification = message.get("id").is_none()
        && message.get("method").and_then(Value::as_str).is_some()
        && message.get("result").is_none()
        && message.get("error").is_none();
    Ok((!notification).then_some(message))
}

#[cfg(test)]
fn read_stdio_response<R: BufRead>(reader: &mut R) -> EaiResult<Value> {
    loop {
        if let Some(message) = read_stdio_message(reader)? {
            return Ok(message);
        }
    }
}

enum StdioResponse {
    FrameStarted,
    FrameFinished(EaiResult<Option<Value>>),
}

type StdioResponses = Receiver<StdioResponse>;
type StdioWorker = JoinHandle<()>;

struct StdioWrite {
    line: String,
    completion: SyncSender<EaiResult<()>>,
}

fn start_stdio_reader(
    mut stdout: BufReader<ChildStdout>,
) -> EaiResult<(StdioResponses, StdioWorker)> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("susi-mcp-stdio-reader".to_string())
        .spawn(move || loop {
            if let Err(error) = stdout.fill_buf() {
                let _ = sender.send(StdioResponse::FrameFinished(Err(EaiError::io(format!(
                    "mcp read: {error}"
                )))));
                break;
            }
            if sender.send(StdioResponse::FrameStarted).is_err() {
                break;
            }
            let result = read_stdio_message(&mut stdout);
            let finished = result.is_err();
            if sender.send(StdioResponse::FrameFinished(result)).is_err() || finished {
                break;
            }
        })
        .map_err(|error| EaiError::io(format!("mcp reader thread: {error}")))?;
    Ok((receiver, reader))
}

fn start_stdio_writer(mut stdin: ChildStdin) -> EaiResult<(SyncSender<StdioWrite>, StdioWorker)> {
    let (sender, receiver) = mpsc::sync_channel::<StdioWrite>(1);
    let writer = thread::Builder::new()
        .name("susi-mcp-stdio-writer".to_string())
        .spawn(move || {
            while let Ok(write) = receiver.recv() {
                let result = writeln!(stdin, "{}", write.line)
                    .and_then(|()| stdin.flush())
                    .map_err(|error| EaiError::io(format!("mcp write: {error}")));
                let finished = result.is_err();
                if write.completion.send(result).is_err() || finished {
                    break;
                }
            }
        })
        .map_err(|error| EaiError::io(format!("mcp writer thread: {error}")))?;
    Ok((sender, writer))
}

fn receive_stdio<T>(
    receiver: &Receiver<T>,
    started: Instant,
    timeout: Duration,
    phase: &str,
) -> EaiResult<T> {
    let remaining = timeout.saturating_sub(started.elapsed());
    let result = if remaining.is_zero() {
        Err(RecvTimeoutError::Timeout)
    } else {
        receiver.recv_timeout(remaining)
    };
    match result {
        Ok(result) => Ok(result),
        Err(RecvTimeoutError::Timeout) => Err(EaiError::io(format!(
            "mcp {phase} timed out after {} ms",
            timeout.as_millis()
        ))),
        Err(RecvTimeoutError::Disconnected) => {
            Err(EaiError::io(format!("mcp stdio {phase} worker stopped")))
        }
    }
}

/// A JSON-RPC transport over a child process's stdio.
#[derive(Debug)]
pub struct StdioTransport {
    child: Child,
    writes: Option<SyncSender<StdioWrite>>,
    responses: Option<StdioResponses>,
    reader: Option<StdioWorker>,
    writer: Option<StdioWorker>,
    timeout: Duration,
    call_timeout: Option<Duration>,
}

impl StdioTransport {
    /// Connect an already authorized child with piped stdin and stdout.
    pub fn from_child(mut child: Child) -> EaiResult<Self> {
        if child.stdin.is_none() || child.stdout.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(EaiError::internal("mcp: child has missing protocol pipes"));
        }
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
        let (writes, writer) = match start_stdio_writer(stdin) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            child,
            writes: Some(writes),
            responses: Some(responses),
            reader: Some(reader),
            writer: Some(writer),
            timeout: Duration::from_secs(DEFAULT_STDIO_TIMEOUT_SECS),
            call_timeout: Some(Duration::from_secs(DEFAULT_STDIO_TIMEOUT_SECS)),
        })
    }

    /// Set the deadline shared by writing a message and reading its response.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.set_timeout(timeout);
        self
    }

    /// Set the per-message deadline in seconds, with a one-second minimum.
    #[must_use]
    pub fn with_timeout_secs(self, timeout_secs: u64) -> Self {
        self.with_timeout(Duration::from_secs(timeout_secs.max(1)))
    }

    fn stop_child(&mut self) {
        self.writes.take();
        self.responses.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        // A descendant can retain either pipe after the root is reaped.
        // Detach workers instead of making close or a timeout join blocked IO.
        let _ = self.reader.take();
        let _ = self.writer.take();
    }

    fn write_message(&mut self, message: &Value, started: Instant) -> EaiResult<()> {
        let line = serde_json::to_string(message)
            .map_err(|error| EaiError::internal(format!("mcp request: {error}")))?;
        let sender = self
            .writes
            .as_ref()
            .ok_or_else(|| EaiError::io("mcp stdio transport is closed"))?;
        if started.elapsed() >= self.timeout {
            return Err(EaiError::io("mcp write timed out before dispatch"));
        }
        let (completion, receiver) = mpsc::sync_channel(1);
        sender
            .try_send(StdioWrite { line, completion })
            .map_err(|_| EaiError::io("mcp stdio writer is unavailable"))?;
        receive_stdio(&receiver, started, self.timeout, "write")?
    }

    fn read_response(&self, started: Instant) -> EaiResult<Value> {
        let responses = self
            .responses
            .as_ref()
            .ok_or_else(|| EaiError::io("mcp stdio transport is closed"))?;
        let mut frame_started = None;
        loop {
            let event = match (self.call_timeout, frame_started) {
                (Some(timeout), _) => receive_stdio(responses, started, timeout, "response")?,
                (None, Some(started)) => {
                    receive_stdio(responses, started, self.timeout, "response frame")?
                }
                (None, None) => responses
                    .recv()
                    .map_err(|_| EaiError::io("mcp stdio response worker stopped"))?,
            };
            match event {
                StdioResponse::FrameStarted => frame_started = Some(Instant::now()),
                StdioResponse::FrameFinished(result) => {
                    if let Some(response) = result? {
                        return Ok(response);
                    }
                    frame_started = None;
                }
            }
        }
    }
}

impl Transport for StdioTransport {
    fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout.max(Duration::from_millis(1));
        self.call_timeout = Some(self.timeout);
    }

    fn set_call_timeout(&mut self, timeout: Option<Duration>) {
        if let Some(timeout) = timeout {
            self.set_timeout(timeout);
        } else {
            self.call_timeout = None;
        }
    }

    fn exchange(&mut self, request: &Value) -> EaiResult<Value> {
        let started = Instant::now();
        let result = self
            .write_message(request, started)
            .and_then(|()| self.read_response(started));
        if result.is_err() {
            self.stop_child();
        }
        result
    }

    fn notify(&mut self, notification: &Value) -> EaiResult<()> {
        let result = self.write_message(notification, Instant::now());
        if result.is_err() {
            self.stop_child();
        }
        result
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

#[cfg(test)]
#[path = "tests/mcp_client.rs"]
mod tests;

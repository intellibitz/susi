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
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Protocol revision this client requests. A copy of
/// `susi_core::mcp_client::PROTOCOL_VERSION` (susi-core is not a
/// dependency); the susi-gmcp parity test keeps the two in step.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// Modern per-request MCP revision supporting official Tasks.
pub const PROTOCOL_VERSION_2026_07_28: &str = "2026-07-28";

/// Extension identifier for Official MCP Tasks.
pub const TASKS_EXTENSION_ID: &str = "io.modelcontextprotocol/tasks";

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const MAX_IDEMPOTENT_RETRIES: usize = 3;

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
    server_capabilities: Value,
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
        Self::open_with_capabilities(addr, bearer, &json!({}), timeout)
    }

    /// Connect to `addr` (`host:port`) while advertising caller-supplied
    /// `capabilities` (for example namespaced `extensions`) in `initialize`,
    /// retaining the server's `result.capabilities` for inspection.
    pub fn open_with_capabilities(
        addr: &str,
        bearer: Option<&str>,
        capabilities: &Value,
        timeout: Duration,
    ) -> Result<Self, WireError> {
        if !capabilities.is_object() {
            return Err(WireError::Protocol(
                "client capabilities must be a JSON object".into(),
            ));
        }
        let (host, port) = split_addr(addr)?;
        ensure_loopback(&host, port)?;
        let mut session = Self {
            host,
            port,
            bearer: bearer.map(str::to_string),
            id: String::new(),
            version: String::new(),
            server_capabilities: json!({}),
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
                "capabilities": capabilities,
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
        session.server_capabilities = result
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| json!({}));
        // Only the status matters (GMCP answers 202 with no body); the
        // reply is dropped unread, as the peer client does.
        session.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(session)
    }

    /// Connect to `addr` (`host:port`) on the stateless `2026-07-28` MCP path,
    /// running `server/discover` without opening or holding a legacy session.
    /// An address that resolves to anything but loopback is refused before a
    /// byte is sent.
    pub fn open_2026_07_28(
        addr: &str,
        bearer: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, WireError> {
        let (host, port) = split_addr(addr)?;
        ensure_loopback(&host, port)?;
        let mut session = Self {
            host,
            port,
            bearer: bearer.map(str::to_string),
            id: String::new(),
            version: PROTOCOL_VERSION_2026_07_28.to_string(),
            server_capabilities: json!({}),
            timeout,
            next_id: 1,
            open: false,
        };
        let discover = session.request_with_capabilities(
            "server/discover",
            &json!({}),
            &json!({}),
            &mut |_| {},
        )?;
        if let Some(versions) = discover.get("supportedVersions").and_then(Value::as_array) {
            let ok = versions
                .iter()
                .any(|v| v.as_str() == Some(PROTOCOL_VERSION_2026_07_28));
            if !ok {
                return Err(WireError::Protocol(format!(
                    "{addr} server/discover does not advertise {PROTOCOL_VERSION_2026_07_28}"
                )));
            }
        }
        session.server_capabilities = discover
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| json!({}));
        Ok(session)
    }

    /// The protocol revision the server chose.
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.version
    }

    /// The server capability object returned by `initialize` or `server/discover`.
    #[must_use]
    pub fn server_capabilities(&self) -> &Value {
        &self.server_capabilities
    }

    /// The `capabilities.extensions` map advertised by the server, if present.
    #[must_use]
    pub fn server_extensions(&self) -> Option<&serde_json::Map<String, Value>> {
        self.server_capabilities
            .get("extensions")
            .and_then(Value::as_object)
    }

    /// Whether the server advertised the official `io.modelcontextprotocol/tasks`
    /// extension in `initialize` or `server/discover`.
    #[must_use]
    pub fn supports_server_tasks(&self) -> bool {
        self.server_extensions()
            .is_some_and(|exts| exts.contains_key(TASKS_EXTENSION_ID))
    }

    /// Change the receive window for later requests.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Call `name` with `args` and return the raw `tools/call` result.
    /// With a `progress_token` the request asks for progress, and every
    /// `notifications/progress` or `notifications/message` frame that
    /// arrives before the result is passed to `on_notification`.
    /// Does not opt into async Tasks; an unexpected `CreateTaskResult` fails closed.
    pub fn call_tool(
        &mut self,
        name: &str,
        args: &Value,
        progress_token: Option<Value>,
        on_notification: &mut dyn FnMut(&Value),
    ) -> Result<Value, WireError> {
        match self.call_tool_inner(
            name,
            args,
            CallToolOpts {
                allow_tasks: false,
                progress_token,
            },
            on_notification,
        )? {
            CallToolOutcome::Complete(value) => Ok(value),
            CallToolOutcome::Task(handle) => Err(WireError::Protocol(format!(
                "server returned unexpected task {} when tasks were disallowed",
                handle.task_id
            ))),
        }
    }

    /// Call `name` with `args`, opting into `io.modelcontextprotocol/tasks`
    /// per request when `allow_tasks` is true.
    ///
    /// Returns [`CallToolOutcome::Task`] only when `allow_tasks` is true and the
    /// server chose asynchronous task execution (`resultType: "task"`). Fails
    /// closed if a server returns `CreateTaskResult` without per-request opt-in.
    pub fn call_tool_with_tasks(
        &mut self,
        name: &str,
        args: &Value,
        allow_tasks: bool,
        on_notification: &mut dyn FnMut(&Value),
    ) -> Result<CallToolOutcome, WireError> {
        self.call_tool_inner(
            name,
            args,
            CallToolOpts {
                allow_tasks,
                progress_token: None,
            },
            on_notification,
        )
    }

    /// Call `name` with `args`, opting into `io.modelcontextprotocol/tasks`
    /// while preserving a synchronous `CallToolResult` return shape: if the
    /// server returns `CreateTaskResult`, polls `tasks/get` (fulfilling any
    /// MRTR `inputRequests` via `on_input_requests`) until terminal settlement.
    ///
    /// Tool-level `isError: true` outcomes return `Ok(CallToolResult)` per
    /// `ext-tasks` `-0837`/`-0892`, while protocol-level `failed` tasks surface
    /// their structured JSON-RPC error as [`WireError::Rpc`].
    pub fn call_tool_resolving_tasks<F>(
        &mut self,
        name: &str,
        args: &Value,
        max_wait: Option<Duration>,
        on_input_requests: F,
    ) -> Result<Value, WireError>
    where
        F: FnMut(&[InputRequestEntry]) -> Result<Value, WireError>,
    {
        match self.call_tool_inner(
            name,
            args,
            CallToolOpts {
                allow_tasks: true,
                progress_token: None,
            },
            &mut |_| {},
        )? {
            CallToolOutcome::Complete(value) => Ok(value),
            CallToolOutcome::Task(handle) => {
                let snapshot = self.poll_task_until_done(&handle, max_wait, on_input_requests)?;
                match snapshot.handle.status {
                    TaskStatus::Completed => snapshot.result.ok_or_else(|| {
                        WireError::Protocol("completed task snapshot missing result".into())
                    }),
                    TaskStatus::Failed => {
                        let err = snapshot.error.as_ref();
                        let code = err
                            .and_then(|e| e.get("code"))
                            .and_then(Value::as_i64)
                            .unwrap_or(-32603);
                        let msg = err
                            .and_then(|e| e.get("message"))
                            .and_then(Value::as_str)
                            .or(snapshot.handle.status_message.as_deref())
                            .unwrap_or("task failed")
                            .to_string();
                        Err(WireError::Rpc(code, msg))
                    }
                    TaskStatus::Cancelled => Err(WireError::Protocol(format!(
                        "task {} was cancelled before completion",
                        snapshot.handle.task_id
                    ))),
                    TaskStatus::Working | TaskStatus::InputRequired => Err(WireError::Protocol(
                        "poll_task_until_done returned non-terminal snapshot".into(),
                    )),
                }
            }
        }
    }

    fn call_tool_inner(
        &mut self,
        name: &str,
        args: &Value,
        opts: CallToolOpts,
        on_notification: &mut dyn FnMut(&Value),
    ) -> Result<CallToolOutcome, WireError> {
        let mut params = json!({"name": name, "arguments": args});
        if let Some(token) = opts.progress_token {
            params["_meta"] = json!({"progressToken": token});
        }
        let caps = if opts.allow_tasks {
            client_capabilities_with_tasks()
        } else {
            json!({})
        };
        let result =
            self.request_with_capabilities("tools/call", &params, &caps, on_notification)?;
        if result.get("resultType").and_then(Value::as_str) == Some("task") {
            if !opts.allow_tasks {
                return Err(WireError::Protocol(
                    "server returned CreateTaskResult when request omitted io.modelcontextprotocol/tasks capability".into(),
                ));
            }
            return Ok(CallToolOutcome::Task(TaskHandle::from_value(
                &result, "task",
            )?));
        }
        Ok(CallToolOutcome::Complete(result))
    }

    /// Fetch the current [`TaskSnapshot`] for `task_id` via `tasks/get`.
    /// Retries only bounded transient I/O or 429/502/503/504 HTTP responses.
    pub fn get_task(&mut self, task_id: &str) -> Result<TaskSnapshot, WireError> {
        validate_task_id(task_id)?;
        let value = self.request_idempotent_task("tasks/get", task_id)?;
        TaskSnapshot::from_get_result(&value)
    }

    /// Deliver `inputResponses` for an `input_required` task via `tasks/update`.
    /// Not retried blindly because responses mutate task input state.
    pub fn update_task(&mut self, task_id: &str, input_responses: &Value) -> Result<(), WireError> {
        validate_task_id(task_id)?;
        if !input_responses.is_object() {
            return Err(WireError::Protocol(
                "tasks/update inputResponses must be a JSON object".into(),
            ));
        }
        let params = json!({
            "taskId": task_id,
            "inputResponses": input_responses,
        });
        let caps = client_capabilities_with_tasks();
        let value = self.request_with_capabilities("tasks/update", &params, &caps, &mut |_| {})?;
        ensure_task_ack(&value, &self.version, "tasks/update")
    }

    /// Cancel `task_id` via `tasks/cancel`.
    /// Retries only bounded transient I/O or 429/502/503/504 HTTP responses.
    pub fn cancel_task(&mut self, task_id: &str) -> Result<(), WireError> {
        validate_task_id(task_id)?;
        let value = self.request_idempotent_task("tasks/cancel", task_id)?;
        ensure_task_ack(&value, &self.version, "tasks/cancel")
    }

    /// Poll `initial` honoring `pollIntervalMs`, bounded by `ttlMs` and optional
    /// `max_wait`, fulfilling valid MRTR `inputRequests` via `on_input_requests`
    /// and rejecting server reuse of already-answered request keys.
    pub fn poll_task_until_done<F>(
        &mut self,
        initial: &TaskHandle,
        max_wait: Option<Duration>,
        mut on_input_requests: F,
    ) -> Result<TaskSnapshot, WireError>
    where
        F: FnMut(&[InputRequestEntry]) -> Result<Value, WireError>,
    {
        let start = Instant::now();
        let ttl_deadline = initial.ttl_ms.map(|ms| start + Duration::from_millis(ms));
        let wait_deadline = max_wait.map(|d| start + d);
        let deadline = match (ttl_deadline, wait_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let mut poll_interval_ms = initial.poll_interval_ms.unwrap_or(50).max(1);
        let mut answered_keys = BTreeSet::new();

        loop {
            let snapshot = self.get_task(&initial.task_id)?;
            for entry in &snapshot.input_requests {
                if answered_keys.contains(&entry.key) {
                    return Err(WireError::Protocol(format!(
                        "server reused answered inputRequests key {:?}",
                        entry.key
                    )));
                }
            }
            if snapshot.handle.status.is_terminal() {
                return Ok(snapshot);
            }
            if snapshot.handle.status == TaskStatus::InputRequired {
                if snapshot.input_requests.is_empty() {
                    return Err(WireError::Protocol(
                        "input_required task snapshot carries no inputRequests".into(),
                    ));
                }
                let responses = on_input_requests(&snapshot.input_requests)?;
                let Some(resp_obj) = responses.as_object() else {
                    return Err(WireError::Protocol(
                        "on_input_requests handler must return a JSON object".into(),
                    ));
                };
                if resp_obj.is_empty() {
                    return Err(WireError::Protocol(
                        "on_input_requests returned empty responses for input_required task".into(),
                    ));
                }
                let pending: BTreeSet<&str> = snapshot
                    .input_requests
                    .iter()
                    .map(|e| e.key.as_str())
                    .collect();
                for key in resp_obj.keys() {
                    if !pending.contains(key.as_str()) {
                        return Err(WireError::Protocol(format!(
                            "response key {key:?} does not match any pending inputRequest"
                        )));
                    }
                    answered_keys.insert(key.clone());
                }
                self.update_task(&initial.task_id, &responses)?;
            }
            poll_interval_ms = snapshot
                .handle
                .poll_interval_ms
                .unwrap_or(poll_interval_ms)
                .max(1);
            let sleep_for = Duration::from_millis(poll_interval_ms);
            if let Some(dl) = deadline {
                let now = Instant::now();
                if now >= dl {
                    return Err(WireError::Protocol(format!(
                        "task {} exceeded ttlMs/deadline before reaching terminal state",
                        initial.task_id
                    )));
                }
                let remaining = dl.saturating_duration_since(now);
                if remaining < sleep_for {
                    return Err(WireError::Protocol(format!(
                        "task {} exceeded ttlMs/deadline before next pollIntervalMs",
                        initial.task_id
                    )));
                }
            }
            std::thread::sleep(sleep_for);
        }
    }

    fn request_idempotent_task(&mut self, method: &str, task_id: &str) -> Result<Value, WireError> {
        let params = json!({ "taskId": task_id });
        let caps = client_capabilities_with_tasks();
        for attempt in 0..=MAX_IDEMPOTENT_RETRIES {
            match self.request_with_capabilities(method, &params, &caps, &mut |_| {}) {
                Ok(value) => return Ok(value),
                Err(err) if attempt < MAX_IDEMPOTENT_RETRIES && is_transient_retryable(&err) => {
                    std::thread::sleep(Duration::from_millis(15 * (attempt as u64 + 1)));
                }
                Err(err) => return Err(err),
            }
        }
        Err(WireError::Protocol(format!(
            "exhausted retries for {method}"
        )))
    }

    /// Any session-scoped JSON-RPC request (`tools/list`, `ping`, ...);
    /// returns its `result`.
    pub fn request(
        &mut self,
        method: &str,
        params: &Value,
        on_notification: &mut dyn FnMut(&Value),
    ) -> Result<Value, WireError> {
        self.request_with_capabilities(method, params, &json!({}), on_notification)
    }

    /// Send a JSON-RPC request, attaching per-request `_meta`
    /// (`io.modelcontextprotocol/protocolVersion` and
    /// `io.modelcontextprotocol/clientCapabilities`) on `2026-07-28`.
    pub fn request_with_capabilities(
        &mut self,
        method: &str,
        params: &Value,
        client_capabilities: &Value,
        on_notification: &mut dyn FnMut(&Value),
    ) -> Result<Value, WireError> {
        if !client_capabilities.is_object() {
            return Err(WireError::Protocol(
                "client capabilities must be a JSON object".into(),
            ));
        }
        let params = if self.version == PROTOCOL_VERSION_2026_07_28 {
            with_modern_meta(params, &self.version, client_capabilities)?
        } else {
            params.clone()
        };
        let id = self.next_id;
        self.next_id += 1;
        let response = self.post(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        let result = read_result(response, id, on_notification)?;
        if method != "tools/call"
            && result.get("resultType").and_then(Value::as_str) == Some("task")
        {
            return Err(WireError::Protocol(format!(
                "server returned unexpected CreateTaskResult for non-tool method {method}"
            )));
        }
        Ok(result)
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
        let mut headers = self.headers(true);
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if self.version == PROTOCOL_VERSION_2026_07_28 || method.starts_with("tasks/") {
                headers.push(("Mcp-Method".to_string(), method.to_string()));
                if let Some(name) = expected_mcp_name(method, message.get("params")) {
                    headers.push(("Mcp-Name".to_string(), name.to_string()));
                }
            }
        }
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
            let _ = (&mut response.body).take(4096).read_to_end(&mut head);
            if matches!(response.status, 400 | 404) {
                if let Some(rpc_err) = parse_jsonrpc_http_error(&head) {
                    return Err(rpc_err);
                }
            }
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

/// Client capability declaration enabling Official MCP Tasks per request.
#[must_use]
pub fn client_capabilities_with_tasks() -> Value {
    json!({ "extensions": { TASKS_EXTENSION_ID: {} } })
}

/// Lifecycle state of an MCP Task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    /// Canonical wire string for this task status.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a wire status string into [`TaskStatus`].
    pub fn parse(raw: &str) -> Result<Self, WireError> {
        match raw {
            "working" => Ok(Self::Working),
            "input_required" => Ok(Self::InputRequired),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(WireError::Protocol(format!(
                "unknown task status {other:?}"
            ))),
        }
    }

    /// True once no further state transitions can occur.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Lightweight handle describing an asynchronous MCP Task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskHandle {
    pub task_id: String,
    pub status: TaskStatus,
    pub status_message: Option<String>,
    pub created_at: String,
    pub last_updated_at: String,
    pub ttl_ms: Option<u64>,
    pub poll_interval_ms: Option<u64>,
}

impl TaskHandle {
    /// Parse a task handle from a `CreateTaskResult` or `GetTaskResult` JSON
    /// value, validating `resultType` against `expected_result_type`.
    pub fn from_value(value: &Value, expected_result_type: &str) -> Result<Self, WireError> {
        let result_type = value.get("resultType").and_then(Value::as_str);
        if result_type != Some(expected_result_type) {
            return Err(WireError::Protocol(format!(
                "expected resultType {expected_result_type:?}, got {result_type:?}"
            )));
        }
        Self::from_fields(value)
    }

    fn from_fields(value: &Value) -> Result<Self, WireError> {
        let task_id = required_nonempty_str(value, "taskId")?;
        let status = TaskStatus::parse(&required_nonempty_str(value, "status")?)?;
        let status_message = optional_str(value, "statusMessage")?;
        let created_at = required_nonempty_str(value, "createdAt")?;
        let last_updated_at = required_nonempty_str(value, "lastUpdatedAt")?;
        let ttl_ms = optional_u64(value, "ttlMs")?;
        let poll_interval_ms = optional_u64(value, "pollIntervalMs")?;
        Ok(Self {
            task_id,
            status,
            status_message,
            created_at,
            last_updated_at,
            ttl_ms,
            poll_interval_ms,
        })
    }
}

/// One validated server-to-client multi-round-trip input request attached to a
/// task in `input_required` state.
#[derive(Debug, Clone, PartialEq)]
pub struct InputRequestEntry {
    pub key: String,
    pub method: String,
    pub params: Option<Value>,
}

/// Full task state returned by `tasks/get`.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskSnapshot {
    pub handle: TaskHandle,
    pub input_requests: Vec<InputRequestEntry>,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

impl TaskSnapshot {
    /// Parse and validate a `GetTaskResult` payload (`resultType: "complete"`).
    pub fn from_get_result(value: &Value) -> Result<Self, WireError> {
        let handle = TaskHandle::from_value(value, "complete")?;
        let input_requests = parse_input_requests(value.get("inputRequests"))?;
        let result = value.get("result").cloned();
        let error = value.get("error").cloned();
        Ok(Self {
            handle,
            input_requests,
            result,
            error,
        })
    }
}

/// Outcome of a `tools/call` invoked via [`Session::call_tool_with_tasks`].
#[derive(Debug, Clone, PartialEq)]
pub enum CallToolOutcome {
    Complete(Value),
    Task(TaskHandle),
}

/// Durable metadata for an active or recently observed MCP Task handle
/// (excludes credentials/tokens).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedTaskHandle {
    pub addr: String,
    pub task_id: String,
    pub protocol_version: String,
    pub status: TaskStatus,
    pub status_message: Option<String>,
    pub created_at: String,
    pub last_updated_at: String,
    pub ttl_ms: Option<u64>,
    pub poll_interval_ms: Option<u64>,
}

impl PersistedTaskHandle {
    /// Build a persistable record from `addr`, negotiated `protocol_version`,
    /// and a server-issued [`TaskHandle`].
    #[must_use]
    pub fn from_handle(addr: &str, protocol_version: &str, handle: &TaskHandle) -> Self {
        Self {
            addr: addr.to_string(),
            task_id: handle.task_id.clone(),
            protocol_version: protocol_version.to_string(),
            status: handle.status,
            status_message: handle.status_message.clone(),
            created_at: handle.created_at.clone(),
            last_updated_at: handle.last_updated_at.clone(),
            ttl_ms: handle.ttl_ms,
            poll_interval_ms: handle.poll_interval_ms,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "addr": self.addr,
            "taskId": self.task_id,
            "protocolVersion": self.protocol_version,
            "status": self.status.as_str(),
            "statusMessage": self.status_message,
            "createdAt": self.created_at,
            "lastUpdatedAt": self.last_updated_at,
            "ttlMs": self.ttl_ms,
            "pollIntervalMs": self.poll_interval_ms,
        })
    }

    fn from_json(value: &Value) -> Result<Self, WireError> {
        let addr = required_nonempty_str(value, "addr")?;
        let protocol_version = required_nonempty_str(value, "protocolVersion")?;
        let handle = TaskHandle::from_fields(value)?;
        Ok(Self {
            addr,
            task_id: handle.task_id,
            protocol_version,
            status: handle.status,
            status_message: handle.status_message,
            created_at: handle.created_at,
            last_updated_at: handle.last_updated_at,
            ttl_ms: handle.ttl_ms,
            poll_interval_ms: handle.poll_interval_ms,
        })
    }
}

/// Fail-closed atomic JSON file store for resumable MCP Task handles.
#[derive(Debug)]
pub struct PersistedTaskHandleStore {
    path: PathBuf,
    handles: Vec<PersistedTaskHandle>,
}

impl PersistedTaskHandleStore {
    /// Open an existing store file or prepare a new empty store if `path` does
    /// not exist. Fails closed on corrupted or schema-invalid JSON.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, WireError> {
        let path = path.into();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    handles: Vec::new(),
                });
            }
            Err(err) => return Err(WireError::Io(err.to_string())),
        };
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| WireError::Protocol(format!("corrupt task handle store: {e}")))?;
        if value.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(WireError::Protocol(
                "corrupt task handle store: unsupported schema version".into(),
            ));
        }
        let items = value
            .get("handles")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                WireError::Protocol("corrupt task handle store: missing handles array".into())
            })?;
        let mut handles = Vec::with_capacity(items.len());
        let mut seen = BTreeSet::new();
        for item in items {
            let entry = PersistedTaskHandle::from_json(item)?;
            if !seen.insert(entry.task_id.clone()) {
                return Err(WireError::Protocol(format!(
                    "corrupt task handle store: duplicate taskId {:?}",
                    entry.task_id
                )));
            }
            handles.push(entry);
        }
        Ok(Self { path, handles })
    }

    /// Upsert `handle` by `task_id` and flush atomically to disk.
    pub fn save(&mut self, handle: PersistedTaskHandle) -> Result<(), WireError> {
        if let Some(existing) = self
            .handles
            .iter_mut()
            .find(|h| h.task_id == handle.task_id)
        {
            *existing = handle;
        } else {
            self.handles.push(handle);
        }
        self.flush_atomic()
    }

    /// Refresh an already-persisted task handle from a [`TaskSnapshot`] and
    /// persist atomically if present.
    pub fn update_from_snapshot(&mut self, snapshot: &TaskSnapshot) -> Result<bool, WireError> {
        let Some(existing) = self
            .handles
            .iter_mut()
            .find(|h| h.task_id == snapshot.handle.task_id)
        else {
            return Ok(false);
        };
        existing.status = snapshot.handle.status;
        existing.status_message = snapshot.handle.status_message.clone();
        existing.last_updated_at = snapshot.handle.last_updated_at.clone();
        existing.ttl_ms = snapshot.handle.ttl_ms;
        existing.poll_interval_ms = snapshot.handle.poll_interval_ms;
        self.flush_atomic()?;
        Ok(true)
    }

    /// Remove `task_id` from the store, flushing if present.
    pub fn remove(&mut self, task_id: &str) -> Result<bool, WireError> {
        let before = self.handles.len();
        self.handles.retain(|h| h.task_id != task_id);
        if self.handles.len() == before {
            return Ok(false);
        }
        self.flush_atomic()?;
        Ok(true)
    }

    /// Lookup a persisted handle by `task_id`.
    #[must_use]
    pub fn get(&self, task_id: &str) -> Option<&PersistedTaskHandle> {
        self.handles.iter().find(|h| h.task_id == task_id)
    }

    /// Return all persisted task handles currently in the store.
    #[must_use]
    pub fn list(&self) -> &[PersistedTaskHandle] {
        &self.handles
    }

    /// Return the store file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn flush_atomic(&self) -> Result<(), WireError> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| WireError::Io(e.to_string()))?;
        }
        let payload = serde_json::to_vec_pretty(&json!({
            "version": 1,
            "handles": self.handles.iter().map(PersistedTaskHandle::to_json).collect::<Vec<_>>(),
        }))
        .map_err(|e| WireError::Protocol(e.to_string()))?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let file_name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("task-handles.json");
        let tmp_path = self
            .path
            .with_file_name(format!(".{file_name}.tmp.{}.{nanos}", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| WireError::Io(e.to_string()))?;
        if let Err(err) = file.write_all(&payload).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            return Err(WireError::Io(err.to_string()));
        }
        drop(file);
        if let Err(err) = fs::rename(&tmp_path, &self.path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(WireError::Io(err.to_string()));
        }
        Ok(())
    }
}

fn validate_task_id(task_id: &str) -> Result<(), WireError> {
    if task_id.is_empty() || task_id.bytes().any(|b| !(0x20..0x7f).contains(&b)) {
        return Err(WireError::Protocol(
            "taskId must be non-empty printable ASCII".into(),
        ));
    }
    Ok(())
}

fn ensure_task_ack(value: &Value, version: &str, method: &str) -> Result<(), WireError> {
    if !value.is_object() {
        return Err(WireError::Protocol(format!(
            "{method} response must be a JSON object"
        )));
    }
    let rt = value.get("resultType").and_then(Value::as_str);
    if version == PROTOCOL_VERSION_2026_07_28 {
        if rt != Some("complete") {
            return Err(WireError::Protocol(format!(
                "{method} expected resultType \"complete\", got {rt:?}"
            )));
        }
    } else if value
        .get("resultType")
        .is_some_and(|v| v.as_str() != Some("complete"))
    {
        return Err(WireError::Protocol(format!(
            "{method} returned invalid resultType {rt:?}"
        )));
    }
    Ok(())
}

fn expected_mcp_name<'a>(method: &str, params: Option<&'a Value>) -> Option<&'a str> {
    let key = match method {
        "tools/call" | "prompts/get" => "name",
        "resources/read" | "resources/subscribe" | "resources/unsubscribe" => "uri",
        "tasks/get" | "tasks/update" | "tasks/cancel" => "taskId",
        _ => return None,
    };
    params?
        .get(key)?
        .as_str()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| (0x20..0x7f).contains(&b)))
}

struct CallToolOpts {
    allow_tasks: bool,
    progress_token: Option<Value>,
}

fn with_modern_meta(
    params: &Value,
    version: &str,
    client_capabilities: &Value,
) -> Result<Value, WireError> {
    let mut out = match params {
        Value::Null => json!({}),
        Value::Object(_) => params.clone(),
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            return Err(WireError::Protocol(
                "JSON-RPC params must be a JSON object or null".into(),
            ))
        }
    };
    let meta = match out.get_mut("_meta") {
        Some(Value::Object(map)) => map,
        Some(Value::Null) | None => {
            out["_meta"] = Value::Object(Map::new());
            match out.get_mut("_meta") {
                Some(Value::Object(map)) => map,
                _ => return Err(WireError::Protocol("failed to initialize _meta".into())),
            }
        }
        Some(_) => {
            return Err(WireError::Protocol(
                "request params._meta must be a JSON object".into(),
            ))
        }
    };
    meta.insert(
        META_PROTOCOL_VERSION.to_string(),
        Value::String(version.to_string()),
    );
    meta.insert(
        META_CLIENT_CAPABILITIES.to_string(),
        client_capabilities.clone(),
    );
    Ok(out)
}

fn parse_jsonrpc_http_error(body: &[u8]) -> Option<WireError> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error")?;
    let code = error.get("code").and_then(Value::as_i64)?;
    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
    Some(WireError::Rpc(code, message.to_string()))
}

fn is_transient_retryable(err: &WireError) -> bool {
    matches!(
        err,
        WireError::Io(_) | WireError::Http(429 | 502 | 503 | 504, _)
    )
}

fn required_nonempty_str(value: &Value, key: &str) -> Result<String, WireError> {
    let s = value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WireError::Protocol(format!("missing or empty string field {key:?}")))?;
    Ok(s.to_string())
}

fn optional_str(value: &Value, key: &str) -> Result<Option<String>, WireError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(WireError::Protocol(format!(
            "field {key:?} must be a string"
        ))),
    }
}

fn optional_u64(value: &Value, key: &str) -> Result<Option<u64>, WireError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| {
            WireError::Protocol(format!("field {key:?} must be a non-negative integer"))
        }),
    }
}

fn parse_input_requests(value: Option<&Value>) -> Result<Vec<InputRequestEntry>, WireError> {
    let map = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Err(WireError::Protocol(
                "task inputRequests must be a JSON object".into(),
            ))
        }
    };
    let mut entries = Vec::with_capacity(map.len());
    for (key, request) in map {
        if key.is_empty() {
            return Err(WireError::Protocol(
                "inputRequests keys must not be empty".into(),
            ));
        }
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| WireError::Protocol(format!("inputRequest {key:?} missing method")))?;
        if !matches!(
            method,
            "sampling/createMessage" | "elicitation/create" | "roots/list"
        ) {
            return Err(WireError::Protocol(format!(
                "disallowed MRTR inputRequest method {method:?} for key {key:?}"
            )));
        }
        let params = match request.get("params") {
            None | Some(Value::Null) if method == "roots/list" => None,
            Some(Value::Object(_)) => request.get("params").cloned(),
            _ => {
                return Err(WireError::Protocol(format!(
                    "invalid params shape on MRTR inputRequest {key:?} ({method})"
                )))
            }
        };
        entries.push(InputRequestEntry {
            key: key.clone(),
            method: method.to_string(),
            params,
        });
    }
    Ok(entries)
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

    #[test]
    fn open_with_capabilities_exchanges_and_retains_server_extensions() {
        let (addr, seen, server) = scripted(vec![
            "HTTP/1.1 200 OK\r\nMcp-Session-Id: ext-1\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"extensions\":{\"io.modelcontextprotocol/tasks\":{\"list\":{}},\"vendor.example/custom\":{\"version\":2}}}}}",
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ]);
        let client_caps = json!({
            "extensions": {
                "io.modelcontextprotocol/tasks": {},
                "vendor.example/client-only": {"enabled": true}
            }
        });
        let session =
            Session::open_with_capabilities(&addr, None, &client_caps, Duration::from_secs(5))
                .unwrap();
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
        let session =
            Session::open_2026_07_28(&addr, Some("tok-1"), Duration::from_secs(5)).unwrap();
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

        // Follow-up without opt-in must omit tasks capability and reject forged CreateTaskResult.
        let forged_err = session
            .call_tool_with_tasks("mission_run", &json!({"x": 2}), false, &mut |_| {})
            .unwrap_err();
        assert!(matches!(forged_err, WireError::Protocol(_)), "{forged_err}");

        // Non-tools/call returning CreateTaskResult must also fail closed.
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

        // tasks/update must NOT retry on transient 503.
        let update_err = session
            .update_task("t-9", &json!({"req-1": {"ok": true}}))
            .unwrap_err();
        assert!(
            matches!(update_err, WireError::Http(503, _)),
            "{update_err}"
        );

        // tasks/cancel IS idempotent and retries 503 -> 200.
        session.cancel_task("t-9").unwrap();

        // 400 JSON-RPC error (-32021) parses into WireError::Rpc without retrying.
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
        // Multi-round-trip flow: working -> input_required (roots/list) -> completed
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

        // Reuse of already-answered inputRequests key fails closed.
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

        // Disallowed MRTR method is rejected.
        let bad_snap = json!({
            "resultType": "complete",
            "taskId": "t-bad",
            "status": "input_required",
            "createdAt": "t0",
            "lastUpdatedAt": "t1",
            "inputRequests": { "evil": { "method": "tools/call", "params": {} } }
        });
        assert!(TaskSnapshot::from_get_result(&bad_snap).is_err());

        // ttlMs deadline stops infinite working loops before sleeping past ttlMs.
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

        let snap = TaskSnapshot::from_get_result(&json!({
            "resultType": "complete",
            "taskId": "task-persist-1",
            "status": "completed",
            "statusMessage": "done",
            "createdAt": "2026-11-15T00:00:00Z",
            "lastUpdatedAt": "2026-11-15T00:00:05Z",
            "ttlMs": 60_000,
            "result": {"ok": true}
        }))
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

        // Corrupt JSON must fail closed instead of wiping store.
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
            // 1. Direct synchronous CallToolResult
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"resultType\":\"complete\",\"content\":[{\"type\":\"text\",\"text\":\"sync-direct\"}],\"isError\":false}}",
            // 2. CreateTaskResult -> Completed with isError: true (must return Ok(CallToolResult))
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"resultType\":\"task\",\"taskId\":\"t-tool-err\",\"status\":\"working\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t0\",\"ttlMs\":5000,\"pollIntervalMs\":5}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-tool-err\",\"status\":\"completed\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t1\",\"ttlMs\":5000,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"tool-fault\"}],\"isError\":true}}}",
            // 3. CreateTaskResult -> Failed (must surface structured WireError::Rpc)
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{\"resultType\":\"task\",\"taskId\":\"t-rpc-err\",\"status\":\"working\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t0\",\"ttlMs\":5000,\"pollIntervalMs\":5}}",
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"jsonrpc\":\"2.0\",\"id\":6,\"result\":{\"resultType\":\"complete\",\"taskId\":\"t-rpc-err\",\"status\":\"failed\",\"statusMessage\":\"rpc crashed\",\"createdAt\":\"t0\",\"lastUpdatedAt\":\"t1\",\"ttlMs\":5000,\"error\":{\"code\":-32603,\"message\":\"downstream crash\"}}}",
            // 4. CreateTaskResult -> Cancelled (must surface WireError::Protocol)
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
}

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

/// Extension identifier for Official MCP Apps (`ext-apps@2026-01-26`).
pub const APPS_EXTENSION_ID: &str = "io.modelcontextprotocol/apps";

/// Maximum cursor pages walked by bounded list pagination (`tools/list`,
/// `resources/list`, `resources/templates/list`, `prompts/list`).
pub const MAX_PAGINATION_PAGES: usize = 50;

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const MAX_IDEMPOTENT_RETRIES: usize = 3;
const MAX_MRTR_ROUNDS: usize = 8;

/// Revisions whose `initialize` and `tools/call` shapes this client speaks.
const SUPPORTED_VERSIONS: [&str; 3] = [PROTOCOL_VERSION, "2025-06-18", "2025-03-26"];

/// Cap on one response body.
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Receive window for the handshake and the closing `DELETE`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Why a call failed.
#[derive(Debug)]
pub enum WireError {
    Io(String),
    Http(u16, String),
    Rpc(i64, String),
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

/// Callback invoked when a server sends an interleaved JSON-RPC request (`HOST-007`).
pub type ServerRequestHandler<'a> = dyn FnMut(&str, &Value) -> Result<Value, WireError> + 'a;

/// Per-call capabilities, progress correlation, and callbacks for [`Session::request_host`].
pub struct HostCall<'a> {
    pub client_capabilities: &'a Value,
    pub progress_token: Option<&'a Value>,
    pub on_notification: &'a mut dyn FnMut(&Value),
    pub on_server_request: &'a mut ServerRequestHandler<'a>,
}

/// Structured HTTP `401` / `403` `WWW-Authenticate` challenge metadata (`HOST-010`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthChallenge {
    pub status: u16,
    pub scheme: String,
    pub realm: Option<String>,
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Headless inspection summary of an MCP Apps `ui://` resource (`HOST-009`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppResourceSummary {
    pub uri: String,
    pub mime_type: Option<String>,
    pub payload_bytes: usize,
    pub csp: Option<Value>,
    pub permissions: Option<Value>,
    pub domain: Option<String>,
    pub meta: Value,
}

/// Live SSE subscription stream opened via `2026-07-28` `subscriptions/listen` (`HOST-005`).
pub struct SubscriptionStream {
    reader: crate::sse::SseReader<BufReader<http::Body>>,
    acknowledged: bool,
}

impl fmt::Debug for SubscriptionStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubscriptionStream")
            .field("acknowledged", &self.acknowledged)
            .finish_non_exhaustive()
    }
}

impl SubscriptionStream {
    #[must_use]
    pub fn is_acknowledged(&self) -> bool {
        self.acknowledged
    }

    /// Read the next JSON-RPC frame from the SSE subscription stream.
    pub fn next_frame(&mut self) -> Option<Result<Value, WireError>> {
        loop {
            let ev = match self.reader.next()? {
                Ok(e) => e,
                Err(e) => return Some(Err(WireError::Io(e.to_string()))),
            };
            let Ok(msg) = serde_json::from_str::<Value>(&ev.data) else {
                continue;
            };
            if msg.get("method").and_then(Value::as_str)
                == Some("notifications/subscriptions/acknowledged")
            {
                self.acknowledged = true;
            }
            return Some(Ok(msg));
        }
    }
}

/// Parse an HTTP `WWW-Authenticate` header from a `401` or `403` response.
#[must_use]
pub fn parse_www_authenticate(status: u16, header_value: &str) -> Option<AuthChallenge> {
    let trimmed = header_value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (scheme, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((s, r)) => (s.trim(), r.trim()),
        None => (trimmed, ""),
    };
    let mut challenge = AuthChallenge {
        status,
        scheme: scheme.to_string(),
        realm: None,
        resource_metadata: None,
        scope: None,
        error: None,
        error_description: None,
    };
    let mut rem = rest;
    while !rem.is_empty() {
        rem = rem.trim_start_matches([',', ' ', '\t']);
        if rem.is_empty() {
            break;
        }
        let Some((raw_key, after_eq)) = rem.split_once('=') else {
            break;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        let after_eq = after_eq.trim_start();
        let (val, next_rem) = if let Some(quoted) = after_eq.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = quoted.char_indices();
            let mut end_idx = quoted.len();
            while let Some((idx, ch)) = chars.next() {
                if ch == '\\' {
                    if let Some((_, esc)) = chars.next() {
                        out.push(esc);
                    }
                } else if ch == '"' {
                    end_idx = idx + 1;
                    break;
                } else {
                    out.push(ch);
                }
            }
            (out, &quoted[end_idx..])
        } else {
            match after_eq.find([',', ' ', '\t']) {
                Some(idx) => (after_eq[..idx].to_string(), &after_eq[idx..]),
                None => (after_eq.to_string(), ""),
            }
        };
        match key.as_str() {
            "realm" => challenge.realm = Some(val),
            "resource_metadata" => challenge.resource_metadata = Some(val),
            "scope" => challenge.scope = Some(val),
            "error" => challenge.error = Some(val),
            "error_description" => challenge.error_description = Some(val),
            _ => {}
        }
        rem = next_rem;
    }
    Some(challenge)
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
    server_info: Value,
    server_capabilities: Value,
    subscribed_uris: BTreeSet<String>,
    last_challenge: std::sync::Mutex<Option<AuthChallenge>>,
    timeout: Duration,
    next_id: u64,
    open: bool,
}

impl Session {
    pub fn open(addr: &str, bearer: Option<&str>, timeout: Duration) -> Result<Self, WireError> {
        Self::open_with_capabilities(addr, bearer, &json!({}), timeout)
    }

    /// Connect to `addr` (`host:port`) while advertising caller-supplied
    /// `capabilities` (for example namespaced `extensions`) in `initialize`,
    pub fn open_with_capabilities(
        addr: &str,
        bearer: Option<&str>,
        capabilities: &Value,
        timeout: Duration,
    ) -> Result<Self, WireError> {
        Self::open_with_client_info(
            addr,
            bearer,
            capabilities,
            ("susi", env!("CARGO_PKG_VERSION")),
            timeout,
        )
    }

    /// Connect to `addr` (`host:port`) with explicit `clientInfo` (`name`, `version`)
    /// and `capabilities`, isolating client identity per server (`HOST-012`).
    pub fn open_with_client_info(
        addr: &str,
        bearer: Option<&str>,
        capabilities: &Value,
        client_info: (&str, &str),
        timeout: Duration,
    ) -> Result<Self, WireError> {
        if !capabilities.is_object() {
            return Err(WireError::Protocol(
                "client capabilities must be a JSON object".into(),
            ));
        }
        if client_info.0.trim().is_empty() || client_info.1.trim().is_empty() {
            return Err(WireError::Protocol(
                "clientInfo name and version must be non-empty".into(),
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
            server_info: json!({}),
            server_capabilities: json!({}),
            subscribed_uris: BTreeSet::new(),
            last_challenge: std::sync::Mutex::new(None),
            timeout,
            next_id: 1,
            open: false,
        };
        let init = session.post(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": PROTOCOL_VERSION, "capabilities": capabilities, "clientInfo": {"name": client_info.0, "version": client_info.1}}}))?;
        session.next_id = 2;
        session.id = init
            .header("mcp-session-id")
            .map(str::to_string)
            .ok_or_else(|| WireError::Protocol(format!("{addr} returned no Mcp-Session-Id")))?;
        session.open = true;
        let result = read_result(init, 1, &mut |_| {})?;
        session.version = negotiated_version(&result)?;
        session.server_info = result
            .get("serverInfo")
            .cloned()
            .unwrap_or_else(|| json!({}));
        session.server_capabilities = result
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| json!({}));
        session.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(session)
    }

    /// Connect to `addr` (`host:port`) on the stateless `2026-07-28` MCP path,
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
            server_info: json!({}),
            server_capabilities: json!({}),
            subscribed_uris: BTreeSet::new(),
            last_challenge: std::sync::Mutex::new(None),
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
        session.server_info = extract_server_info(&discover);
        session.server_capabilities = discover
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| json!({}));
        Ok(session)
    }

    /// Server `Mcp-Session-Id` (`""` on stateless `2026-07-28` sessions).
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.id
    }

    /// The protocol revision the server chose.
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.version
    }

    /// Server `Implementation` metadata (`name`, `version`, optional `title`) from handshake.
    #[must_use]
    pub fn server_info(&self) -> &Value {
        &self.server_info
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

    /// Whether the server advertised the official MCP Apps extension (`apps` or `ui`).
    #[must_use]
    pub fn supports_server_apps(&self) -> bool {
        self.server_extensions().is_some_and(|exts| {
            exts.contains_key(APPS_EXTENSION_ID) || exts.contains_key("io.modelcontextprotocol/ui")
        })
    }

    /// Resource URIs currently subscribed on this session.
    #[must_use]
    pub fn subscribed_resources(&self) -> &BTreeSet<String> {
        &self.subscribed_uris
    }

    /// Most recent `WWW-Authenticate` challenge observed on HTTP `401` / `403`, if any.
    #[must_use]
    pub fn last_auth_challenge(&self) -> Option<AuthChallenge> {
        self.last_challenge
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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
        let params = json!({ "taskId": task_id, "inputResponses": input_responses,});
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
        let response =
            self.post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
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

    /// Execute a host-mode JSON-RPC request that delivers every `notifications/*`
    /// frame (correlating `notifications/progress` to `progress_token` when set)
    /// and dispatches live server-to-client MRTR requests (`roots/list`,
    pub fn request_host(
        &mut self,
        method: &str,
        params: &Value,
        call: HostCall<'_>,
    ) -> Result<Value, WireError> {
        if !call.client_capabilities.is_object() {
            return Err(WireError::Protocol(
                "client capabilities must be a JSON object".into(),
            ));
        }
        let mut req_params = params.clone();
        if let Some(tok) = call.progress_token {
            if let Some(obj) = req_params.as_object_mut() {
                match obj.get_mut("_meta") {
                    Some(Value::Object(m)) => {
                        m.insert("progressToken".into(), tok.clone());
                    }
                    _ => {
                        obj.insert("_meta".into(), json!({ "progressToken": tok }));
                    }
                }
            }
        }
        let params = if self.version == PROTOCOL_VERSION_2026_07_28 {
            with_modern_meta(&req_params, &self.version, call.client_capabilities)?
        } else {
            req_params
        };
        let id = self.next_id;
        self.next_id += 1;
        let response =
            self.post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let streaming = response
            .header("content-type")
            .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("text/event-stream"));
        let result = if streaming {
            self.read_host_sse(BufReader::new(response.body), id, call)?
        } else {
            let mut bytes = Vec::new();
            BufReader::new(response.body.take(MAX_RESPONSE_BYTES + 1)).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_RESPONSE_BYTES {
                return Err(WireError::Protocol(format!(
                    "response exceeds {MAX_RESPONSE_BYTES} bytes"
                )));
            }
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(message) => match classify(&message, id) {
                    Frame::Answer(res) => res?,
                    Frame::Notification | Frame::Other => {
                        return Err(WireError::Protocol(
                            "JSON response does not answer the request id".into(),
                        ))
                    }
                },
                Err(_) => self.read_host_sse(&bytes[..], id, call)?,
            }
        };
        if method != "tools/call"
            && result.get("resultType").and_then(Value::as_str) == Some("task")
        {
            return Err(WireError::Protocol(format!(
                "server returned unexpected CreateTaskResult for non-tool method {method}"
            )));
        }
        Ok(result)
    }

    fn read_host_sse<R: io::BufRead>(
        &self,
        reader: R,
        id: u64,
        call: HostCall<'_>,
    ) -> Result<Value, WireError> {
        let HostCall {
            progress_token,
            on_notification,
            on_server_request,
            ..
        } = call;
        for event in crate::sse::SseReader::new(reader) {
            let event = event?;
            let Ok(message) = serde_json::from_str::<Value>(&event.data) else {
                continue;
            };
            if let Some(srv_method) = message.get("method").and_then(Value::as_str) {
                if let Some(srv_id) = message.get("id") {
                    if !srv_id.is_null() {
                        let srv_params =
                            message.get("params").cloned().unwrap_or_else(|| json!({}));
                        let reply = match on_server_request(srv_method, &srv_params) {
                            Ok(res) => json!({"jsonrpc": "2.0", "id": srv_id, "result": res}),
                            Err(WireError::Rpc(code, msg)) => {
                                json!({ "jsonrpc": "2.0", "id": srv_id, "error": {"code": code, "message": msg}})
                            }
                            Err(other) => {
                                json!({ "jsonrpc": "2.0", "id": srv_id, "error": {"code": -32603, "message": other.to_string()}})
                            }
                        };
                        let _ = self.post(&reply)?;
                        continue;
                    }
                } else if srv_method.starts_with("notifications/") {
                    if srv_method == "notifications/progress" {
                        let tok = message.get("params").and_then(|p| p.get("progressToken"));
                        if let Some(expected_tok) = progress_token {
                            if tok != Some(expected_tok) {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }
                    on_notification(&message);
                    continue;
                }
            }
            if let Frame::Answer(result) = classify(&message, id) {
                return result;
            }
        }
        Err(WireError::Protocol(
            "no matching response frame in SSE stream".into(),
        ))
    }

    /// Return canonical capability discovery document (`server/discover` on `2026-07-28`,
    pub fn discover(&mut self) -> Result<Value, WireError> {
        if self.version == PROTOCOL_VERSION_2026_07_28 {
            let mut doc = self.request_with_capabilities(
                "server/discover",
                &json!({}),
                &json!({}),
                &mut |_| {},
            )?;
            let info = extract_server_info(&doc);
            self.server_info = info.clone();
            self.server_capabilities = doc
                .get("capabilities")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if let Some(obj) = doc.as_object_mut() {
                if !obj.contains_key("serverInfo") {
                    obj.insert("serverInfo".to_string(), info);
                }
            }
            Ok(doc)
        } else {
            Ok(
                json!({ "protocolVersion": self.version, "serverInfo": self.server_info, "capabilities": self.server_capabilities,}),
            )
        }
    }

    fn list_paginated(&mut self, method: &str, array_key: &str) -> Result<Vec<Value>, WireError> {
        let mut params = json!({});
        let mut items = Vec::new();
        let mut seen_cursors = BTreeSet::new();
        for _ in 0..MAX_PAGINATION_PAGES {
            let result = self.request(method, &params, &mut |_| {})?;
            let page = result
                .get(array_key)
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    WireError::Protocol(format!("{method} response missing `{array_key}` array"))
                })?;
            items.extend(page.iter().cloned());
            let cursor = result
                .get("nextCursor")
                .or_else(|| result.get("next_cursor"))
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty());
            match cursor {
                Some(c) => {
                    if !seen_cursors.insert(c.to_string()) {
                        return Err(WireError::Protocol(format!(
                            "{method} repeated pagination cursor {c:?}"
                        )));
                    }
                    params = json!({ "cursor": c });
                }
                None => return Ok(items),
            }
        }
        Err(WireError::Protocol(format!(
            "{method} exceeded {MAX_PAGINATION_PAGES} pagination pages"
        )))
    }

    /// List all tools across cursor pages (`HOST-004`).
    pub fn list_all_tools(&mut self) -> Result<Vec<Value>, WireError> {
        self.list_paginated("tools/list", "tools")
    }

    /// List all static resources across cursor pages (`HOST-005`).
    pub fn list_all_resources(&mut self) -> Result<Vec<Value>, WireError> {
        self.list_paginated("resources/list", "resources")
    }

    /// List all RFC 6570 resource URI templates across cursor pages (`HOST-005`).
    pub fn list_all_resource_templates(&mut self) -> Result<Vec<Value>, WireError> {
        self.list_paginated("resources/templates/list", "resourceTemplates")
    }

    /// Read a resource by URI (`resources/read`, `HOST-005`).
    pub fn read_resource(&mut self, uri: &str) -> Result<Value, WireError> {
        if uri.trim().is_empty() {
            return Err(WireError::Protocol("resource URI must be non-empty".into()));
        }
        self.request("resources/read", &json!({ "uri": uri }), &mut |_| {})
    }

    /// Subscribe to resource updates (`resources/subscribe` on `2025-11-25`..`2025-03-26`,
    pub fn subscribe_resource(&mut self, uri: &str) -> Result<(), WireError> {
        if uri.trim().is_empty() {
            return Err(WireError::Protocol("resource URI must be non-empty".into()));
        }
        if self.version != PROTOCOL_VERSION_2026_07_28 {
            let _ = self.request("resources/subscribe", &json!({ "uri": uri }), &mut |_| {})?;
        }
        self.subscribed_uris.insert(uri.to_string());
        Ok(())
    }

    /// Unsubscribe from resource updates (`resources/unsubscribe`, `HOST-005`).
    pub fn unsubscribe_resource(&mut self, uri: &str) -> Result<(), WireError> {
        if self.version != PROTOCOL_VERSION_2026_07_28 {
            let _ = self.request("resources/unsubscribe", &json!({ "uri": uri }), &mut |_| {})?;
        }
        self.subscribed_uris.remove(uri);
        Ok(())
    }

    /// Open a live `2026-07-28` SSE `subscriptions/listen` stream (`HOST-005`).
    pub fn open_subscription_stream(
        &mut self,
        notifications_filter: &Value,
        client_capabilities: &Value,
    ) -> Result<SubscriptionStream, WireError> {
        if !notifications_filter.is_object() {
            return Err(WireError::Protocol(
                "subscriptions/listen notifications filter must be a JSON object".into(),
            ));
        }
        let raw_params = json!({ "notifications": notifications_filter });
        let params = if self.version == PROTOCOL_VERSION_2026_07_28 {
            with_modern_meta(&raw_params, &self.version, client_capabilities)?
        } else {
            raw_params
        };
        let id = self.next_id;
        self.next_id += 1;
        let response = self.post(&json!({ "jsonrpc": "2.0", "id": id, "method": "subscriptions/listen", "params": params}))?;
        let streaming = response
            .header("content-type")
            .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("text/event-stream"));
        if !streaming {
            let result = read_result(response, id, &mut |_| {})?;
            return Err(WireError::Protocol(format!(
                "expected text/event-stream for subscriptions/listen, got JSON {result}"
            )));
        }
        let mut stream = SubscriptionStream {
            reader: crate::sse::SseReader::new(BufReader::new(response.body)),
            acknowledged: false,
        };
        match stream.next_frame() {
            Some(Ok(frame)) => {
                if let Some(err) = frame.get("error") {
                    let code = err.get("code").and_then(Value::as_i64).unwrap_or(-32603);
                    let msg = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("subscriptions/listen rejected")
                        .to_string();
                    return Err(WireError::Rpc(code, msg));
                }
                if !stream.acknowledged {
                    return Err(WireError::Protocol(format!(
                        "subscriptions/listen first frame was not acknowledged: {frame}"
                    )));
                }
                Ok(stream)
            }
            Some(Err(e)) => Err(e),
            None => Err(WireError::Protocol(
                "subscriptions/listen stream closed before acknowledgement".into(),
            )),
        }
    }

    /// List all prompts across cursor pages (`HOST-006`).
    pub fn list_all_prompts(&mut self) -> Result<Vec<Value>, WireError> {
        self.list_paginated("prompts/list", "prompts")
    }

    /// Materialize a named prompt (`prompts/get`, `HOST-006`).
    pub fn get_prompt(
        &mut self,
        name: &str,
        arguments: Option<&Value>,
    ) -> Result<Value, WireError> {
        if name.trim().is_empty() {
            return Err(WireError::Protocol("prompt name must be non-empty".into()));
        }
        let params = match arguments {
            Some(args) => json!({ "name": name, "arguments": args }),
            None => json!({ "name": name }),
        };
        self.request("prompts/get", &params, &mut |_| {})
    }

    /// Complete an argument on a prompt (`completion/complete`, `ref/prompt`, `HOST-006`).
    pub fn complete_prompt(
        &mut self,
        prompt_name: &str,
        arg_name: &str,
        arg_value: &str,
        context_args: Option<&Value>,
    ) -> Result<Value, WireError> {
        let mut params = json!({ "ref": { "type": "ref/prompt", "name": prompt_name}, "argument": { "name": arg_name, "value": arg_value}});
        if let Some(ctx) = context_args {
            params["context"] = json!({ "arguments": ctx });
        }
        self.request("completion/complete", &params, &mut |_| {})
    }

    /// Complete an argument on a resource URI template (`completion/complete`, `ref/resource`, `HOST-006`).
    pub fn complete_resource(
        &mut self,
        template_uri: &str,
        arg_name: &str,
        arg_value: &str,
        context_args: Option<&Value>,
    ) -> Result<Value, WireError> {
        let mut params = json!({ "ref": { "type": "ref/resource", "uri": template_uri}, "argument": { "name": arg_name, "value": arg_value}});
        if let Some(ctx) = context_args {
            params["context"] = json!({ "arguments": ctx });
        }
        self.request("completion/complete", &params, &mut |_| {})
    }

    /// Headlessly inspect an MCP Apps `ui://` resource (`HOST-009`).
    pub fn inspect_app_resource(&mut self, uri: &str) -> Result<AppResourceSummary, WireError> {
        if !uri.starts_with("ui://") {
            return Err(WireError::Protocol(format!(
                "MCP Apps resource URI must use `ui://` scheme, got {uri:?}"
            )));
        }
        let read = self.read_resource(uri)?;
        parse_app_resource_summary(uri, &read)
    }

    /// Invoke `tools/call` resolving `2026-07-28` inline MRTR turn-arounds
    /// (`resultType: "input_required"` -> `requestState` + `inputResponses`) with
    /// per-call key deduplication and fail-closed `CreateTaskResult` rejection (`HOST-007`).
    pub fn call_tool_with_mrtr<F>(
        &mut self,
        name: &str,
        args: &Value,
        client_capabilities: &Value,
        mut on_input_requests: F,
    ) -> Result<Value, WireError>
    where
        F: FnMut(&[InputRequestEntry]) -> Result<Value, WireError>,
    {
        let mut request_state: Option<Value> = None;
        let mut input_responses: Option<Value> = None;
        let mut answered = BTreeSet::new();
        for _ in 0..MAX_MRTR_ROUNDS {
            let mut params = json!({ "name": name, "arguments": args });
            if let Some(st) = request_state.take() {
                params["requestState"] = st;
            }
            if let Some(ir) = input_responses.take() {
                params["inputResponses"] = ir;
            }
            let res = self.request_with_capabilities(
                "tools/call",
                &params,
                client_capabilities,
                &mut |_| {},
            )?;
            match res.get("resultType").and_then(Value::as_str) {
                Some("task") => {
                    return Err(WireError::Protocol(
                        "server returned unexpected CreateTaskResult during synchronous MRTR call"
                            .into(),
                    ))
                }
                Some("input_required") => {
                    let entries = parse_input_requests(res.get("inputRequests"))?;
                    if entries.is_empty() {
                        return Err(WireError::Protocol(
                            "input_required tool result carried no inputRequests".into(),
                        ));
                    }
                    for e in &entries {
                        if answered.contains(&e.key) {
                            return Err(WireError::Protocol(format!(
                                "server reused answered MRTR key {:?}",
                                e.key
                            )));
                        }
                    }
                    let responses = on_input_requests(&entries)?;
                    let Some(obj) = responses.as_object() else {
                        return Err(WireError::Protocol(
                            "MRTR callback must return a JSON object".into(),
                        ));
                    };
                    let pending: BTreeSet<&str> = entries.iter().map(|e| e.key.as_str()).collect();
                    for k in obj.keys() {
                        if !pending.contains(k.as_str()) {
                            return Err(WireError::Protocol(format!(
                                "MRTR response key {k:?} was not pending"
                            )));
                        }
                        answered.insert(k.clone());
                    }
                    request_state = res.get("requestState").cloned();
                    input_responses = Some(responses);
                }
                _ => return Ok(res),
            }
        }
        Err(WireError::Protocol(format!(
            "exceeded {MAX_MRTR_ROUNDS} MRTR turn-around rounds for tool {name}"
        )))
    }

    /// Emit cooperative `notifications/cancelled` for an in-flight JSON-RPC `request_id` (`HOST-011`).
    pub fn notify_cancelled(
        &mut self,
        request_id: Value,
        reason: Option<&str>,
    ) -> Result<(), WireError> {
        let params = match reason {
            Some(r) => json!({ "requestId": request_id, "reason": r }),
            None => json!({ "requestId": request_id }),
        };
        let _ = self.post(
            &json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": params}),
        )?;
        Ok(())
    }

    /// Emit `notifications/roots/list_changed` on this session (`HOST-007`).
    pub fn notify_roots_list_changed(&mut self) -> Result<(), WireError> {
        let _ =
            self.post(&json!({ "jsonrpc": "2.0", "method": "notifications/roots/list_changed"}))?;
        Ok(())
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
            if matches!(response.status, 401 | 403) {
                let ch = response
                    .header("www-authenticate")
                    .and_then(|v| parse_www_authenticate(response.status, v));
                *self
                    .last_challenge
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = ch;
            }
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
    pub extra: Map<String, Value>,
}

impl PersistedTaskHandle {
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
            extra: Map::new(),
        }
    }

    /// Reconstruct a live [`TaskHandle`] from this persisted record for crash/restart resume.
    #[must_use]
    pub fn to_task_handle(&self) -> TaskHandle {
        TaskHandle {
            task_id: self.task_id.clone(),
            status: self.status,
            status_message: self.status_message.clone(),
            created_at: self.created_at.clone(),
            last_updated_at: self.last_updated_at.clone(),
            ttl_ms: self.ttl_ms,
            poll_interval_ms: self.poll_interval_ms,
        }
    }

    fn to_json(&self) -> Value {
        let mut map = self.extra.clone();
        map.insert("addr".into(), json!(self.addr));
        map.insert("taskId".into(), json!(self.task_id));
        map.insert("protocolVersion".into(), json!(self.protocol_version));
        map.insert("status".into(), json!(self.status.as_str()));
        map.insert("statusMessage".into(), json!(self.status_message));
        map.insert("createdAt".into(), json!(self.created_at));
        map.insert("lastUpdatedAt".into(), json!(self.last_updated_at));
        map.insert("ttlMs".into(), json!(self.ttl_ms));
        map.insert("pollIntervalMs".into(), json!(self.poll_interval_ms));
        Value::Object(map)
    }

    fn from_json(value: &Value) -> Result<Self, WireError> {
        let addr = required_nonempty_str(value, "addr")?;
        let protocol_version = required_nonempty_str(value, "protocolVersion")?;
        let handle = TaskHandle::from_fields(value)?;
        let mut extra = Map::new();
        if let Some(obj) = value.as_object() {
            for (k, v) in obj {
                if !matches!(
                    k.as_str(),
                    "addr"
                        | "taskId"
                        | "protocolVersion"
                        | "status"
                        | "statusMessage"
                        | "createdAt"
                        | "lastUpdatedAt"
                        | "ttlMs"
                        | "pollIntervalMs"
                ) {
                    extra.insert(k.clone(), v.clone());
                }
            }
        }
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
            extra,
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

    /// Open `path` fail-closed, or quarantine a corrupt/invalid store file
    /// atomically to a sibling `.corrupt` snapshot and recover an empty store.
    pub fn open_or_quarantine(
        path: impl Into<PathBuf>,
    ) -> Result<(Self, Option<PathBuf>), WireError> {
        let path = path.into();
        match Self::open(&path) {
            Ok(store) => Ok((store, None)),
            Err(WireError::Protocol(_)) => {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("task-handles.json");
                let quarantined =
                    path.with_file_name(format!("{name}.corrupt.{}.{nanos}", std::process::id()));
                fs::rename(&path, &quarantined).map_err(|e| WireError::Io(e.to_string()))?;
                Ok((
                    Self {
                        path,
                        handles: Vec::new(),
                    },
                    Some(quarantined),
                ))
            }
            Err(other) => Err(other),
        }
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
        let payload = serde_json::to_vec_pretty(&json!({ "version": 1, "handles": self.handles.iter().map(PersistedTaskHandle::to_json).collect::<Vec<_>>(),}))
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
        let mut open_opts = fs::OpenOptions::new();
        open_opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open_opts.mode(0o600);
        }
        let mut file = open_opts
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
    Answer(Result<Value, WireError>),
    Notification,
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

pub(crate) fn parse_app_resource_summary(
    uri: &str,
    read: &Value,
) -> Result<AppResourceSummary, WireError> {
    let first = read
        .get("contents")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .ok_or_else(|| {
            WireError::Protocol(format!("resources/read for {uri} returned empty contents"))
        })?;
    let payload_bytes = first.get("text").and_then(Value::as_str).map_or_else(
        || {
            first
                .get("blob")
                .and_then(Value::as_str)
                .map_or(0, str::len)
        },
        str::len,
    );
    let meta = first.get("_meta").cloned().unwrap_or_else(|| json!({}));
    let ui_meta = meta.get("ui").unwrap_or(&meta);
    let csp = match ui_meta.get("csp") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Object(_)) => Some(v.clone()),
        Some(_) => {
            return Err(WireError::Protocol(format!(
                "MCP Apps {uri} `_meta.ui.csp` must be a JSON object"
            )))
        }
    };
    let permissions = match ui_meta
        .get("permissions")
        .or_else(|| ui_meta.get("sandbox"))
    {
        None | Some(Value::Null) => None,
        Some(v @ Value::Object(_)) => Some(v.clone()),
        Some(_) => {
            return Err(WireError::Protocol(format!(
                "MCP Apps {uri} `_meta.ui.permissions` must be a JSON object"
            )))
        }
    };
    let domain = match ui_meta.get("domain") {
        None | Some(Value::Null) => None,
        Some(Value::String(d))
            if !d.is_empty()
                && !d.contains("..")
                && d.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')) =>
        {
            Some(d.clone())
        }
        Some(_) => {
            return Err(WireError::Protocol(format!(
                "MCP Apps {uri} `_meta.ui.domain` must be a valid hostname"
            )))
        }
    };
    Ok(AppResourceSummary {
        uri: uri.to_string(),
        mime_type: first
            .get("mimeType")
            .and_then(Value::as_str)
            .map(str::to_string),
        payload_bytes,
        csp,
        permissions,
        domain,
        meta,
    })
}

fn extract_server_info(doc: &Value) -> Value {
    doc.get("serverInfo")
        .or_else(|| {
            doc.get("_meta")
                .and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo"))
        })
        .cloned()
        .unwrap_or_else(|| json!({}))
}

#[cfg(test)]
#[path = "tests/mcp.rs"]
mod tests;

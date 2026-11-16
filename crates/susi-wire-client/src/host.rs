//! Multi-server MCP host runtime (`VC-203-017`, `HOST-001..HOST-018`).
//!
//! [`McpHost`] connects to multiple concurrent MCP servers over stateless
//! `2026-07-28` Streamable HTTP, stateful `2025-11-25`..`2025-03-26` Streamable
//! HTTP, and line-delimited JSON-RPC 2.0 stdio/duplex streams while strictly
//! isolating per-server client identity, bearer credentials, protocol version,
//! `Mcp-Session-Id`, `file://` roots, MRTR callback handlers (`roots/list`,
//! `sampling/createMessage`, `elicitation/create`), subscriptions, notifications,
//! and durable task handles.

use crate::http::{self, Request};
use crate::mcp::{
    parse_app_resource_summary, parse_www_authenticate, AppResourceSummary, AuthChallenge,
    CallToolOutcome, HostCall, InputRequestEntry, PersistedTaskHandle, PersistedTaskHandleStore,
    Session, SubscriptionStream, TaskHandle, TaskSnapshot, TaskStatus, WireError,
    APPS_EXTENSION_ID, MAX_PAGINATION_PAGES, PROTOCOL_VERSION, TASKS_EXTENSION_ID,
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};
use susi_paths::http_body::read_line_capped;

/// Per-server client identity advertised by [`McpHost`] (`HOST-012`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostClientIdentity {
    pub name: String,
    pub version: String,
    pub principal: Option<String>,
}

impl Default for HostClientIdentity {
    fn default() -> Self {
        Self {
            name: "susi-host".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            principal: None,
        }
    }
}

/// Confined `file://` root exposed to a specific server via `roots/list` (`HOST-007`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootEntry {
    pub uri: String,
    pub name: Option<String>,
}

impl RootEntry {
    pub fn new(uri: impl Into<String>, name: Option<String>) -> Result<Self, WireError> {
        let uri = uri.into();
        if !uri.starts_with("file://") {
            return Err(WireError::Protocol(format!(
                "root URI must use `file://` scheme, got {uri:?}"
            )));
        }
        let path_part = &uri["file://".len()..];
        if path_part.is_empty() || path_part.split(['/', '\\']).any(|seg| seg == "..") {
            return Err(WireError::Protocol(format!(
                "root URI contains invalid or traversal path: {uri:?}"
            )));
        }
        Ok(Self { uri, name })
    }

    fn to_json(&self) -> Value {
        match &self.name {
            Some(n) => json!({ "uri": self.uri, "name": n }),
            None => json!({ "uri": self.uri }),
        }
    }
}

/// Callback invoked when a server requests `sampling/createMessage` (`HOST-007`).
pub type SamplingCallback = Box<dyn FnMut(&Value) -> Result<Value, WireError> + Send>;
/// Callback invoked when a server requests `elicitation/create` (`HOST-007`).
pub type ElicitationCallback = Box<dyn FnMut(&Value) -> Result<Value, WireError> + Send>;

/// Per-server capability, identity, and MRTR callback policy (`HOST-007`, `HOST-012`).
#[derive(Default)]
pub struct HostPolicy {
    pub identity: HostClientIdentity,
    roots: Option<Vec<RootEntry>>,
    sampling: Option<SamplingCallback>,
    elicitation: Option<ElicitationCallback>,
    pub enable_tasks: bool,
    pub enable_apps: bool,
    pub extra_extensions: Map<String, Value>,
}

impl fmt::Debug for HostPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostPolicy")
            .field("identity", &self.identity)
            .field("roots", &self.roots)
            .field("has_sampling", &self.sampling.is_some())
            .field("has_elicitation", &self.elicitation.is_some())
            .field("enable_tasks", &self.enable_tasks)
            .field("enable_apps", &self.enable_apps)
            .finish()
    }
}

impl HostPolicy {
    #[must_use]
    pub fn strict(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            identity: HostClientIdentity {
                name: name.into(),
                version: version.into(),
                principal: None,
            },
            ..Self::default()
        }
    }

    #[must_use]
    pub fn clone_meta(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            roots: self.roots.clone(),
            sampling: None,
            elicitation: None,
            enable_tasks: self.enable_tasks,
            enable_apps: self.enable_apps,
            extra_extensions: self.extra_extensions.clone(),
        }
    }

    #[must_use]
    pub fn with_principal(mut self, principal: impl Into<String>) -> Self {
        self.identity.principal = Some(principal.into());
        self
    }

    #[must_use]
    pub fn with_roots(mut self, roots: Vec<RootEntry>) -> Self {
        self.roots = Some(roots);
        self
    }

    #[must_use]
    pub fn with_sampling<F>(mut self, handler: F) -> Self
    where
        F: FnMut(&Value) -> Result<Value, WireError> + Send + 'static,
    {
        self.sampling = Some(Box::new(handler));
        self
    }

    #[must_use]
    pub fn with_elicitation<F>(mut self, handler: F) -> Self
    where
        F: FnMut(&Value) -> Result<Value, WireError> + Send + 'static,
    {
        self.elicitation = Some(Box::new(handler));
        self
    }

    #[must_use]
    pub fn with_tasks(mut self, enabled: bool) -> Self {
        self.enable_tasks = enabled;
        self
    }

    #[must_use]
    pub fn with_apps(mut self, enabled: bool) -> Self {
        self.enable_apps = enabled;
        self
    }

    #[must_use]
    pub fn with_extension(mut self, id: impl Into<String>, settings: Value) -> Self {
        self.extra_extensions.insert(id.into(), settings);
        self
    }

    #[must_use]
    pub fn advertised_capabilities(&self) -> Value {
        let mut caps = Map::new();
        if self.roots.is_some() {
            caps.insert("roots".into(), json!({ "listChanged": true }));
        }
        if self.sampling.is_some() {
            caps.insert("sampling".into(), json!({}));
        }
        if self.elicitation.is_some() {
            caps.insert("elicitation".into(), json!({ "form": {}, "url": {} }));
        }
        let mut exts = self.extra_extensions.clone();
        if self.enable_tasks {
            exts.insert(TASKS_EXTENSION_ID.into(), json!({}));
        }
        if self.enable_apps {
            exts.insert(APPS_EXTENSION_ID.into(), json!({}));
        }
        if !exts.is_empty() {
            caps.insert("extensions".into(), Value::Object(exts));
        }
        Value::Object(caps)
    }

    pub fn dispatch_mrtr(&mut self, method: &str, params: &Value) -> Result<Value, WireError> {
        match method {
            "roots/list" => {
                let Some(roots) = &self.roots else {
                    return Err(WireError::Rpc(
                        -32601,
                        "client did not advertise `roots` capability".into(),
                    ));
                };
                Ok(json!({ "roots": roots.iter().map(RootEntry::to_json).collect::<Vec<_>>() }))
            }
            "sampling/createMessage" => {
                let Some(cb) = &mut self.sampling else {
                    return Err(WireError::Rpc(
                        -32601,
                        "client did not advertise `sampling` capability".into(),
                    ));
                };
                if params.get("messages").and_then(Value::as_array).is_none() {
                    return Err(WireError::Rpc(
                        -32602,
                        "sampling/createMessage requires a `messages` array".into(),
                    ));
                }
                cb(params)
            }
            "elicitation/create" => {
                let Some(cb) = &mut self.elicitation else {
                    return Err(WireError::Rpc(
                        -32601,
                        "client did not advertise `elicitation` capability".into(),
                    ));
                };
                cb(params)
            }
            "ping" => Ok(json!({})),
            other => Err(WireError::Rpc(
                -32601,
                format!("unsupported server-to-client method `{other}`"),
            )),
        }
    }

    pub fn fulfill_input_requests(
        &mut self,
        entries: &[InputRequestEntry],
    ) -> Result<Value, WireError> {
        let mut out = Map::new();
        let empty = json!({});
        for entry in entries {
            let val = self.dispatch_mrtr(&entry.method, entry.params.as_ref().unwrap_or(&empty))?;
            let normalized = if entry.method == "elicitation/create"
                && val.get("action").and_then(Value::as_str) == Some("accept")
            {
                val.get("content").cloned().unwrap_or(val)
            } else {
                val
            };
            out.insert(entry.key.clone(), normalized);
        }
        Ok(Value::Object(out))
    }
}

/// HTTP endpoint descriptor for [`McpHost::connect_http_2026_07_28`] and [`McpHost::connect_http_session`].
#[derive(Debug, Clone, Copy)]
pub struct HttpEndpoint<'a> {
    pub addr: &'a str,
    pub bearer: Option<&'a str>,
    pub timeout: Duration,
}

/// Bidirectional stream bundle for [`McpHost::connect_duplex`].
pub struct DuplexStreams {
    pub reader: Box<dyn BufRead + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Invoked exactly once when connection ownership ends, including failed setup.
    pub on_close: Option<Box<dyn FnOnce() + Send>>,
}

/// Argument completion request parameters for [`McpHost::complete`].
#[derive(Debug, Clone, Copy)]
pub struct CompletionQuery<'a> {
    pub is_resource: bool,
    pub target: &'a str,
    pub arg_name: &'a str,
    pub arg_value: &'a str,
    pub context_args: Option<&'a Value>,
}

/// Bidirectional line-delimited JSON-RPC 2.0 peer for stdio and duplex pipes (`HOST-001`).
pub struct DuplexPeer {
    reader: Box<dyn BufRead + Send>,
    writer: Box<dyn Write + Send>,
    on_close: Option<Box<dyn FnOnce() + Send>>,
    version: String,
    server_info: Value,
    server_capabilities: Value,
    subscribed_uris: BTreeSet<String>,
    next_id: u64,
    oversized_frame: bool,
}

impl fmt::Debug for DuplexPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DuplexPeer")
            .field("version", &self.version)
            .field("server_info", &self.server_info)
            .finish_non_exhaustive()
    }
}

impl Drop for DuplexPeer {
    fn drop(&mut self) {
        self.close();
    }
}

impl DuplexPeer {
    fn close(&mut self) {
        if let Some(close) = self.on_close.take() {
            close();
        }
    }
    fn new(streams: DuplexStreams) -> Self {
        Self {
            reader: streams.reader,
            writer: streams.writer,
            on_close: streams.on_close,
            version: String::new(),
            server_info: Value::Null,
            server_capabilities: Value::Null,
            subscribed_uris: BTreeSet::new(),
            next_id: 2,
            oversized_frame: false,
        }
    }
    fn ensure_open(&self) -> Result<(), WireError> {
        if self.oversized_frame {
            return Err(WireError::Protocol(
                "duplex JSON-RPC stream is closed after an oversized line".into(),
            ));
        }
        Ok(())
    }
    fn initialize(
        &mut self,
        policy: &mut HostPolicy,
        notifications: &mut Vec<Value>,
    ) -> Result<(), WireError> {
        let caps = policy.advertised_capabilities();
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION,"capabilities":caps,"clientInfo":{"name":policy.identity.name,"version":policy.identity.version}}});
        send_line(&mut *self.writer, &init)?;
        let result = recv_until_id(
            (
                &mut *self.reader,
                &mut *self.writer,
                &mut self.oversized_frame,
            ),
            1,
            None,
            (policy, notifications),
        )?;
        let raw_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                WireError::Protocol("duplex initialize result carries no protocolVersion".into())
            })?;
        if !matches!(
            raw_version,
            "2026-07-28" | "2025-11-25" | "2025-06-18" | "2025-03-26"
        ) {
            return Err(WireError::Protocol(format!(
                "unsupported duplex MCP protocol version {raw_version:?}"
            )));
        }
        self.version = raw_version.to_string();
        self.server_info = result
            .get("serverInfo")
            .cloned()
            .unwrap_or_else(|| json!({}));
        self.server_capabilities = result
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| json!({}));
        send_line(
            &mut *self.writer,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )?;
        Ok(())
    }

    fn request(
        &mut self,
        method: &str,
        params: &Value,
        progress_token: Option<&Value>,
        ctx: (&mut HostPolicy, &mut Vec<Value>),
    ) -> Result<Value, WireError> {
        self.ensure_open()?;
        let mut req_params = params.clone();
        if let (Some(tok), Some(obj)) = (progress_token, req_params.as_object_mut()) {
            match obj.get_mut("_meta") {
                Some(Value::Object(m)) => {
                    m.insert("progressToken".into(), tok.clone());
                }
                _ => {
                    obj.insert("_meta".into(), json!({ "progressToken": tok }));
                }
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        send_line(
            &mut *self.writer,
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":req_params}),
        )?;
        let result = recv_until_id(
            (
                &mut *self.reader,
                &mut *self.writer,
                &mut self.oversized_frame,
            ),
            id,
            progress_token,
            ctx,
        );
        if self.oversized_frame {
            self.close();
        }
        result
    }

    fn notify(&mut self, method: &str, params: Option<Value>) -> Result<(), WireError> {
        self.ensure_open()?;
        let msg = match params {
            Some(p) => json!({"jsonrpc":"2.0","method":method,"params":p}),
            None => json!({"jsonrpc":"2.0","method":method}),
        };
        send_line(&mut *self.writer, &msg)
    }
}

/// Maximum buffered notifications retained per hosted server (`HOST-011`).
pub const MAX_BUFFERED_NOTIFICATIONS: usize = 256;
const MAX_INTERLEAVED_FRAMES: usize = 1024;
const MAX_DUPLEX_LINE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn push_bounded_notification(buf: &mut Vec<Value>, msg: Value) {
    if buf.len() >= MAX_BUFFERED_NOTIFICATIONS {
        buf.remove(0);
    }
    buf.push(msg);
}

fn send_line(writer: &mut dyn Write, msg: &Value) -> Result<(), WireError> {
    let text = serde_json::to_string(msg).map_err(|e| WireError::Protocol(e.to_string()))?;
    writeln!(writer, "{text}")?;
    writer.flush()?;
    Ok(())
}

fn recv_until_id(
    io: (&mut dyn BufRead, &mut dyn Write, &mut bool),
    expected_id: u64,
    progress_token: Option<&Value>,
    ctx: (&mut HostPolicy, &mut Vec<Value>),
) -> Result<Value, WireError> {
    let (mut reader, writer, oversized_frame) = io;
    let (policy, notifications) = ctx;
    let mut line = Vec::new();
    let mut seen_srv_ids = BTreeSet::new();
    let mut frames_seen = 0usize;
    loop {
        line.clear();
        // Acquire at most the accepted limit plus one byte, including without a
        // newline. Oversize poisons the connection: its tail is never reused.
        // This bounds memory, not time spent in a blocking reader or cleanup.
        let read = read_line_capped(&mut reader, &mut line, (MAX_DUPLEX_LINE_BYTES + 1) as u64);
        if line.len() > MAX_DUPLEX_LINE_BYTES {
            *oversized_frame = true;
            return Err(WireError::Protocol(
                "duplex JSON-RPC line exceeded 16 MiB".into(),
            ));
        }
        read?;
        if line.is_empty() {
            return Err(WireError::Io("EOF reading JSON-RPC peer stream".into()));
        }
        let text = std::str::from_utf8(&line)
            .map_err(|_| WireError::Io("stream did not contain valid UTF-8".into()))?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        frames_seen += 1;
        if frames_seen > MAX_INTERLEAVED_FRAMES {
            return Err(WireError::Protocol(format!(
                "peer exceeded {MAX_INTERLEAVED_FRAMES} interleaved frames before answering id {expected_id}"
            )));
        }
        let msg: Value = serde_json::from_str(trimmed)
            .map_err(|e| WireError::Protocol(format!("invalid JSON-RPC frame: {e}")))?;
        if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(WireError::Protocol(
                "JSON-RPC frame must carry `\"jsonrpc\": \"2.0\"`".into(),
            ));
        }
        if let Some(srv_method) = msg.get("method").and_then(Value::as_str) {
            if let Some(srv_id) = msg.get("id").filter(|id| !id.is_null()) {
                let id_key = srv_id.to_string();
                if !seen_srv_ids.insert(id_key.clone()) {
                    return Err(WireError::Protocol(format!(
                        "duplicate server-originated JSON-RPC request id {id_key}"
                    )));
                }
                let srv_params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
                let reply = match policy.dispatch_mrtr(srv_method, &srv_params) {
                    Ok(res) => json!({"jsonrpc":"2.0","id":srv_id,"result":res}),
                    Err(WireError::Rpc(code, err_msg)) => {
                        json!({"jsonrpc":"2.0","id":srv_id,"error":{"code":code,"message":err_msg}})
                    }
                    Err(other) => {
                        json!({"jsonrpc":"2.0","id":srv_id,"error":{"code":-32603,"message":other.to_string()}})
                    }
                };
                send_line(writer, &reply)?;
                continue;
            } else if srv_method.starts_with("notifications/") {
                if srv_method == "notifications/progress" {
                    let tok = msg.get("params").and_then(|p| p.get("progressToken"));
                    if progress_token.is_none() || tok != progress_token {
                        continue;
                    }
                }
                push_bounded_notification(notifications, msg);
                continue;
            }
        }
        if msg.get("id").and_then(Value::as_u64) != Some(expected_id) {
            continue;
        }
        if let Some(err) = msg.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(-32603);
            let text = err
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| err.to_string(), str::to_string);
            return Err(WireError::Rpc(code, text));
        }
        return msg
            .get("result")
            .cloned()
            .ok_or_else(|| WireError::Protocol("JSON-RPC frame missing result".into()));
    }
}

enum PeerBackend {
    Http(Box<Session>, String),
    Duplex(DuplexPeer),
}

struct HostedServer {
    backend: PeerBackend,
    policy: HostPolicy,
    notifications: Vec<Value>,
    task_store: Option<PersistedTaskHandleStore>,
}

struct ListBudget {
    items: usize,
    bytes: usize,
}

impl Write for ListBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("catalog byte limit exceeded"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl HostedServer {
    fn list_paginated(&mut self, method: &str, key: &str) -> Result<Vec<Value>, WireError> {
        self.list_paginated_with_budget(method, key, None)
    }

    fn list_paginated_with_budget(
        &mut self,
        method: &str,
        key: &str,
        mut budget: Option<ListBudget>,
    ) -> Result<Vec<Value>, WireError> {
        match &mut self.backend {
            PeerBackend::Http(_, _) if budget.is_some() => Err(WireError::Protocol(
                "bounded tool listing is only supported for duplex servers".into(),
            )),
            PeerBackend::Http(s, _) => match (method, key) {
                ("tools/list", _) => s.list_all_tools(),
                ("resources/list", _) => s.list_all_resources(),
                ("resources/templates/list", _) => s.list_all_resource_templates(),
                _ => s.list_all_prompts(),
            },
            PeerBackend::Duplex(p) => {
                let mut params = json!({});
                let mut items = Vec::new();
                let mut seen = BTreeSet::new();
                for _ in 0..MAX_PAGINATION_PAGES {
                    let res = p.request(
                        method,
                        &params,
                        None,
                        (&mut self.policy, &mut self.notifications),
                    )?;
                    let page = res.get(key).and_then(Value::as_array).ok_or_else(|| {
                        WireError::Protocol(format!("{method} response missing `{key}`"))
                    })?;
                    if let Some(budget) = &mut budget {
                        budget.items = budget.items.checked_sub(page.len()).ok_or_else(|| {
                            WireError::Protocol(format!("{method} exceeded catalog item limit"))
                        })?;
                        for item in page {
                            serde_json::to_writer(&mut *budget, item).map_err(|_| {
                                WireError::Protocol(format!("{method} exceeded catalog byte limit"))
                            })?;
                        }
                    }
                    items.extend(page.iter().cloned());
                    let cursor = res
                        .get("nextCursor")
                        .or_else(|| res.get("next_cursor"))
                        .and_then(Value::as_str)
                        .filter(|c| !c.is_empty());
                    match cursor {
                        Some(c) if seen.insert(c.to_string()) => params = json!({ "cursor": c }),
                        Some(c) => {
                            return Err(WireError::Protocol(format!(
                                "{method} repeated pagination cursor {c:?}"
                            )))
                        }
                        None => return Ok(items),
                    }
                }
                Err(WireError::Protocol(format!(
                    "{method} exceeded {MAX_PAGINATION_PAGES} pagination pages"
                )))
            }
        }
    }
}

/// Probe an HTTP MCP endpoint's `401` / `403` `WWW-Authenticate` challenge (`HOST-010`).
pub fn probe_http_auth_challenge(
    addr: &str,
    bearer: Option<&str>,
    timeout: Duration,
) -> Result<Option<AuthChallenge>, WireError> {
    let (host, port_str) = addr
        .rsplit_once(':')
        .ok_or_else(|| WireError::Protocol(format!("invalid host:port {addr:?}")))?;
    let port: u16 = port_str
        .parse()
        .map_err(|_| WireError::Protocol(format!("invalid port in {addr:?}")))?;
    let host_clean = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let body = br#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{}}"#;
    let mut headers = vec![
        ("Accept", "application/json, text/event-stream".to_string()),
        ("Content-Type", "application/json".to_string()),
        ("MCP-Protocol-Version", "2026-07-28".to_string()),
        ("Mcp-Method", "server/discover".to_string()),
    ];
    if let Some(tok) = bearer {
        headers.push(("Authorization", format!("Bearer {tok}")));
    }
    let refs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let resp = http::request_streaming_by(
        host_clean,
        port,
        Request {
            method: "POST",
            path: "/mcp",
            headers: &refs,
            body: Some(body),
        },
        timeout,
        Some(Instant::now() + timeout),
    )
    .map_err(WireError::Io)?;
    if matches!(resp.status, 401 | 403) {
        return Ok(resp
            .header("www-authenticate")
            .and_then(|v| parse_www_authenticate(resp.status, v)));
    }
    Ok(None)
}

/// Canonical multi-server MCP host managing isolated server connections (`VC-203-017`).
#[derive(Default)]
pub struct McpHost {
    servers: BTreeMap<String, HostedServer>,
}

impl fmt::Debug for McpHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpHost")
            .field("server_ids", &self.server_ids())
            .finish()
    }
}

impl McpHost {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_vacant(&self, id: &str) -> Result<(), WireError> {
        if self.servers.contains_key(id) {
            return Err(WireError::Protocol(format!(
                "server `{id}` is already connected"
            )));
        }
        Ok(())
    }

    pub fn connect_http_2026_07_28(
        &mut self,
        server_id: impl Into<String>,
        endpoint: HttpEndpoint<'_>,
        policy: HostPolicy,
    ) -> Result<Value, WireError> {
        let id = server_id.into();
        self.ensure_vacant(&id)?;
        let session = Session::open_2026_07_28(endpoint.addr, endpoint.bearer, endpoint.timeout)?;
        let summary = json!({"serverId":id,"transport":"streamable-http","protocolVersion":session.protocol_version(),"serverInfo":session.server_info(),"capabilities":session.server_capabilities()});
        self.servers.insert(
            id,
            HostedServer {
                backend: PeerBackend::Http(Box::new(session), endpoint.addr.to_string()),
                policy,
                notifications: Vec::new(),
                task_store: None,
            },
        );
        Ok(summary)
    }

    pub fn connect_http_session(
        &mut self,
        server_id: impl Into<String>,
        endpoint: HttpEndpoint<'_>,
        policy: HostPolicy,
    ) -> Result<Value, WireError> {
        let id = server_id.into();
        self.ensure_vacant(&id)?;
        let caps = policy.advertised_capabilities();
        let session = Session::open_with_client_info(
            endpoint.addr,
            endpoint.bearer,
            &caps,
            (&policy.identity.name, &policy.identity.version),
            endpoint.timeout,
        )?;
        let summary = json!({"serverId":id,"transport":"streamable-http-session","sessionId":session.session_id(),"protocolVersion":session.protocol_version(),"serverInfo":session.server_info(),"capabilities":session.server_capabilities()});
        self.servers.insert(
            id,
            HostedServer {
                backend: PeerBackend::Http(Box::new(session), endpoint.addr.to_string()),
                policy,
                notifications: Vec::new(),
                task_store: None,
            },
        );
        Ok(summary)
    }

    pub fn connect_duplex(
        &mut self,
        server_id: impl Into<String>,
        streams: DuplexStreams,
        mut policy: HostPolicy,
    ) -> Result<Value, WireError> {
        let id = server_id.into();
        // Own cleanup before vacancy validation or any fallible handshake I/O.
        let mut peer = DuplexPeer::new(streams);
        self.ensure_vacant(&id)?;
        let mut notifications = Vec::new();
        peer.initialize(&mut policy, &mut notifications)?;
        let summary = json!({"serverId":id,"transport":"duplex-stdio","protocolVersion":peer.version,"serverInfo":peer.server_info,"capabilities":peer.server_capabilities});
        self.servers.insert(
            id,
            HostedServer {
                backend: PeerBackend::Duplex(peer),
                policy,
                notifications,
                task_store: None,
            },
        );
        Ok(summary)
    }

    pub fn attach_task_store(
        &mut self,
        server_id: &str,
        store: PersistedTaskHandleStore,
    ) -> Result<(), WireError> {
        self.srv_mut(server_id)?.task_store = Some(store);
        Ok(())
    }

    pub fn disconnect(&mut self, server_id: &str) -> Result<(), WireError> {
        let srv = self
            .servers
            .remove(server_id)
            .ok_or_else(|| WireError::Protocol(format!("unknown server `{server_id}`")))?;
        if let PeerBackend::Http(session, _) = srv.backend {
            (*session).close()?;
        }
        Ok(())
    }

    #[must_use]
    pub fn server_ids(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }

    #[must_use]
    pub fn snapshot(&self) -> Value {
        let entries: Vec<Value> = self.servers.iter().map(|(id, srv)| {
            let ident = json!({"name":srv.policy.identity.name,"version":srv.policy.identity.version,"principal":srv.policy.identity.principal});
            let caps = srv.policy.advertised_capabilities();
            let tasks_n = srv.task_store.as_ref().map_or(0, |st| st.list().len());
            let roots_n = srv.policy.roots.as_ref().map_or(0, Vec::len);
            match &srv.backend {
                PeerBackend::Http(s, addr) => json!({"serverId":id,"transport":"http","endpoint":addr,"sessionId":s.session_id(),"protocolVersion":s.protocol_version(),"clientIdentity":ident,"advertisedCapabilities":caps,"allowedRootsCount":roots_n,"persistedTaskCount":tasks_n,"serverInfo":s.server_info(),"serverCapabilities":s.server_capabilities(),"subscribedResources":s.subscribed_resources(),"bufferedNotifications":srv.notifications.len()}),
                PeerBackend::Duplex(p) => json!({"serverId":id,"transport":"duplex","sessionId":"","protocolVersion":p.version,"clientIdentity":ident,"advertisedCapabilities":caps,"allowedRootsCount":roots_n,"persistedTaskCount":tasks_n,"serverInfo":p.server_info,"serverCapabilities":p.server_capabilities,"subscribedResources":p.subscribed_uris,"bufferedNotifications":srv.notifications.len()}),
            }
        }).collect();
        let limits = json!({"maxBufferedNotifications":MAX_BUFFERED_NOTIFICATIONS,"maxInterleavedFrames":MAX_INTERLEAVED_FRAMES,"maxPaginationPages":MAX_PAGINATION_PAGES});
        json!({ "serverCount": entries.len(), "limits": limits, "servers": entries })
    }

    fn srv_mut(&mut self, server_id: &str) -> Result<&mut HostedServer, WireError> {
        self.servers
            .get_mut(server_id)
            .ok_or_else(|| WireError::Protocol(format!("unknown server `{server_id}`")))
    }

    pub fn discover(&mut self, server_id: &str) -> Result<Value, WireError> {
        match &mut self.srv_mut(server_id)?.backend {
            PeerBackend::Http(s, _) => s.discover(),
            PeerBackend::Duplex(p) => Ok(
                json!({"protocolVersion":p.version,"serverInfo":p.server_info,"capabilities":p.server_capabilities}),
            ),
        }
    }

    pub fn list_tools(&mut self, server_id: &str) -> Result<Vec<Value>, WireError> {
        self.srv_mut(server_id)?
            .list_paginated("tools/list", "tools")
    }

    /// List duplex tools within a cumulative item and serialized-definition budget.
    /// Bytes are the sum of each tool's JSON encoding, excluding page envelopes
    /// and array separators. Check each borrowed page before cloning its tools;
    /// an exceeded budget stops pagination. The existing frame cap still applies.
    /// HTTP servers are refused before listing; [`Self::list_tools`] is unchanged.
    pub fn list_tools_bounded(
        &mut self,
        server_id: &str,
        max_items: usize,
        max_bytes: usize,
    ) -> Result<Vec<Value>, WireError> {
        self.srv_mut(server_id)?.list_paginated_with_budget(
            "tools/list",
            "tools",
            Some(ListBudget {
                items: max_items,
                bytes: max_bytes,
            }),
        )
    }

    pub fn call_tool(
        &mut self,
        server_id: &str,
        name: &str,
        args: &Value,
        progress_token: Option<&Value>,
    ) -> Result<Value, WireError> {
        let srv = self.srv_mut(server_id)?;
        let caps = srv.policy.advertised_capabilities();
        let params = json!({ "name": name, "arguments": args });
        let result = match &mut srv.backend {
            PeerBackend::Http(s, _) => {
                let (policy_ref, notes_ref) = (&mut srv.policy, &mut srv.notifications);
                let mut note_cb = |n: &Value| push_bounded_notification(notes_ref, n.clone());
                let mut srv_cb = |m: &str, p: &Value| policy_ref.dispatch_mrtr(m, p);
                s.request_host(
                    "tools/call",
                    &params,
                    HostCall {
                        client_capabilities: &caps,
                        progress_token,
                        on_notification: &mut note_cb,
                        on_server_request: &mut srv_cb,
                    },
                )?
            }
            PeerBackend::Duplex(p) => p.request(
                "tools/call",
                &params,
                progress_token,
                (&mut srv.policy, &mut srv.notifications),
            )?,
        };
        if !srv.policy.enable_tasks
            && result.get("resultType").and_then(Value::as_str) == Some("task")
        {
            return Err(WireError::Protocol(
                "server returned unexpected CreateTaskResult when tasks were disabled".into(),
            ));
        }
        Ok(result)
    }

    pub fn call_tool_mrtr(
        &mut self,
        server_id: &str,
        name: &str,
        args: &Value,
    ) -> Result<Value, WireError> {
        let srv = self.srv_mut(server_id)?;
        let mut policy_caps = srv.policy.advertised_capabilities();
        if let Some(exts) = policy_caps
            .get_mut("extensions")
            .and_then(Value::as_object_mut)
        {
            exts.remove(TASKS_EXTENSION_ID);
        }
        match &mut srv.backend {
            PeerBackend::Http(s, _) => {
                let policy_ref = &mut srv.policy;
                s.call_tool_with_mrtr(name, args, &policy_caps, |entries| {
                    policy_ref.fulfill_input_requests(entries)
                })
            }
            PeerBackend::Duplex(p) => p.request(
                "tools/call",
                &json!({ "name": name, "arguments": args }),
                None,
                (&mut srv.policy, &mut srv.notifications),
            ),
        }
    }

    pub fn call_tool_resolving_tasks(
        &mut self,
        server_id: &str,
        name: &str,
        args: &Value,
        max_wait: Option<Duration>,
    ) -> Result<Value, WireError> {
        let srv = self.srv_mut(server_id)?;
        if !srv.policy.enable_tasks {
            return Err(WireError::Protocol(format!(
                "tasks are not enabled on host policy for `{server_id}`"
            )));
        }
        let PeerBackend::Http(s, addr) = &mut srv.backend else {
            return Err(WireError::Protocol(
                "ext-tasks polling requires an HTTP session".into(),
            ));
        };
        let notes_ref = &mut srv.notifications;
        match s.call_tool_with_tasks(name, args, true, &mut |n| {
            push_bounded_notification(notes_ref, n.clone())
        })? {
            CallToolOutcome::Complete(val) => Ok(val),
            CallToolOutcome::Task(handle) => {
                if let Some(store) = &mut srv.task_store {
                    store.save(PersistedTaskHandle::from_handle(
                        addr,
                        s.protocol_version(),
                        &handle,
                    ))?;
                }
                let policy_ref = &mut srv.policy;
                let snapshot = s.poll_task_until_done(&handle, max_wait, |entries| {
                    policy_ref.fulfill_input_requests(entries)
                })?;
                if let Some(store) = &mut srv.task_store {
                    let _ = store.update_from_snapshot(&snapshot)?;
                }
                settle_task_snapshot(snapshot)
            }
        }
    }

    pub fn resume_persisted_task(
        &mut self,
        server_id: &str,
        task_id: &str,
        max_wait: Option<Duration>,
    ) -> Result<Value, WireError> {
        let srv = self.srv_mut(server_id)?;
        if !srv.policy.enable_tasks {
            return Err(WireError::Protocol(format!(
                "tasks are not enabled on host policy for `{server_id}`"
            )));
        }
        let persisted = srv
            .task_store
            .as_ref()
            .and_then(|st| st.get(task_id))
            .cloned()
            .ok_or_else(|| {
                WireError::Protocol(format!(
                    "persisted task `{task_id}` not found on `{server_id}`"
                ))
            })?;
        let PeerBackend::Http(s, addr) = &mut srv.backend else {
            return Err(WireError::Protocol(
                "ext-tasks resume requires an HTTP session".into(),
            ));
        };
        if persisted.addr != *addr {
            return Err(WireError::Protocol(format!(
                "persisted task `{task_id}` bound to `{}`, cannot resume against `{addr}`",
                persisted.addr
            )));
        }
        let handle = persisted.to_task_handle();
        let policy_ref = &mut srv.policy;
        let snapshot = s.poll_task_until_done(&handle, max_wait, |entries| {
            policy_ref.fulfill_input_requests(entries)
        })?;
        if let Some(store) = &mut srv.task_store {
            let _ = store.update_from_snapshot(&snapshot)?;
        }
        settle_task_snapshot(snapshot)
    }

    pub fn update_task(
        &mut self,
        server_id: &str,
        task_id: &str,
        input_responses: &Value,
    ) -> Result<(), WireError> {
        let PeerBackend::Http(s, _) = &mut self.srv_mut(server_id)?.backend else {
            return Err(WireError::Protocol(
                "tasks/update requires HTTP session".into(),
            ));
        };
        s.update_task(task_id, input_responses)
    }

    pub fn start_task_tool(
        &mut self,
        server_id: &str,
        name: &str,
        args: &Value,
    ) -> Result<TaskHandle, WireError> {
        let srv = self.srv_mut(server_id)?;
        if !srv.policy.enable_tasks {
            return Err(WireError::Protocol(format!(
                "tasks are not enabled on host policy for `{server_id}`"
            )));
        }
        let PeerBackend::Http(s, addr) = &mut srv.backend else {
            return Err(WireError::Protocol(
                "ext-tasks requires an HTTP session".into(),
            ));
        };
        let notes_ref = &mut srv.notifications;
        match s.call_tool_with_tasks(name, args, true, &mut |n| {
            push_bounded_notification(notes_ref, n.clone())
        })? {
            CallToolOutcome::Task(handle) => {
                if let Some(store) = &mut srv.task_store {
                    store.save(PersistedTaskHandle::from_handle(
                        addr,
                        s.protocol_version(),
                        &handle,
                    ))?;
                }
                Ok(handle)
            }
            CallToolOutcome::Complete(_) => Err(WireError::Protocol(format!(
                "tool `{name}` completed synchronously instead of returning a task handle"
            ))),
        }
    }

    pub fn get_task(&mut self, server_id: &str, task_id: &str) -> Result<TaskSnapshot, WireError> {
        let srv = self.srv_mut(server_id)?;
        let PeerBackend::Http(s, _) = &mut srv.backend else {
            return Err(WireError::Protocol(
                "tasks/get requires HTTP session".into(),
            ));
        };
        let snap = s.get_task(task_id)?;
        if let Some(store) = &mut srv.task_store {
            let _ = store.update_from_snapshot(&snap)?;
        }
        Ok(snap)
    }

    pub fn cancel_task(&mut self, server_id: &str, task_id: &str) -> Result<(), WireError> {
        let PeerBackend::Http(s, _) = &mut self.srv_mut(server_id)?.backend else {
            return Err(WireError::Protocol(
                "tasks/cancel requires HTTP session".into(),
            ));
        };
        s.cancel_task(task_id)
    }

    pub fn list_resources(&mut self, server_id: &str) -> Result<Vec<Value>, WireError> {
        self.srv_mut(server_id)?
            .list_paginated("resources/list", "resources")
    }

    pub fn list_resource_templates(&mut self, server_id: &str) -> Result<Vec<Value>, WireError> {
        self.srv_mut(server_id)?
            .list_paginated("resources/templates/list", "resourceTemplates")
    }

    pub fn read_resource(&mut self, server_id: &str, uri: &str) -> Result<Value, WireError> {
        let srv = self.srv_mut(server_id)?;
        match &mut srv.backend {
            PeerBackend::Http(s, _) => s.read_resource(uri),
            PeerBackend::Duplex(p) => p.request(
                "resources/read",
                &json!({ "uri": uri }),
                None,
                (&mut srv.policy, &mut srv.notifications),
            ),
        }
    }

    pub fn subscribe_resource(&mut self, server_id: &str, uri: &str) -> Result<(), WireError> {
        let srv = self.srv_mut(server_id)?;
        match &mut srv.backend {
            PeerBackend::Http(s, _) => s.subscribe_resource(uri),
            PeerBackend::Duplex(p) => {
                let _ = p.request(
                    "resources/subscribe",
                    &json!({ "uri": uri }),
                    None,
                    (&mut srv.policy, &mut srv.notifications),
                )?;
                p.subscribed_uris.insert(uri.to_string());
                Ok(())
            }
        }
    }

    pub fn unsubscribe_resource(&mut self, server_id: &str, uri: &str) -> Result<(), WireError> {
        let srv = self.srv_mut(server_id)?;
        match &mut srv.backend {
            PeerBackend::Http(s, _) => s.unsubscribe_resource(uri),
            PeerBackend::Duplex(p) => {
                let _ = p.request(
                    "resources/unsubscribe",
                    &json!({ "uri": uri }),
                    None,
                    (&mut srv.policy, &mut srv.notifications),
                )?;
                p.subscribed_uris.remove(uri);
                Ok(())
            }
        }
    }

    pub fn open_subscription_stream(
        &mut self,
        server_id: &str,
        notifications_filter: &Value,
    ) -> Result<SubscriptionStream, WireError> {
        let srv = self.srv_mut(server_id)?;
        let caps = srv.policy.advertised_capabilities();
        let PeerBackend::Http(s, _) = &mut srv.backend else {
            return Err(WireError::Protocol(
                "SSE subscription stream requires an HTTP session".into(),
            ));
        };
        s.open_subscription_stream(notifications_filter, &caps)
    }

    pub fn list_prompts(&mut self, server_id: &str) -> Result<Vec<Value>, WireError> {
        self.srv_mut(server_id)?
            .list_paginated("prompts/list", "prompts")
    }

    pub fn get_prompt(
        &mut self,
        server_id: &str,
        name: &str,
        arguments: Option<&Value>,
    ) -> Result<Value, WireError> {
        let srv = self.srv_mut(server_id)?;
        match &mut srv.backend {
            PeerBackend::Http(s, _) => s.get_prompt(name, arguments),
            PeerBackend::Duplex(p) => {
                let params = match arguments {
                    Some(a) => json!({ "name": name, "arguments": a }),
                    None => json!({ "name": name }),
                };
                p.request(
                    "prompts/get",
                    &params,
                    None,
                    (&mut srv.policy, &mut srv.notifications),
                )
            }
        }
    }

    pub fn complete(
        &mut self,
        server_id: &str,
        query: CompletionQuery<'_>,
    ) -> Result<Value, WireError> {
        let srv = self.srv_mut(server_id)?;
        match &mut srv.backend {
            PeerBackend::Http(s, _) if query.is_resource => s.complete_resource(
                query.target,
                query.arg_name,
                query.arg_value,
                query.context_args,
            ),
            PeerBackend::Http(s, _) => s.complete_prompt(
                query.target,
                query.arg_name,
                query.arg_value,
                query.context_args,
            ),
            PeerBackend::Duplex(p) => {
                let r = if query.is_resource {
                    json!({ "type": "ref/resource", "uri": query.target })
                } else {
                    json!({ "type": "ref/prompt", "name": query.target })
                };
                let mut params =
                    json!({"ref":r,"argument":{"name":query.arg_name,"value":query.arg_value}});
                if let Some(ctx) = query.context_args {
                    params["context"] = json!({ "arguments": ctx });
                }
                p.request(
                    "completion/complete",
                    &params,
                    None,
                    (&mut srv.policy, &mut srv.notifications),
                )
            }
        }
    }

    pub fn inspect_app(
        &mut self,
        server_id: &str,
        uri: &str,
    ) -> Result<AppResourceSummary, WireError> {
        if !uri.starts_with("ui://") {
            return Err(WireError::Protocol(format!(
                "MCP Apps resource URI must use `ui://` scheme, got {uri:?}"
            )));
        }
        let read = self.read_resource(server_id, uri)?;
        parse_app_resource_summary(uri, &read)
    }

    pub fn update_roots(
        &mut self,
        server_id: &str,
        roots: Vec<RootEntry>,
    ) -> Result<(), WireError> {
        let srv = self.srv_mut(server_id)?;
        srv.policy.roots = Some(roots);
        match &mut srv.backend {
            PeerBackend::Http(s, _) => s.notify_roots_list_changed(),
            PeerBackend::Duplex(p) => p.notify("notifications/roots/list_changed", None),
        }
    }

    pub fn cancel_request(
        &mut self,
        server_id: &str,
        request_id: Value,
        reason: Option<&str>,
    ) -> Result<(), WireError> {
        let srv = self.srv_mut(server_id)?;
        match &mut srv.backend {
            PeerBackend::Http(s, _) => s.notify_cancelled(request_id, reason),
            PeerBackend::Duplex(p) => {
                let params = match reason {
                    Some(r) => json!({ "requestId": request_id, "reason": r }),
                    None => json!({ "requestId": request_id }),
                };
                p.notify("notifications/cancelled", Some(params))
            }
        }
    }

    pub fn drain_notifications(&mut self, server_id: &str) -> Result<Vec<Value>, WireError> {
        Ok(std::mem::take(&mut self.srv_mut(server_id)?.notifications))
    }
}

fn settle_task_snapshot(snapshot: TaskSnapshot) -> Result<Value, WireError> {
    match snapshot.handle.status {
        TaskStatus::Completed => snapshot
            .result
            .ok_or_else(|| WireError::Protocol("completed task missing result".into())),
        TaskStatus::Failed => {
            let code = snapshot
                .error
                .as_ref()
                .and_then(|e| e.get("code"))
                .and_then(Value::as_i64)
                .unwrap_or(-32603);
            let msg = snapshot
                .error
                .as_ref()
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("task failed")
                .to_string();
            Err(WireError::Rpc(code, msg))
        }
        other @ (TaskStatus::Working | TaskStatus::InputRequired | TaskStatus::Cancelled) => {
            Err(WireError::Protocol(format!(
                "task {} settled in non-completed status {}",
                snapshot.handle.task_id,
                other.as_str()
            )))
        }
    }
}

#[cfg(test)]
#[path = "tests/host.rs"]
mod tests;
